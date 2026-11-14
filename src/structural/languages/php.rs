// Copyright 2026 Muvon Un Limited
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! PHP grammar: functions, methods and type containers, plus the graph
//! methods (use statements, calls, extends/implements, PSR-4 style
//! namespace resolution).

use super::javascript::{join_path, parent_dir};
use super::{parse_call_target, path_ends_with_components, simple_type_name, CallTarget, Language, TypeRelationKind};
use crate::structural::graph::registry::{normalize_separators, resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Php {}

/// Directories a namespace is mapped into, PSR-4 style.
const NAMESPACE_ROOT_DIRS: [&str; 3] = ["src", "lib", "app"];

impl Language for Php {
	fn name(&self) -> &'static str {
		"php"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_php::LANGUAGE_PHP.into()
	}

	// `use App\Models\User;`; the names of a group `use App\{` sit on lines
	// of their own, which the default name-list rule accepts.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&["use", "\\", "require", "include"]
	}

	// `class A extends B implements C,` newline `D`.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["extends", "implements"]
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The type containers are restored (chunking drops them as too
		// large); namespaces and use statements are dropped — a namespace is
		// one per file and a use declares nothing.
		vec![
			"function_definition",
			"method_declaration",
			"class_declaration",
			"interface_declaration",
			"trait_declaration",
			"enum_declaration",
		]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		self.get_outline_kinds()
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			// `use Namespace\Class;` / `use Namespace\Class as Alias;`
			"namespace_use_declaration" => {
				if let Ok(use_text) = node.utf8_text(contents.as_bytes()) {
					imports.extend(parse_php_use_statement(use_text));
				}
			}
			// Every top-level item is potentially exportable. A namespace's
			// name field is `namespace_name`, not `name`.
			"function_definition"
			| "method_declaration"
			| "class_declaration"
			| "interface_declaration"
			| "trait_declaration"
			| "enum_declaration"
			| "namespace_definition" => {
				let mut cursor = node.walk();
				let name_node = node
					.children(&mut cursor)
					.find(|child| matches!(child.kind(), "name" | "namespace_name"));
				if let Some(name_node) = name_node {
					if let Ok(name) = name_node.utf8_text(contents.as_bytes()) {
						exports.push(name.to_string());
					}
				}
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		let target = match node.kind() {
			"function_call_expression" => node
				.child_by_field_name("function")
				.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
				// `\App\helper()`: the namespace is not part of the callee name.
				.and_then(|text| parse_call_target(last_namespace_segment(text))),
			"member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
				method_call_target(node, contents)
			}
			_ => None,
		};
		target.into_iter().collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// `class Foo extends Bar implements I, J { }`
		// `interface A extends B, C { }`
		// Backed enums can implement interfaces (but not extend).
		let mut relations = Vec::new();
		let mut cursor = node.walk();
		match node.kind() {
			"class_declaration" => {
				for child in node.children(&mut cursor) {
					match child.kind() {
						"base_clause" => {
							collect_clause_names(child, contents, TypeRelationKind::Extends, &mut relations)
						}
						"class_interface_clause" => {
							collect_clause_names(child, contents, TypeRelationKind::Implements, &mut relations)
						}
						_ => {}
					}
				}
			}
			"interface_declaration" => {
				for child in node.children(&mut cursor) {
					if child.kind() == "base_clause" {
						collect_clause_names(child, contents, TypeRelationKind::Extends, &mut relations);
					}
				}
			}
			"enum_declaration" => {
				for child in node.children(&mut cursor) {
					if child.kind() == "class_interface_clause" {
						collect_clause_names(child, contents, TypeRelationKind::Implements, &mut relations);
					}
				}
			}
			_ => {}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			return find_matching_php_file(&resolve_relative_path(source_file, import_path), all_files);
		}

		// A bare file name (`Config.php`) is looked up next to the source first.
		if import_path.ends_with(".php") || !import_path.contains('/') {
			let target = join_path(parent_dir(source_file), import_path);
			if let Some(found) = find_matching_php_file(&target, all_files) {
				return Some(found);
			}
		}

		resolve_namespace_import(&normalize_separators(import_path), source_file, all_files)
	}
}

/// The text after the last `\` of a namespaced name (`\App\Foo` -> `Foo`).
fn last_namespace_segment(text: &str) -> &str {
	text.rsplit('\\').next().unwrap_or(text)
}

/// `$obj->run()`, `Foo::run()` and `\App\Foo::run()`: the method name with the
/// receiver (`$obj`, `Foo`) as qualifier. A receiver that is not a plain
/// name or variable (`getRepo()->save()`) leaves the call unqualified
/// instead of dropping it.
fn method_call_target(node: Node, contents: &str) -> Option<CallTarget> {
	let name = node.child_by_field_name("name")?.utf8_text(contents.as_bytes()).ok()?;
	let receiver_field = if node.kind() == "scoped_call_expression" {
		"scope"
	} else {
		"object"
	};
	let qualified = node
		.child_by_field_name(receiver_field)
		.and_then(|receiver| receiver.utf8_text(contents.as_bytes()).ok())
		.and_then(|receiver| parse_call_target(&format!("{}.{name}", last_namespace_segment(receiver))));
	qualified.or_else(|| parse_call_target(name))
}

/// Full namespace paths of a use statement, backslashes turned into `/` so
/// `resolve_namespace_import` can match them against file paths. Handles
/// `use A\B\C;`, `use A\B\C as Alias;`, `use function A\f;` and
/// `use A\B\{C, D as E};`.
fn parse_php_use_statement(use_text: &str) -> Vec<String> {
	// `use function` and `use const` are import forms too; leaving the
	// keyword in place yields an unresolvable path.
	let Some(after_use) = use_text.trim().strip_prefix("use ") else {
		return Vec::new();
	};
	let after_use = after_use.trim_start();
	let rest = after_use
		.strip_prefix("function ")
		.or_else(|| after_use.strip_prefix("const "))
		.unwrap_or(after_use)
		.trim()
		.trim_end_matches(';');

	// Grouped: `Prefix\{A, B as C, D}`
	if let (Some(brace_start), Some(brace_end)) = (rest.find('{'), rest.rfind('}')) {
		if brace_end <= brace_start {
			return Vec::new();
		}
		let prefix = rest[..brace_start].trim_end_matches('\\').replace('\\', "/");
		return rest[brace_start + 1..brace_end]
			.split(',')
			.map(|item| {
				let item = item.trim();
				let path = item
					.find(" as ")
					.map_or(item, |as_pos| &item[..as_pos])
					.replace('\\', "/");
				if prefix.is_empty() {
					path
				} else {
					format!("{prefix}/{path}")
				}
			})
			.filter(|full| !full.is_empty())
			.collect();
	}

	// Simple: `A\B\C` or `A\B\C as Alias`
	let path = rest.find(" as ").map_or(rest, |as_pos| &rest[..as_pos]);
	let normalized = path.replace('\\', "/");
	if normalized.is_empty() {
		return Vec::new();
	}
	vec![normalized]
}

/// Match `target` against the registry as is and with `.php`. Exact only: a
/// file that merely shares the name is not a match.
fn find_matching_php_file(target: &str, registry: &FileRegistry) -> Option<String> {
	if let Some(found) = registry.find_normalized(target) {
		return Some(found.to_string());
	}

	if !target.ends_with(".php") {
		if let Some(found) = registry.find_normalized(&format!("{target}.php")) {
			return Some(found.to_string());
		}
	}

	None
}

/// PSR-4 style resolution: prefixes of the namespace path, longest first,
/// mapped through the common source directories, directly and relative to
/// the importing file.
fn resolve_namespace_import(file_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let source_dir = parent_dir(source_file);
	let namespace_parts: Vec<&str> = file_path.split('/').collect();
	let vendor = namespace_parts.first().unwrap_or(&"").to_lowercase();
	let vendor_rest = namespace_parts[1..].join("/");

	for end_idx in (1..=namespace_parts.len()).rev() {
		let partial_path = namespace_parts[0..end_idx].join("/");
		let lowercase_path = partial_path.to_lowercase();

		let mut candidates = Vec::new();
		// App\Config -> src/App/Config.php
		for root_dir in NAMESPACE_ROOT_DIRS {
			candidates.push(format!("{root_dir}/{partial_path}.php"));
		}
		// App\Config -> src/app/config.php
		for root_dir in NAMESPACE_ROOT_DIRS {
			candidates.push(format!("{root_dir}/{lowercase_path}.php"));
		}
		candidates.push(format!("{partial_path}.php"));
		candidates.push(format!("{partial_path}/index.php"));
		// Vendor\Package\Class -> vendor/package/src/Class.php
		candidates.push(format!("vendor/{lowercase_path}.php"));
		candidates.push(format!("vendor/{vendor}/src/{vendor_rest}.php"));

		for candidate in &candidates {
			if let Some(found) = registry.find_exact_file(candidate) {
				return Some(found);
			}
			if let Some(found) = find_matching_php_file(&join_path(source_dir, candidate), registry) {
				return Some(found);
			}
		}
	}

	resolve_namespace_suffix(&namespace_parts, registry)
}

/// A namespace whose root maps to an unusual source directory: the longest
/// tail of its path (at least two segments, so `Models/User.php` but never a
/// bare `User.php`) that a file path ends with on a component boundary. The
/// alphabetically first such file wins so the answer is stable.
fn resolve_namespace_suffix(namespace_parts: &[&str], registry: &FileRegistry) -> Option<String> {
	const MIN_SUFFIX_SEGMENTS: usize = 2;
	if namespace_parts.len() < MIN_SUFFIX_SEGMENTS {
		return None;
	}
	let class_name = namespace_parts.last()?;
	let file_name = format!("{class_name}.php");
	let candidates = registry.files_named(&file_name);
	if candidates.is_empty() {
		return None;
	}
	let max_start = namespace_parts.len() - MIN_SUFFIX_SEGMENTS;
	for start in 0..=max_start {
		let suffix = format!("{}.php", namespace_parts[start..].join("/"));
		if let Some(found) = candidates
			.iter()
			.filter(|file| path_ends_with_components(file, &suffix))
			.min()
		{
			return Some(found.clone());
		}
	}
	None
}

/// Pull type names out of a PHP `base_clause` (extends) or
/// `class_interface_clause` (implements).
fn collect_clause_names(
	clause: Node,
	contents: &str,
	kind: TypeRelationKind,
	relations: &mut Vec<(TypeRelationKind, String)>,
) {
	let mut cursor = clause.walk();
	for child in clause.children(&mut cursor) {
		if matches!(child.kind(), "name" | "qualified_name" | "relative_name") {
			if let Ok(text) = child.utf8_text(contents.as_bytes()) {
				if let Some(name) = simple_type_name(text) {
					relations.push((kind, name));
				}
			}
		}
	}
}

#[cfg(test)]
#[path = "php_tests.rs"]
mod php_tests;
