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
//! TypeScript grammar, chosen by the script's `lang` attribute.

use tree_sitter::Parser;

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

	let mut sources = Vec::new();
	let mut stack = vec![tree.root_node()];
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
