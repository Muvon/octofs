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

//! Lua grammar: named function declarations, plus the graph facts
//! (`require` imports, module-return exports, calls, owners).

use super::{extract_call_target, parse_call_target, path_ends_with_name, CallTarget, Language};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Lua {}

impl Language for Lua {
	fn name(&self) -> &'static str {
		"lua"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_lua::LANGUAGE.into()
	}

	// `require("m")`, `require 'm'` and the long-string form `require [[m]]`:
	// the name is the last segment of the module path.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		path_ends_with_name(line, name, "\"']")
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `function_definition` is anonymous in this grammar; named local and
		// table methods are represented by `function_declaration`.
		vec!["function_declaration"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// The `name` field holds a possibly dotted path (`M.foo`); the item is
		// named by its last segment.
		let name = node.child_by_field_name("name")?;
		let text = name.utf8_text(contents.as_bytes()).ok()?;
		extract_call_target(text)
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// `function_definition` is anonymous in this grammar.
		vec!["function_declaration"]
	}

	// `function M.encode()` is owned by `M`.
	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		let name = node.child_by_field_name("name")?;
		let text = name.utf8_text(contents.as_bytes()).ok()?;
		parse_call_target(text).and_then(|target| target.qualifier)
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"function_call" => {
				if get_function_call_name(node, contents).as_deref() == Some("require") {
					if let Some(module_name) = get_require_module_name(node, contents) {
						imports.push(module_name);
					}
				}
			}
			"return_statement" if is_module_level_return(node) => {
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					match child.kind() {
						"expression_list" => extract_exports_from_expression_list(child, contents, &mut exports),
						"table_constructor" => extract_exports_from_table(child, contents, &mut exports),
						"identifier" => {
							if let Ok(name) = child.utf8_text(contents.as_bytes()) {
								exports.push(name.to_string());
							}
						}
						_ => {}
					}
				}
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "function_call" {
			return Vec::new();
		}
		let Some(func_node) = node.child_by_field_name("name") else {
			return Vec::new();
		};
		let Ok(text) = func_node.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		let name = text.trim();
		// `require` is an import, not a call.
		if name == "require" {
			return Vec::new();
		}
		parse_call_target(name).into_iter().collect()
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		// `./module` and `../utils`: resolve_relative_path joins against the
		// source directory and normalizes `..` itself.
		if import_path.starts_with("./") || import_path.starts_with("../") {
			let relative_path = resolve_relative_path(source_file, import_path);
			return all_files.find_exact_file(&format!("{relative_path}.{LUA_EXTENSION}"));
		}

		// Dotted module names (`utils.helpers`) map to directories; the last
		// segment is a file or an `init.lua`. Lua's `package.path` usually
		// starts at the project root, but a sibling module also resolves, so
		// the requiring file's directory is tried first, then the root.
		let module_path = import_path.replace('.', "/");
		if module_path.is_empty() {
			return None;
		}
		let source_dir = source_file
			.rsplit_once(['/', '\\'])
			.map_or("", |(directory, _)| directory);
		[source_dir, ""]
			.into_iter()
			.find_map(|base_dir| find_lua_module(&module_path, base_dir, all_files))
	}
}

const LUA_EXTENSION: &str = "lua";

fn join_path(base: &str, part: &str) -> String {
	if base.is_empty() {
		part.to_string()
	} else {
		format!("{base}/{part}")
	}
}

/// `a/b/c` below `base_dir` as `c.lua` or `c/init.lua`.
fn find_lua_module(module_path: &str, base_dir: &str, registry: &FileRegistry) -> Option<String> {
	let module_base = join_path(base_dir, module_path);
	registry
		.find_exact_file(&format!("{module_base}.{LUA_EXTENSION}"))
		.or_else(|| registry.find_exact_file(&format!("{module_base}/init.{LUA_EXTENSION}")))
}

/// First identifier child of a `function_call`: the callee name.
fn get_function_call_name(node: Node, contents: &str) -> Option<String> {
	let mut cursor = node.walk();
	let child = node.children(&mut cursor).find(|child| child.kind() == "identifier")?;
	child.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// The first string argument of a `require(...)` call, unquoted.
fn get_require_module_name(node: Node, contents: &str) -> Option<String> {
	let mut cursor = node.walk();
	let arguments = node.children(&mut cursor).find(|child| child.kind() == "arguments")?;
	let mut argument_cursor = arguments.walk();
	let string = arguments
		.children(&mut argument_cursor)
		.find(|child| child.kind() == "string")?;
	let text = string.utf8_text(contents.as_bytes()).ok()?;
	Some(text.trim_matches('"').trim_matches('\'').to_string())
}

/// A `return` outside every function body is the module's export statement.
fn is_module_level_return(node: Node) -> bool {
	let mut current = node.parent();
	while let Some(parent) = current {
		match parent.kind() {
			"function_declaration" | "function_definition" | "local_function" => return false,
			"chunk" => return true,
			_ => current = parent.parent(),
		}
	}
	true
}

fn extract_exports_from_expression_list(node: Node, contents: &str, exports: &mut Vec<String>) {
	let mut cursor = node.walk();
	for child in node.children(&mut cursor) {
		match child.kind() {
			"identifier" => {
				if let Ok(name) = child.utf8_text(contents.as_bytes()) {
					exports.push(name.to_string());
				}
			}
			"table_constructor" => extract_exports_from_table(child, contents, exports),
			_ => {}
		}
	}
}

fn extract_exports_from_table(node: Node, contents: &str, exports: &mut Vec<String>) {
	let mut cursor = node.walk();
	for field in node.children(&mut cursor) {
		if field.kind() != "field" {
			continue;
		}
		let mut field_cursor = field.walk();
		for child in field.children(&mut field_cursor) {
			if child.kind() == "identifier" {
				if let Ok(name) = child.utf8_text(contents.as_bytes()) {
					if is_valid_lua_identifier(name) {
						exports.push(name.to_string());
					}
				}
			}
		}
	}
}

fn is_valid_lua_identifier(text: &str) -> bool {
	const LUA_KEYWORDS: &[&str] = &[
		"and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not",
		"or", "repeat", "return", "then", "true", "until", "while",
	];
	if LUA_KEYWORDS.contains(&text) {
		return false;
	}
	let mut chars = text.chars();
	matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
		&& chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
#[path = "lua_tests.rs"]
mod lua_tests;
