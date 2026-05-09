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

// Pure-Rust line-based content search with optional regex and context support.

use anyhow::{anyhow, Result};
use regex::Regex;

/// A contiguous block of matched/context lines.
pub struct MatchBlock {
	/// 1-indexed line numbers in this block.
	pub line_numbers: Vec<usize>,
}

/// Pattern matcher: literal substring or compiled regex.
/// Compile once outside hot loops; cheap to share via reference.
pub enum Matcher {
	Literal(String),
	Regex(Regex),
}

impl Matcher {
	/// Build a matcher. `regex=false` → literal substring match.
	/// `regex=true` → compile as regex (case-insensitivity via `(?i)` prefix in pattern).
	pub fn new(pattern: &str, regex: bool) -> Result<Self> {
		if regex {
			Regex::new(pattern)
				.map(Matcher::Regex)
				.map_err(|e| anyhow!("Invalid regex pattern: {}", e))
		} else {
			Ok(Matcher::Literal(pattern.to_string()))
		}
	}

	#[inline]
	fn is_match(&self, line: &str) -> bool {
		match self {
			Matcher::Literal(s) => line.contains(s.as_str()),
			Matcher::Regex(re) => re.is_match(line),
		}
	}

	pub fn is_empty_pattern(&self) -> bool {
		match self {
			Matcher::Literal(s) => s.is_empty(),
			Matcher::Regex(_) => false,
		}
	}
}

/// Search `content` line-by-line using `matcher`, returning contiguous blocks of
/// matching line numbers (expanded by `context_lines` before/after each match).
pub fn search_lines(content: &str, matcher: &Matcher, context_lines: usize) -> Vec<MatchBlock> {
	let lines: Vec<&str> = content.lines().collect();
	let total = lines.len();
	if total == 0 || matcher.is_empty_pattern() {
		return Vec::new();
	}

	let match_indices: Vec<usize> = lines
		.iter()
		.enumerate()
		.filter(|(_, line)| matcher.is_match(line))
		.map(|(i, _)| i)
		.collect();

	if match_indices.is_empty() {
		return Vec::new();
	}

	let mut ranges: Vec<(usize, usize)> = Vec::new();
	for &idx in &match_indices {
		let start = idx.saturating_sub(context_lines);
		let end = (idx + context_lines).min(total - 1);
		if let Some(last) = ranges.last_mut() {
			if start <= last.1 + 1 {
				last.1 = last.1.max(end);
				continue;
			}
		}
		ranges.push((start, end));
	}

	ranges
		.into_iter()
		.map(|(start, end)| MatchBlock {
			line_numbers: (start..=end).map(|i| i + 1).collect(),
		})
		.collect()
}

/// Backward-compatible literal-only convenience wrapper.
#[cfg(test)]
pub fn search_content(content: &str, pattern: &str, context_lines: usize) -> Vec<MatchBlock> {
	let m = Matcher::new(pattern, false).expect("literal matcher cannot fail");
	search_lines(content, &m, context_lines)
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
