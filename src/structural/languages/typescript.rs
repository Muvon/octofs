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

//! TypeScript grammar: functions, methods, classes, interfaces, type aliases
//! and enums, plus the graph methods. Imports, calls and resolution reuse the
//! JavaScript ones; TypeScript adds `implements`, interface inheritance and
//! type-declaration file lookups.

use super::javascript::{
	function_value_name, join_path, js_call_target, parse_js_export_statement, parse_js_import_statement_full_path,
	re_export_source, JavaScript,
};
use super::{enclosing_owner, SymbolKind};
use super::{extract_symbol_by_kinds, simple_type_name, CallTarget, Language, TypeRelationKind};
use super::{PatternGrammar, DOLLAR_EXPANDO};
use super::{C_STYLE_COMMENTS, FUNCTION_KEYWORD};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use crate::structural::graph::symbols::ImportBinding;
use tree_sitter::Node;

pub(crate) struct TypeScript {}

/// Extensions tried when an import omits its own; TypeScript first so a
/// TypeScript project resolves to its sources over compiled output.
const RESOLVE_EXTENSIONS: [&str; 6] = ["ts", "tsx", "d.ts", "js", "jsx", "mjs"];
/// Extension an import may name -> TypeScript extensions the file may really
/// have, in preference order.
const EMITTED_TO_SOURCE_EXTENSIONS: [(&str, &[&str]); 4] = [
	(".js", &[".ts", ".tsx", ".d.ts"]),
	(".jsx", &[".tsx", ".ts", ".d.ts"]),
	(".mjs", &[".mts", ".d.ts"]),
	(".cjs", &[".cts", ".d.ts"]),
];
const INDEX_FILES: [&str; 5] = ["index.ts", "index.tsx", "index.d.ts", "index.js", "index.jsx"];
/// Directories a bare module specifier is tried under.
const MODULE_SOURCE_DIRS: [&str; 6] = ["src", "lib", "types", "typings", "components", "utils"];
/// Entry-point locations tried inside `node_modules/<package>/`.
const PACKAGE_ENTRY_FILES: [&str; 5] = [
	"index.d.ts",
	"lib/index.d.ts",
	"src/index.ts",
	"dist/index.d.ts",
	"types/index.d.ts",
];

impl Language for TypeScript {
	fn name(&self) -> &'static str {
		"typescript"
	}

	fn extensions(&self) -> &'static [&'static str] {
		&["ts"]
	}

	fn container_kinds(&self) -> &'static [(&'static str, &'static str)] {
		super::javascript::CONTAINER_KINDS
	}

	fn definition_kinds(&self) -> &'static [&'static str] {
		&[
			"function_declaration",
			"generator_function_declaration",
			"class_declaration",
			"abstract_class_declaration",
			"method_definition",
			"interface_declaration",
			"type_alias_declaration",
			"enum_declaration",
			"internal_module",
			"module",
			"variable_declarator",
		]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
	}

	fn pattern_grammar(&self) -> Option<PatternGrammar> {
		Some(PatternGrammar {
			name: self.name(),
			grammar: self.get_ts_language(),
			expando: DOLLAR_EXPANDO,
		})
	}

	// Same string-literal module path as JavaScript.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		JavaScript {}.line_may_import(line, name)
	}

	fn line_may_reexport(&self, line: &str, name: &str, path: &str) -> bool {
		JavaScript {}.line_may_reexport(line, name, path)
	}

	// `implements A,` newline `B` and multi-parent `interface X extends A,`
	// newline `B` leave a bare name on its own line, which the default
	// name-list rule accepts.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["extends", "implements"]
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// Mirrors JavaScript plus the TypeScript-only declaration forms;
		// import/export statements are dropped. internal_module/module are the
		// namespace/module containers, so their members are attributable.
		vec![
			"function_declaration",
			"method_definition",
			"arrow_function",
			"function_expression",
			"class_declaration",
			"interface_declaration",
			"type_alias_declaration",
			"enum_declaration",
			"internal_module",
			"module",
		]
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		enclosing_owner(
			node,
			contents,
			&[
				"class_declaration",
				"abstract_class_declaration",
				"class",
				"interface_declaration",
				"internal_module",
				"module",
			],
		)
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		C_STYLE_COMMENTS
	}

	// Generic arguments of a parent (`extends Base<T>`).
	fn inheritance_name_extras(&self) -> &'static str {
		"<>?:"
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		&[
			"class",
			"const",
			"let",
			"var",
			"interface",
			"type",
			"enum",
			"namespace",
			"module",
		]
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		FUNCTION_KEYWORD
	}

	// The kinds TypeScript shares with JavaScript are JavaScript's.
	fn symbol_kind(&self, node: Node, contents: &str) -> SymbolKind {
		match node.kind() {
			"abstract_class_declaration" => SymbolKind::Class,
			"interface_declaration" => SymbolKind::Interface,
			"type_alias_declaration" => SymbolKind::Type,
			"enum_declaration" => SymbolKind::Enum,
			_ => JavaScript {}.symbol_kind(node, contents),
		}
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// The outline's namespace containers are not symbols of their own.
		vec![
			"function_declaration",
			"method_definition",
			"arrow_function",
			"function_expression",
			"class_declaration",
			"abstract_class_declaration",
			"interface_declaration",
			"type_alias_declaration",
			"enum_declaration",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if matches!(node.kind(), "arrow_function" | "function_expression") {
			return function_value_name(node, contents);
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name"])
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"import_statement" => {
				// The module path, not the bound names: the graph feeds it to
				// `resolve_import`, which resolves a path to a file.
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					if let Some(imported_items) = parse_js_import_statement_full_path(import_text) {
						imports.extend(imported_items);
					}
				}
			}
			"export_statement" => {
				// Same patterns as JavaScript plus `export type { Foo }`.
				imports.extend(re_export_source(node, contents));
				if let Ok(export_text) = node.utf8_text(contents.as_bytes()) {
					exports.extend(parse_ts_export_statement(export_text));
				}
			}
			"function_declaration"
			| "method_definition"
			| "arrow_function"
			| "class_declaration"
			| "abstract_class_declaration"
			| "interface_declaration"
			| "type_alias_declaration" => {
				let is_exported = node.parent().is_some_and(|parent| parent.kind() == "export_statement");
				if is_exported {
					let mut cursor = node.walk();
					let name_node = node
						.children(&mut cursor)
						.find(|child| matches!(child.kind(), "identifier" | "type_identifier"));
					if let Some(name_node) = name_node {
						if let Ok(name) = name_node.utf8_text(contents.as_bytes()) {
							exports.push(name.to_string());
						}
					}
				}
			}
			"call_expression" => imports.extend(super::javascript::dynamic_import_source(node, contents)),
			_ => {}
		}

		(imports, exports)
	}

	fn is_directory_index(&self, path: &str) -> bool {
		super::javascript::is_index_file(path)
	}

	fn default_export_name(&self, node: Node, contents: &str) -> Option<String> {
		super::javascript::default_export_name(node, contents)
	}

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		super::javascript::import_bindings(node, contents, imports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		js_call_target(node, contents).into_iter().collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let mut relations = Vec::new();
		match node.kind() {
			// `class Foo extends Bar implements Baz, Qux { }`, also class
			// expressions such as `const Foo = class extends Bar {}`.
			"class_declaration" | "abstract_class_declaration" | "class" => {
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					if child.kind() == "class_heritage" {
						collect_heritage(child, contents, &mut relations);
					}
				}
			}
			// `interface A extends B, C { }`
			"interface_declaration" => {
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					if matches!(child.kind(), "extends_type_clause" | "extends_clause") {
						collect_clause_names(child, contents, TypeRelationKind::Extends, &mut relations);
					}
				}
			}
			_ => {}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			return find_matching_ts_file(&resolve_relative_path(source_file, import_path), all_files);
		}

		// Absolute and module imports try the JavaScript resolution first, then
		// the TypeScript-specific patterns.
		let javascript = JavaScript {};
		if let Some(found) = javascript.resolve_import(import_path, source_file, all_files) {
			return Some(found);
		}
		resolve_ts_module_import(import_path, all_files)
	}
}

/// `.tsx` files: TypeScript with JSX, which needs its own grammar (the
/// TypeScript one reads `<div>` as a type assertion, losing the components
/// and the calls inside the markup). Everything but the grammar is the
/// TypeScript behaviour.
pub(crate) struct Tsx {}

impl Language for Tsx {
	fn name(&self) -> &'static str {
		"tsx"
	}

	// JSX needs the TSX grammar: the TypeScript one reads `<div>` as a type
	// assertion and loses the components and calls inside it.
	fn extensions(&self) -> &'static [&'static str] {
		&["tsx"]
	}

	fn container_kinds(&self) -> &'static [(&'static str, &'static str)] {
		TypeScript {}.container_kinds()
	}

	fn definition_kinds(&self) -> &'static [&'static str] {
		TypeScript {}.definition_kinds()
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_typescript::LANGUAGE_TSX.into()
	}

	fn pattern_grammar(&self) -> Option<PatternGrammar> {
		Some(PatternGrammar {
			name: self.name(),
			grammar: self.get_ts_language(),
			expando: DOLLAR_EXPANDO,
		})
	}

	fn line_may_import(&self, line: &str, name: &str) -> bool {
		TypeScript {}.line_may_import(line, name)
	}

	fn line_may_reexport(&self, line: &str, name: &str, path: &str) -> bool {
		TypeScript {}.line_may_reexport(line, name, path)
	}

	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		TypeScript {}.inheritance_line_markers()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		TypeScript {}.get_outline_kinds()
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		TypeScript {}.extract_symbol_owner(node, contents)
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		TypeScript {}.comment_starts()
	}

	fn inheritance_name_extras(&self) -> &'static str {
		TypeScript {}.inheritance_name_extras()
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		TypeScript {}.declaration_keywords()
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		TypeScript {}.callable_keywords()
	}

	fn symbol_kind(&self, node: Node, contents: &str) -> SymbolKind {
		TypeScript {}.symbol_kind(node, contents)
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		TypeScript {}.get_symbol_kinds()
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		TypeScript {}.extract_declaration_name(node, contents)
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		TypeScript {}.extract_imports_exports(node, contents)
	}

	fn is_directory_index(&self, path: &str) -> bool {
		super::javascript::is_index_file(path)
	}

	fn default_export_name(&self, node: Node, contents: &str) -> Option<String> {
		super::javascript::default_export_name(node, contents)
	}

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		super::javascript::import_bindings(node, contents, imports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		TypeScript {}.extract_function_calls(node, contents)
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		TypeScript {}.extract_type_relations(node, contents)
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		TypeScript {}.resolve_import(import_path, source_file, all_files)
	}
}

/// Match `target` against the registry as is, as the TypeScript source of a
/// `.js`-style specifier, with an extension, and as a directory index. Exact
/// only: a file that merely shares the name of the import is not a match.
fn find_matching_ts_file(target: &str, registry: &FileRegistry) -> Option<String> {
	if let Some(found) = registry.find_normalized(target) {
		return Some(found.to_string());
	}

	// ESM TypeScript imports name the compiled file (`./a.js`) while the
	// repository holds `a.ts`.
	for (emitted_extension, source_extensions) in EMITTED_TO_SOURCE_EXTENSIONS {
		if let Some(stem) = target.strip_suffix(emitted_extension) {
			for source_extension in source_extensions {
				if let Some(found) = registry.find_normalized(&format!("{stem}{source_extension}")) {
					return Some(found.to_string());
				}
			}
		}
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

/// Bare specifiers: `@types` packages, common source directories, then
/// declaration files shipped inside `node_modules`.
fn resolve_ts_module_import(import_path: &str, registry: &FileRegistry) -> Option<String> {
	if !import_path.starts_with('@') {
		let types_index = format!("node_modules/@types/{import_path}/index.d.ts");
		if let Some(found) = find_matching_ts_file(&types_index, registry) {
			return Some(found);
		}
	}

	for source_dir in MODULE_SOURCE_DIRS {
		if let Some(found) = find_matching_ts_file(&join_path(source_dir, import_path), registry) {
			return Some(found);
		}
	}

	for entry_file in PACKAGE_ENTRY_FILES {
		let package_path = format!("node_modules/{import_path}/{entry_file}");
		if let Some(found) = find_matching_ts_file(&package_path, registry) {
			return Some(found);
		}
	}

	None
}

/// Exported names of an export statement. Never fails: a statement that
/// matches no pattern exports nothing.
fn parse_ts_export_statement(export_text: &str) -> Vec<String> {
	let cleaned = export_text.trim();

	// `export type { Foo, Bar }`
	if let Some(type_export) = cleaned.strip_prefix("export type ") {
		if let (Some(start), Some(end)) = (type_export.find('{'), type_export.find('}')) {
			let items = type_export.get(start + 1..end).unwrap_or_default();
			return items
				.split(',')
				.map(str::trim)
				.filter(|item| !item.is_empty())
				.map(str::to_string)
				.collect();
		}
	}

	// `export interface Foo {}` / `export type Foo = ...`
	if let Some(rest) = cleaned.strip_prefix("export ") {
		if rest.starts_with("interface ") || rest.starts_with("type ") {
			let parts: Vec<&str> = rest.split_whitespace().collect();
			if let Some(token) = parts.get(1) {
				return vec![token.trim_end_matches('=').trim_end_matches('{').to_string()];
			}
		}
	}

	parse_js_export_statement(export_text).unwrap_or_default()
}

/// Collect both extends and implements targets of a `class_heritage` node.
fn collect_heritage(heritage: Node, contents: &str, relations: &mut Vec<(TypeRelationKind, String)>) {
	let mut cursor = heritage.walk();
	for child in heritage.children(&mut cursor) {
		match child.kind() {
			"extends_clause" => collect_clause_names(child, contents, TypeRelationKind::Extends, relations),
			"implements_clause" => collect_clause_names(child, contents, TypeRelationKind::Implements, relations),
			_ => {}
		}
	}
}

/// Pull type names out of an extends/implements clause under the given kind.
fn collect_clause_names(
	clause: Node,
	contents: &str,
	kind: TypeRelationKind,
	relations: &mut Vec<(TypeRelationKind, String)>,
) {
	let mut cursor = clause.walk();
	for child in clause.children(&mut cursor) {
		if matches!(
			child.kind(),
			// `extends ns.Base` is a member expression in a class heritage.
			"identifier" | "type_identifier" | "nested_type_identifier" | "generic_type" | "member_expression"
		) {
			if let Ok(text) = child.utf8_text(contents.as_bytes()) {
				if let Some(name) = simple_type_name(text) {
					relations.push((kind, name));
				}
			}
		}
	}
}

#[cfg(test)]
#[path = "typescript_tests.rs"]
mod typescript_tests;
