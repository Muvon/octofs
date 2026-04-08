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

// Pure-Rust fixed-string content search with context line support.
// Replaces external ripgrep (`rg -F`) invocations.

/// A contiguous block of matched/context lines.
pub struct MatchBlock {
	/// 1-indexed line numbers in this block.
	pub line_numbers: Vec<usize>,
}

/// Search `content` for the literal `pattern`, returning contiguous blocks of
/// matching line numbers (expanded by `context_lines` before/after each match).
/// Blocks are separated where gaps exist (analogous to rg `--` separators).
pub fn search_content(content: &str, pattern: &str, context_lines: usize) -> Vec<MatchBlock> {
	let lines: Vec<&str> = content.lines().collect();
	let total = lines.len();
	if total == 0 || pattern.is_empty() {
		return Vec::new();
	}

	// Collect 0-indexed positions of matching lines
	let match_indices: Vec<usize> = lines
		.iter()
		.enumerate()
		.filter(|(_, line)| line.contains(pattern))
		.map(|(i, _)| i)
		.collect();

	if match_indices.is_empty() {
		return Vec::new();
	}

	// Expand each match by context_lines, clamp to [0, total-1]
	// Merge overlapping/adjacent ranges into contiguous blocks
	let mut ranges: Vec<(usize, usize)> = Vec::new();
	for &idx in &match_indices {
		let start = idx.saturating_sub(context_lines);
		let end = (idx + context_lines).min(total - 1);
		if let Some(last) = ranges.last_mut() {
			if start <= last.1 + 1 {
				// Merge with previous range
				last.1 = last.1.max(end);
				continue;
			}
		}
		ranges.push((start, end));
	}

	// Convert ranges to MatchBlocks with 1-indexed line numbers
	ranges
		.into_iter()
		.map(|(start, end)| MatchBlock {
			line_numbers: (start..=end).map(|i| i + 1).collect(),
		})
		.collect()
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
		// match at line 2, context [1,3] → block [1,2,3]
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
		// match@2 context [1,3], match@4 context [3,5] → merged [1,5]
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
}
