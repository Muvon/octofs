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

//! Symbol navigation: definitions and references of a name, plus the
//! enclosing-symbol breadcrumbs content searches show. Pure content-in API
//! like `outline` — the path label only picks the language, nothing here
//! touches the filesystem or network.

use tree_sitter::{Node, Parser};

use crate::structural::languages::{self, Language};
use crate::structural::signature_extractor;
use crate::structural::{language_for_extension, path_extension, unsupported_message};

/// One definition or reference hit. Lines are 1-based; `column` is the 0-based
/// byte column where the defining/used identifier starts.
#[derive(Debug)]
pub struct SymbolHit {
	/// First line of the definition, or the line of the reference.
	pub line: usize,
	/// Lines the definition spans; 1 for references.
	pub line_count: usize,
	pub column: usize,
	/// References only: the hit is the defining identifier itself.
	pub is_definition: bool,
	/// Enclosing named containers, outermost first (e.g. "impl Store › fn flush").
	/// None at file top level.
	pub breadcrumb: Option<String>,
}

/// Name filter for symbol queries. `*` in the spec is a wildcard
/// (e.g. `handle_*`, `*Config`); anything else matches exactly.
#[derive(Clone)]
pub enum NameMatcher {
	Exact(String),
	Wildcard(regex::Regex),
}

impl NameMatcher {
	pub fn new(spec: &str) -> Result<Self, String> {
		if spec.contains('*') {
			let re = format!(
				"^{}$",
				spec.split('*').map(regex::escape).collect::<Vec<_>>().join(".*")
			);
			regex::Regex::new(&re)
				.map(NameMatcher::Wildcard)
				.map_err(|e| format!("Invalid symbol wildcard {spec:?}: {e}"))
		} else {
			Ok(NameMatcher::Exact(spec.to_string()))
		}
	}

	pub fn matches(&self, name: &str) -> bool {
		match self {
			NameMatcher::Exact(s) => s == name,
			NameMatcher::Wildcard(re) => re.is_match(name),
		}
	}

	/// Whether the spec contains a wildcard. Wildcard definitions are always
	/// previewed; an exact name may return the complete definition.
	pub fn is_wildcard(&self) -> bool {
		matches!(self, NameMatcher::Wildcard(_))
	}
}

/// Find symbol definitions by name: every node of a definition kind whose
/// declared name satisfies `matcher`. The error is the outline's unsupported
/// message — a single file with no grammar is a caller error.
pub fn find_definitions(path_label: &str, contents: &str, matcher: &NameMatcher) -> Result<Vec<SymbolHit>, String> {
	symbol_hits_by_language(language(path_label)?, contents, matcher, false)
}

/// Find references by name: every identifier usage whose text satisfies
/// `matcher`. Definition sites are flagged so the caller can tag them.
pub fn find_references(path_label: &str, contents: &str, matcher: &NameMatcher) -> Result<Vec<SymbolHit>, String> {
	symbol_hits_by_language(language(path_label)?, contents, matcher, true)
}

/// Definitions or references of `matcher`, by language. The languages without
/// a navigable symbol grammar (markdown, svelte, css, json) never reach the
/// tree walk.
fn symbol_hits_by_language(
	language: &str,
	contents: &str,
	matcher: &NameMatcher,
	references: bool,
) -> Result<Vec<SymbolHit>, String> {
	match language {
		// Markdown has no grammar: its headings are the definitions.
		"markdown" => return Ok(markdown_heading_hits(contents, matcher)),
		// Svelte code lives in <script> blocks, re-parsed with the JS/TS
		// grammar and mapped back to file coordinates.
		"svelte" => return Ok(svelte_symbol_hits(contents, matcher, references)),
		// css/json outlines are structural but name no navigable symbols: say
		// so instead of reporting a miss indistinguishable from a wrong name.
		"css" | "json" => {
			return Err(format!(
				"symbol navigation is not supported for {language} files — use flags: \"s\" for an outline or `content` for text"
			))
		}
		_ => {}
	}
	let lang = languages::get_language(language)
		.ok_or_else(|| format!("No grammar implementation for language '{language}'."))?;
	let Some(tree) = parse_tree(lang.as_ref(), contents)? else {
		return Ok(Vec::new());
	};
	if references {
		Ok(reference_hits(lang.as_ref(), &tree, contents, matcher))
	} else {
		Ok(definition_hits(lang.as_ref(), &tree, contents, matcher))
	}
}

fn definition_hits(
	lang: &dyn Language,
	tree: &tree_sitter::Tree,
	contents: &str,
	matcher: &NameMatcher,
) -> Vec<SymbolHit> {
	let def_kinds = definition_kinds(lang.name());
	let containers = container_kinds(lang.name());
	let mut hits = Vec::new();
	for node in descendants(tree.root_node()) {
		if !def_kinds.contains(&node.kind()) {
			continue;
		}
		// One source of truth with the outline: the same per-language
		// declaration-name rules decide what a definition is called.
		let Some(name) = lang.extract_declaration_name(node, contents) else {
			continue;
		};
		if !matcher.matches(&name) {
			continue;
		}
		let start = node.start_position();
		let end = node.end_position();
		hits.push(SymbolHit {
			line: start.row + 1,
			line_count: end.row - start.row + 1,
			column: start.column,
			is_definition: true,
			breadcrumb: breadcrumb_for(lang, node, contents, containers),
		});
	}
	hits
}

fn reference_hits(
	lang: &dyn Language,
	tree: &tree_sitter::Tree,
	contents: &str,
	matcher: &NameMatcher,
) -> Vec<SymbolHit> {
	let def_kinds = definition_kinds(lang.name());
	let containers = container_kinds(lang.name());
	let mut hits = Vec::new();
	for node in descendants(tree.root_node()) {
		// Identifier usages are named leaf tokens; interior nodes carry
		// structure, not a name.
		if !node.is_named() || node.child_count() > 0 || !is_identifier_kind(node.kind()) {
			continue;
		}
		let Ok(text) = node.utf8_text(contents.as_bytes()) else {
			continue;
		};
		if !matcher.matches(text) {
			continue;
		}
		let start = node.start_position();
		hits.push(SymbolHit {
			line: start.row + 1,
			line_count: 1,
			column: start.column,
			is_definition: is_definition_site(lang, node, text, def_kinds, contents),
			breadcrumb: breadcrumb_for(lang, node, contents, containers),
		});
	}
	hits
}

/// Per-line enclosing-symbol breadcrumbs for a whole file: entry `i` is the
/// chain of containers enclosing line `i+1`. None when the file has no
/// grammar, no containers, or cannot be parsed — breadcrumbs are decoration
/// and must never fail the search that asked for them.
pub fn line_breadcrumbs(path_label: &str, contents: &str) -> Option<Vec<Option<String>>> {
	let language = language(path_label).ok()?;
	let containers = container_kinds(language);
	if containers.is_empty() {
		return None;
	}
	let lang = languages::get_language(language)?;
	let tree = parse_tree(lang.as_ref(), contents).ok()??;
	let line_count = contents.lines().count();
	// One pass: collect the containers with their breadcrumb (pre-order, so
	// start rows are non-decreasing), then sweep the rows once with a stack of
	// open containers — repainting every container's full row range made
	// nested files O(lines·depth).
	let mut spans: Vec<(usize, usize, String)> = Vec::new();
	for node in descendants(tree.root_node()) {
		if let Some((_, label)) = containers.iter().find(|(kind, _)| *kind == node.kind()) {
			let start = node.start_position().row;
			let end = node.end_position().row;
			spans.push((
				start,
				end,
				container_breadcrumb(lang.as_ref(), node, contents, containers, label),
			));
		}
	}
	let mut out = vec![None; line_count];
	let mut active: Vec<&(usize, usize, String)> = Vec::new();
	let mut next_span = 0usize;
	for (row, slot) in out.iter_mut().enumerate() {
		while active.last().is_some_and(|(_, end, _)| *end < row) {
			active.pop();
		}
		while next_span < spans.len() && spans[next_span].0 == row {
			active.push(&spans[next_span]);
			next_span += 1;
		}
		// Tree ranges nest, so the open spans form a chain and the top is the
		// innermost — the chain a repaint would have left on this row.
		if let Some((_, _, breadcrumb)) = active.last() {
			*slot = Some(breadcrumb.clone());
		}
	}
	Some(out)
}

fn language(path_label: &str) -> Result<&'static str, String> {
	let extension = path_extension(path_label);
	language_for_extension(&extension).ok_or_else(|| unsupported_message(&extension))
}

fn parse_tree(lang: &dyn Language, contents: &str) -> Result<Option<tree_sitter::Tree>, String> {
	let mut parser = Parser::new();
	parser
		.set_language(&lang.get_ts_language())
		.map_err(|e| format!("tree-sitter grammar mismatch for {}: {e}", lang.name()))?;
	// None only without input or on cancellation; both are impossible here.
	Ok(parser.parse(contents, None))
}

/// Pre-order walk of every node (parents before children), cursor-based so
/// both the recursion depth and the per-child cost stay linear: `child(i)` is
/// O(i), which made wide files quadratic.
fn descendants(root: Node) -> Vec<Node> {
	let mut out = Vec::new();
	let mut cursor = root.walk();
	'walk: loop {
		out.push(cursor.node());
		if cursor.goto_first_child() {
			continue 'walk;
		}
		loop {
			if cursor.goto_next_sibling() {
				continue 'walk;
			}
			if !cursor.goto_parent() {
				return out;
			}
		}
	}
}

fn is_identifier_kind(kind: &str) -> bool {
	// Covers identifier/type_identifier/field_identifier/property_identifier
	// (most grammars), `name` (PHP), `constant` (Ruby).
	kind.contains("identifier") || kind == "name" || kind == "constant"
}

/// Markdown definitions are its headings — the outline's own extraction, so
/// the two agree on what a heading is (fenced code excluded).
fn markdown_heading_hits(contents: &str, matcher: &NameMatcher) -> Vec<SymbolHit> {
	signature_extractor::extract_markdown_signatures(contents)
		.into_iter()
		.filter(|item| matcher.matches(&item.name))
		.map(|item| {
			// The heading text's column: the first non-`#`, non-space byte of
			// the heading line.
			let column = contents
				.lines()
				.nth(item.start_line - 1)
				.map(|line| line.len() - line.trim_start_matches('#').trim_start().len())
				.unwrap_or(0);
			SymbolHit {
				line: item.start_line,
				line_count: 1,
				column,
				is_definition: true,
				breadcrumb: None,
			}
		})
		.collect()
}

/// Svelte symbols live in the `<script>` blocks: each block's text is
/// searched with its own grammar and the hits mapped back to the file's
/// coordinates (the block is a contiguous slice of the file).
fn svelte_symbol_hits(contents: &str, matcher: &NameMatcher, references: bool) -> Vec<SymbolHit> {
	let mut hits = Vec::new();
	for (language, script, start_row, start_column) in languages::svelte::script_sources(contents) {
		let Ok(block_hits) = symbol_hits_by_language(language, &script, matcher, references) else {
			continue;
		};
		hits.extend(block_hits.into_iter().map(|mut hit| {
			// The script's line k is the file's line start_row + k; only the
			// script's first line carries the tag's column offset.
			if hit.line == 1 {
				hit.column += start_column;
			}
			hit.line += start_row;
			hit
		}));
	}
	hits.sort_by_key(|hit| hit.line);
	hits
}

/// Whether an identifier node is the defining name of a declaration. Elixir
/// has no `name` field, so any enclosing declaration naming the identifier
/// counts.
fn is_definition_site(lang: &dyn Language, node: Node, text: &str, def_kinds: &[&str], contents: &str) -> bool {
	if lang.name() == "elixir" {
		let mut ancestor = node.parent();
		while let Some(current) = ancestor {
			if lang.extract_declaration_name(current, contents).as_deref() == Some(text) {
				return true;
			}
			ancestor = current.parent();
		}
		return false;
	}
	node.parent().is_some_and(|parent| {
		def_kinds.contains(&parent.kind())
			&& parent
				.child_by_field_name("name")
				.is_some_and(|name| name.id() == node.id())
	})
}

/// Breadcrumb of the containers enclosing `node` (not `node` itself),
/// outermost first.
fn breadcrumb_for(lang: &dyn Language, node: Node, contents: &str, containers: &[(&str, &str)]) -> Option<String> {
	let mut parts = Vec::new();
	let mut ancestor = node.parent();
	while let Some(current) = ancestor {
		if let Some((_, label)) = containers.iter().find(|(kind, _)| *kind == current.kind()) {
			let name = lang
				.extract_declaration_name(current, contents)
				.unwrap_or_else(|| "_".to_string());
			parts.push(format!("{label} {name}"));
		}
		ancestor = current.parent();
	}
	if parts.is_empty() {
		return None;
	}
	parts.reverse();
	Some(parts.join(" › "))
}

/// Breadcrumb of the container chain down to `node` itself, outermost first.
/// `label` is the display label of `node`'s own kind — the caller matched it
/// against `containers` already.
fn container_breadcrumb(
	lang: &dyn Language,
	node: Node,
	contents: &str,
	containers: &[(&str, &str)],
	label: &str,
) -> String {
	let mut parts = Vec::new();
	let mut ancestor = node.parent();
	while let Some(current) = ancestor {
		if let Some((_, ancestor_label)) = containers.iter().find(|(kind, _)| *kind == current.kind()) {
			let name = lang
				.extract_declaration_name(current, contents)
				.unwrap_or_else(|| "_".to_string());
			parts.push(format!("{ancestor_label} {name}"));
		}
		ancestor = current.parent();
	}
	parts.reverse();
	let name = lang
		.extract_declaration_name(node, contents)
		.unwrap_or_else(|| "_".to_string());
	parts.push(format!("{label} {name}"));
	parts.join(" › ")
}

/// (tree-sitter kind, short display label) pairs that form symbol breadcrumbs
/// for the given language.
fn container_kinds(language: &str) -> &'static [(&'static str, &'static str)] {
	match language {
		"rust" => &[
			("function_item", "fn"),
			("impl_item", "impl"),
			("trait_item", "trait"),
			("struct_item", "struct"),
			("enum_item", "enum"),
			("mod_item", "mod"),
		],
		"python" => &[("function_definition", "def"), ("class_definition", "class")],
		"javascript" | "typescript" => &[
			("function_declaration", "function"),
			("generator_function_declaration", "function"),
			("method_definition", "method"),
			("class_declaration", "class"),
			("interface_declaration", "interface"),
			("enum_declaration", "enum"),
			// tree-sitter-typescript's namespace/module containers.
			("internal_module", "namespace"),
			("module", "module"),
		],
		"go" => &[
			("function_declaration", "func"),
			("method_declaration", "method"),
			("type_declaration", "type"),
		],
		"java" => &[
			("class_declaration", "class"),
			("interface_declaration", "interface"),
			("enum_declaration", "enum"),
			("record_declaration", "record"),
			("annotation_type_declaration", "annotation"),
			("method_declaration", "method"),
			("constructor_declaration", "constructor"),
		],
		"cpp" => &[
			("function_definition", "fn"),
			("class_specifier", "class"),
			("struct_specifier", "struct"),
			("union_specifier", "union"),
			("namespace_definition", "namespace"),
		],
		"php" => &[
			("function_definition", "function"),
			("method_declaration", "method"),
			("class_declaration", "class"),
			("interface_declaration", "interface"),
			("trait_declaration", "trait"),
			("enum_declaration", "enum"),
		],
		"ruby" => &[
			("method", "def"),
			("singleton_method", "def"),
			("class", "class"),
			("module", "module"),
		],
		"lua" => &[("function_declaration", "function")],
		"bash" => &[("function_definition", "function")],
		"swift" => &[
			("function_declaration", "func"),
			("class_declaration", "class"),
			("protocol_declaration", "protocol"),
		],
		// Elixir declarations all use the generic `call` node. Adding `call`
		// here would make every nested invocation look like a container.
		"elixir" => &[],
		_ => &[],
	}
}

/// Tree-sitter kinds that define named symbols, per language.
fn definition_kinds(language: &str) -> &'static [&'static str] {
	match language {
		"rust" => &[
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"impl_item",
			"mod_item",
			"const_item",
			"static_item",
			"type_item",
			"union_item",
			"macro_definition",
		],
		"javascript" => &[
			"function_declaration",
			"generator_function_declaration",
			"class_declaration",
			"method_definition",
			"variable_declarator",
		],
		"typescript" => &[
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
		],
		"python" => &["function_definition", "class_definition"],
		"go" => &[
			"function_declaration",
			"method_declaration",
			"type_spec",
			"const_spec",
			"var_spec",
		],
		"java" => &[
			"class_declaration",
			"interface_declaration",
			"enum_declaration",
			"record_declaration",
			"method_declaration",
			"constructor_declaration",
		],
		"cpp" => &[
			"function_definition",
			"class_specifier",
			"struct_specifier",
			"enum_specifier",
			"namespace_definition",
			"type_definition",
		],
		"php" => &[
			"function_definition",
			"method_declaration",
			"class_declaration",
			"interface_declaration",
			"trait_declaration",
		],
		"ruby" => &["method", "singleton_method", "class", "module"],
		"lua" => &["function_declaration"],
		"bash" => &["function_definition"],
		"swift" => &["function_declaration", "class_declaration", "protocol_declaration"],
		"elixir" => &["call"],
		_ => &[],
	}
}

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod symbols_tests;
