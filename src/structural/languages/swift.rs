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

//! Swift grammar: functions, classes, protocols, initializers and type
//! aliases.

use super::{
	extract_symbol_by_kinds, files_in_dir_matching, parse_call_target, simple_type_name, CallTarget, Language,
	TypeRelationKind,
};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Swift {}

impl Language for Swift {
	fn name(&self) -> &'static str {
		"swift"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_swift::LANGUAGE.into()
	}

	// An import is one line: the module path follows `import` (after any
	// `@testable` / kind keyword) with no list or block form.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&["import"]
	}

	// `: Base,` newline `Proto` leaves a bare name on its own line, which the
	// default name-list rule accepts.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["class", "struct", "enum", "extension", "protocol", "actor"]
	}

	// `import Module` names the directory that holds the module's files.
	fn is_directory_index(&self, _path: &str) -> bool {
		true
	}
	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Same kinds as the outline: properties, subscripts and imports
		// declare no symbol of their own.
		self.get_outline_kinds()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `class_declaration` covers class / struct / enum / extension / actor
		// in this grammar; property/subscript/import declarations are dropped.
		vec![
			"function_declaration",
			"class_declaration",
			"protocol_declaration",
			"init_declaration",
			"typealias_declaration",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "init_declaration" {
			return Some("init".to_string());
		}
		for field in ["name", "type"] {
			if let Some(name) = node.child_by_field_name(field) {
				if let Ok(text) = name.utf8_text(contents.as_bytes()) {
					if let Some(simple) = simple_type_name(text) {
						return Some(simple);
					}
				}
			}
		}
		extract_symbol_by_kinds(node, contents, &["simple_identifier", "type_identifier", "user_type"])
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"import_declaration" => {
				// The full module path (`Foundation`, `UIKit`).
				let mut cursor = node.walk();
				let parts: Vec<&str> = node
					.children(&mut cursor)
					.filter(|child| child.kind() == "identifier")
					.filter_map(|child| child.utf8_text(contents.as_bytes()).ok())
					.collect();
				if !parts.is_empty() {
					imports.push(parts.join("."));
				}
			}
			// Swift exports what carries a `public` or `open` access modifier.
			"function_declaration"
			| "class_declaration"
			| "protocol_declaration"
			| "property_declaration"
			| "typealias_declaration"
			| "subscript_declaration"
			| "init_declaration"
				if is_swift_public(node, contents) =>
			{
				exports.extend(declared_names(node, contents));
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "call_expression" {
			return Vec::new();
		}
		// The grammar has no field for the callee: it is the first child.
		node.child(0)
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(parse_call_target)
			.into_iter()
			.collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// `class Foo: Bar, Baz`: Bar may be a superclass or a protocol, and
		// without semantic analysis they cannot be told apart, so every
		// specifier is reported as Implements. A protocol's inheritance is
		// always Extends.
		let kind = match node.kind() {
			"class_declaration" => TypeRelationKind::Implements,
			"protocol_declaration" => TypeRelationKind::Extends,
			_ => return Vec::new(),
		};
		let mut relations = Vec::new();
		let mut cursor = node.walk();
		for specifier in node.children(&mut cursor) {
			if specifier.kind() != "inheritance_specifier" {
				continue;
			}
			if let Some(name) = specifier
				.child_by_field_name("inherits_from")
				.and_then(|inherited| swift_type_name(inherited, contents))
			{
				relations.push((kind, name));
			}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		self.resolve_import_files(import_path, source_file, all_files)
			.into_iter()
			.next()
	}

	/// `import Models` names a module: every Swift file of its directory. A
	/// relative import names one file.
	fn resolve_import_files(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Vec<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			resolve_relative_import(import_path, source_file, all_files)
				.into_iter()
				.collect()
		} else {
			resolve_module_import(import_path, all_files)
		}
	}
}

/// Names a declaration introduces, for the export list: the `name` field of
/// functions, types and aliases, and the identifiers bound by a property's
/// pattern. Initializers and subscripts export nothing.
fn declared_names(node: Node, contents: &str) -> Vec<String> {
	let mut names = Vec::new();
	match node.kind() {
		"function_declaration" | "class_declaration" | "protocol_declaration" | "typealias_declaration" => {
			let mut cursor = node.walk();
			for name_node in node.children_by_field_name("name", &mut cursor) {
				match name_node.kind() {
					"simple_identifier" | "type_identifier" => names.extend(node_text(name_node, contents)),
					// A qualified type name wraps its parts in a user_type.
					"user_type" => collect_direct_children(name_node, "type_identifier", contents, &mut names),
					_ => {}
				}
			}
		}
		"property_declaration" => {
			if let Some(pattern) = node.child_by_field_name("name") {
				collect_kind(pattern, "simple_identifier", contents, &mut names);
			}
		}
		_ => {}
	}
	names
}

fn node_text(node: Node, contents: &str) -> Option<String> {
	node.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// Text of every node of `kind` in the subtree of `node` (including `node`
/// itself), in document order.
fn collect_kind(node: Node, kind: &str, contents: &str, out: &mut Vec<String>) {
	let mut cursor = node.walk();
	loop {
		let current = cursor.node();
		if current.kind() == kind {
			out.extend(node_text(current, contents));
		}
		if cursor.goto_first_child() {
			continue;
		}
		// A cursor made from `node` never leaves its subtree, so running out
		// of parents ends the walk.
		while !cursor.goto_next_sibling() {
			if !cursor.goto_parent() {
				return;
			}
		}
	}
}

/// Whether the declaration has a `public` or `open` visibility modifier.
fn is_swift_public(node: Node, contents: &str) -> bool {
	let mut cursor = node.walk();
	for modifiers in node.children(&mut cursor).filter(|child| child.kind() == "modifiers") {
		let mut modifier_cursor = modifiers.walk();
		for modifier in modifiers.children(&mut modifier_cursor) {
			if modifier.kind() == "visibility_modifier" {
				if let Ok(text) = modifier.utf8_text(contents.as_bytes()) {
					if text == "public" || text == "open" {
						return true;
					}
				}
			}
		}
	}
	false
}

/// A type name from a `type_identifier` or `user_type` node; a qualified
/// `user_type` yields its last segment.
fn swift_type_name(node: Node, contents: &str) -> Option<String> {
	match node.kind() {
		"type_identifier" => node_text(node, contents),
		// Only the last segment names the type (`Foo.Bar` is `Bar`), like
		// every other language's relation targets.
		"user_type" => {
			let mut parts = Vec::new();
			collect_direct_children(node, "type_identifier", contents, &mut parts);
			parts.pop()
		}
		_ => node.utf8_text(contents.as_bytes()).ok().and_then(simple_type_name),
	}
}

fn collect_direct_children(node: Node, kind: &str, contents: &str, out: &mut Vec<String>) {
	let mut cursor = node.walk();
	for child in node.children(&mut cursor).filter(|child| child.kind() == kind) {
		out.extend(node_text(child, contents));
	}
}

/// A `./` or `../` import: the path with or without a `.swift` extension.
fn resolve_relative_import(import_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let base = resolve_relative_path(source_file, import_path);
	if base.ends_with(".swift") {
		return registry.find_exact_file(&base);
	}
	registry.find_exact_file(&format!("{base}.swift"))
}

/// A module or framework import (`import Models`): the Swift files of the
/// first directory of that name, else a file called `<module>.swift`.
fn resolve_module_import(module_name: &str, registry: &FileRegistry) -> Vec<String> {
	let mut directories: Vec<&String> = registry.dirs_named(module_name).iter().collect();
	directories.sort();
	let in_directory = directories
		.into_iter()
		.map(|directory| files_in_dir_matching(registry, directory, |file| file.ends_with(".swift")))
		.find(|files| !files.is_empty());
	in_directory.unwrap_or_else(|| {
		let file_name = format!("{module_name}.swift");
		registry
			.files_named(&file_name)
			.iter()
			.min()
			.cloned()
			.into_iter()
			.collect()
	})
}

#[cfg(test)]
#[path = "swift_tests.rs"]
mod swift_tests;
