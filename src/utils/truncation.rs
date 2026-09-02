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

use super::line_hash::line_id;

/// Rough token estimate: ~4 chars per token (good enough for truncation decisions).
pub fn estimate_tokens(content: &str) -> usize {
	content.len().div_ceil(4)
}

/// Render one line as "N:hh|content" — the single line-id format all tools share.
fn render_line(line_1idx: usize, content: &str) -> String {
	format!("{}|{}", line_id(line_1idx, content), content)
}

/// Format content with composite line ids. A ranged view renders exactly the
/// requested lines plus one trailing marker with the remaining/total line count.
pub fn format_content_with_line_numbers(
	lines: &[&str],
	start_line_number: usize,
	view_range: Option<(usize, i64)>,
) -> String {
	let render = |i: usize| render_line(start_line_number + i, lines[i]);

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

		let mut result_lines: Vec<String> = (start_idx..end_idx).map(render).collect();

		// The caller chose this range; the only thing worth adding is how much
		// file lies beyond it. No head/tail context lines — they cost tokens on
		// every ranged read and carry nothing the model asked for.
		let remaining = lines.len() - end_idx;
		if start_idx > 0 || remaining > 0 {
			result_lines.push(if remaining == 0 {
				format!("[end of file, {} lines total]", lines.len())
			} else {
				format!("[... {remaining} more lines, {} total]", lines.len())
			});
		}

		result_lines.join("\n")
	} else {
		(0..lines.len()).map(render).collect::<Vec<_>>().join("\n")
	}
}

/// Format extracted content with composite line ids and smart truncation.
/// `start_line` is the 1-indexed position of `lines[0]` in the source file, so
/// the ids match what a `view` of the source shows.
pub fn format_extracted_content_smart(
	lines: &[&str],
	start_line: usize,
	max_display_lines: Option<usize>,
) -> String {
	let render = |i: usize| render_line(start_line + i, lines[i]);

	let max_lines = max_display_lines.unwrap_or(50);

	if lines.len() <= max_lines {
		(0..lines.len()).map(render).collect::<Vec<_>>().join("\n")
	} else {
		let show_first = (max_lines * 2) / 3;
		let show_last = max_lines - show_first - 1;

		let mut result_lines = Vec::new();

		for i in 0..show_first {
			result_lines.push(render(i));
		}

		let hidden_lines = lines.len() - show_first - show_last;
		result_lines.push(format!("[...{} lines more]", hidden_lines));

		for i in (lines.len() - show_last)..lines.len() {
			result_lines.push(render(i));
		}

		result_lines.join("\n")
	}
}
