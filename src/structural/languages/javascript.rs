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

//! JavaScript grammar: functions, methods, named arrow functions and classes,
//! plus the graph methods (imports, calls, class inheritance, import
//! resolution) that TypeScript, PHP, Python and Svelte reuse in part.

use super::{parse_call_target, path_ends_with_name, simple_type_name, CallTarget, Language, TypeRelationKind};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use tree_sitter::Node;

/// What closes the module path of a JavaScript or TypeScript import.
pub(super) const SCRIPT_PATH_END: &str = "'\"`";

pub(crate) struct JavaScript {}

/// Extensions tried when an import omits its own; the TypeScript ones are
/// included so a JavaScript file can import a TypeScript sibling.
const RESOLVE_EXTENSIONS: [&str; 5] = ["js", "jsx", "mjs", "ts", "tsx"];
const INDEX_FILES: [&str; 5] = ["index.js", "index.jsx", "index.mjs", "index.ts", "index.tsx"];
/// Roots a bare module specifier is tried under, in order: `src`, then the
/// repository root (the empty directory).
const MODULE_ROOT_DIRS: [&str; 2] = ["src", ""];
/// Length of " from " in an import statement.
const FROM_KEYWORD_LEN: usize = 6;

impl Language for JavaScript {
	fn name(&self) -> &'static str {
		"javascript"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_javascript::LANGUAGE.into()
	}

	// The module path is a string literal: the name is its last segment,
	// before the extension and the closing quote.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		path_ends_with_name(line, name, SCRIPT_PATH_END)
	}

	// `export { a } from './m'` and `export * from './m'`; the closing line of
	// a multi-line list starts with `}`.
	fn line_may_reexport(&self, line: &str, name: &str, _path: &str) -> bool {
		let text = line.trim_start();
		(text.starts_with("export") || text.starts_with('}')) && self.line_may_import(line, name)
	}

	// JavaScript has a single parent, and `extends` stays on the parent's line.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["extends"]
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The class container is included (chunking drops it as too large);
		// import/export statements are dropped — they declare nothing an
		// outline needs, and anonymous arrow functions have no name.
		vec![
			"function_declaration",
			"method_definition",
			"arrow_function",
			"class_declaration",
		]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		self.get_outline_kinds()
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "arrow_function" {
			// An arrow function's name is the variable it is assigned to.
			let parent = node.parent()?;
			if parent.kind() != "variable_declarator" {
				return None;
			}
			return parent
				.child_by_field_name("name")
				.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
				.map(str::to_string);
		}
		super::extract_symbol_by_kinds(node, contents, &["identifier", "name"])
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"import_statement" => {
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					if let Some(imported_paths) = parse_js_import_statement_full_path(import_text) {
						imports.extend(imported_paths);
					}
				}
			}
			"export_statement" => {
				imports.extend(re_export_source(node, contents));
				if let Ok(export_text) = node.utf8_text(contents.as_bytes()) {
					if let Some(exported_items) = parse_js_export_statement(export_text) {
						exports.extend(exported_items);
					}
				}
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		js_call_target(node, contents).into_iter().collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// Vanilla JS only has class inheritance: `class Foo extends Bar { }`.
		// `class_heritage` is a single-target wrapper around an `extends`
		// keyword plus the parent expression.
		let mut relations = Vec::new();
		if matches!(node.kind(), "class_declaration" | "class") {
			let mut cursor = node.walk();
			for child in node.children(&mut cursor) {
				if child.kind() != "class_heritage" {
					continue;
				}
				let mut heritage_cursor = child.walk();
				for parent in child.children(&mut heritage_cursor) {
					if matches!(parent.kind(), "identifier" | "member_expression" | "call_expression") {
						if let Ok(text) = parent.utf8_text(contents.as_bytes()) {
							if let Some(name) = simple_type_name(text) {
								relations.push((TypeRelationKind::Extends, name));
							}
						}
					}
				}
			}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			resolve_relative_import(import_path, source_file, all_files)
		} else if import_path.starts_with('/') {
			find_matching_js_file(import_path, all_files)
		} else {
			resolve_module_import(import_path, all_files)
		}
	}
}

/// The callee of a `call_expression` or `new_expression`, shared with
/// TypeScript whose grammar uses the same node kinds.
pub(crate) fn js_call_target(node: Node, contents: &str) -> Option<CallTarget> {
	let callee_field = match node.kind() {
		"call_expression" => "function",
		"new_expression" => "constructor",
		_ => return None,
	};
	let callee = node.child_by_field_name(callee_field)?;
	parse_call_target(callee.utf8_text(contents.as_bytes()).ok()?)
}

/// Join a directory and a name with `/`; an empty side is dropped, like
/// `Path::join`. Shared by the other languages' string-only resolvers.
pub(crate) fn join_path(directory: &str, name: &str) -> String {
	if directory.is_empty() {
		name.to_string()
	} else if name.is_empty() {
		directory.to_string()
	} else {
		format!("{directory}/{name}")
	}
}

/// Directory part of a path; empty for a bare file name.
pub(crate) fn parent_dir(path: &str) -> &str {
	path.rsplit_once(['/', '\\'])
		.map(|(directory, _)| directory)
		.unwrap_or_default()
}

/// Match `target` against the registry as is, with an extension, and as a
/// directory index. Exact only: a file that merely shares the name of the
/// import is not a match.
fn find_matching_js_file(target: &str, registry: &FileRegistry) -> Option<String> {
	if let Some(found) = registry.find_verbatim(target) {
		return Some(found.to_string());
	}
	if let Some(found) = registry.find_normalized(target) {
		return Some(found.to_string());
	}

	for extension in RESOLVE_EXTENSIONS {
		if let Some(found) = registry.find_normalized(&format!("{target}.{extension}")) {
			return Some(found.to_string());
		}
	}

	INDEX_FILES
		.iter()
		.find_map(|index_file| registry.find_normalized(&join_path(target, index_file)))
		.map(str::to_string)
}

/// Resolve `./utils` or `../components/Button` against the importing file.
fn resolve_relative_import(import_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	find_matching_js_file(&resolve_relative_path(source_file, import_path), registry)
}

/// Bare module specifiers. The configured path aliases of a tsconfig or
/// jsconfig are not read, so only the two conventional roots are tried: the
/// `src` directory, then the repository root. Anything else is an external
/// package.
fn resolve_module_import(import_path: &str, registry: &FileRegistry) -> Option<String> {
	MODULE_ROOT_DIRS
		.iter()
		.find_map(|root_dir| find_matching_js_file(&join_path(root_dir, import_path), registry))
}

/// The module path of an import statement: after the `from` clause or, for a
/// side-effect import (`import './styles.css';`), straight after `import`.
pub(crate) fn parse_js_import_statement_full_path(import_text: &str) -> Option<Vec<String>> {
	let cleaned = import_text.trim();
	let module_part = match cleaned.find(" from ") {
		Some(from_pos) => cleaned.get(from_pos + FROM_KEYWORD_LEN..),
		None => cleaned.strip_prefix("import "),
	}?;

	let module_path = module_part
		.trim()
		.trim_start_matches('\'')
		.trim_start_matches('"')
		.trim_end_matches(';')
		.trim_end_matches('\'')
		.trim_end_matches('"');
	if module_path.is_empty() {
		return None;
	}
	Some(vec![module_path.to_string()])
}

/// The module a re-export names (`export { f } from './a'`). It is an import
/// edge too: the re-exporting file depends on that module.
pub(crate) fn re_export_source(node: Node, contents: &str) -> Option<String> {
	let source = node.child_by_field_name("source")?;
	let text = source.utf8_text(contents.as_bytes()).ok()?;
	let module_path = text.trim_matches(|c| c == '\'' || c == '"');
	(!module_path.is_empty()).then(|| module_path.to_string())
}

pub(crate) fn parse_js_export_statement(export_text: &str) -> Option<Vec<String>> {
	let cleaned = export_text.trim();

	// Export statements are usually on the first line.
	let first_line = cleaned.lines().next().unwrap_or(cleaned);
	let rest = first_line.strip_prefix("export ")?;

	// `export function foo() {}` is checked before the brace form: a one-line
	// `export function foo() { ... }` also contains braces, and the brace
	// branch would read its body as a named-export list.
	if rest.starts_with("function ")
		|| rest.starts_with("const ")
		|| rest.starts_with("let ")
		|| rest.starts_with("var ")
	{
		// The token still carries whatever follows the name on the same line
		// (`foo()`, `foo=`), so cut at the first character that cannot appear
		// in a JS identifier.
		let parts: Vec<&str> = rest.split_whitespace().collect();
		if let Some(token) = parts.get(1) {
			let name = token
				.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
				.next()
				.unwrap_or(token);
			if !name.is_empty() {
				return Some(vec![name.to_string()]);
			}
		}
	}

	// `export { foo, bar as baz }` exports the local names.
	if let (Some(start), Some(end)) = (first_line.find('{'), first_line.find('}')) {
		let items = first_line.get(start + 1..end)?;
		let exports = items
			.split(',')
			.map(|item| {
				let item = item.trim();
				item.find(" as ").map_or(item, |as_pos| &item[..as_pos])
			})
			.filter(|name| !name.is_empty())
			.map(str::to_string)
			.collect();
		return Some(exports);
	}

	// `export default class Name` / `export class Name`
	let class_rest = rest.strip_prefix("default ").unwrap_or(rest);
	if class_rest.starts_with("class ") {
		let parts: Vec<&str> = class_rest.split_whitespace().collect();
		if let Some(token) = parts.get(1) {
			return Some(vec![token.trim_end_matches('{').to_string()]);
		}
	}

	None
}

#[cfg(test)]
#[path = "javascript_tests.rs"]
mod javascript_tests;
