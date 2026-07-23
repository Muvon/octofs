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

// Shared truncation utilities for smart content display across MCP tools

/// Rough token estimate: ~4 chars per token (good enough for truncation decisions).
pub fn estimate_tokens(content: &str) -> usize {
	content.len().div_ceil(4)
}

/// Format content with line identifiers (numbers or hashes) and smart elision for display.
pub fn format_content_with_line_numbers(
	lines: &[&str],
	start_line_number: usize,
	view_range: Option<(usize, i64)>,
) -> String {
	// Compute prefixes based on active line mode
	let prefixes: Vec<String> = if super::line_hash::is_hash_mode() {
		super::line_hash::compute_line_hashes(lines)
	} else {
		lines
			.iter()
			.enumerate()
			.map(|(i, _)| format!("{}", start_line_number + i))
			.collect()
	};

	if let Some((start, end)) = view_range {
		let start_idx = if start == 0 {
			0
		} else {
			start.saturating_sub(1)
		};
		let end_idx = if end == -1 {
			lines.len()
		} else {
			(end as usize).min(lines.len())
		};

		if start_idx >= lines.len() || start_idx > end_idx {
			return if start_idx >= lines.len() {
				format!(
					"Start line {} exceeds content length ({} lines)",
					start,
					lines.len()
				)
			} else {
				format!(
					"Start line {} must be less than or equal to end line {}",
					start, end
				)
			};
		}

		let mut result_lines = Vec::new();

		if start_idx > 3 {
			for (i, line) in lines.iter().enumerate().take(2) {
				result_lines.push(format!("{}:{}", prefixes[i], line));
			}
			if start_idx > 5 {
				result_lines.push(format!("[...{} lines more]", start_idx - 2));
			} else {
				for (i, line) in lines.iter().enumerate().take(start_idx).skip(2) {
					result_lines.push(format!("{}:{}", prefixes[i], line));
				}
			}
		} else {
			for (i, line) in lines.iter().enumerate().take(start_idx) {
				result_lines.push(format!("{}:{}", prefixes[i], line));
			}
		}

		for (i, line) in lines.iter().enumerate().take(end_idx).skip(start_idx) {
			result_lines.push(format!("{}:{}", prefixes[i], line));
		}

		let remaining_lines = lines.len() - end_idx;
		if remaining_lines > 3 {
			if remaining_lines > 5 {
				result_lines.push(format!("[...{} lines more]", remaining_lines - 2));
				for (i, line) in lines.iter().enumerate().skip(lines.len() - 2) {
					result_lines.push(format!("{}:{}", prefixes[i], line));
				}
			} else {
				for (i, line) in lines.iter().enumerate().skip(end_idx) {
					result_lines.push(format!("{}:{}", prefixes[i], line));
				}
			}
		} else {
			for (i, line) in lines.iter().enumerate().skip(end_idx) {
				result_lines.push(format!("{}:{}", prefixes[i], line));
			}
		}

		result_lines.join("\n")
	} else {
		lines
			.iter()
			.enumerate()
			.map(|(i, line)| format!("{}:{}", prefixes[i], line))
			.collect::<Vec<_>>()
			.join("\n")
	}
}

/// Format extracted content with proper line identifiers and smart truncation.
/// When `hashes` is provided and hash mode is active, uses those as prefixes.
/// Otherwise falls back to sequential line numbers or auto-computed hashes.
pub fn format_extracted_content_smart(
	lines: &[&str],
	start_line: usize,
	max_display_lines: Option<usize>,
	hashes: Option<&[String]>,
) -> String {
	let prefixes: Vec<String> = if let Some(h) = hashes {
		h.to_vec()
	} else if super::line_hash::is_hash_mode() {
		super::line_hash::compute_line_hashes(lines)
	} else {
		lines
			.iter()
			.enumerate()
			.map(|(i, _)| format!("{}", start_line + i))
			.collect()
	};

	let max_lines = max_display_lines.unwrap_or(50);

	if lines.len() <= max_lines {
		lines
			.iter()
			.enumerate()
			.map(|(i, line)| format!("{}:{}", prefixes[i], line))
			.collect::<Vec<_>>()
			.join("\n")
	} else {
		let show_first = (max_lines * 2) / 3;
		let show_last = max_lines - show_first - 1;

		let mut result_lines = Vec::new();

		for (i, line) in lines.iter().enumerate().take(show_first) {
			result_lines.push(format!("{}:{}", prefixes[i], line));
		}

		let hidden_lines = lines.len() - show_first - show_last;
		result_lines.push(format!("[...{} lines more]", hidden_lines));

		let skip_count = lines.len() - show_last;
		for (i, line) in lines.iter().enumerate().skip(skip_count) {
			result_lines.push(format!("{}:{}", prefixes[i], line));
		}

		result_lines.join("\n")
	}
}
