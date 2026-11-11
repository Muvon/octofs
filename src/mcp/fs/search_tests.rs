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

use super::*;

fn search_content(content: &str, pattern: &str, context_lines: usize) -> Vec<MatchBlock> {
	let matcher = Matcher::new(pattern, false).expect("literal matcher cannot fail");
	search_lines(content, &matcher, context_lines, false)
}

#[test]
fn test_no_matches() {
	let blocks = search_content("hello\nworld\n", "xyz", 0);
	assert!(blocks.is_empty());
}

#[test]
fn test_single_match_no_context() {
	let blocks = search_content("aaa\nbbb\nccc\n", "bbb", 0);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![2]);
}

#[test]
fn test_single_match_with_context() {
	let blocks = search_content("aaa\nbbb\nccc\nddd\neee\n", "ccc", 1);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![2, 3, 4]);
}

#[test]
fn test_multiple_matches_merge() {
	let blocks = search_content("a\nb\nc\nd\ne\n", "b", 1);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![1, 2, 3]);
}

#[test]
fn test_multiple_matches_separate_blocks() {
	let blocks = search_content("a\nmatch\nc\nd\ne\nf\nmatch\nh\n", "match", 0);
	assert_eq!(blocks.len(), 2);
	assert_eq!(blocks[0].line_numbers, vec![2]);
	assert_eq!(blocks[1].line_numbers, vec![7]);
}

#[test]
fn test_context_merges_adjacent_matches() {
	let blocks = search_content("a\nmatch\nc\nmatch\ne\n", "match", 1);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![1, 2, 3, 4, 5]);
}

#[test]
fn test_context_clamps_to_bounds() {
	let blocks = search_content("match\nb\nc\n", "match", 3);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![1, 2, 3]);
}

#[test]
fn test_empty_pattern() {
	let blocks = search_content("hello\n", "", 0);
	assert!(blocks.is_empty());
}

#[test]
fn test_empty_content() {
	let blocks = search_content("", "hello", 0);
	assert!(blocks.is_empty());
}

#[test]
fn test_regex_alternation() {
	let m = Matcher::new("TODO|FIXME", true).unwrap();
	let blocks = search_lines("a\nTODO\nc\nFIXME\ne\n", &m, 0, false);
	assert_eq!(blocks.len(), 2);
	assert_eq!(blocks[0].line_numbers, vec![2]);
	assert_eq!(blocks[1].line_numbers, vec![4]);
}

#[test]
fn test_regex_case_insensitive() {
	let m = Matcher::new("(?i)error", true).unwrap();
	let blocks = search_lines("ok\nERROR here\nError again\nfine\n", &m, 0, false);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![2, 3]);
}

#[test]
fn test_regex_invalid_returns_error() {
	let err = Matcher::new("[unclosed", true).err().unwrap();
	assert!(err.to_string().to_lowercase().contains("regex"));
}

#[test]
fn test_literal_treats_regex_chars_literally() {
	// Regression guard: literal mode must NOT interpret regex metacharacters.
	let m = Matcher::new("backward_step()", false).unwrap();
	let blocks = search_lines("line1\nbackward_step()\nline3\n", &m, 0, false);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![2]);
}

fn render(content: &str, pattern: &str, regex: bool, flags: &str) -> Option<String> {
	let matcher = Matcher::new(pattern, regex).unwrap();
	render_file_matches(content, &matcher, 0, ViewFlags::parse(Some(flags), true).unwrap())
}

#[test]
fn test_flags_default_is_matching_lines() {
	let flags = ViewFlags::parse(None, true).unwrap();
	assert_eq!(flags.output, SearchOutput::Lines);
	assert!(!flags.invert && !flags.long);
}

#[test]
fn test_flag_l_is_files_only_for_search_and_long_for_listing() {
	let search = ViewFlags::parse(Some("l"), true).unwrap();
	assert_eq!(search.output, SearchOutput::FilesOnly);
	assert!(!search.long);

	let listing = ViewFlags::parse(Some("-l"), false).unwrap();
	assert_eq!(listing.output, SearchOutput::Lines);
	assert!(listing.long);
}

#[test]
fn test_flags_reject_unknown_conflicting_and_search_only_letters() {
	let unknown = ViewFlags::parse(Some("x"), true).unwrap_err().to_string();
	assert!(unknown.contains("Unknown view flag 'x'"), "got: {unknown}");

	let conflict = ViewFlags::parse(Some("cl"), true).unwrap_err().to_string();
	assert!(conflict.contains("alternatives"), "got: {conflict}");

	let no_text = ViewFlags::parse(Some("ov"), true).unwrap_err().to_string();
	assert!(no_text.contains("cannot be combined"), "got: {no_text}");

	let listing = ViewFlags::parse(Some("c"), false).unwrap_err().to_string();
	assert!(listing.contains("apply to a `content` search"), "got: {listing}");
}

#[test]
fn test_render_count_and_files_only() {
	let content = "a TODO\nb\nc TODO TODO\n";
	assert_eq!(render(content, "TODO", false, "c").as_deref(), Some("2"));
	assert_eq!(render(content, "TODO", false, "l").as_deref(), Some(""));
	assert_eq!(render(content, "absent", false, "c"), None);
	assert_eq!(render(content, "absent", false, "l"), None);
}

#[test]
fn test_render_inverted_lines_and_count() {
	let content = "keep\ndrop me\nkeep too\n";
	let lines = render(content, "drop", false, "v").unwrap();
	assert!(lines.contains("|keep") && lines.contains("|keep too"), "got: {lines}");
	assert!(!lines.contains("drop me"), "got: {lines}");
	assert_eq!(render(content, "drop", false, "vc").as_deref(), Some("2"));
	// A pattern found nowhere leaves every line selected.
	assert_eq!(render(content, "absent", false, "vc").as_deref(), Some("3"));
}

#[test]
fn test_render_only_matching_text() {
	let content = "id=12 id=7\nnone\nid=400\n";
	assert_eq!(
		render(content, r"id=\d+", true, "o").as_deref(),
		Some("1|id=12\n1|id=7\n3|id=400")
	);
	assert_eq!(
		render(content, "id=", false, "o").as_deref(),
		Some("1|id=\n1|id=\n3|id=")
	);
	assert_eq!(render(content, r"zz\d", true, "o"), None);
}

#[test]
fn test_output_label_and_separator() {
	assert_eq!(SearchOutput::Count.label("a.rs", "3"), "a.rs\t3");
	assert_eq!(SearchOutput::FilesOnly.label("a.rs", ""), "a.rs");
	assert_eq!(SearchOutput::Lines.label("a.rs", "1:aa|x"), "a.rs:\n1:aa|x");
	assert_eq!(SearchOutput::Count.separator(), "\n");
	assert_eq!(SearchOutput::OnlyMatching.separator(), "\n\n");
}
