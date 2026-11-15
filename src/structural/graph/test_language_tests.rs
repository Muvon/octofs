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

//! Languages for graph tests. No shipped language implements graph extraction
//! yet, so these small Rust-grammar languages exercise the extraction, import
//! resolution and embedding paths end to end.

use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
use crate::structural::languages::{
	enclosing_owner, parse_call_target, simple_type_name, CalleeSyntax, EmbeddedSource, Language, SymbolKind,
	TypeRelationKind,
};

const CALLEE_SYNTAX: CalleeSyntax = CalleeSyntax {
	member_separators: &["::", "."],
	self_receivers: &["self", "Self"],
	angle_generics: true,
	..CalleeSyntax::DOTTED
};
use tree_sitter::Node;

fn field_text(node: Node, field: &str, contents: &str) -> Option<String> {
	let child = node.child_by_field_name(field)?;
	child.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// Rust grammar with symbols, `use` imports, calls and `impl ... for`
/// relations. Imports starting `crate::` resolve to `src/<module>.rs`, imports
/// starting `./` to a sibling `.js` file, everything else is external.
pub(super) struct TestLanguage;

impl Language for TestLanguage {
	fn name(&self) -> &'static str {
		"rust"
	}

	// A test language is reached by name, never by a file extension.
	fn extensions(&self) -> &'static [&'static str] {
		&[]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec!["function_item"]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		vec!["function_item", "struct_item", "trait_item"]
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		enclosing_owner(node, contents, &["impl_item", "trait_item"])
	}

	fn symbol_kind(&self, node: Node, _contents: &str) -> SymbolKind {
		match node.kind() {
			"function_item" => SymbolKind::Function,
			"struct_item" => SymbolKind::Struct,
			_ => SymbolKind::Trait,
		}
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		if node.kind() != "use_declaration" {
			return (Vec::new(), Vec::new());
		}
		let Some(argument) = node.child_by_field_name("argument") else {
			return (Vec::new(), Vec::new());
		};
		let path = if argument.kind() == "use_as_clause" {
			argument.child_by_field_name("path").unwrap_or(argument)
		} else {
			argument
		};
		let imports = path
			.utf8_text(contents.as_bytes())
			.map(|text| vec![text.to_string()])
			.unwrap_or_default();
		(imports, Vec::new())
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<crate::structural::languages::CallTarget> {
		if node.kind() != "call_expression" {
			return Vec::new();
		}
		field_text(node, "function", contents)
			.and_then(|text| parse_call_target(&text, &CALLEE_SYNTAX))
			.into_iter()
			.collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		if node.kind() != "impl_item" {
			return Vec::new();
		}
		field_text(node, "trait", contents)
			.and_then(|text| simple_type_name(&text))
			.map(|name| (TypeRelationKind::Implements, name))
			.into_iter()
			.collect()
	}

	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			return field_text(node, "type", contents).and_then(|text| simple_type_name(&text));
		}
		self.extract_declaration_name(node, contents)
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if let Some(module_path) = import_path.strip_prefix("crate::") {
			let (module, _item) = module_path.rsplit_once("::")?;
			return all_files.find_file_with_extensions(&format!("src/{}", module.replace("::", "/")), &["rs"]);
		}
		if import_path.starts_with("./") {
			return all_files.find_file_with_extensions(&resolve_relative_path(source_file, import_path), &["js"]);
		}
		None
	}
}

/// A host language whose every file embeds one fixed Rust block three lines
/// down, standing in for a Svelte `<script>`.
pub(super) struct EmbeddingLanguage;

pub(super) const EMBEDDED_BLOCK: &str = "fn helper() {}\nfn run() { helper(); }\n";
pub(super) const EMBEDDED_BLOCK_START_LINE: u32 = 3;

impl Language for EmbeddingLanguage {
	fn name(&self) -> &'static str {
		"host"
	}

	// A test language is reached by name, never by a file extension.
	fn extensions(&self) -> &'static [&'static str] {
		&[]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		Vec::new()
	}

	fn extract_embedded_sources(&self, _root: Node, _contents: &str) -> Vec<EmbeddedSource> {
		vec![EmbeddedSource {
			language: "rust",
			contents: EMBEDDED_BLOCK.to_string(),
			start_line: EMBEDDED_BLOCK_START_LINE,
		}]
	}
}

/// Implements only the required trait methods, so it shows what a language
/// that has not opted into the graph contributes: nothing but its file node.
pub(super) struct BareLanguage;

impl Language for BareLanguage {
	fn name(&self) -> &'static str {
		"bare"
	}

	// A test language is reached by name, never by a file extension.
	fn extensions(&self) -> &'static [&'static str] {
		&[]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec!["function_item"]
	}
}

/// Language lookup for tests: `rust`, `host` and `bare`, nothing else.
pub(super) fn lookup(name: &str) -> Option<Box<dyn Language>> {
	match name {
		"rust" => Some(Box::new(TestLanguage)),
		"host" => Some(Box::new(EmbeddingLanguage)),
		"bare" => Some(Box::new(BareLanguage)),
		_ => None,
	}
}
