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

//! Rust grammar: items, types, traits, modules, constants and macros.

use super::{extract_symbol_by_kinds, parse_call_target, simple_type_name, CallTarget, Language, TypeRelationKind};
use crate::structural::graph::registry::{normalize_separators, FileRegistry};
use tree_sitter::Node;

/// File names whose child modules sit next to them rather than in a directory
/// named after the file.
const MODULE_ROOT_FILES: [&str; 3] = ["mod.rs", "lib.rs", "main.rs"];

pub(crate) struct Rust {}

impl Language for Rust {
	fn name(&self) -> &'static str {
		"rust"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// impl_item is left out: it declares no symbol of its own, its methods
		// do (owned by the impl's type).
		vec![
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"mod_item",
			"const_item",
			"macro_definition",
		]
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// impl_item is included: the impl header line is an item, so methods of
		// different impls are distinguishable in an outline.
		vec![
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"impl_item",
			"mod_item",
			"const_item",
			"macro_definition",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			// `impl Trait for Type` names both sides; the type alone would
			// collide with the inherent impl of the same type.
			let type_name = node
				.child_by_field_name("type")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name)?;
			let trait_name = node
				.child_by_field_name("trait")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name);
			return Some(match trait_name {
				Some(trait_name) => format!("{trait_name} for {type_name}"),
				None => type_name,
			});
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"use_declaration" => {
				if let Ok(use_text) = node.utf8_text(contents.as_bytes()) {
					imports.extend(expand_rust_use_paths(use_text));
				}
			}
			"mod_item" => {
				if let Some(name) = declared_name(node, contents) {
					// A mod_item without a body is a file-level declaration
					// (`mod foo;`): it depends on foo.rs or foo/mod.rs, so it is
					// emitted as a self-relative import for resolve_import.
					if node.child_by_field_name("body").is_none() {
						imports.push(format!("self::{name}"));
					}
					if is_public(node, contents) {
						exports.push(name);
					}
				}
			}
			"function_item" | "struct_item" | "enum_item" | "trait_item" | "const_item" | "macro_definition"
				if is_public(node, contents) =>
			{
				exports.extend(declared_name(node, contents));
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		let callee = match node.kind() {
			"call_expression" => node.child_by_field_name("function"),
			// The macro name (`vec`, or a path such as `crate::utils::debug_log`)
			// without its `!`.
			"macro_invocation" => node.child_by_field_name("macro"),
			_ => None,
		};
		callee
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(parse_call_target)
			.into_iter()
			.collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let mut relations = Vec::new();
		match node.kind() {
			// `impl Trait for Type` makes Type implement Trait; an inherent
			// `impl Type {}` has no `trait` field and emits nothing.
			"impl_item" => {
				if let Some(name) = node
					.child_by_field_name("trait")
					.and_then(|trait_node| trait_node.utf8_text(contents.as_bytes()).ok())
					.and_then(simple_type_name)
				{
					relations.push((TypeRelationKind::Implements, name));
				}
			}
			// `trait A: B + C` makes A extend B and C.
			"trait_item" => {
				if let Some(bounds) = node.child_by_field_name("bounds") {
					let mut cursor = bounds.walk();
					for child in bounds.children(&mut cursor) {
						if matches!(
							child.kind(),
							"type_identifier" | "scoped_type_identifier" | "generic_type"
						) {
							if let Some(name) = child.utf8_text(contents.as_bytes()).ok().and_then(simple_type_name) {
								relations.push((TypeRelationKind::Extends, name));
							}
						}
					}
				}
			}
			_ => {}
		}
		relations
	}

	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			let type_node = node.child_by_field_name("type")?;
			return simple_type_name(type_node.utf8_text(contents.as_bytes()).ok()?);
		}
		self.extract_declaration_name(node, contents)
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		let source_file = normalize_separators(source_file);
		let module_dir = module_directory(&source_file);
		if let Some(module_path) = import_path.strip_prefix("crate::") {
			return resolve_crate_import(module_path, &source_file, all_files);
		}
		if let Some(module_path) = import_path.strip_prefix("self::") {
			// `self::x` names a child module of the current module (`mod x;`
			// lands here too), never the source file itself.
			return resolve_relative_import(module_path, &module_dir, all_files);
		}
		if import_path.starts_with("super::") {
			return resolve_super_import(import_path, &module_dir, all_files);
		}
		// A bare path is relative to the current module (`use child::Item`);
		// otherwise it is tried against the crate root, which finds nothing for
		// third-party crates.
		if let Some(found) = resolve_relative_import(import_path, &module_dir, all_files) {
			return Some(found);
		}
		import_path
			.contains("::")
			.then(|| resolve_crate_import(import_path, &source_file, all_files))
			.flatten()
	}
}

/// Text of the node's `name` field.
fn declared_name(node: Node, contents: &str) -> Option<String> {
	node.child_by_field_name("name")
		.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
		.map(str::to_string)
}

/// Whether the item carries a `pub` visibility modifier (`pub`, `pub(crate)`).
fn is_public(node: Node, contents: &str) -> bool {
	let mut cursor = node.walk();
	let modifier = node
		.children(&mut cursor)
		.find(|child| child.kind() == "visibility_modifier");
	modifier
		.and_then(|modifier| modifier.utf8_text(contents.as_bytes()).ok())
		.is_some_and(|text| text.contains("pub"))
}

/// Directory part of a forward-slash path; empty for a bare file name.
fn directory_of(path: &str) -> &str {
	path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn join_path(directory: &str, relative: &str) -> String {
	if directory.is_empty() {
		relative.to_string()
	} else {
		format!("{directory}/{relative}")
	}
}

/// Expand a Rust `use` statement into one fully qualified path per leaf:
/// `use crate::a::{B, c::D};` gives `crate::a::B` and `crate::a::c::D`, a glob
/// (`utils::*`) gives the module itself and an alias (`Arc as StdArc`) is
/// dropped.
fn expand_rust_use_paths(use_text: &str) -> Vec<String> {
	// A re-export carries a visibility prefix (`pub use`, `pub(crate) use`);
	// stripping only "use " would leave it unmatched and drop the import. The
	// prefix only counts when `pub` is a whole token, so `use public::thing;`
	// is left alone.
	let trimmed = use_text.trim();
	let without_visibility = trimmed
		.strip_prefix("pub")
		.filter(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('('))
		.map(|rest| {
			let rest = rest.trim_start();
			match rest.strip_prefix('(') {
				Some(inner) => inner.split_once(')').map_or(rest, |(_, tail)| tail).trim_start(),
				None => rest,
			}
		})
		.unwrap_or(trimmed);
	let cleaned = match without_visibility.strip_prefix("use ") {
		Some(rest) => rest.trim_end_matches(';').trim(),
		None => return Vec::new(),
	};
	let mut results = Vec::new();
	expand_use_tree(cleaned, "", &mut results);
	results
}

/// Expand a use-tree fragment under the accumulated `prefix`. An explicit
/// stack instead of recursion: brace groups can nest as deep as the source
/// likes, and the leaves must keep their source order.
fn expand_use_tree(fragment: &str, prefix: &str, out: &mut Vec<String>) {
	let mut pending: Vec<(&str, String)> = vec![(fragment, prefix.to_string())];
	while let Some((fragment, prefix)) = pending.pop() {
		let fragment = fragment.trim();

		if let Some(brace_start) = fragment.find('{') {
			let before_brace = fragment[..brace_start].trim_end_matches(':');
			let new_prefix = if prefix.is_empty() {
				before_brace.to_string()
			} else {
				format!("{prefix}::{before_brace}")
			};
			let inner = &fragment[brace_start + 1..];
			if let Some(brace_end) = find_matching_brace(inner) {
				// Pushed in reverse so the first item is expanded first.
				for item in split_top_level_commas(&inner[..brace_end]).into_iter().rev() {
					pending.push((item.trim(), new_prefix.clone()));
				}
			}
		} else {
			let without_alias = fragment.find(" as ").map_or(fragment, |position| &fragment[..position]);
			let without_glob = without_alias.trim_end_matches('*').trim_end_matches("::");
			if without_glob.is_empty() {
				continue;
			}
			out.push(if prefix.is_empty() {
				without_glob.to_string()
			} else {
				format!("{prefix}::{without_glob}")
			});
		}
	}
}

/// Index of the `}` that closes the `{` already consumed.
fn find_matching_brace(text: &str) -> Option<usize> {
	let mut depth = 1usize;
	for (index, character) in text.char_indices() {
		match character {
			'{' => depth += 1,
			'}' => {
				depth -= 1;
				if depth == 0 {
					return Some(index);
				}
			}
			_ => {}
		}
	}
	None
}

/// Split on commas that are not inside a nested brace group.
fn split_top_level_commas(text: &str) -> Vec<&str> {
	let mut parts = Vec::new();
	let mut depth = 0usize;
	let mut start = 0;
	for (index, character) in text.char_indices() {
		match character {
			'{' => depth += 1,
			'}' => depth = depth.saturating_sub(1),
			',' if depth == 0 => {
				parts.push(&text[start..index]);
				start = index + 1;
			}
			_ => {}
		}
	}
	parts.push(&text[start..]);
	parts
}

/// The crate root (`lib.rs` or `main.rs`) in the source file's directory or
/// the nearest ancestor that has one.
fn find_crate_root(source_file: &str, registry: &FileRegistry) -> Option<String> {
	let mut directory = directory_of(source_file);
	loop {
		for root_file in ["lib.rs", "main.rs"] {
			if let Some(found) = registry.find_exact_file(&join_path(directory, root_file)) {
				return Some(found);
			}
		}
		if directory.is_empty() {
			return None;
		}
		directory = directory_of(directory);
	}
}

/// Resolve `crate::module::Item`: the deepest module path that names a file,
/// as `module.rs` or `module/mod.rs` under the crate root's directory.
fn resolve_crate_import(module_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let parts: Vec<&str> = module_path.split("::").collect();
	let crate_root = find_crate_root(source_file, registry)?;
	let crate_dir = directory_of(&crate_root);

	for end in (1..=parts.len()).rev() {
		let module = parts[..end].join("/");
		if let Some(found) = registry.find_exact_file(&join_path(crate_dir, &format!("{module}.rs"))) {
			return Some(found);
		}
		if let Some(found) = registry.find_exact_file(&join_path(crate_dir, &format!("{module}/mod.rs"))) {
			return Some(found);
		}
	}

	// A single segment that matched no module file is an item declared at the
	// crate root, e.g. `crate::Url` for a `pub struct Url` in lib.rs. A longer
	// path that resolved nothing is a module we failed to find, and pointing
	// it at the root would invent an edge.
	if parts.len() == 1 {
		return Some(crate_root);
	}
	None
}

/// Directory that holds the child modules of the module `source_file` defines:
/// next to a module root (`mod.rs`, `lib.rs`, `main.rs`), inside a `<name>/`
/// directory for any other file.
fn module_directory(source_file: &str) -> String {
	let (directory, name) = source_file.rsplit_once('/').unwrap_or(("", source_file));
	if MODULE_ROOT_FILES.contains(&name) {
		directory.to_string()
	} else {
		join_path(directory, name.strip_suffix(".rs").unwrap_or(name))
	}
}

/// `super::` (any number of times) then a module path: each `super` moves to
/// the directory of the parent module's children. A lone item name that is no
/// child module (`super::Item`) lives in the parent module's own file.
fn resolve_super_import(import_path: &str, module_dir: &str, registry: &FileRegistry) -> Option<String> {
	let mut rest = import_path;
	let mut parent = module_dir;
	while let Some(after) = rest.strip_prefix("super::") {
		rest = after;
		parent = directory_of(parent);
	}
	resolve_relative_import(rest, parent, registry).or_else(|| {
		if rest.contains("::") {
			return None;
		}
		find_module_file(parent, registry)
	})
}

/// The file that defines the module whose children live in `module_dir`.
fn find_module_file(module_dir: &str, registry: &FileRegistry) -> Option<String> {
	let (parent, name) = module_dir.rsplit_once('/').unwrap_or(("", module_dir));
	[
		join_path(module_dir, "mod.rs"),
		join_path(parent, &format!("{name}.rs")),
		join_path(module_dir, "lib.rs"),
		join_path(module_dir, "main.rs"),
	]
	.iter()
	.find_map(|candidate| registry.find_exact_file(candidate))
}

/// Resolve a module path against `base_dir`, longest prefix first.
fn resolve_relative_import(module_path: &str, base_dir: &str, registry: &FileRegistry) -> Option<String> {
	let parts: Vec<&str> = module_path.split("::").collect();
	for end in (1..=parts.len()).rev() {
		let Some((last, leading)) = parts[..end].split_last() else {
			continue;
		};
		let directory = leading
			.iter()
			.fold(base_dir.to_string(), |directory, part| join_path(&directory, part));
		if let Some(found) = registry.find_exact_file(&join_path(&directory, &format!("{last}.rs"))) {
			return Some(found);
		}
		if let Some(found) = registry.find_exact_file(&join_path(&directory, &format!("{last}/mod.rs"))) {
			return Some(found);
		}
	}
	None
}

#[cfg(test)]
#[path = "rust_tests.rs"]
mod rust_tests;
