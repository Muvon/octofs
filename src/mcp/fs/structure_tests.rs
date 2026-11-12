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

//! Tests for outline rendering: `N:hh|content` lines identical to a normal
//! view, headers, empty results and truncation notices.

use super::{join_file_blocks, render_file_block, render_file_outline, TRUNCATION_NOTICE_RESERVE};
use crate::structural::{outline, OutlineItem};
use crate::utils::line_hash::line_id;

fn render(src: &str, path: &str, budget: usize) -> String {
	let items = outline(path, src).unwrap();
	render_file_outline(path, src, &items, budget)
}

#[test]
fn single_file_outline_renders_view_line_ids_verbatim() {
	let src = "mod foo {\n\tfn bar() {}\n}\nstruct Baz;\n";
	let out = render(src, "a.rs", usize::MAX);
	let lines: Vec<&str> = out.lines().collect();
	assert_eq!(lines[0], "a.rs\t4");
	// Every item line is exactly what a whole-file view renders for that
	// line: same number, same hash, same original indentation. The mod's body
	// opens on its own line, so its signature is one line; the closing brace
	// (line 3) is body and never appears.
	assert_eq!(lines[1], format!("{}|mod foo {{", line_id(1, "mod foo {")));
	assert_eq!(lines[2], format!("{}|\tfn bar() {{}}", line_id(2, "\tfn bar() {}")));
	assert_eq!(lines[3], format!("{}|struct Baz;", line_id(4, "struct Baz;")));
	assert_eq!(lines.len(), 4);
}

#[test]
fn multi_line_signatures_render_their_capped_lines() {
	let src = "fn long<T>(\n\tx: T,\n) where\n\tT: Clone,\n{\n\ttodo!()\n}\n";
	let out = render(src, "a.rs", usize::MAX);
	let lines: Vec<&str> = out.lines().collect();
	assert_eq!(lines.len(), 5); // header + 4 capped signature lines
	assert!(lines[4].starts_with(&format!("{}|", line_id(4, "\tT: Clone,"))));
}

#[test]
fn empty_outline_says_so() {
	let out = render("let x = 1;\n", "a.rs", usize::MAX);
	assert_eq!(out, "a.rs\t1\nNo declarations found.");
}

#[test]
fn truncated_outline_counts_omitted_items() {
	let src = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
	let out = render(src, "a.rs", 60);
	assert!(out.contains("[outline truncated: "), "got: {out}");
	assert!(out.contains("items omitted"), "got: {out}");
	// The header always survives.
	assert!(out.starts_with("a.rs\t4"), "got: {out}");
}

#[test]
fn directory_outline_joins_file_blocks_and_reports_omissions() {
	let blocks = vec!["f1.rs:\n1:aa|x".to_string(), "f2.rs:\n1:bb|y".to_string()];
	let joined = join_file_blocks("src", blocks.clone(), usize::MAX);
	assert_eq!(joined, "f1.rs:\n1:aa|x\n\nf2.rs:\n1:bb|y");

	let tight = join_file_blocks("src", blocks, 2 + "f1.rs:\n1:aa|x".len() + TRUNCATION_NOTICE_RESERVE);
	assert!(tight.contains("1 file"), "got: {tight}");
	assert!(
		tight.contains("narrow `path`, `pattern` or `max_depth`"),
		"got: {tight}"
	);
}

#[test]
fn directory_outline_with_no_qualifying_files_says_so() {
	assert_eq!(
		join_file_blocks("src", Vec::new(), usize::MAX),
		"No supported files with declarations found under src."
	);
}

#[test]
fn directory_block_uses_the_content_search_header_style() {
	let src = "fn a() {}\n";
	let items = outline("a.rs", src).unwrap();
	let block = render_file_block("a.rs", src, &items, usize::MAX);
	assert_eq!(block, format!("a.rs:\n{}|fn a() {{}}", line_id(1, "fn a() {}")));
}
#[test]
fn items_past_the_files_end_render_what_exists() {
	// A broken parse can report a line beyond the file; rendering must skip
	// it rather than panic.
	let items = vec![OutlineItem {
		kind: "function".to_string(),
		name: "x".to_string(),
		start_line: 9,
		sig_lines: 2,
	}];
	let out = render_file_outline("a.rs", "fn x() {}\n", &items, usize::MAX);
	assert!(out.starts_with("a.rs\t1"), "got: {out}");
}

#[test]
fn single_file_truncation_notice_names_symbol_and_ranged_view() {
	let mut src = String::new();
	for i in 0..40 {
		src.push_str(&format!("fn f{i}() {{\n\t{i}\n}}\n"));
	}
	let out = render(&src, "a.rs", 600);
	let last = out.lines().last().unwrap();
	assert!(last.starts_with("[outline truncated: "), "got: {last}");
	assert!(
		last.ends_with("items omitted — narrow with `symbol` or a ranged view]"),
		"got: {last}"
	);
}
