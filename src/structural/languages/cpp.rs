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

//! C++ grammar: function definitions and type/namespace containers.

use super::{
	extract_call_target, extract_symbol_by_kinds, find_graph_symbol_owner, parse_call_target, path_ends_with_name,
	simple_type_name, CallTarget, Language, TypeRelationKind,
};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Cpp {}

impl Language for Cpp {
	fn name(&self) -> &'static str {
		"cpp"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_cpp::LANGUAGE.into()
	}

	// The header is quoted or angle-bracketed on a preprocessor line; the name
	// is its last segment, before the extension.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		line.contains('#') && path_ends_with_name(line, name, "\">")
	}

	// A base list may put each base on its own line (`private C {`), which the
	// default name-list rule accepts.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["class", "struct"]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Same kinds as the outline: plain `declaration` nodes would name
		// every local variable and #include declares nothing.
		self.get_outline_kinds()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The type containers are restored (chunking drops them as too
		// large); plain `declaration` nodes are dropped — they match every
		// local variable declaration and #include directive, which declare
		// nothing an outline needs.
		vec![
			"function_definition",
			"class_specifier",
			"struct_specifier",
			"enum_specifier",
			"namespace_definition",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "function_definition" {
			let declarator = node.child_by_field_name("declarator")?;
			let text = declarator.utf8_text(contents.as_bytes()).ok()?;
			let callable = text.split('(').next().unwrap_or(text);
			let name_part = callable.rsplit("::").next().unwrap_or(callable).trim();
			// Destructors keep their `~` and operators keep their operator
			// token: the generic call-target trim would reduce `~Foo` to `Foo`
			// (colliding with the constructor) and `operator+` to `operator`
			// (colliding with every other operator).
			if name_part.starts_with('~') {
				return Some(name_part.to_string());
			}
			if let Some(rest) = name_part.strip_prefix("operator") {
				if !rest.is_empty() && !rest.starts_with(|character: char| character.is_alphanumeric()) {
					return Some(name_part.to_string());
				}
			}
			return extract_call_target(callable);
		}
		// Bodyless specifiers are type REFERENCES (`struct stat st;`) or forward
		// declarations, not definitions — naming them would mint a bogus item
		// at every usage site.
		if matches!(node.kind(), "class_specifier" | "struct_specifier" | "enum_specifier")
			&& node.child_by_field_name("body").is_none()
		{
			return None;
		}
		node.child_by_field_name("name")
			.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
			.map(str::to_string)
			.or_else(|| {
				extract_symbol_by_kinds(
					node,
					contents,
					&["identifier", "type_identifier", "namespace_identifier"],
				)
			})
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		// An out-of-line definition (`void Widget::resize() {}`) names its
		// owner in the declarator; inline methods fall back to the enclosing
		// class.
		if node.kind() == "function_definition" {
			let owner = node
				.child_by_field_name("declarator")
				.and_then(|declarator| declarator.utf8_text(contents.as_bytes()).ok())
				.and_then(|text| parse_call_target(text.split('(').next().unwrap_or(text)))
				.and_then(|target| target.qualifier)
				.and_then(|qualifier| simple_type_name(&qualifier));
			if owner.is_some() {
				return owner;
			}
		}
		find_graph_symbol_owner(node, contents)
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"preproc_include" => {
				imports.extend(node.utf8_text(contents.as_bytes()).ok().and_then(parse_cpp_include));
			}
			"function_definition" => exports.extend(function_name(node, contents)),
			"declaration" => {
				// A prototype exports its function name; anything else is a
				// global variable or constant.
				if function_declarator_of(node).is_some() {
					exports.extend(function_name(node, contents));
				} else {
					exports.extend(variable_names(node, contents));
				}
			}
			"class_specifier" | "struct_specifier" | "enum_specifier" => {
				exports.extend(first_child_text(node, contents, &["type_identifier", "identifier"]));
			}
			"namespace_definition" => {
				exports.extend(first_child_text(
					node,
					contents,
					&["identifier", "namespace_identifier"],
				));
			}
			"template_declaration" => {
				// The templated function or type sits inside the declaration.
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					let name = match child.kind() {
						"function_definition" => function_name(child, contents),
						"class_specifier" | "struct_specifier" => {
							first_child_text(child, contents, &["type_identifier", "identifier"])
						}
						_ => None,
					};
					exports.extend(name.map(|name| format!("template<{name}>")));
				}
			}
			// The grammar emits `preproc_def` / `preproc_function_def`.
			"preproc_def" | "preproc_function_def" => {
				exports.extend(first_child_text(node, contents, &["identifier"]));
			}
			"type_definition" => {
				exports.extend(first_child_text(node, contents, &["type_identifier"]));
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if is_misparsed_call(node) {
			return node
				.child_by_field_name("declarator")
				.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
				.and_then(parse_call_target)
				.into_iter()
				.collect();
		}
		if node.kind() != "call_expression" {
			return Vec::new();
		}
		node.child_by_field_name("function")
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(parse_call_target)
			.into_iter()
			.collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// `class Foo : public Bar, private Baz {}` and `struct Foo : Bar {}`.
		// C++ does not distinguish extends from implements, so every base is
		// reported as Extends.
		let mut relations = Vec::new();
		if matches!(node.kind(), "class_specifier" | "struct_specifier") {
			let mut cursor = node.walk();
			for clause in node
				.children(&mut cursor)
				.filter(|child| child.kind() == "base_class_clause")
			{
				let mut base_cursor = clause.walk();
				for base in clause.children(&mut base_cursor) {
					if matches!(
						base.kind(),
						"type_identifier" | "qualified_identifier" | "template_type" | "scoped_type_identifier"
					) {
						if let Some(name) = base.utf8_text(contents.as_bytes()).ok().and_then(simple_type_name) {
							relations.push((TypeRelationKind::Extends, name));
						}
					}
				}
			}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if let Some(header) = import_path.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
			// Local include: #include "header.h"
			return resolve_local_include(header, source_file, all_files);
		}
		if let Some(header) = import_path.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')) {
			// System include: only resolves when the header is in the project.
			return all_files.find_exact_file(header);
		}
		// `parse_cpp_include` hands over the bare header path, so this is the
		// common case: the including file's directory, the path as given, then
		// the usual include directories.
		resolve_local_include(import_path, source_file, all_files)
			.or_else(|| all_files.find_exact_file(import_path))
			.or_else(|| {
				INCLUDE_DIRECTORIES
					.iter()
					.find_map(|directory| all_files.find_exact_file(&format!("{directory}/{import_path}")))
			})
	}
}

/// Directories searched for a header that is neither next to the including
/// file nor at the given path.
const INCLUDE_DIRECTORIES: [&str; 4] = ["include", "inc", "headers", "lib"];

/// Header path of an `#include "x.h"` / `#include <x.h>` directive, without
/// the quotes or angle brackets. A macro-expanded include has neither and
/// yields nothing. Nested conditionals indent the keyword (`#  include`), so
/// the `#` and the keyword are matched apart.
fn parse_cpp_include(include_text: &str) -> Option<String> {
	let include_part = include_text
		.trim()
		.strip_prefix('#')?
		.trim_start()
		.strip_prefix("include")?
		.trim();
	// Up to the closing delimiter, not the end of the text: a trailing
	// comment may follow the header on the same line.
	let quoted = include_part.strip_prefix('"').and_then(|rest| rest.split_once('"'));
	let bracketed = include_part.strip_prefix('<').and_then(|rest| rest.split_once('>'));
	quoted.or(bracketed).map(|(header, _)| header.to_string())
}

/// A header named relative to the including file's directory.
fn resolve_local_include(header: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	registry.find_exact_file(&resolve_relative_path(source_file, header))
}

/// Name under a declarator, looking through pointer, reference, array and
/// init declarators. An out-of-line member definition nests its name under a
/// `qualified_identifier`, which carries `name` rather than `declarator`.
fn declarator_name<'a>(node: Node<'a>, contents: &'a str) -> Option<&'a str> {
	let mut current = node;
	loop {
		current = match current.kind() {
			"identifier" | "field_identifier" => return current.utf8_text(contents.as_bytes()).ok(),
			"qualified_identifier" => current.child_by_field_name("name")?,
			_ => current.child_by_field_name("declarator")?,
		};
	}
}

fn function_declarator_of(node: Node) -> Option<Node> {
	let mut cursor = node.walk();
	let declarator = node
		.children(&mut cursor)
		.find(|child| child.kind() == "function_declarator");
	declarator
}

/// Function name of a definition or prototype.
fn function_name(node: Node, contents: &str) -> Option<String> {
	let declarator = function_declarator_of(node)?;
	declarator_name(declarator, contents).map(str::to_string)
}

/// A call statement the C++ grammar could not read: C code that names a
/// variable `new` (`__list_add(new, prev, next);`) parses as an error node
/// holding a function declarator directly inside a function body. A real
/// declaration in a body is a `declaration` node, never an error node.
fn is_misparsed_call(node: Node) -> bool {
	node.kind() == "function_declarator"
		&& node
			.child_by_field_name("declarator")
			.is_some_and(|callee| callee.kind() == "identifier")
		&& node.parent().is_some_and(|parent| {
			parent.kind() == "ERROR" && parent.parent().is_some_and(|body| body.kind() == "compound_statement")
		})
}

/// Variable names of a declaration (`int a, *b;`).
fn variable_names(node: Node, contents: &str) -> Vec<String> {
	let mut cursor = node.walk();
	let names = node
		.children_by_field_name("declarator", &mut cursor)
		.filter_map(|declarator| declarator_name(declarator, contents).map(str::to_string))
		.collect();
	names
}

/// Text of the first direct child whose kind is one of `kinds`.
fn first_child_text(node: Node, contents: &str, kinds: &[&str]) -> Option<String> {
	let mut cursor = node.walk();
	let child = node.children(&mut cursor).find(|child| kinds.contains(&child.kind()))?;
	child.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

#[cfg(test)]
#[path = "cpp_tests.rs"]
mod cpp_tests;
