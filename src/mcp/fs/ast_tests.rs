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

//! Unit tests for the AST-pattern search mode of `view`.

use super::*;
use crate::structural::ast_search::AstMatch;

fn matches(pairs: &[(usize, usize)]) -> Vec<AstMatch> {
	pairs
		.iter()
		.map(|&(line, line_count)| AstMatch { line, line_count })
		.collect()
}

#[test]
fn render_ast_body_renders_line_ids_breadcrumbs_and_context() {
	let src = "mod m {\n\tfn a() {}\n\tfn b() {}\n}\n";
	let body = render_ast_body("t.rs", src, &matches(&[(2, 1), (3, 1)]), 1, ViewFlags::default()).expect("body");
	assert!(body.starts_with("[mod m › fn a]"), "breadcrumb missing: {body}");
	assert!(body.contains("fn a() {}"), "match line missing: {body}");
	// Context pulls in the line before the first match and the closing brace
	// after the last one.
	assert!(
		body.lines().any(|l| l.starts_with("1:")),
		"context line missing: {body}"
	);
	assert!(body.contains("}"), "trailing context missing: {body}");
	// Every rendered source line carries an N:hh id usable by batch_edit.
	for line in body.lines().filter(|l| !l.starts_with('[')) {
		assert!(line.contains('|'), "no line id on {line:?} in:\n{body}");
	}
}

#[test]
fn render_ast_body_dedupes_identical_spans() {
	let src = "fn a() {}\n";
	// Nested nodes can match with the same span; it renders once.
	let body = render_ast_body("t.rs", src, &matches(&[(1, 1), (1, 1)]), 0, ViewFlags::default()).expect("body");
	assert_eq!(
		body.lines().filter(|l| l.contains('|') && l.contains("fn a")).count(),
		1,
		"got: {body}"
	);
}
fn render_ast_body_count_and_files_only() {
	let src = "fn a() {}\nfn b() {}\n";
	let count = render_ast_body(
		"t.rs",
		src,
		&matches(&[(1, 1), (2, 1)]),
		0,
		ViewFlags {
			output: SearchOutput::Count,
			..ViewFlags::default()
		},
	);
	assert_eq!(count.as_deref(), Some("2"));
	let files_only = render_ast_body(
		"t.rs",
		src,
		&matches(&[(1, 1)]),
		0,
		ViewFlags {
			output: SearchOutput::FilesOnly,
			..ViewFlags::default()
		},
	);
	assert_eq!(files_only.as_deref(), Some(""));
	assert!(render_ast_body("t.rs", src, &[], 0, ViewFlags::default()).is_none());
}

#[test]
fn render_match_lines_caps_long_spans_with_elision_marker() {
	// 40-line function body: the span renders its first 30 lines, the elision
	// marker, then its last line.
	let mut src = String::from("fn a() {\n");
	for i in 0..40 {
		src.push_str(&format!("\tlet v{i} = {i};\n"));
	}
	src.push_str("}\n");
	let (body, omitted) = render_match_lines("t.rs", &src, &matches(&[(1, 42)]), 0, usize::MAX);
	assert_eq!(omitted, 0);
	assert!(body.contains("… 12 lines omitted …"), "elision marker missing: {body}");
	assert!(body.contains("let v0 = 0;"), "first span line missing: {body}");
	// The cap keeps the span's last line so the match's extent stays visible.
	assert!(
		body.lines().next_back().is_some_and(|l| l.ends_with('}')),
		"last span line missing: {body}"
	);
	assert!(!body.contains("let v30 = 30;"), "capped line rendered: {body}");
}

#[test]
fn render_match_lines_drops_matches_over_the_byte_budget() {
	let src = "fn a() {}\n".repeat(200);
	let pairs: Vec<(usize, usize)> = (1..=200).map(|n| (n, 1)).collect();
	let (body, omitted) = render_match_lines("t.rs", &src, &matches(&pairs), 0, 2048);
	assert!(omitted > 0, "no matches omitted: {body}");
	assert!(body.len() <= 2048, "body over budget: {}", body.len());
}

#[test]
fn join_ast_blocks_reports_parse_errors_once_per_language() {
	let mut errors = BTreeMap::new();
	errors.insert("python".to_string(), "Multiple AST nodes".to_string());
	let out = join_ast_blocks(
		"src",
		vec!["a.rs:\n1:ab|fn a() {}".to_string()],
		"fn $F($$$A) {}",
		Some("rust"),
		Some(&errors),
		ViewFlags::default(),
	);
	assert_eq!(out.matches("skipped python files").count(), 1, "got: {out}");
	assert!(out.contains("a.rs:"), "got: {out}");
}

#[test]
fn join_ast_blocks_zero_matches_yields_the_miss_diagnostic() {
	let out = join_ast_blocks(
		"src",
		vec![],
		"fn $F($$$A) {}",
		Some("rust"),
		None,
		ViewFlags::default(),
	);
	assert!(out.contains("No AST matches"), "got: {out}");
	assert!(out.contains("kind=function_item"), "got: {out}");
	let out = join_ast_blocks("src", vec![], "fn $F($$$A) {}", None, None, ViewFlags::default());
	assert!(out.contains("No AST-searchable files found under src."), "got: {out}");
}

#[test]
fn unsupported_language_message_guides_css_json_and_extensionless() {
	assert!(unsupported_language_message("a.css").contains("not supported for css files"));
	assert!(unsupported_language_message("a.json").contains("not supported for json files"));
	assert!(unsupported_language_message("a.txt").contains("No grammar for extension 'txt'"));
	assert!(unsupported_language_message("noext").contains("no file extension"));
}
