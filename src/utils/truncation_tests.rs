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

//! Unit tests for truncation formatting: ranged views, boundary ranges and
//! smart middle-elision of extracted content.

use super::*;

#[test]
fn estimate_tokens_rounds_up_and_scales_with_bytes() {
	assert_eq!(estimate_tokens(0), 0);
	assert_eq!(estimate_tokens(1), 1);
	assert_eq!(estimate_tokens(7), 2);
	assert_eq!(estimate_tokens(35), 10);
}

#[test]
fn format_megabytes_renders_one_decimal() {
	assert_eq!(format_megabytes(0), "0.0 MB");
	assert_eq!(format_megabytes(1024 * 1024), "1.0 MB");
	assert_eq!(format_megabytes(85_200_000), "81.3 MB");
}

#[test]
fn whole_file_render_numbers_every_line_without_a_range() {
	let lines = vec!["alpha", "beta"];
	let out = format_content_with_line_numbers(&lines, 1, None);
	let rendered: Vec<&str> = out.split('\n').collect();
	assert_eq!(rendered.len(), 2);
	assert!(rendered[0].ends_with("|alpha"));
	assert!(rendered[0].starts_with("1:"));
	assert!(rendered[1].ends_with("|beta"));
}

#[test]
fn ranged_view_start_zero_reads_from_the_beginning() {
	let lines = vec!["a", "b", "c"];
	let out = format_content_with_line_numbers(&lines, 1, Some((0, 2)));
	assert!(out.contains("|a"));
	assert!(out.contains("|b"));
	assert!(!out.contains("|c"));
	assert!(out.contains("1 more line"));
}

#[test]
fn ranged_view_negative_end_reads_to_the_end_of_file() {
	let lines = vec!["a", "b", "c"];
	let out = format_content_with_line_numbers(&lines, 1, Some((2, -1)));
	assert!(!out.contains("|a"));
	assert!(out.contains("|b"));
	assert!(out.contains("|c"));
	assert!(out.contains("[end of file, 3 lines total]"));
}

#[test]
fn ranged_view_start_past_eof_reports_it_as_content() {
	let lines = vec!["a"];
	let out = format_content_with_line_numbers(&lines, 1, Some((9, -1)));
	assert_eq!(out, "Start line 9 exceeds content length (1 lines)");
}

#[test]
fn ranged_view_inverted_range_reports_the_ordering_error() {
	let lines = vec!["a", "b", "c"];
	let out = format_content_with_line_numbers(&lines, 1, Some((3, 1)));
	assert_eq!(out, "Start line 3 must be less than or equal to end line 1");
}

#[test]
fn ranged_view_whole_file_range_emits_no_total_marker() {
	let lines = vec!["a", "b"];
	let out = format_content_with_line_numbers(&lines, 1, Some((1, 2)));
	assert!(!out.contains("total"), "out was: {out}");
}

#[test]
fn extracted_content_within_limit_renders_all_lines_verbatim() {
	let lines: Vec<&str> = (0..5).map(|_| "x").collect();
	let out = format_extracted_content_smart(&lines, 10, Some(5));
	assert_eq!(out.split('\n').count(), 5);
	// ids reflect the start_line offset into the source file
	assert!(out.split('\n').next().unwrap().starts_with("10:"));
}

#[test]
fn extracted_content_over_limit_keeps_two_thirds_head_and_rest_tail() {
	let total = 100;
	let max = 9;
	let lines: Vec<String> = (0..total).map(|i| format!("line{i}")).collect();
	let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
	let out = format_extracted_content_smart(&refs, 1, Some(max));
	let rendered: Vec<&str> = out.split('\n').collect();
	// 6 head + 1 elision marker + 2 tail = max lines of output
	assert_eq!(rendered.len(), max);
	assert!(rendered[6].starts_with("[...92 lines more]"));
	assert!(rendered[0].ends_with("|line0"));
	assert!(rendered[5].ends_with("|line5"));
	assert!(rendered[7].ends_with("|line98"));
	assert!(rendered[8].ends_with("|line99"));
}

#[test]
fn extracted_content_defaults_to_fifty_display_lines() {
	let lines: Vec<String> = (0..80).map(|i| format!("v{i}")).collect();
	let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
	let out = format_extracted_content_smart(&refs, 1, None);
	assert_eq!(out.split('\n').count(), 50);
}
