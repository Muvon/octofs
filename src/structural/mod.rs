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
mod languages;
mod signature_extractor;
pub mod symbols;

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

/// Where a declaration's body opens, deciding which lines of the item are its
/// signature. Markdown headings never pass through this — they are always one
/// line.
#[derive(Clone, Copy)]
pub(crate) enum BodyOpen {
	/// First line containing `{` (rust, go, java, cpp, js, ts, php, swift,
	/// bash, css, json).
	Brace,
	/// First line whose trimmed text ends with `:` (python).
	PythonColon,
	/// First line whose trimmed text ends with `do` (elixir).
	ElixirDo,
	/// The first line only (ruby, lua).
	FirstLine,
}

pub(crate) fn body_open_style(language: &str) -> BodyOpen {
	match language {
		"python" => BodyOpen::PythonColon,
		"elixir" => BodyOpen::ElixirDo,
		"ruby" | "lua" => BodyOpen::FirstLine,
		_ => BodyOpen::Brace,
	}
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
		"ts" | "tsx" => "typescript",
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
