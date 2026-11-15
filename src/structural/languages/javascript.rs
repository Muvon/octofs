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

use super::{enclosing_owner, CalleeSyntax, SymbolKind};
use super::{parse_call_target, path_ends_with_name, simple_type_name, CallTarget, Language, TypeRelationKind};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use crate::structural::graph::symbols::{ImportBinding, WILDCARD};
use tree_sitter::Node;

/// What closes the module path of a JavaScript or TypeScript import.
pub(super) const SCRIPT_PATH_END: &str = "'\"`";

// `$` and `#` (private members) are identifier characters; `?.` chains.
const CALLEE_SYNTAX: CalleeSyntax = CalleeSyntax {
	member_separators: &["?.", "."],
	identifier_extras: &['$', '#'],
	self_receivers: &["this"],
	..CalleeSyntax::DOTTED
};

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
			"function_expression",
			"class_declaration",
		]
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		&["//", "/*"]
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		&["class", "const", "let", "var"]
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		&["function"]
	}

	// `$el`, `$store`: the `$` is part of the name.
	fn identifier_prefix(&self) -> Option<u8> {
		Some(b'$')
	}

	fn symbol_kind(&self, node: Node, _contents: &str) -> SymbolKind {
		match node.kind() {
			"function_declaration" | "method_definition" | "arrow_function" | "function_expression" => {
				SymbolKind::Function
			}
			"class_declaration" => SymbolKind::Class,
			_ => SymbolKind::Other,
		}
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		enclosing_owner(node, contents, &["class_declaration", "class"])
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		self.get_outline_kinds()
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if matches!(node.kind(), "arrow_function" | "function_expression") {
			return function_value_name(node, contents);
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
			"call_expression" => imports.extend(dynamic_import_source(node, contents)),
			_ => {}
		}

		(imports, exports)
	}

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		import_bindings(node, contents, imports)
	}

	fn default_export_name(&self, node: Node, contents: &str) -> Option<String> {
		default_export_name(node, contents)
	}

	fn is_directory_index(&self, path: &str) -> bool {
		is_index_file(path)
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

/// The name an arrow function or function expression takes from where its
/// value is stored: `const f = () => {}`, a class field `handle = () => {}`,
/// an object property `{ run: function () {} }` or an export / prototype
/// assignment (`exports.f = () => {}`, `Foo.prototype.m = function () {}`).
/// Anything else (an argument, a returned value) is anonymous.
pub(crate) fn function_value_name(node: Node, contents: &str) -> Option<String> {
	let parent = node.parent()?;
	let (name_field, value_field) = match parent.kind() {
		"variable_declarator" => (parent.child_by_field_name("name"), "value"),
		"field_definition" | "public_field_definition" => (
			parent
				.child_by_field_name("property")
				.or_else(|| parent.child_by_field_name("name")),
			"value",
		),
		"pair" => (
			parent
				.child_by_field_name("key")
				.filter(|key| matches!(key.kind(), "property_identifier" | "identifier")),
			"value",
		),
		"assignment_expression" => (
			parent
				.child_by_field_name("left")
				.filter(|left| is_export_or_prototype_target(*left, contents))
				.and_then(|left| left.child_by_field_name("property")),
			"right",
		),
		_ => (None, "value"),
	};
	let name_field = name_field?;
	let is_the_value = parent
		.child_by_field_name(value_field)
		.is_some_and(|value| value.id() == node.id());
	if !is_the_value {
		return None;
	}
	name_field.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// `exports.f`, `module.exports.f` and `Foo.prototype.m`: the places a
/// function assigned to a property is a symbol of the file.
fn is_export_or_prototype_target(left: Node, contents: &str) -> bool {
	if left.kind() != "member_expression" {
		return false;
	}
	let Some(object) = left.child_by_field_name("object") else {
		return false;
	};
	let Ok(text) = object.utf8_text(contents.as_bytes()) else {
		return false;
	};
	text == "exports" || text == "module.exports" || text.ends_with(".prototype")
}

/// Whether the file is an `index` file: importing its directory names it.
/// Shared with TypeScript.
pub(crate) fn is_index_file(path: &str) -> bool {
	path.rsplit('/')
		.next()
		.and_then(|file_name| file_name.split('.').next())
		== Some("index")
}

/// The name of `export default function name() {}`, `export default class
/// Name {}` and `export default name;`.
pub(crate) fn default_export_name(node: Node, contents: &str) -> Option<String> {
	// Asked of every node of a file: only an export statement can hold the
	// `default` keyword, so the other nodes' children are not walked.
	if node.kind() != "export_statement" {
		return None;
	}
	let mut cursor = node.walk();
	let is_default = node.children(&mut cursor).any(|child| child.kind() == "default");
	if !is_default {
		return None;
	}
	let named = node
		.child_by_field_name("declaration")
		.and_then(|declaration| declaration.child_by_field_name("name"))
		.or_else(|| {
			node.child_by_field_name("value")
				.filter(|value| value.kind() == "identifier")
		})?;
	named.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// The names an `import` statement or a `require` call binds, shared with
/// TypeScript.
pub(crate) fn import_bindings(node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
	let Some(path) = imports.first() else {
		return Vec::new();
	};
	if node.kind() == "call_expression" {
		return required_bindings(node, contents, path);
	}
	let Ok(text) = node.utf8_text(contents.as_bytes()) else {
		return Vec::new();
	};
	let text = text.trim();
	let mut bindings = Vec::new();
	let clause = text
		.trim_start_matches("import")
		.split(" from ")
		.next()
		.unwrap_or_default()
		.trim();
	if clause == "export *" {
		bindings.push(ImportBinding {
			local_name: WILDCARD.to_string(),
			imported_name: Some(WILDCARD.to_string()),
			import_path: path.clone(),
		});
	}
	// `import Foo from './foo'` and `import Foo, { a } from './foo'`: the
	// default import is named after what the importer chose, which need not
	// be what the file calls it, so it binds like a namespace.
	let default_name = clause
		.trim_start_matches("type ")
		.split(',')
		.next()
		.unwrap_or_default()
		.trim();
	if text.starts_with("import")
		&& !default_name.is_empty()
		&& default_name
			.chars()
			.all(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
	{
		bindings.push(ImportBinding {
			local_name: default_name.to_string(),
			imported_name: None,
			import_path: path.clone(),
		});
	}
	if let Some(alias) = clause.strip_prefix("* as ") {
		bindings.push(ImportBinding {
			local_name: alias.trim().to_string(),
			imported_name: None,
			import_path: path.clone(),
		});
	}
	if let (Some(start), Some(end)) = (clause.find('{'), clause.rfind('}')) {
		// `get` rather than a slice: a stray `}` before the `{` is
		// malformed input, not a reason to panic.
		for item in clause.get(start + 1..end).unwrap_or_default().split(',') {
			let mut parts = item.trim().trim_start_matches("type ").split_whitespace();
			let Some(imported) = parts.next() else {
				continue;
			};
			let local = if parts.next() == Some("as") {
				parts.next().unwrap_or(imported)
			} else {
				imported
			};
			bindings.push(ImportBinding {
				local_name: local.to_string(),
				imported_name: Some(imported.to_string()),
				import_path: path.clone(),
			});
		}
	}
	bindings
}

/// `const x = require('./x')` and `const { a, b: c } = require('./x')`: the
/// names sit on the declarator that holds the call.
fn required_bindings(call: Node, contents: &str, path: &str) -> Vec<ImportBinding> {
	let Some(pattern) = call
		.parent()
		.filter(|parent| parent.kind() == "variable_declarator")
		.and_then(|declarator| declarator.child_by_field_name("name"))
	else {
		return Vec::new();
	};
	let pattern_text = pattern.utf8_text(contents.as_bytes()).unwrap_or_default().trim();
	if pattern.kind() == "identifier" {
		return vec![ImportBinding {
			local_name: pattern_text.to_string(),
			imported_name: None,
			import_path: path.to_string(),
		}];
	}
	if pattern.kind() != "object_pattern" {
		return Vec::new();
	}
	let inner = pattern_text.trim_start_matches('{').trim_end_matches('}');
	inner
		.split(',')
		.filter_map(|item| {
			// `a`, `a: b` and `a = 1` all name the property `a`.
			let imported = item.split([':', '=']).next().unwrap_or_default().trim();
			let local = item.split_once(':').map_or(imported, |(_, renamed)| {
				renamed.split('=').next().unwrap_or_default().trim()
			});
			(!imported.is_empty() && !local.is_empty()).then(|| ImportBinding {
				local_name: local.to_string(),
				imported_name: Some(imported.to_string()),
				import_path: path.to_string(),
			})
		})
		.collect()
}

/// The module of `require('./x')` or `import('./x')` when the argument is a
/// string literal; a computed argument names no file.
pub(crate) fn dynamic_import_source(node: Node, contents: &str) -> Option<String> {
	if node.kind() != "call_expression" {
		return None;
	}
	let function = node.child_by_field_name("function")?;
	let loads_a_module = function.kind() == "import"
		|| (function.kind() == "identifier" && function.utf8_text(contents.as_bytes()).ok() == Some("require"));
	if !loads_a_module {
		return None;
	}
	let argument = node.child_by_field_name("arguments")?.named_child(0)?;
	if argument.kind() != "string" {
		return None;
	}
	let text = argument.utf8_text(contents.as_bytes()).ok()?;
	let module_path = text.trim_matches(|c| c == '\'' || c == '"');
	(!module_path.is_empty()).then(|| module_path.to_string())
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
	parse_call_target(callee.utf8_text(contents.as_bytes()).ok()?, &CALLEE_SYNTAX)
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

	// The specifier is the first quoted string of that part; what follows is
	// an import attribute (`with { type: 'json' }`) or the closing `;`.
	let module_part = module_part.trim();
	let quote = module_part.chars().next().filter(|first| matches!(first, '\'' | '"'))?;
	let inner = &module_part[1..];
	let module_path = &inner[..inner.find(quote)?];
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
