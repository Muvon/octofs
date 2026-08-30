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

	/// Whole-buffer prefilter: if the pattern cannot match anywhere in the file,
	/// no line can match either. Lets callers skip splitting non-matching files
	/// into lines, which dominates a tree-wide search. Regex is exempt: `^`/`$`
	/// anchor to the buffer, not to lines, so a whole-buffer test would reject
	/// files whose individual lines do match.
	#[inline]
	fn matches_anywhere(&self, content: &str) -> bool {
		match self {
			Matcher::Literal(s) => content.contains(s.as_str()),
			Matcher::Regex(_) => true,
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
	if matcher.is_empty_pattern() || !matcher.matches_anywhere(content) {
		return Vec::new();
	}

	let lines: Vec<&str> = content.lines().collect();
	let total = lines.len();
	if total == 0 {
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

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
