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

//! Bash grammar: named function definitions, plus the graph facts
//! (`source` / `.` imports, command calls).

use super::{parse_call_target, path_ends_with_name, CallTarget, Language};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

/// What may follow the last segment of a sourced path.
const BASH_PATH_END: &str = "\"' \t;&|)\n";

pub(crate) struct Bash {}

impl Language for Bash {
	fn name(&self) -> &'static str {
		"bash"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_bash::LANGUAGE.into()
	}

	// `source file` or `. file`: the file is the command's first argument, so
	// the name ends a word (optionally quoted, with an extension).
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		(line.contains("source") || line.contains(". ")) && path_ends_with_name(line, name, BASH_PATH_END)
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// Only named functions are items; `command` nodes (source/. calls)
		// declare nothing.
		vec!["function_definition"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// Bash function names are `word` nodes exposed via the `name` field;
		// the default identifier/name scan never matches them — and on
		// `command` nodes it would match `command_name`, turning every
		// command into a bogus item.
		if node.kind() == "function_definition" {
			return node
				.child_by_field_name("name")
				.and_then(|n| n.utf8_text(contents.as_bytes()).ok())
				.map(str::to_string);
		}
		None
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		vec!["function_definition"]
	}

	// A `source` inside a function is reported by the function's node (which
	// does not descend into nested functions); one anywhere else, such as at
	// the top level of the script, is reported by its own `command` node.
	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		match node.kind() {
			"function_definition" => extract_bash_sources_from_node(node, contents, &mut imports),
			"command" if !is_inside_function(node) => {
				imports.extend(node.utf8_text(contents.as_bytes()).ok().and_then(parse_bash_source));
			}
			_ => {}
		}
		// Bash has no explicit exports.
		(imports, Vec::new())
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "command" {
			return Vec::new();
		}
		// The `name` field, not the first child: a `command` can start with a
		// `variable_assignment` (`DEBUG=1 cmd`).
		let Some(command_name) = node.child_by_field_name("name") else {
			return Vec::new();
		};
		let Ok(text) = command_name.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		let name = text.trim();
		// `source` / `.` are imports, not calls.
		if name == "source" || name == "." {
			return Vec::new();
		}
		parse_call_target(name)
			.or_else(|| hyphenated_function_call(name))
			.into_iter()
			.collect()
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			all_files.find_exact_file(&resolve_relative_path(source_file, import_path))
		} else {
			all_files.find_exact_file(import_path)
		}
	}
}

/// Collect `source file` / `. file` targets under `function`, without
/// entering a nested function: the graph walk visits it on its own, so
/// descending would report its imports twice.
fn extract_bash_sources_from_node(function: Node, contents: &str, imports: &mut Vec<String>) {
	let mut stack: Vec<Node> = Vec::new();
	push_children_reversed(function, &mut stack);
	while let Some(node) = stack.pop() {
		if node.kind() == "command" {
			if let Ok(command_text) = node.utf8_text(contents.as_bytes()) {
				if let Some(source_file) = parse_bash_source(command_text) {
					imports.push(source_file);
				}
			}
		}
		if node.kind() != "function_definition" {
			push_children_reversed(node, &mut stack);
		}
	}
}

/// Pushed in reverse so popping yields source order.
fn push_children_reversed<'a>(node: Node<'a>, stack: &mut Vec<Node<'a>>) {
	let mut cursor = node.walk();
	let children: Vec<Node<'a>> = node.children(&mut cursor).collect();
	stack.extend(children.into_iter().rev());
}

/// The file named by a `source file` / `. file` command, quotes stripped.
fn parse_bash_source(command_text: &str) -> Option<String> {
	let mut words = command_text.split_whitespace();
	if !matches!(words.next()?, "source" | ".") {
		return None;
	}
	let filename = words.next()?;
	Some(filename.trim_matches('"').trim_matches('\'').to_string())
}

/// Whether the node sits inside a function body, whose own node reports its
/// `source` commands.
fn is_inside_function(node: Node) -> bool {
	let mut current = node.parent();
	while let Some(parent) = current {
		if parent.kind() == "function_definition" {
			return true;
		}
		current = parent.parent();
	}
	false
}

/// A command named like a hyphenated function (`my-func`): valid for a Bash
/// function, but not an identifier for `parse_call_target`.
fn hyphenated_function_call(name: &str) -> Option<CallTarget> {
	let mut characters = name.chars();
	let first = characters.next()?;
	let is_function_name = (first.is_alphabetic() || first == '_')
		&& name.contains('-')
		&& characters.all(|character| character.is_alphanumeric() || matches!(character, '_' | '-'));
	is_function_name.then(|| CallTarget {
		name: name.to_string(),
		qualifier: None,
	})
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod bash_tests;
