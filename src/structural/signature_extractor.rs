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

//! Signature extraction over a parsed tree-sitter tree: walks the tree, keeps
//! the nodes the language calls outline items, and records each item's
//! signature line span (up to where its body opens, capped).

use super::languages::Language;
use super::{BodyOpen, OutlineItem, MAX_SIGNATURE_LINES};
use tree_sitter::Node;

/// Extract the outline items of a parsed file. Items are sorted by line.
pub(super) fn extract_signatures(node: Node, contents: &str, lang: &dyn Language) -> Vec<OutlineItem> {
	let mut items = Vec::new();
	let kinds = lang.get_outline_kinds();
	let style = super::body_open_style(lang.name());
	// Sliced once: every item's signature span is resolved against it.
	let lines: Vec<&str> = contents.lines().collect();

	// Iterative pre-order walk: a pathologically deep parse (10k nested
	// blocks) is valid input for tree-sitter, and a recursive visitor would
	// overflow the stack one level per nesting.
	let mut cursor = node.walk();
	'walk: loop {
		let node = cursor.node();
		let node_kind = node.kind();
		if kinds.contains(&node_kind) && lang.is_signature_node(node, contents) {
			let start_row = node.start_position().row;
			let end_row = node.end_position().row;
			if let Some(name) = extract_name(node, contents, lang) {
				let kind = lang
					.extract_declaration_kind(node, contents)
					.map(str::to_string)
					.unwrap_or_else(|| map_node_kind_to_simple(node_kind));
				let sig_lines = signature_line_span(&lines, start_row, end_row, style);
				items.push(OutlineItem {
					kind,
					name,
					start_line: start_row + 1,
					sig_lines,
				});
			}
		}
		// Children before the next sibling: a container item (class, module)
		// and its nested items both appear, nesting conveyed by indentation.
		if cursor.goto_first_child() {
			continue 'walk;
		}
		loop {
			if cursor.goto_next_sibling() {
				continue 'walk;
			}
			if !cursor.goto_parent() {
				break 'walk;
			}
		}
	}

	items.sort_by_key(|item| item.start_line);
	items
}

/// Extract the name of a declaration node (function, class, etc.).
fn extract_name(node: Node, contents: &str, lang: &dyn Language) -> Option<String> {
	if let Some(name) = lang.extract_signature_name(node, contents) {
		return Some(name);
	}

	// Look for identifier nodes among the direct children.
	for child in node.children(&mut node.walk()) {
		// Anonymous tokens (keywords) are never declaration names.
		if !child.is_named() {
			continue;
		}
		if child.kind() == "identifier" || child.kind().contains("name") || child.kind().contains("function_name") {
			if let Ok(name) = child.utf8_text(contents.as_bytes()) {
				if !name.is_empty() {
					return Some(name.to_string());
				}
			}
		}
	}
	None
}

/// How many source lines the item's signature spans: from its start line up to
/// (and including) the line where the body opens, never past the node's last
/// line and never more than [`MAX_SIGNATURE_LINES`]. When no body opens within
/// that window the item has no body — the whole (capped) span is the
/// signature.
fn signature_line_span(lines: &[&str], start_row: usize, end_row: usize, style: BodyOpen) -> usize {
	let last_row = end_row
		.min(start_row + MAX_SIGNATURE_LINES - 1)
		.min(lines.len().saturating_sub(1));
	if last_row < start_row {
		return 1;
	}
	match style {
		BodyOpen::FirstLine => 1,
		BodyOpen::Brace => body_open_offset(lines, start_row, last_row, |line| line.contains('{')),
		BodyOpen::PythonColon => body_open_offset(lines, start_row, last_row, |line| line.trim_end().ends_with(':')),
		BodyOpen::ElixirDo => body_open_offset(lines, start_row, last_row, |line| line.trim_end().ends_with("do")),
	}
}

/// Lines from the item's start to its last (capped) line, as a count from the
/// start: the offset of the first line matching `opens` plus one, or the whole
/// window when no line opens the body.
fn body_open_offset(lines: &[&str], start_row: usize, last_row: usize, opens: impl Fn(&str) -> bool) -> usize {
	lines
		.iter()
		.take(last_row + 1)
		.skip(start_row)
		.position(|line| opens(line))
		.map_or(last_row - start_row + 1, |offset| offset + 1)
}

/// Map tree-sitter node kinds to simpler, unified kinds for display.
fn map_node_kind_to_simple(kind: &str) -> String {
	match kind {
		k if k.contains("function") => "function".to_string(),
		k if k.contains("method") => "method".to_string(),
		k if k.contains("class") => "class".to_string(),
		// Before the struct arm: "conSTRUCTor_declaration" contains "struct".
		k if k.contains("constructor") || k.contains("init_declaration") => "constructor".to_string(),
		k if k.contains("struct") => "struct".to_string(),
		k if k.contains("enum") => "enum".to_string(),
		k if k.contains("interface") => "interface".to_string(),
		k if k.contains("trait") => "trait".to_string(),
		k if k.contains("impl") => "impl".to_string(),
		// Before the mod/module arm: "internal_module" contains "module".
		k if k.contains("internal_module") => "namespace".to_string(),
		k if k.contains("mod") || k.contains("module") => "module".to_string(),
		k if k.contains("namespace") => "namespace".to_string(),
		k if k.contains("const") => "constant".to_string(),
		k if k.contains("macro") => "macro".to_string(),
		k if k.contains("type") => "type".to_string(),
		_ => kind.to_string(),
	}
}

/// Extract the outline of markdown content: its headings, one line each. A
/// `#`-prefixed line inside a fenced code block (a shell shebang, a Python
/// comment, a C #include, ...) is not a heading.
pub(super) fn extract_markdown_signatures(contents: &str) -> Vec<OutlineItem> {
	let mut items = Vec::new();
	let mut fence_marker: Option<String> = None;

	for (line_idx, line) in contents.lines().enumerate() {
		let trimmed = line.trim();

		if let Some(fence) = detect_code_fence(trimmed) {
			match &fence_marker {
				None => fence_marker = Some(fence),
				Some(marker) if trimmed.starts_with(marker.as_str()) => fence_marker = None,
				_ => {}
			}
			continue;
		}

		if fence_marker.is_some() {
			continue;
		}

		if trimmed.starts_with('#') {
			let heading_level = trimmed.chars().take_while(|&c| c == '#').count();
			let heading_text = trimmed.trim_start_matches('#').trim();

			if !heading_text.is_empty() {
				items.push(OutlineItem {
					kind: format!("heading{heading_level}"),
					name: heading_text.to_string(),
					start_line: line_idx + 1,
					sig_lines: 1,
				});
			}
		}
	}

	items
}

/// Detect a code fence opening or closing: ``` or ~~~ (or longer runs),
/// ignoring leading indentation.
fn detect_code_fence(line: &str) -> Option<String> {
	let trimmed = line.trim_start();
	if trimmed.starts_with("```") {
		let fence_end = trimmed.chars().take_while(|&c| c == '`').count();
		Some("`".repeat(fence_end))
	} else if trimmed.starts_with("~~~") {
		let fence_end = trimmed.chars().take_while(|&c| c == '~').count();
		Some("~".repeat(fence_end))
	} else {
		None
	}
}

#[cfg(test)]
#[path = "signature_extractor_tests.rs"]
mod signature_extractor_tests;
