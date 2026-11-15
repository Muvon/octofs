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

//! Svelte grammar: not an outline language itself — the svelte grammar only
//! wraps the `<script>` blocks whose code is the outline. This module finds
//! those blocks and hands their raw text (plus the row it starts at, so item
//! lines can be offset back to file coordinates) to the JavaScript or
//! TypeScript grammar, chosen by the script's `lang` attribute. The same
//! blocks join the code graph through `extract_embedded_sources`.

use super::javascript::JavaScript;
use super::typescript::TypeScript;
use super::{EmbeddedSource, Language};
use crate::structural::graph::registry::FileRegistry;
use tree_sitter::{Node, Parser};

/// The Svelte file as a graph language. It declares no symbols itself: its
/// `<script>` blocks are handed to the JavaScript or TypeScript grammar by
/// `extract_embedded_sources`.
pub(crate) struct Svelte {}

impl Language for Svelte {
	fn name(&self) -> &'static str {
		"svelte"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_svelte_ng::LANGUAGE.into()
	}

	// Scripts are JavaScript or TypeScript; imports share one literal form.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		JavaScript {}.line_may_import(line, name)
	}

	// A script may be TypeScript, the stricter of the two.
	fn line_may_inherit(&self, line: &str, name: &str) -> bool {
		TypeScript {}.line_may_inherit(line, name)
	}

	// Its scripts are what the graph reads.
	fn has_graph_symbols(&self) -> bool {
		true
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		TypeScript {}.comment_starts()
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		TypeScript {}.declaration_keywords()
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		TypeScript {}.callable_keywords()
	}

	fn identifier_prefix(&self) -> Option<u8> {
		TypeScript {}.identifier_prefix()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The outline of a Svelte file is its scripts' outline, built from
		// `script_sources`; the markup nodes declare nothing.
		Vec::new()
	}

	fn extract_embedded_sources(&self, root: Node, contents: &str) -> Vec<EmbeddedSource> {
		script_blocks(root, contents)
			.into_iter()
			.map(|(language, source, start_row, _)| EmbeddedSource {
				language,
				contents: source,
				start_line: start_row as u32,
			})
			.collect()
	}

	// Svelte imports are JavaScript imports.
	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		JavaScript {}.resolve_import(import_path, source_file, all_files)
	}
}

/// One `<script>` block: (language, script text, 0-based row and column the
/// text starts at).
pub(crate) fn script_sources(contents: &str) -> Vec<(&'static str, String, usize, usize)> {
	let mut parser = Parser::new();
	if parser.set_language(&tree_sitter_svelte_ng::LANGUAGE.into()).is_err() {
		return Vec::new();
	}
	let Some(tree) = parser.parse(contents, None) else {
		return Vec::new();
	};
	script_blocks(tree.root_node(), contents)
}

/// The `<script>` blocks below `root`, language chosen by the `lang` attribute.
fn script_blocks(root: Node, contents: &str) -> Vec<(&'static str, String, usize, usize)> {
	let mut sources = Vec::new();
	let mut stack = vec![root];
	while let Some(node) = stack.pop() {
		if node.kind() == "script_element" {
			let script_text = node.utf8_text(contents.as_bytes()).unwrap_or_default();
			let language = if script_text.contains("lang=\"ts\"")
				|| script_text.contains("lang='ts'")
				|| script_text.contains("lang=\"typescript\"")
			{
				"typescript"
			} else {
				"javascript"
			};
			for child in node.children(&mut node.walk()) {
				if child.kind() == "raw_text" {
					if let Ok(source) = child.utf8_text(contents.as_bytes()) {
						let start = child.start_position();
						sources.push((language, source.to_string(), start.row, start.column));
					}
				}
			}
			continue;
		}
		let mut children: Vec<_> = node.children(&mut node.walk()).collect();
		children.reverse();
		stack.extend(children);
	}
	sources
}

#[cfg(test)]
#[path = "svelte_tests.rs"]
mod svelte_tests;
