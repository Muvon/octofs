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

//! Language registry for structural outline extraction: one implementation
//! per tree-sitter grammar, exposing the node kinds that become outline items
//! and how to name them.

pub(crate) mod bash;
pub(crate) mod cpp;
pub(crate) mod css;
pub(crate) mod elixir;
pub(crate) mod go;
pub(crate) mod java;
pub(crate) mod javascript;
pub(crate) mod json;
pub(crate) mod lua;
pub(crate) mod php;
pub(crate) mod python;
pub(crate) mod ruby;
pub(crate) mod rust;
pub(crate) mod svelte;
pub(crate) mod swift;
pub(crate) mod typescript;

use tree_sitter::Node;

/// A tree-sitter grammar plus the outline decisions for it.
pub(crate) trait Language: Send + Sync {
	/// Name of the language.
	fn name(&self) -> &'static str;

	/// The tree-sitter grammar to parse with.
	fn get_ts_language(&self) -> tree_sitter::Language;

	/// Node kinds that become outline items. Defaults to the chunking kinds;
	/// languages override to expose container declarations (classes,
	/// interfaces, enums) that chunking deliberately drops, and to drop
	/// non-declarations (imports, calls, statements) that declare nothing.
	fn get_outline_kinds(&self) -> Vec<&'static str>;

	/// Whether a listed node becomes an item. Macro-oriented grammars such as
	/// Elixir classify their `call` nodes by call target instead of kind.
	fn is_signature_node(&self, node: Node, contents: &str) -> bool {
		let _ = (node, contents);
		true
	}

	/// Declared name of the item at this node, or None when the node has no
	/// single declarable name (such nodes are skipped). The default covers the
	/// dominant tree-sitter convention: a direct child whose kind is or
	/// contains "identifier", "name", or "type_identifier".
	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}

	/// Name shown by the outline. Usually identical to the declaration name,
	/// but declaration forms that do not create a standalone symbol may still
	/// expose a useful name.
	fn extract_signature_name(&self, node: Node, contents: &str) -> Option<String> {
		self.extract_declaration_name(node, contents)
	}

	/// Coarse declaration kind when it cannot be inferred from the grammar's
	/// node kind. Callers fall back to their normal node-kind mapping.
	fn extract_declaration_kind(&self, node: Node, contents: &str) -> Option<&'static str> {
		let _ = (node, contents);
		None
	}
}

/// Gets a language implementation by its name.
pub(crate) fn get_language(name: &str) -> Option<Box<dyn Language>> {
	match name {
		"rust" => Some(Box::new(rust::Rust {})),
		"javascript" => Some(Box::new(javascript::JavaScript {})),
		"typescript" => Some(Box::new(typescript::TypeScript {})),
		"python" => Some(Box::new(python::Python {})),
		"go" => Some(Box::new(go::Go {})),
		"java" => Some(Box::new(java::Java {})),
		"cpp" => Some(Box::new(cpp::Cpp {})),
		"php" => Some(Box::new(php::Php {})),
		"bash" => Some(Box::new(bash::Bash {})),
		"ruby" => Some(Box::new(ruby::Ruby {})),
		"lua" => Some(Box::new(lua::Lua {})),
		"json" => Some(Box::new(json::Json {})),
		"swift" => Some(Box::new(swift::Swift {})),
		"css" => Some(Box::new(css::Css {})),
		"elixir" => Some(Box::new(elixir::Elixir {})),
		_ => None,
	}
}

/// Extract a symbol from a node by finding a child matching any of multiple
/// kinds. A kind also matches when the child's kind merely contains it, which
/// is how "type_identifier" and "property_identifier" hit "identifier".
pub(crate) fn extract_symbol_by_kinds(node: Node, contents: &str, target_kinds: &[&str]) -> Option<String> {
	for child in node.children(&mut node.walk()) {
		// Anonymous tokens (keywords such as TS's `namespace`) are never
		// declaration names, and their kinds can contain a target kind
		// ("namespace" contains "name").
		if !child.is_named() {
			continue;
		}
		if target_kinds.contains(&child.kind()) || target_kinds.iter().any(|k| child.kind().contains(k)) {
			if let Ok(text) = child.utf8_text(contents.as_bytes()) {
				return Some(text.to_string());
			}
		}
	}
	None
}

/// The simple (unqualified, non-generic) type name from a possibly qualified
/// or generic type expression: `std::collections::HashMap<K, V>` → `HashMap`,
/// `com.example.Foo` → `Foo`, `App\Models\User` → `User`, `Store[T]` → `Store`.
pub(crate) fn simple_type_name(text: &str) -> Option<String> {
	// Cut at the first bracket of any kind: trimming only the ends leaves the
	// opening bracket behind (`Store[T]` → `Store[T`).
	let stripped = text.split(['<', '[', '(']).next().unwrap_or(text);
	let after_colons = stripped.rsplit("::").next().unwrap_or(stripped);
	let after_backslash = after_colons.rsplit('\\').next().unwrap_or(after_colons);
	let after_dots = after_backslash.rsplit('.').next().unwrap_or(after_backslash);
	let trimmed = after_dots
		.trim()
		.trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
	if trimmed.is_empty() {
		None
	} else {
		Some(trimmed.to_string())
	}
}

/// Parse a textual callee expression to its simple (unqualified) name.
/// Handles the shared forms used by the supported languages: `foo`,
/// `module.foo`, `Type::method`, `ptr->method`, and optional chaining. Generic
/// arguments on the callable are removed before splitting. Returns None when
/// the callee is not a plain identifier path — the qualifier segments must be
/// identifier-like too, so expressions like `1 + 2` never name an item.
pub(crate) fn extract_call_target(text: &str) -> Option<String> {
	let mut trimmed = text.trim().trim_start_matches('&').trim_start_matches('*');
	if trimmed.is_empty() {
		return None;
	}

	let mut without_generics = String::with_capacity(trimmed.len());
	let mut generic_depth = 0u32;
	for character in trimmed.chars() {
		match character {
			'<' => generic_depth += 1,
			'>' if generic_depth > 0 => generic_depth -= 1,
			_ if generic_depth == 0 => without_generics.push(character),
			_ => {}
		}
	}
	trimmed = without_generics.trim();
	let normalized = trimmed.replace("?.", ".").replace("->", ".");
	let segments: Vec<&str> = normalized
		.split(['.', ':'])
		.map(str::trim)
		.filter(|segment| !segment.is_empty())
		.collect();
	let (name, qualifier_segments) = segments.split_last()?;
	let name = name.trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
	if name.is_empty()
		|| !name
			.chars()
			.all(|character| character.is_alphanumeric() || character == '_')
	{
		return None;
	}
	if qualifier_segments.iter().any(|segment| {
		!segment
			.chars()
			.all(|character: char| character.is_alphanumeric() || matches!(character, '_' | '$' | '@' | '#'))
	}) {
		return None;
	}
	Some(name.to_string())
}
