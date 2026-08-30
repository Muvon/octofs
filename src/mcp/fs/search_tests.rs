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
	search_lines(content, &matcher, context_lines)
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
	let blocks = search_lines("a\nTODO\nc\nFIXME\ne\n", &m, 0);
	assert_eq!(blocks.len(), 2);
	assert_eq!(blocks[0].line_numbers, vec![2]);
	assert_eq!(blocks[1].line_numbers, vec![4]);
}

#[test]
fn test_regex_case_insensitive() {
	let m = Matcher::new("(?i)error", true).unwrap();
	let blocks = search_lines("ok\nERROR here\nError again\nfine\n", &m, 0);
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
	let blocks = search_lines("line1\nbackward_step()\nline3\n", &m, 0);
	assert_eq!(blocks.len(), 1);
	assert_eq!(blocks[0].line_numbers, vec![2]);
}
