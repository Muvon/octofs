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

//! Ruby grammar: methods, singleton methods, classes and modules, plus the
//! graph facts (require imports, calls, superclass relations).

use super::{
	extract_symbol_by_kinds, parse_call_target, path_ends_with_components, simple_type_name, CallTarget, Language,
	TypeRelationKind,
};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Ruby {}

impl Language for Ruby {
	fn name(&self) -> &'static str {
		"ruby"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_ruby::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `call` is dropped: a call's method-name child matches the default
		// name scan, which would turn every call site into a bogus item.
		vec!["method", "singleton_method", "class", "module"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if matches!(node.kind(), "class" | "module") {
			// Names are `constant` nodes; qualified names (`Foo::Bar`) are
			// `scope_resolution` nodes whose LAST `constant` is the defined one.
			let mut cursor = node.walk();
			for child in node.children(&mut cursor) {
				let name_node = match child.kind() {
					"constant" => Some(child),
					"scope_resolution" => {
						let mut inner = child.walk();
						child.children(&mut inner).filter(|c| c.kind() == "constant").last()
					}
					_ => None,
				};
				if let Some(name_node) = name_node {
					return name_node.utf8_text(contents.as_bytes()).ok().map(String::from);
				}
			}
			return None;
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		vec!["method", "singleton_method", "class", "module"]
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		if node.kind() == "call" {
			if let Ok(call_text) = node.utf8_text(contents.as_bytes()) {
				if let Some(required_file) = parse_ruby_require(call_text) {
					imports.push(required_file);
				}
			}
		}
		// Ruby has no explicit exports.
		(imports, Vec::new())
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "call" {
			return Vec::new();
		}
		// The `method` field is the callee; for `receiver.method(...)` the first
		// identifier child would be the receiver.
		let Some(method) = node.child_by_field_name("method") else {
			return Vec::new();
		};
		let Ok(text) = method.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		let name = text.trim();
		// require/require_relative/load are imports, not calls.
		if matches!(name, "require" | "require_relative" | "load") {
			return Vec::new();
		}
		let raw = node
			.child_by_field_name("receiver")
			.and_then(|receiver| receiver.utf8_text(contents.as_bytes()).ok())
			.map(|receiver| format!("{receiver}.{name}"))
			.unwrap_or_else(|| name.to_string());
		parse_call_target(&raw).into_iter().collect()
	}

	// A class reports its superclass (Extends) and the modules it mixes in
	// (`include`/`extend`/`prepend`, reported as Implements); a module only
	// the latter. The relation source is the class or module itself.
	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let mut out = Vec::new();
		if !matches!(node.kind(), "class" | "module") {
			return out;
		}
		if let Some(superclass) = node.child_by_field_name("superclass") {
			let mut cursor = superclass.walk();
			for child in superclass.children(&mut cursor) {
				push_constant_relation(child, contents, TypeRelationKind::Extends, &mut out);
			}
		}
		collect_mixins(node, contents, &mut out);
		out
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if let Some(relative_path) = import_path.strip_prefix("relative:") {
			resolve_relative_require(relative_path, source_file, all_files)
		} else if import_path.starts_with("./") || import_path.starts_with("../") {
			resolve_relative_require(import_path, source_file, all_files)
		} else {
			resolve_absolute_require(import_path, all_files)
		}
	}
}

const RUBY_EXTENSION: &str = "rb";

/// Conventional Ruby load paths probed for a bare `require`.
const LOAD_PATHS: [&str; 3] = ["lib", "app", "config"];

/// Method names that mix a module into the enclosing class or module.
const MIXIN_METHODS: [&str; 3] = ["include", "extend", "prepend"];

/// `include Mod` / `extend Mod` / `prepend Mod` statements directly in the
/// body of a class or module (not in a nested one: it reports its own).
fn collect_mixins(container: Node, contents: &str, out: &mut Vec<(TypeRelationKind, String)>) {
	// Newer grammars wrap the statements in a `body_statement`; older ones
	// put them straight under the container.
	let mut statements: Vec<Node> = Vec::new();
	let mut cursor = container.walk();
	for child in container.children(&mut cursor) {
		if child.kind() == "body_statement" {
			let mut body_cursor = child.walk();
			statements.extend(child.children(&mut body_cursor));
		} else {
			statements.push(child);
		}
	}
	for statement in statements.into_iter().filter(|statement| statement.kind() == "call") {
		if statement.child_by_field_name("receiver").is_some() {
			continue;
		}
		let is_mixin = statement
			.child_by_field_name("method")
			.and_then(|method| method.utf8_text(contents.as_bytes()).ok())
			.is_some_and(|method| MIXIN_METHODS.contains(&method));
		let Some(arguments) = statement.child_by_field_name("arguments") else {
			continue;
		};
		if !is_mixin {
			continue;
		}
		let mut argument_cursor = arguments.walk();
		for argument in arguments.children(&mut argument_cursor) {
			push_constant_relation(argument, contents, TypeRelationKind::Implements, out);
		}
	}
}

/// A `constant` or `Outer::Name` node as a relation to its simple name.
fn push_constant_relation(
	node: Node,
	contents: &str,
	kind: TypeRelationKind,
	out: &mut Vec<(TypeRelationKind, String)>,
) {
	if matches!(node.kind(), "constant" | "scope_resolution") {
		if let Some(name) = node.utf8_text(contents.as_bytes()).ok().and_then(simple_type_name) {
			out.push((kind, name));
		}
	}
}

fn parse_ruby_require(call_text: &str) -> Option<String> {
	let trimmed = call_text.trim();
	if let Some(rest) = trimmed.strip_prefix("require ") {
		if let Some(filename) = extract_ruby_string_literal(rest) {
			return Some(filename);
		}
	}
	if let Some(rest) = trimmed.strip_prefix("require_relative ") {
		if let Some(filename) = extract_ruby_string_literal(rest) {
			// Marked so the resolver anchors it at the requiring file.
			return Some(format!("relative:{filename}"));
		}
	}
	if let Some(rest) = trimmed.strip_prefix("load ") {
		if let Some(filename) = extract_ruby_string_literal(rest) {
			return Some(filename);
		}
	}
	None
}

fn extract_ruby_string_literal(text: &str) -> Option<String> {
	let text = text.trim();
	let inner = text
		.strip_prefix('"')
		.and_then(|rest| rest.strip_suffix('"'))
		.or_else(|| text.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')))?;
	Some(inner.to_string())
}

fn resolve_relative_require(import_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let relative_path = resolve_relative_path(source_file, import_path);
	registry.find_file_with_extensions(&relative_path, &[RUBY_EXTENSION])
}

fn resolve_absolute_require(import_path: &str, registry: &FileRegistry) -> Option<String> {
	if let Some(result) = registry.find_file_with_extensions(import_path, &[RUBY_EXTENSION]) {
		return Some(result);
	}
	for load_path in LOAD_PATHS {
		let full_path = format!("{load_path}/{import_path}");
		if let Some(result) = registry.find_file_with_extensions(&full_path, &[RUBY_EXTENSION]) {
			return Some(result);
		}
	}
	// Vendored gems: a file under `vendor/gems` whose path ends with the
	// required name on a component boundary.
	let vendored_suffix = format!("{import_path}.{RUBY_EXTENSION}");
	let file_name = vendored_suffix.rsplit('/').next()?;
	registry
		.files_named(file_name)
		.iter()
		.filter(|file| file.contains("vendor/gems") && path_ends_with_components(file, &vendored_suffix))
		.min()
		.cloned()
}

#[cfg(test)]
#[path = "ruby_tests.rs"]
mod ruby_tests;
