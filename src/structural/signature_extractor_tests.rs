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

//! Tests for signature extraction over a parsed tree: kind mapping, markdown
//! heading extraction with fence tracking, and the signature line span.

use super::{extract_markdown_signatures, map_node_kind_to_simple, signature_line_span};
use crate::structural::BodyOpen;

#[test]
fn node_kinds_map_to_simple_display_kinds() {
	assert_eq!(map_node_kind_to_simple("function_item"), "function");
	assert_eq!(map_node_kind_to_simple("method_definition"), "method");
	assert_eq!(map_node_kind_to_simple("class_declaration"), "class");
	assert_eq!(map_node_kind_to_simple("struct_item"), "struct");
	assert_eq!(map_node_kind_to_simple("enum_item"), "enum");
	assert_eq!(map_node_kind_to_simple("interface_declaration"), "interface");
	assert_eq!(map_node_kind_to_simple("trait_item"), "trait");
	assert_eq!(map_node_kind_to_simple("mod_item"), "module");
	assert_eq!(map_node_kind_to_simple("const_item"), "constant");
	assert_eq!(map_node_kind_to_simple("macro_definition"), "macro");
	assert_eq!(map_node_kind_to_simple("type_alias_declaration"), "type");
	assert_eq!(map_node_kind_to_simple("namespace_definition"), "namespace");
	// Unknown kinds fall back to themselves.
	assert_eq!(map_node_kind_to_simple("rule_set"), "rule_set");
}

#[test]
fn signature_span_stops_at_the_body_opening_line() {
	let lines: Vec<&str> = vec!["fn foo(", "x: T,", ") where T: Clone,", "{", "\ttodo!()"];
	// Brace opens on line 4 (row 3): the signature is lines 1-4.
	assert_eq!(signature_line_span(&lines, 0, 4, BodyOpen::Brace), 4);
	// A one-line item with the brace on its own line is just that line.
	assert_eq!(signature_line_span(&lines, 3, 4, BodyOpen::Brace), 1);
	// No brace within the node: the whole (capped) node is the signature.
	assert_eq!(signature_line_span(&lines, 0, 1, BodyOpen::Brace), 2);
	// The span never exceeds the cap even when the body opens later.
	assert_eq!(signature_line_span(&lines, 0, 4, BodyOpen::Brace), 4);

	let python: Vec<&str> = vec!["def foo(", "x: int,", ") -> str:", "\tpass"];
	assert_eq!(signature_line_span(&python, 0, 3, BodyOpen::PythonColon), 3);

	let elixir: Vec<&str> = vec!["def foo(", "x,", ") do", "\t:ok", "end"];
	assert_eq!(signature_line_span(&elixir, 0, 4, BodyOpen::ElixirDo), 3);

	// Ruby and lua items are their first line.
	assert_eq!(signature_line_span(&elixir, 0, 4, BodyOpen::FirstLine), 1);
}

#[test]
fn markdown_headings_inside_fenced_code_blocks_are_ignored() {
	let markdown = "# Real Heading\n\nSome text.\n\n```bash\n#!/bin/bash\necho hi\n```\n\n## Another Heading\n";
	let items = extract_markdown_signatures(markdown);
	let names: Vec<&str> = items.iter().map(|s| s.name.as_str()).collect();
	assert_eq!(names, vec!["Real Heading", "Another Heading"]);
}

#[test]
fn markdown_tilde_fences_and_longer_backtick_runs_are_tracked() {
	// A ``` fence inside a ~~~ block does not close it, and vice versa.
	let markdown = "~~~\n```\n# not a heading\n```\n~~~\n\n# Real\n";
	let items = extract_markdown_signatures(markdown);
	let names: Vec<&str> = items.iter().map(|s| s.name.as_str()).collect();
	assert_eq!(names, vec!["Real"]);
}

#[test]
fn markdown_heading_levels_are_reported() {
	let markdown = "# One\n## Two\n### Three\n";
	let items = extract_markdown_signatures(markdown);
	let kinds: Vec<&str> = items.iter().map(|s| s.kind.as_str()).collect();
	assert_eq!(kinds, vec!["heading1", "heading2", "heading3"]);
}

#[test]
fn markdown_empty_and_hash_only_headings_are_skipped() {
	assert!(extract_markdown_signatures("").is_empty());
	let hash_only = "#\n## \n";
	assert!(extract_markdown_signatures(hash_only).is_empty());
}
