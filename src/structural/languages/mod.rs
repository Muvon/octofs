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

use crate::structural::graph::registry::FileRegistry;

/// Kind of type-level relationship a language may report from an AST node,
/// emitted as an extends or implements edge between symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRelationKind {
	/// Inheritance: subclass extends superclass; trait/interface inherits from another.
	Extends,
	/// Interface or trait implementation: a concrete type satisfies an interface/trait.
	Implements,
}

/// A callable reference extracted from a language AST.
///
/// `name` is always the terminal callable (`run` in `service.run()`), while
/// `qualifier` preserves the receiver/module/type (`service`). Keeping both
/// avoids turning every segment of a qualified expression into a bogus call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTarget {
	pub name: String,
	pub qualifier: Option<String>,
}

/// A code region embedded in another supported language (for example a
/// JavaScript/TypeScript `<script>` inside Svelte).
#[derive(Debug, Clone)]
pub struct EmbeddedSource {
	pub language: &'static str,
	pub contents: String,
	/// 0-based line of the file where `contents` begins; added to every line
	/// the embedded block reports.
	pub start_line: u32,
}

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

	/// Node kinds that become symbol-level graph nodes. Empty by default, so a
	/// language contributes no symbols until it opts in; kinds listed here must
	/// declare something (not imports, calls or statements).
	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		Vec::new()
	}

	/// Import and export names declared at this node, as `(imports, exports)`.
	/// The caller walks the tree; this inspects one node.
	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let _ = (node, contents);
		(Vec::new(), Vec::new())
	}

	/// Callees invoked at this node, empty unless the node is a call. The
	/// caller walks the tree and tracks the line.
	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		let _ = (node, contents);
		Vec::new()
	}

	/// Owning type/module for a declared symbol, when it is syntactically known.
	/// The default handles the common class/module/impl ancestor shapes;
	/// languages with receiver syntax on the declaration itself (notably Go)
	/// override it.
	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		find_graph_symbol_owner(node, contents)
	}

	/// Code regions embedded in another language that should join the graph.
	fn extract_embedded_sources(&self, root: Node, contents: &str) -> Vec<EmbeddedSource> {
		let _ = (root, contents);
		Vec::new()
	}

	/// Extends/implements relations declared at this node as `(kind, target
	/// name)` pairs: `class Foo extends Bar implements Baz` yields
	/// `[(Extends, "Bar"), (Implements, "Baz")]`. The caller walks the tree.
	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let _ = (node, contents);
		Vec::new()
	}

	/// Name of the declaration that owns relations emitted for this node. Most
	/// languages name it on the same node; languages with separate
	/// implementation blocks override this.
	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		self.extract_declaration_name(node, contents)
	}

	/// Resolve an import string to a path in `all_files`, or None when it does
	/// not point at a file of the repository (an external dependency).
	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		let _ = (import_path, source_file, all_files);
		None
	}

	/// Substrings of which at least one appears on the same line as the imported
	/// module/file name in every import form this language's extractor understands
	/// that the line holds a keyword for. Empty means "no cheap filter": every line
	/// naming the module is a candidate.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&[]
	}

	/// Same, for the line that names a parent type (extends / implements / embeds /
	/// mixes in) in every form `extract_type_relations` understands.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&[]
	}

	/// Whether `line` may be the line of an import of the module `name` (a file
	/// stem or directory name). Sound, not exact: it never rejects a line the
	/// extractor reads an import from. The default accepts a line with a marker
	/// and a line that holds nothing but names, which is how the continuation
	/// lines of a grouped import look. Languages whose imports end in a quoted
	/// path override it.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		let markers = self.import_line_markers();
		contains_identifier(line, name)
			&& (markers.is_empty()
				|| markers.iter().any(|marker| marker_in_line(line, marker))
				|| name_list_line(line, name, ""))
	}

	/// Whether `line` may name the parent type `name` of a declaration, with
	/// the same soundness rule as `line_may_import`.
	fn line_may_inherit(&self, line: &str, name: &str) -> bool {
		inheritance_line(self.inheritance_line_markers(), line, name, INHERITANCE_LIST_EXTRA)
	}

	/// Every repository file an import names; empty for an external
	/// dependency. Languages whose imports name a package or directory rather
	/// than one file (Go, Java wildcards, Swift modules) override this with
	/// all the files of that package; everything else names at most one file.
	fn resolve_import_files(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Vec<String> {
		self.resolve_import(import_path, source_file, all_files)
			.into_iter()
			.collect()
	}
}

/// Characters other than identifier characters, spaces and brackets that a
/// line of parent names may hold: bounds (`+`, `&`), generics, `:` and `;`.
const INHERITANCE_LIST_EXTRA: &str = "+&<>:;'?";

fn is_identifier_char(character: char) -> bool {
	character.is_alphanumeric() || character == '_' || character == '$'
}

/// `name` occurs in `line` as a whole identifier.
pub(crate) fn contains_identifier(line: &str, name: &str) -> bool {
	!name.is_empty()
		&& line.match_indices(name).any(|(start, _)| {
			line[..start]
				.chars()
				.next_back()
				.is_none_or(|before| !is_identifier_char(before))
				&& line[start + name.len()..]
					.chars()
					.next()
					.is_none_or(|after| !is_identifier_char(after))
		})
}

/// A keyword marker must stand alone (`use` is not in `user`); a symbol
/// marker (`::`, `\`) is a plain substring.
fn marker_in_line(line: &str, marker: &str) -> bool {
	let keyword = marker
		.chars()
		.next()
		.zip(marker.chars().last())
		.is_some_and(|(first, last)| is_identifier_char(first) && is_identifier_char(last));
	if keyword {
		contains_identifier(line, marker)
	} else {
		line.contains(marker)
	}
}

/// A parent-type line: a marker keyword, or names only (`extra` widens what
/// counts as a name list for the language). No markers keeps every line that
/// names the type.
pub(crate) fn inheritance_line(markers: &[&str], line: &str, name: &str, extra: &str) -> bool {
	contains_identifier(line, name)
		&& (markers.is_empty()
			|| markers.iter().any(|marker| marker_in_line(line, marker))
			|| name_list_line(line, name, extra))
}

/// The line without a trailing comment.
fn without_comment(line: &str) -> &str {
	let cut = ["//", "#", "/*"].iter().filter_map(|start| line.find(start)).min();
	cut.map_or(line, |index| &line[..index])
}

/// A line that holds `name` among names only: identifiers, spaces, commas and
/// brackets (plus `extra`), such as the `b,` of a parenthesised import group.
/// A name used in code (`query = x`, `query.filter()`) has other characters.
fn name_list_line(line: &str, name: &str, extra: &str) -> bool {
	let code = without_comment(line);
	contains_identifier(code, name)
		&& code.chars().all(|character| {
			is_identifier_char(character)
				|| character.is_whitespace()
				|| ",(){}".contains(character)
				|| extra.contains(character)
		})
}

/// `name` is the last segment of a path written in `line`: whole identifier,
/// then any file extensions, then one of `closers` (or the end of the line
/// when `closers` holds a newline). A URL string that merely contains the
/// name does not qualify.
pub(crate) fn path_ends_with_name(line: &str, name: &str, closers: &str) -> bool {
	if name.is_empty() {
		return false;
	}
	line.match_indices(name).any(|(start, _)| {
		if line[..start].chars().next_back().is_some_and(is_identifier_char) {
			return false;
		}
		let mut rest = &line[start + name.len()..];
		while let Some(after_dot) = rest.strip_prefix('.') {
			let extension_len = after_dot
				.find(|character: char| !is_identifier_char(character))
				.unwrap_or(after_dot.len());
			if extension_len == 0 {
				break;
			}
			rest = &after_dot[extension_len..];
		}
		match rest.chars().next() {
			None => closers.contains('\n'),
			Some(next) => closers.contains(next),
		}
	})
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
		"svelte" => Some(Box::new(svelte::Svelte {})),
		_ => None,
	}
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

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

/// Whether `path` ends with `suffix` on a path-component boundary:
/// `a/b/c.go` ends with `b/c.go` and `c.go`, but `abc.go` does not end with
/// `c.go`.
pub(crate) fn path_ends_with_components(path: &str, suffix: &str) -> bool {
	path.strip_suffix(suffix)
		.is_some_and(|head| head.is_empty() || head.ends_with('/'))
}

/// Every file directly in `directory` that `accept`s, in alphabetical order.
pub(crate) fn files_in_dir_matching(
	registry: &FileRegistry,
	directory: &str,
	accept: impl Fn(&str) -> bool,
) -> Vec<String> {
	let mut files: Vec<String> = registry
		.files_in_dir(directory)
		.iter()
		.filter(|file| accept(file.as_str()))
		.cloned()
		.collect();
	files.sort();
	files
}

/// Parse a textual callee expression into its terminal name and qualifier.
/// Handles the shared forms used by the supported languages: `foo`,
/// `module.foo`, `Type::method`, `ptr->method`, and optional chaining. Generic
/// arguments on the callable are removed before splitting. Returns None when
/// the callee is not a plain identifier path — the qualifier segments must be
/// identifier-like too, so expressions like `1 + 2` never name an item.
pub(crate) fn parse_call_target(text: &str) -> Option<CallTarget> {
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
	let qualifier = if qualifier_segments.is_empty() {
		None
	} else {
		Some(qualifier_segments.join("::"))
	};
	Some(CallTarget {
		name: name.to_string(),
		qualifier,
	})
}

/// The simple (unqualified) name of a textual callee expression: the terminal
/// segment of [`parse_call_target`], for callers that do not need the
/// qualifier.
pub(crate) fn extract_call_target(text: &str) -> Option<String> {
	parse_call_target(text).map(|target| target.name)
}

/// Nearest enclosing declaration that provides a stable method owner: a
/// class, struct, interface, trait, module, namespace, extension or `impl`.
pub(crate) fn find_graph_symbol_owner(node: Node, contents: &str) -> Option<String> {
	let mut current = node.parent();
	while let Some(parent) = current {
		let kind = parent.kind();
		let is_owner = !kind.contains("body")
			&& (kind.contains("class")
				|| kind.contains("struct")
				|| kind.contains("interface")
				|| kind.contains("trait")
				|| kind.contains("module")
				|| kind.contains("namespace")
				|| kind.contains("extension")
				|| kind == "impl_item");
		if is_owner {
			// `type` first: it names the receiver of `impl Trait for Foo`.
			for field in ["type", "name"] {
				if let Some(name_node) = parent.child_by_field_name(field) {
					if let Ok(text) = name_node.utf8_text(contents.as_bytes()) {
						if let Some(name) = simple_type_name(text) {
							return Some(name);
						}
					}
				}
			}
			return extract_symbol_by_kinds(parent, contents, &["identifier", "name", "type_identifier", "constant"])
				.and_then(|name| simple_type_name(&name));
		}
		current = parent.parent();
	}
	None
}
