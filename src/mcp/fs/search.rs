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

use crate::utils::line_hash::line_id_at;
use anyhow::{anyhow, bail, Result};
use regex::Regex;

/// What a content search reports for each file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchOutput {
	/// Matching lines with their line ids.
	#[default]
	Lines,
	/// Number of matching lines (`c`).
	Count,
	/// The file path alone (`l`).
	FilesOnly,
	/// The matched text of each match, without the rest of its line (`o`).
	OnlyMatching,
}

impl SearchOutput {
	/// Count and files-only results are one row per file; the others are a block of lines.
	pub fn is_compact(self) -> bool {
		matches!(self, SearchOutput::Count | SearchOutput::FilesOnly)
	}

	/// Put one file's result under its path: a header above a block, or a single row.
	pub fn label(self, path: &str, body: &str) -> String {
		match self {
			SearchOutput::Count => format!("{path}\t{body}"),
			SearchOutput::FilesOnly => path.to_string(),
			SearchOutput::Lines | SearchOutput::OnlyMatching => format!("{path}:\n{body}"),
		}
	}

	/// Text between the results of two files.
	pub fn separator(self) -> &'static str {
		if self.is_compact() {
			"\n"
		} else {
			"\n\n"
		}
	}
}

/// The `flags` letters of a `view` call. They follow `grep` and `ls`, which callers
/// already know, so one short parameter covers what would be four booleans.
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewFlags {
	/// Listing only: add size and modification time to each row (`l` without `content`).
	pub long: bool,
	pub output: SearchOutput,
	/// Report the lines that do not match (`v`).
	pub invert: bool,
}

impl ViewFlags {
	/// Parse the letters. `l` means files-only for a search and a long row for a listing,
	/// as it does for `grep` and `ls`. A leading `-` is accepted because callers type it.
	pub fn parse(flags: Option<&str>, has_content: bool) -> Result<Self> {
		let mut parsed = ViewFlags::default();
		for flag in flags.unwrap_or_default().chars() {
			let output = match flag {
				'-' => continue,
				'v' => {
					parsed.invert = true;
					continue;
				}
				'l' if !has_content => {
					parsed.long = true;
					continue;
				}
				'l' => SearchOutput::FilesOnly,
				'c' => SearchOutput::Count,
				'o' => SearchOutput::OnlyMatching,
				other => bail!("Unknown view flag {other:?}: the flags are l, c, o and v."),
			};
			if parsed.output != SearchOutput::Lines && parsed.output != output {
				bail!("View flags l, c and o are alternatives: pass one of them.");
			}
			parsed.output = output;
		}
		if !has_content && (parsed.invert || parsed.output != SearchOutput::Lines) {
			bail!("View flags c, o and v apply to a `content` search; without `content` only l (size and modification time) applies.");
		}
		if parsed.invert && parsed.output == SearchOutput::OnlyMatching {
			bail!("View flags o and v cannot be combined: a non-matching line has no matched text.");
		}
		Ok(parsed)
	}
}

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

	/// Every non-empty match in `line`, in order.
	fn matches_in<'a>(&self, line: &'a str) -> Vec<&'a str> {
		match self {
			Matcher::Literal(s) => line.matches(s.as_str()).collect(),
			Matcher::Regex(re) => re
				.find_iter(line)
				.map(|found| found.as_str())
				.filter(|text| !text.is_empty())
				.collect(),
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
/// `invert` selects the lines that do not match instead.
pub fn search_lines(content: &str, matcher: &Matcher, context_lines: usize, invert: bool) -> Vec<MatchBlock> {
	// The whole-buffer prefilter proves no line matches, which under `invert` means every line is selected.
	if matcher.is_empty_pattern() || (!invert && !matcher.matches_anywhere(content)) {
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
		.filter(|(_, line)| matcher.is_match(line) != invert)
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

/// One file's search result in the shape `flags` asks for, without its path, or `None`
/// when the file has nothing to report. Files-only output has an empty body: the path
/// is the whole result.
pub fn render_file_matches(content: &str, matcher: &Matcher, context_lines: usize, flags: ViewFlags) -> Option<String> {
	if matcher.is_empty_pattern() {
		return None;
	}
	match flags.output {
		SearchOutput::Lines => {
			let blocks = search_lines(content, matcher, context_lines, flags.invert);
			if blocks.is_empty() {
				return None;
			}
			// Blocks follow each other directly: every line carries its number, so a jump
			// in numbering already marks a gap, and a separator line would cost 2 tokens per gap.
			let lines: Vec<&str> = content.lines().collect();
			let rendered: Vec<String> = blocks
				.iter()
				.flat_map(|block| &block.line_numbers)
				.map(|&n| format!("{}|{}", line_id_at(&lines, n), lines[n - 1]))
				.collect();
			Some(rendered.join("\n"))
		}
		SearchOutput::Count | SearchOutput::FilesOnly => {
			if !flags.invert && !matcher.matches_anywhere(content) {
				return None;
			}
			let count = content
				.lines()
				.filter(|line| matcher.is_match(line) != flags.invert)
				.count();
			match (count, flags.output) {
				(0, _) => None,
				(_, SearchOutput::Count) => Some(count.to_string()),
				_ => Some(String::new()),
			}
		}
		SearchOutput::OnlyMatching => {
			if !matcher.matches_anywhere(content) {
				return None;
			}
			// The matched text is not the whole line, so it carries the line number alone:
			// a line id here would vouch for content the caller never saw.
			let rendered: Vec<String> = content
				.lines()
				.enumerate()
				.flat_map(|(i, line)| {
					matcher
						.matches_in(line)
						.into_iter()
						.map(move |text| format!("{}|{}", i + 1, text))
				})
				.collect();
			(!rendered.is_empty()).then(|| rendered.join("\n"))
		}
	}
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
