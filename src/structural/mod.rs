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

//! Structural outline extraction: the declarations and signatures of a file,
//! with bodies elided. Pure content-in API — no filesystem or network access.
//! Callers pass a path label (only its extension picks the language) and the
//! file contents, and get structured items back.

pub mod ast_search;
pub mod graph;
mod languages;
mod signature_extractor;
pub mod symbols;

pub(crate) use languages::{graph_languages, module_root_files, BodyOpen, PackageScope};
use tree_sitter::Parser;

/// One outline item: a declaration with the span of its signature lines.
#[derive(Debug)]
pub struct OutlineItem {
	/// Simplified declaration kind: "function", "struct", "class", "heading2", ...
	pub kind: String,
	/// The declared name (selector text for CSS, pair key for JSON, "init" for
	/// Swift initializers).
	pub name: String,
	/// 1-indexed line of the item's first signature line.
	pub start_line: usize,
	/// How many source lines the signature spans (>= 1, capped at
	/// [`MAX_SIGNATURE_LINES`]). The body starts after them.
	pub sig_lines: usize,
}

/// Maximum signature lines one item may show. Long signatures (Rust where
/// clauses, Go parameter lists) are cut at this many lines; the body follows
/// them anyway, so the next item's line number still bounds what was elided.
pub const MAX_SIGNATURE_LINES: usize = 4;

/// Whether `line` may hold an import of the module `name` in `language`: a
/// pre-filter for text scans that never rejects a line the extractor reads an
/// import from. A language without an implementation keeps every line.
pub(crate) fn line_may_import(language: &str, line: &str, name: &str) -> bool {
	languages::get_language(language).is_none_or(|language| language.line_may_import(line, name))
}

/// Whether `line` of the file `path` may re-export what the module `name`
/// declares, in `language`. A language without an implementation has none.
pub(crate) fn line_may_reexport(language: &str, line: &str, name: &str, path: &str) -> bool {
	languages::get_language(language).is_some_and(|language| language.line_may_reexport(line, name, path))
}

/// Same for the line that names the parent type `name` of a declaration
/// (`extends`, `implements`, ...).
pub(crate) fn line_may_inherit(language: &str, line: &str, name: &str) -> bool {
	languages::get_language(language).is_none_or(|language| language.line_may_inherit(line, name))
}

/// Extract the outline of `contents`, picking the language from the extension
/// of `path_label`. The error is a one-line message: an unsupported extension
/// names it and the supported languages. Parsing itself never fails — a
/// syntactically broken file yields whatever tree-sitter recovers.
pub fn outline(path_label: &str, contents: &str) -> Result<Vec<OutlineItem>, String> {
	let extension = path_extension(path_label);
	let language = language_for_extension(&extension).ok_or_else(|| unsupported_message(&extension))?;
	match language {
		"markdown" => Ok(signature_extractor::extract_markdown_signatures(contents)),
		"svelte" => Ok(svelte_outline(contents)),
		_ => parse_outline(language, contents),
	}
}

/// Whether `path_label`'s extension has a grammar — the cheap pre-check a
/// directory outline runs before reading a file.
pub fn is_supported(path_label: &str) -> bool {
	language_for_extension(&path_extension(path_label)).is_some()
}

/// The language of the file at `path_label`, when it has a grammar.
fn language_of(path_label: &str) -> Option<Box<dyn languages::Language>> {
	languages::get_language(language_for_extension(&path_extension(path_label))?)
}

/// The names importers of the file write (see `Language::importer_names`).
pub(crate) fn importer_names(path: &str, declared: &[&str]) -> Vec<String> {
	language_of(path)
		.map(|language| language.importer_names(path, declared))
		.unwrap_or_default()
}

/// Whether the file stands for its directory (see `Language::is_directory_index`).
pub(crate) fn is_directory_index(path: &str) -> bool {
	language_of(path).is_some_and(|language| language.is_directory_index(path))
}

/// How the language of the file groups files into packages (see
/// `Language::package_scope`).
pub(crate) fn package_scope(path: &str) -> PackageScope {
	language_of(path).map_or(PackageScope::NoPackage, |language| language.package_scope())
}

/// What the code index reads of a language when it scans raw text: it ranks
/// files by looking at the words around a name, without parsing.
#[derive(Clone, Copy, Default)]
pub(crate) struct SpellingRules {
	/// Words after which a name is being declared.
	pub declaration_keywords: &'static [&'static str],
	/// The declaration words whose name is followed by a parameter list.
	pub callable_keywords: &'static [&'static str],
	/// The byte that sits in front of a variable's name (`$`), if any.
	pub identifier_prefix: Option<u8>,
}

/// The spelling rules of a language; none for a name without a language.
pub(crate) fn spelling_rules(language: &str) -> SpellingRules {
	languages::get_language(language).map_or_else(SpellingRules::default, |language| SpellingRules {
		declaration_keywords: language.declaration_keywords(),
		callable_keywords: language.callable_keywords(),
		identifier_prefix: language.identifier_prefix(),
	})
}

fn parse_outline(language: &str, contents: &str) -> Result<Vec<OutlineItem>, String> {
	let lang = languages::get_language(language)
		.ok_or_else(|| format!("No grammar implementation for language '{language}'."))?;
	let mut parser = Parser::new();
	parser
		.set_language(&lang.get_ts_language())
		.map_err(|e| format!("tree-sitter grammar mismatch for {language}: {e}"))?;
	// `parse` returns None only without input or on cancellation; both are
	// impossible here, but an empty outline beats a panic.
	let items = parser
		.parse(contents, None)
		.map(|tree| signature_extractor::extract_signatures(tree.root_node(), contents, lang.as_ref()))
		.unwrap_or_default();
	Ok(items)
}

/// A `.svelte` file's outline is the outline of its `<script>` blocks: the
/// svelte grammar only wraps them, so each block's raw text is re-parsed with
/// the JavaScript or TypeScript grammar (by its `lang` attribute) and the
/// item lines are offset back to the file's coordinates.
fn svelte_outline(contents: &str) -> Vec<OutlineItem> {
	let mut items = Vec::new();
	for (language, script, start_row, _) in languages::svelte::script_sources(contents) {
		if let Ok(script_items) = parse_outline(language, &script) {
			items.extend(script_items.into_iter().map(|mut item| {
				// The script text is a contiguous slice of the file, so its
				// line k is the file's line start_row + k.
				item.start_line += start_row;
				item
			}));
		}
	}
	items.sort_by_key(|item| item.start_line);
	items
}

pub(crate) fn path_extension(path_label: &str) -> String {
	// Only the last component matters, so URLs (ssh://host/x.rs) resolve the
	// same as plain paths.
	path_label
		.rsplit(['/', '\\'])
		.next()
		.unwrap_or(path_label)
		.rsplit_once('.')
		.map(|(_, ext)| ext.to_ascii_lowercase())
		.unwrap_or_default()
}

pub(crate) fn language_for_extension(extension: &str) -> Option<&'static str> {
	Some(match extension {
		"rs" => "rust",
		"py" => "python",
		"js" | "mjs" | "jsx" => "javascript",
		"ts" => "typescript",
		// JSX needs the TSX grammar: the TypeScript one reads `<div>` as a type
		// assertion and loses the components and calls inside it.
		"tsx" => "tsx",
		"go" => "go",
		"java" => "java",
		"cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "cppm" | "ixx" | "mxx" | "ccm" | "cxxm" => "cpp",
		"php" => "php",
		"sh" | "bash" => "bash",
		"rb" => "ruby",
		"lua" => "lua",
		"json" => "json",
		"swift" => "swift",
		"svelte" => "svelte",
		"css" | "scss" | "sass" => "css",
		"ex" | "exs" => "elixir",
		"md" | "markdown" => "markdown",
		_ => return None,
	})
}

pub(crate) fn unsupported_message(extension: &str) -> String {
	if extension.is_empty() {
		format!("No grammar: the path has no file extension. Supported languages: {SUPPORTED_LANGUAGES}.")
	} else {
		format!("No grammar for extension '{extension}'. Supported languages: {SUPPORTED_LANGUAGES}.")
	}
}

const SUPPORTED_LANGUAGES: &str =
	"rust, python, javascript, typescript, go, java, cpp, php, bash, ruby, lua, json, swift, svelte, css, elixir, markdown";

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
