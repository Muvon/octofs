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

// Text editing module - handling string replacement, line operations, and insertions

use super::super::McpToolCall;
use super::core::save_file_history;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::fs as tokio_fs;
use tokio::sync::Mutex as AsyncMutex;

// Thread-safe file locking infrastructure for concurrent write protection.
// Outer map uses std::sync::Mutex (held briefly, no await while locked).
// Per-file locks use tokio::sync::Mutex (held across async file I/O).
static FILE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();

fn get_file_locks() -> &'static Mutex<HashMap<String, Arc<AsyncMutex<()>>>> {
	FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

// Build the lock-map key for a path. Canonicalize when possible so aliases
// (`x`, `./x`, `/abs/x`, symlinks) map to the same lock — otherwise two
// requests targeting the same file would not serialize and could corrupt it.
// Falls back to the raw path string if canonicalize fails (file may not exist
// yet, e.g. for `text_editor create`).
pub fn lock_key_for(path: &Path) -> String {
	match path.canonicalize() {
		Ok(canon) => canon.to_string_lossy().to_string(),
		Err(_) => path.to_string_lossy().to_string(),
	}
}

// Acquire a file-specific lock to prevent concurrent writes to the same file
async fn acquire_file_lock(path: &Path) -> Result<Arc<AsyncMutex<()>>> {
	let key = lock_key_for(path);

	let file_lock = {
		let mut locks = get_file_locks().lock().expect("file locks poisoned");
		locks
			.entry(key)
			.or_insert_with(|| Arc::new(AsyncMutex::new(())))
			.clone()
	};

	Ok(file_lock)
}

// Helper function to resolve line indices, supporting negative indexing
// Negative indices count from the end: -1 = last line, -2 = second-to-last, etc.
fn resolve_line_index(index: i64, total_lines: usize) -> Result<usize, String> {
	if index == 0 {
		return Err("Line numbers are 1-indexed, use 1 for first line".to_string());
	}

	if index > 0 {
		let pos_index = index as usize;
		if pos_index > total_lines {
			return Err(format!(
				"Line {} exceeds file length ({} lines)",
				index, total_lines
			));
		}
		Ok(pos_index)
	} else {
		// Negative indexing: -1 = last line, -2 = second-to-last, etc.
		let from_end = (-index) as usize;
		if from_end > total_lines {
			return Err(format!(
				"Negative index {} exceeds file length ({} lines)",
				index, total_lines
			));
		}
		Ok(total_lines - from_end + 1)
	}
}

// Helper function to resolve line range with negative indexing support
fn resolve_line_range_batch(
	start: i64,
	end: i64,
	total_lines: usize,
) -> Result<(usize, usize), String> {
	let resolved_start = resolve_line_index(start, total_lines)?;
	let resolved_end = resolve_line_index(end, total_lines)?;

	if resolved_start > resolved_end {
		return Err(format!(
			"Start line ({}) cannot be greater than end line ({})",
			start, end
		));
	}

	Ok((resolved_start, resolved_end))
}

// Batch operation structures for the new single-file, multi-operation approach
#[derive(Debug, Clone)]
struct BatchOperation {
	operation_type: OperationType,
	line_range: LineRange,
	content: String,
	operation_index: usize,
}

// Unresolved batch operation with raw line indices (may be negative)
#[derive(Debug, Clone)]
struct UnresolvedBatchOperation {
	operation_type: OperationType,
	line_range: UnresolvedLineRange,
	content: String,
	operation_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum OperationType {
	Insert,
	Replace,
}

#[derive(Debug, Clone)]
enum LineRange {
	Single(usize),       // Insert after this line (0 = beginning of file)
	Range(usize, usize), // Replace this range (inclusive, 1-indexed)
}

#[derive(Debug, Clone)]
enum UnresolvedLineRange {
	Single(i64),               // Insert after this line (may be negative)
	Range(i64, i64),           // Replace this range (may be negative)
	Hash(String),              // Insert after line identified by hash
	HashRange(String, String), // Replace range identified by hashes
}

// Resolve unresolved line range to actual line range using file length.
// `lines` is needed for hash resolution (the full file's lines).
fn resolve_unresolved_line_range(
	unresolved: &UnresolvedLineRange,
	total_lines: usize,
	lines: &[&str],
) -> Result<LineRange, String> {
	match unresolved {
		UnresolvedLineRange::Single(line) => {
			// Insert: 0 = before line 1 (beginning of file) — valid special case
			if *line == 0 {
				return Ok(LineRange::Single(0));
			}
			let resolved = resolve_line_index(*line, total_lines)?;
			Ok(LineRange::Single(resolved))
		}
		UnresolvedLineRange::Range(start, end) => {
			let (resolved_start, resolved_end) =
				resolve_line_range_batch(*start, *end, total_lines)?;
			Ok(LineRange::Range(resolved_start, resolved_end))
		}
		UnresolvedLineRange::Hash(hash) => {
			let line = crate::utils::line_hash::resolve_hash_to_line(hash, lines)?;
			Ok(LineRange::Single(line))
		}
		UnresolvedLineRange::HashRange(start_hash, end_hash) => {
			let start = crate::utils::line_hash::resolve_hash_to_line(start_hash, lines)?;
			let end = crate::utils::line_hash::resolve_hash_to_line(end_hash, lines)?;
			if start > end {
				return Err(format!(
					"Hash range is reversed: '{}' is line {} but '{}' is line {} (which comes before it). \
					Did you mean line_range: [\"{}\", \"{}\"]?",
					start_hash, start, end_hash, end, end_hash, start_hash
				));
			}
			Ok(LineRange::Range(start, end))
		}
	}
}

/// Normalize a string for whitespace-insensitive comparison.
/// Trims each line and collapses runs of whitespace into a single space.
fn normalize_whitespace(s: &str) -> String {
	s.lines()
		.map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
		.collect::<Vec<_>>()
		.join("\n")
}

/// Find all byte-offset positions of `needle` in `haystack` (non-overlapping).
fn find_all_positions(haystack: &str, needle: &str) -> Vec<usize> {
	let mut positions = Vec::new();
	let mut start = 0;
	while let Some(pos) = haystack[start..].find(needle) {
		positions.push(start + pos);
		start += pos + needle.len();
	}
	positions
}

/// Convert a byte offset in `content` to a 1-indexed line number.
fn byte_offset_to_line(content: &str, offset: usize) -> usize {
	content[..offset].matches('\n').count() + 1
}

/// Compute line-by-line similarity ratio between two multi-line strings (0.0..1.0).
/// Uses a simple longest-common-subsequence ratio per line, averaged.
fn similarity_ratio(a: &str, b: &str) -> f64 {
	let a_lines: Vec<&str> = a.lines().collect();
	let b_lines: Vec<&str> = b.lines().collect();
	if a_lines.is_empty() && b_lines.is_empty() {
		return 1.0;
	}
	let max_lines = a_lines.len().max(b_lines.len());
	let mut total = 0.0;
	for i in 0..max_lines {
		let la = a_lines.get(i).unwrap_or(&"");
		let lb = b_lines.get(i).unwrap_or(&"");
		total += line_similarity(la, lb);
	}
	total / max_lines as f64
}

/// Character-level similarity between two strings using longest common subsequence.
fn line_similarity(a: &str, b: &str) -> f64 {
	let a_chars: Vec<char> = a.chars().collect();
	let b_chars: Vec<char> = b.chars().collect();
	let total = a_chars.len() + b_chars.len();
	if total == 0 {
		return 1.0;
	}
	let lcs_len = lcs_length(&a_chars, &b_chars);
	(2.0 * lcs_len as f64) / total as f64
}

/// Longest common subsequence length (O(n*m) DP, capped for performance).
fn lcs_length(a: &[char], b: &[char]) -> usize {
	// Cap to avoid quadratic blowup on very large inputs
	const MAX_CHARS: usize = 2000;
	let a = if a.len() > MAX_CHARS {
		&a[..MAX_CHARS]
	} else {
		a
	};
	let b = if b.len() > MAX_CHARS {
		&b[..MAX_CHARS]
	} else {
		b
	};

	let mut prev = vec![0usize; b.len() + 1];
	let mut curr = vec![0usize; b.len() + 1];
	for &ac in a {
		for (j, &bc) in b.iter().enumerate() {
			curr[j + 1] = if ac == bc {
				prev[j] + 1
			} else {
				prev[j + 1].max(curr[j])
			};
		}
		std::mem::swap(&mut prev, &mut curr);
		curr.iter_mut().for_each(|v| *v = 0);
	}
	*prev.last().unwrap_or(&0)
}

/// Diagnose why two text blocks differ.
fn diagnose_mismatch(expected: &str, actual: &str) -> String {
	let exp_norm = normalize_whitespace(expected);
	let act_norm = normalize_whitespace(actual);
	if exp_norm == act_norm {
		return "whitespace/indentation mismatch only".to_string();
	}
	// Check if it's just leading whitespace per line
	let exp_trimmed: Vec<&str> = expected.lines().map(|l| l.trim()).collect();
	let act_trimmed: Vec<&str> = actual.lines().map(|l| l.trim()).collect();
	if exp_trimmed == act_trimmed {
		return "indentation mismatch only".to_string();
	}
	"content differs".to_string()
}

/// Find the top N closest matching windows in `content` for `needle`.
/// Returns vec of (start_line_1indexed, window_text, similarity).
fn find_closest_matches(content: &str, needle: &str, top_n: usize) -> Vec<(usize, String, f64)> {
	let content_lines: Vec<&str> = content.lines().collect();
	let needle_lines: Vec<&str> = needle.lines().collect();
	let needle_count = needle_lines.len().max(1);

	if content_lines.len() < needle_count {
		return Vec::new();
	}

	let mut candidates: Vec<(usize, String, f64)> = Vec::new();

	for start in 0..=(content_lines.len() - needle_count) {
		let window: String = content_lines[start..start + needle_count].join("\n");
		let sim = similarity_ratio(needle, &window);
		// Only consider windows with at least 40% similarity
		if sim >= 0.4 {
			candidates.push((start + 1, window, sim));
		}
	}

	// Sort by similarity descending
	candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
	candidates.truncate(top_n);
	candidates
}

/// Detect the leading whitespace (indentation) of the first non-empty line.
fn detect_indent(text: &str) -> &str {
	for line in text.lines() {
		if !line.trim().is_empty() {
			let trimmed = line.trim_start();
			return &line[..line.len() - trimmed.len()];
		}
	}
	""
}

/// Adjust indentation of `new_text` to match the actual indentation at the match site.
/// `provided_old` is the old_text as given by the caller (may have wrong indent).
/// `actual_old` is the actual text found in the file at the match location.
fn adjust_indentation(new_text: &str, provided_old: &str, actual_old: &str) -> String {
	let provided_indent = detect_indent(provided_old);
	let actual_indent = detect_indent(actual_old);

	if provided_indent == actual_indent {
		return new_text.to_string();
	}

	// Determine if we're using tabs or spaces from the actual file
	let provided_len = provided_indent.len();

	new_text
		.lines()
		.map(|line| {
			if line.trim().is_empty() {
				return line.to_string();
			}
			// Strip the provided indent prefix if present, then prepend actual indent
			if provided_len > 0 && line.starts_with(provided_indent) {
				format!("{}{}", actual_indent, &line[provided_len..])
			} else {
				// Line doesn't start with expected indent — prepend the delta
				let line_indent_len = line.len() - line.trim_start().len();
				if line_indent_len >= provided_len {
					// Extra indent beyond base — preserve the extra part
					format!("{}{}", actual_indent, &line[provided_len..])
				} else {
					// Less indent than base — just prepend actual
					format!("{}{}", actual_indent, line.trim_start())
				}
			}
		})
		.collect::<Vec<_>>()
		.join("\n")
}

/// Atomic write: write to a temp file in the same directory, then rename over the target.
/// Build a unified-style diff for a str_replace operation showing CONTEXT lines before/after.
// `start` is 0-indexed position of the first replaced line in `orig_lines`.
fn build_str_replace_diff(
	orig_lines: &[&str],
	new_lines: &[&str],
	start: usize,
	old_line_count: usize,
	new_text_lines: &[&str],
) -> String {
	const CONTEXT: usize = 2;
	let mut diff: Vec<String> = Vec::new();

	// Context before
	let ctx_before_start = start.saturating_sub(CONTEXT);
	if ctx_before_start > 0 {
		diff.push("...".to_string());
	}
	for (i, line) in orig_lines
		.iter()
		.enumerate()
		.take(start)
		.skip(ctx_before_start)
	{
		diff.push(format!("{}: {}", i + 1, line));
	}

	// Removed lines
	for (i, line) in orig_lines
		.iter()
		.enumerate()
		.skip(start)
		.take(old_line_count)
	{
		diff.push(format!("-{}: {}", i + 1, line));
	}

	// Added lines
	// In the new file the inserted block starts at `start + 1` (1-indexed)
	let new_block_start = start + 1;
	for (i, line) in new_text_lines.iter().enumerate() {
		diff.push(format!("+{}: {}", new_block_start + i, line));
	}

	// Context after: read from new_lines (already has the replacement applied)
	let new_after_start = start + new_text_lines.len(); // 0-indexed in new_lines
	let ctx_after_end = (new_after_start + CONTEXT).min(new_lines.len());
	for (i, line) in new_lines
		.iter()
		.enumerate()
		.take(ctx_after_end)
		.skip(new_after_start)
	{
		diff.push(format!("{}: {}", i + 1, line));
	}
	if ctx_after_end < new_lines.len() {
		diff.push("...".to_string());
	}

	diff.join("\n")
}

// Guarantees the file is never in a partial/corrupt state if the process is interrupted.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()> {
	let parent_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
	let tmp_path = parent_dir.join(format!(
		".octofs_tmp_{}.tmp",
		path.file_name().unwrap_or_default().to_string_lossy()
	));
	tokio_fs::write(&tmp_path, content)
		.await
		.map_err(|e| anyhow!("Failed to write temp file for '{}': {}", path.display(), e))?;
	if let Err(e) = tokio_fs::rename(&tmp_path, path).await {
		// Clean up temp file on rename failure
		let _ = tokio_fs::remove_file(&tmp_path).await;
		return Err(anyhow!(
			"Failed to atomically replace '{}': {}",
			path.display(),
			e
		));
	}
	Ok(())
}
// Replace a string in a file with progressive matching strategy:
// 1. Exact match (original behavior)
// 2. Whitespace-normalized fuzzy match with indentation adjustment
// 3. Rich diagnostics with closest candidates on failure
pub async fn str_replace_spec(path: &Path, old_text: &str, new_text: &str) -> Result<String> {
	if !path.exists() {
		bail!("File not found");
	}

	// Acquire file lock to prevent concurrent writes
	let file_lock = acquire_file_lock(path).await?;
	let _lock_guard = file_lock.lock().await;

	// Read the file content
	let content = tokio_fs::read_to_string(path)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;

	// === Stage 1: Exact match ===
	let occurrences = content.matches(old_text).count();

	if occurrences == 1 {
		// Perfect exact match — replace directly
		let orig_lines: Vec<&str> = content.lines().collect();
		let old_line_count = old_text.lines().count();
		// Find the 0-indexed start line of the match
		let match_offset = content.find(old_text).unwrap_or(0);
		let match_start = byte_offset_to_line(&content, match_offset) - 1;

		save_file_history(path).await?;
		let new_content = content.replace(old_text, new_text);
		atomic_write(path, &new_content).await?;

		if old_line_count > 1 {
			crate::mcp::hint_accumulator::push_hint(&format!(
				"`str_replace` matched {} lines. Prefer `batch_edit` when you know the line range — it's faster and avoids content-search ambiguity.",
				old_line_count
			));
		}

		let new_lines: Vec<&str> = new_content.lines().collect();
		let new_text_lines: Vec<&str> = new_text.lines().collect();
		let diff = build_str_replace_diff(
			&orig_lines,
			&new_lines,
			match_start,
			old_line_count,
			&new_text_lines,
		);
		return Ok(diff);
	}

	if occurrences > 1 {
		// Multiple exact matches — show locations to help disambiguate
		let positions = find_all_positions(&content, old_text);
		let use_hashes = crate::utils::line_hash::is_hash_mode();
		let file_lines: Vec<&str> = content.lines().collect();
		let hashes: Vec<String> = if use_hashes {
			crate::utils::line_hash::compute_line_hashes(&file_lines)
		} else {
			Vec::new()
		};
		let locations: Vec<String> = positions
			.iter()
			.enumerate()
			.map(|(i, &offset)| {
				let line = byte_offset_to_line(&content, offset);
				if use_hashes {
					format!("  {}. hash {} (line {})", i + 1, hashes[line - 1], line)
				} else {
					format!("  {}. line {}", i + 1, line)
				}
			})
			.collect();

		bail!(
			"Found {} matches for replacement text at:\n{}\nAdd more surrounding context to make a unique match, or use `batch_edit` with the specific {}.",
			occurrences,
			locations.join("\n"),
			if use_hashes { "hash range" } else { "line range" }
		);
	}

	// === Stage 2: Whitespace-normalized fuzzy match ===
	let norm_old = normalize_whitespace(old_text);
	let norm_content = normalize_whitespace(&content);
	let norm_occurrences = norm_content.matches(&norm_old).count();

	if norm_occurrences == 1 {
		// Found exactly one whitespace-normalized match — map back to original content
		// We need to find the actual text in the original content that corresponds
		let content_lines: Vec<&str> = content.lines().collect();
		let old_lines: Vec<&str> = old_text.lines().collect();
		let old_line_count = old_lines.len();

		let mut match_start = None;
		for start in 0..=content_lines.len().saturating_sub(old_line_count) {
			let window: Vec<&str> = content_lines[start..start + old_line_count].to_vec();
			let window_norm: Vec<String> = window
				.iter()
				.map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
				.collect();
			let old_norm: Vec<String> = old_lines
				.iter()
				.map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
				.collect();
			if window_norm == old_norm {
				match_start = Some(start);
				break;
			}
		}

		if let Some(start) = match_start {
			let actual_old = content_lines[start..start + old_line_count].join("\n");
			let adjusted_new = adjust_indentation(new_text, old_text, &actual_old);

			save_file_history(path).await?;
			let new_content = content.replace(&actual_old, &adjusted_new);
			atomic_write(path, &new_content).await?;

			crate::mcp::hint_accumulator::push_hint(
				"Replaced via fuzzy match (whitespace-normalized). Indentation was auto-adjusted to match the file.",
			);

			let new_lines: Vec<&str> = new_content.lines().collect();
			let new_text_lines: Vec<&str> = adjusted_new.lines().collect();
			let diff = build_str_replace_diff(
				&content_lines,
				&new_lines,
				start,
				old_line_count,
				&new_text_lines,
			);
			return Ok(diff);
		}
	}

	// === Stage 3: No match — provide rich diagnostics ===
	let candidates = find_closest_matches(&content, old_text, 3);

	let mut msg = String::from(
		"No exact match found. Make sure you pass raw content (no escaped \\t, \\n).\n",
	);

	if candidates.is_empty() {
		msg.push_str("No similar text found in the file. Verify the content exists.");
	} else {
		let use_hashes = crate::utils::line_hash::is_hash_mode();
		let diag_lines: Vec<&str> = content.lines().collect();
		let diag_hashes: Vec<String> = if use_hashes {
			crate::utils::line_hash::compute_line_hashes(&diag_lines)
		} else {
			Vec::new()
		};

		msg.push_str("Closest matches:\n");
		let old_line_count = old_text.lines().count();
		for (i, (line_num, window, sim)) in candidates.iter().enumerate() {
			let diagnosis = diagnose_mismatch(old_text, window);
			let end_line = line_num + old_line_count - 1;
			if use_hashes {
				let start_hash = &diag_hashes[line_num - 1];
				let end_hash = &diag_hashes[end_line - 1];
				msg.push_str(&format!(
					"\n  {}. Hashes {}-{} ({:.0}% similar, {})\n",
					i + 1,
					start_hash,
					end_hash,
					sim * 100.0,
					diagnosis
				));
			} else {
				msg.push_str(&format!(
					"\n  {}. Lines {}-{} ({:.0}% similar, {})\n",
					i + 1,
					line_num,
					end_line,
					sim * 100.0,
					diagnosis
				));
			}
			// Show first 3 lines of the candidate as preview
			for (j, line) in window.lines().take(3).enumerate() {
				let pfx = if use_hashes {
					diag_hashes[line_num - 1 + j].clone()
				} else {
					format!("{}", line_num + j)
				};
				msg.push_str(&format!("     {}: {}\n", pfx, line));
			}
			if old_line_count > 3 {
				msg.push_str(&format!("     ... ({} more lines)\n", old_line_count - 3));
			}
		}
		msg.push_str(&format!(
			"\nTip: use `batch_edit` with the {} shown above, or fix the `old_text` content.",
			if use_hashes {
				"hash range"
			} else {
				"line range"
			}
		));
	}

	bail!("{}", msg);
}

// Returns true for lines that are pure structural punctuation (e.g. `}`, `]`, `}`).
// These are exempt from duplicate-line detection because they legitimately appear
// at range boundaries without indicating the AI included surrounding context.
//
// Only SINGLE closing tokens (optionally trailed by one `,` or `;`) qualify.
// Compound closers like `});` or `}),` are NOT noise — they carry real semantic
// meaning and duplicating them breaks code.
fn is_structural_noise(line: &str) -> bool {
	let trimmed = line.trim();
	// Empty or only whitespace
	if trimmed.is_empty() {
		return true;
	}
	// Strip one optional trailing `,` or `;`
	let core = trimmed.trim_end_matches([',', ';']);
	// Must be exactly one closing bracket/brace/paren after stripping the trailer
	matches!(core, "}" | "]" | ")")
}

// Check whether `content` (the replacement) duplicates the line immediately
// before or after the range [start_line, end_line] (1-indexed) in `file_lines`.
//
// Returns an error string if duplication is detected, Ok(()) otherwise.
// Structural noise lines are exempt — they legitimately appear at boundaries.
fn check_replace_duplicates(
	content_lines: &[&str],
	file_lines: &[&str],
	start_line: usize,
	end_line: usize,
	operation_index: usize,
	hashes: &[String],
	use_hashes: bool,
) -> Result<(), String> {
	if content_lines.is_empty() {
		return Ok(());
	}

	// Format a line reference as hash or number
	let line_id = |line_1idx: usize| -> String {
		if use_hashes {
			hashes[line_1idx - 1].clone()
		} else {
			format!("line {}", line_1idx)
		}
	};
	let range_id = |s: usize, e: usize| -> String {
		if use_hashes {
			format!("[{},{}]", hashes[s - 1], hashes[e - 1])
		} else {
			format!("[{}-{}]", s, e)
		}
	};

	// First content line matches the line immediately before the range
	if start_line > 1 {
		let line_before = file_lines[start_line - 2];
		if content_lines[0] == line_before && !is_structural_noise(line_before) {
			return Err(format!(
				"Duplicate line detected in operation {}: content's first line matches {} \
				(just before the replacement range {}). \
				{}: {:?}. Do NOT include surrounding unchanged lines — \
				only provide the lines that replace {}.",
				operation_index,
				line_id(start_line - 1),
				range_id(start_line, end_line),
				line_id(start_line - 1),
				line_before,
				range_id(start_line, end_line)
			));
		}
	}
	// Last content line matches the line immediately after the range
	if end_line < file_lines.len() {
		let line_after = file_lines[end_line];
		let last = content_lines[content_lines.len() - 1];
		if last == line_after && !is_structural_noise(line_after) {
			return Err(format!(
				"Duplicate line detected in operation {}: content's last line matches {} \
				(just after the replacement range {}). \
				{}: {:?}. Do NOT include surrounding unchanged lines — \
				only provide the lines that replace {}.",
				operation_index,
				line_id(end_line + 1),
				range_id(start_line, end_line),
				line_id(end_line + 1),
				line_after,
				range_id(start_line, end_line)
			));
		}
	}
	Ok(())
}

// Check for conflicting operations that would corrupt the file.
//
// Conflict rules:
//   Replace vs Replace — ranges overlap → conflict (both try to modify same lines)
//   Insert  vs Insert  — same anchor line → conflict (ambiguous ordering)
//   Insert  vs Replace — NEVER conflict. Insert operates in the *gap* after a line,
//                        Replace operates on the line's *content*. They are independent.
fn detect_conflicts(
	operations: &[BatchOperation],
	hashes: &[String],
	use_hashes: bool,
) -> Result<(), String> {
	let id = |line_1idx: usize| -> String {
		if use_hashes {
			hashes[line_1idx - 1].clone()
		} else {
			format!("{}", line_1idx)
		}
	};

	for i in 0..operations.len() {
		for j in (i + 1)..operations.len() {
			let op1 = &operations[i];
			let op2 = &operations[j];

			match (&op1.operation_type, &op2.operation_type) {
				// Two replaces: check if ranges overlap
				(OperationType::Replace, OperationType::Replace) => {
					let (s1, e1) = replace_range(&op1.line_range);
					let (s2, e2) = replace_range(&op2.line_range);
					// Ranges overlap when s1 <= e2 AND s2 <= e1
					if s1 <= e2 && s2 <= e1 {
						return Err(format!(
							"Conflicting operations: operation {} (replace [{},{}]) and {} (replace [{},{}]) have overlapping ranges",
							op1.operation_index, id(s1), id(e1), op2.operation_index, id(s2), id(e2)
						));
					}
				}
				// Two inserts: conflict only if same anchor line (ambiguous order)
				(OperationType::Insert, OperationType::Insert) => {
					let line1 = insert_anchor(&op1.line_range);
					let line2 = insert_anchor(&op2.line_range);
					if line1 == line2 {
						return Err(format!(
							"Conflicting operations: operation {} and {} both insert after {}",
							op1.operation_index,
							op2.operation_index,
							id(line1)
						));
					}
				}
				// Insert + Replace: never conflict — they operate on different
				// conceptual positions (gap vs content)
				(OperationType::Insert, OperationType::Replace)
				| (OperationType::Replace, OperationType::Insert) => {}
			}
		}
	}
	Ok(())
}

// Extract the (start, end) range from a replace operation's LineRange
fn replace_range(line_range: &LineRange) -> (usize, usize) {
	match line_range {
		LineRange::Range(start, end) => (*start, *end),
		LineRange::Single(line) => (*line, *line),
	}
}

// Extract the anchor line from an insert operation's LineRange
fn insert_anchor(line_range: &LineRange) -> usize {
	match line_range {
		LineRange::Single(line) => *line,
		LineRange::Range(start, _) => *start,
	}
}

// Apply all operations to the original file content.
//
// Two-phase approach: replaces first, then inserts.
// All line numbers reference the ORIGINAL file. Replaces among themselves are
// applied in reverse order (highest start first) so earlier replaces don't shift
// later ones. After all replaces, we compute an offset map so inserts can find
// their correct position in the (now modified) line array.
async fn apply_batch_operations(
	original_content: &str,
	operations: &[BatchOperation],
) -> Result<String> {
	let mut lines: Vec<String> = original_content.lines().map(|s| s.to_string()).collect();
	let original_len = lines.len();

	// Separate into replaces and inserts
	let mut replaces: Vec<&BatchOperation> = operations
		.iter()
		.filter(|op| op.operation_type == OperationType::Replace)
		.collect();
	let mut inserts: Vec<&BatchOperation> = operations
		.iter()
		.filter(|op| op.operation_type == OperationType::Insert)
		.collect();

	// Sort replaces by start position descending (highest first)
	replaces.sort_by(|a, b| {
		let sa = match &a.line_range {
			LineRange::Range(s, _) => *s,
			LineRange::Single(l) => *l,
		};
		let sb = match &b.line_range {
			LineRange::Range(s, _) => *s,
			LineRange::Single(l) => *l,
		};
		sb.cmp(&sa)
	});

	// Phase 1: Apply all replaces (reverse order preserves original line refs)
	// Track each replace's offset: (original_end, delta) where delta = new_lines - old_lines
	let mut replace_deltas: Vec<(usize, usize, i64)> = Vec::with_capacity(replaces.len());

	for operation in &replaces {
		let (start, end) = match operation.line_range {
			LineRange::Range(start, end) => (start, end),
			LineRange::Single(line) => (line, line),
		};

		// Validate line range (1-indexed)
		if start == 0 || end == 0 {
			return Err(anyhow!("Line numbers must be 1-indexed (start from 1)"));
		}
		if start > original_len || end > original_len {
			return Err(anyhow!(
				"Line range [{}, {}] is beyond file length {}",
				start,
				end,
				original_len
			));
		}
		if start > end {
			return Err(anyhow!("Invalid line range: start {} > end {}", start, end));
		}

		let old_count = end - start + 1;
		let content_lines: Vec<String> = operation.content.lines().map(|s| s.to_string()).collect();
		let new_count = content_lines.len();

		// Remove old lines (0-indexed)
		let start_idx = start - 1;
		for _ in 0..old_count {
			lines.remove(start_idx);
		}

		// Insert new content
		for (i, line) in content_lines.into_iter().enumerate() {
			lines.insert(start_idx + i, line);
		}

		replace_deltas.push((start, end, new_count as i64 - old_count as i64));
	}

	// Phase 2: Apply inserts with adjusted positions.
	// For each insert's original anchor line, compute how much it shifted due to replaces.
	// Sort inserts descending so they don't interfere with each other.
	inserts.sort_by(|a, b| {
		let la = match &a.line_range {
			LineRange::Single(l) => *l,
			LineRange::Range(s, _) => *s,
		};
		let lb = match &b.line_range {
			LineRange::Single(l) => *l,
			LineRange::Range(s, _) => *s,
		};
		lb.cmp(&la)
	});

	for operation in &inserts {
		let original_anchor = match operation.line_range {
			LineRange::Single(line) => line,
			_ => return Err(anyhow!("Insert operation must use single line number")),
		};

		// Validate against original file length
		if original_anchor > original_len {
			return Err(anyhow!(
				"Insert position {} is beyond file length {}",
				original_anchor,
				original_len
			));
		}

		// Compute adjusted position: start from original anchor, apply offsets
		// from all replaces that END at or before this anchor.
		// A replace at [s,e] with delta D shifts everything after line e by D.
		// If anchor >= e (insert is after the replaced region), apply the full delta.
		// If anchor < s (insert is before the replaced region), no shift.
		// If s <= anchor < e (insert is inside the replaced region), the anchor
		// falls within replacement content — shift by (anchor - s) positions into
		// the new content, capped at the new content length.
		let mut adjusted = original_anchor as i64;
		for &(rs, re, delta) in &replace_deltas {
			if original_anchor >= re {
				// Insert anchor is at or after the replace's end — full shift
				adjusted += delta;
			} else if original_anchor >= rs {
				// Insert anchor is inside the replaced range.
				// Map it proportionally: anchor was at offset (anchor - rs) into
				// the old range. In the new content, cap at new_count.
				let old_count = (re - rs + 1) as i64;
				let new_count = old_count + delta;
				let offset_in_old = (original_anchor - rs) as i64;
				// Proportional position in new content, capped
				let offset_in_new = offset_in_old.min(new_count);
				// The replace starts at rs in original. After this replace,
				// position rs maps to rs (unchanged start). So anchor maps to
				// rs + offset_in_new. But we started with adjusted = original_anchor,
				// so the delta to apply is: (rs as i64 + offset_in_new) - original_anchor as i64
				adjusted += (rs as i64 + offset_in_new) - original_anchor as i64;
			}
			// else: anchor is before the replace — no shift needed
		}

		let insert_pos = adjusted.max(0) as usize;

		// Split content by lines and insert
		let content_lines: Vec<String> = operation.content.lines().map(|s| s.to_string()).collect();

		if insert_pos == 0 {
			for (i, line) in content_lines.into_iter().enumerate() {
				lines.insert(i, line);
			}
		} else {
			let clamped = insert_pos.min(lines.len());
			for (i, line) in content_lines.into_iter().enumerate() {
				lines.insert(clamped + i, line);
			}
		}
	}

	// Preserve original file ending format
	let result = lines.join("\n");
	if original_content.ends_with('\n') && !result.ends_with('\n') {
		Ok(format!("{}\n", result))
	} else {
		Ok(result)
	}
}

// Parse line_range from JSON value (supports numbers, arrays, and hash strings)
fn parse_line_range(
	value: &Value,
	operation_type: &OperationType,
) -> Result<UnresolvedLineRange, String> {
	match value {
		Value::Number(n) => {
			let line = n.as_i64().ok_or("Line number must be an integer")?;
			match operation_type {
				// 0 is valid for insert: means "insert at beginning of file"
				OperationType::Insert => Ok(UnresolvedLineRange::Single(line)),
				OperationType::Replace => {
					if line == 0 {
						return Err(
							"Replace line numbers are 1-indexed, use 1 for first line".to_string()
						);
					}
					Ok(UnresolvedLineRange::Range(line, line)) // Single line replace
				}
			}
		}
		Value::String(s) => {
			// Hash-based line identifier (e.g., "a3bd")
			Ok(UnresolvedLineRange::Hash(s.clone()))
		}
		Value::Array(arr) => {
			if arr.is_empty() {
				return Err("Line range array must have 1 or 2 elements".to_string());
			}
			// Check if array contains strings (hash range) or numbers (line range)
			if arr[0].is_string() {
				// Hash range: ["start_hash", "end_hash"]
				if arr.len() != 2 {
					return Err("Hash range array must have exactly 2 elements".to_string());
				}
				let start_hash = arr[0].as_str().ok_or("Hash must be a string")?.to_string();
				let end_hash = arr[1].as_str().ok_or("Hash must be a string")?.to_string();
				match operation_type {
					OperationType::Insert => {
						Err("Insert operation cannot use hash range - use single hash".to_string())
					}
					OperationType::Replace => {
						Ok(UnresolvedLineRange::HashRange(start_hash, end_hash))
					}
				}
			} else if arr.len() == 1 {
				let line = arr[0].as_i64().ok_or("Line number must be an integer")?;
				match operation_type {
					// 0 is valid for insert: means "insert at beginning of file"
					OperationType::Insert => Ok(UnresolvedLineRange::Single(line)),
					OperationType::Replace => {
						if line == 0 {
							return Err("Replace line numbers are 1-indexed, use 1 for first line"
								.to_string());
						}
						Ok(UnresolvedLineRange::Range(line, line))
					}
				}
			} else if arr.len() == 2 {
				let start = arr[0].as_i64().ok_or("Start line must be an integer")?;
				let end = arr[1].as_i64().ok_or("End line must be an integer")?;
				if start == 0 || end == 0 {
					return Err(
						"Replace line numbers are 1-indexed, use 1 for first line".to_string()
					);
				}
				match operation_type {
					OperationType::Insert => Err(
						"Insert operation cannot use line range - use single line number"
							.to_string(),
					),
					OperationType::Replace => Ok(UnresolvedLineRange::Range(start, end)),
				}
			} else {
				Err("Line range array must have 1 or 2 elements".to_string())
			}
		}
		_ => Err("Line range must be a number, array, or hash string".to_string()),
	}
}

// NEW REVOLUTIONARY BATCH_EDIT: Single file, multiple operations, original line numbers
pub async fn batch_edit_spec(call: &McpToolCall, operations: &[Value]) -> Result<String> {
	// Extract path from the call parameters - NEW: single file only
	let path_str = match call.parameters.get("path").and_then(|v| v.as_str()) {
		Some(p) => p,
		None => {
			bail!("Missing required 'path' parameter for batch_edit");
		}
	};

	// Fail fast: validate operations array before touching the filesystem
	if operations.is_empty() {
		bail!("Operations array is empty — nothing to do.");
	}

	const MAX_OPERATIONS: usize = 50;
	if operations.len() > MAX_OPERATIONS {
		bail!(
			"Too many operations: {} (max {}). Split into multiple calls.",
			operations.len(),
			MAX_OPERATIONS
		);
	}

	let path = super::core::resolve_path(path_str, &call.workdir);
	// Check if file exists
	if !path.exists() {
		bail!("File not found: {}", path_str);
	}

	// Acquire file lock to prevent concurrent writes
	let file_lock = acquire_file_lock(&path).await?;
	let _lock_guard = file_lock.lock().await;

	// Read original file content
	let original_content = tokio_fs::read_to_string(&path)
		.await
		.map_err(|e| anyhow!("Failed to read file '{}': {}", path_str, e))?;

	// Parse and validate all operations (with unresolved line ranges)
	let mut unresolved_operations = Vec::new();
	let mut failed_operations = 0;
	let mut operation_details = Vec::new();

	for (index, operation) in operations.iter().enumerate() {
		let operation_obj = match operation.as_object() {
			Some(obj) => obj,
			None => {
				failed_operations += 1;
				operation_details.push(json!({
					"operation_index": index,
					"status": "failed",
					"error": "Operation must be an object"
				}));
				continue;
			}
		};

		// Extract operation type
		let op_type_str = match operation_obj.get("operation").and_then(|v| v.as_str()) {
			Some(op) => op,
			None => {
				failed_operations += 1;
				operation_details.push(json!({
					"operation_index": index,
					"status": "failed",
					"error": "Missing 'operation' field"
				}));
				continue;
			}
		};

		// Parse operation type
		let operation_type = match op_type_str {
			"insert" => OperationType::Insert,
			"replace" => OperationType::Replace,
			_ => {
				failed_operations += 1;
				operation_details.push(json!({
					"operation_index": index,
					"operation": op_type_str,
					"status": "failed",
					"error": format!("Unsupported operation type: '{}'. Supported operations: insert, replace", op_type_str)
				}));
				continue;
			}
		};

		// Extract line_range
		let line_range = match operation_obj.get("line_range") {
			Some(range_value) => match parse_line_range(range_value, &operation_type) {
				Ok(range) => range,
				Err(e) => {
					failed_operations += 1;
					operation_details.push(json!({
						"operation_index": index,
						"operation": op_type_str,
						"status": "failed",
						"error": format!("Invalid 'line_range': {}", e)
					}));
					continue;
				}
			},
			None => {
				failed_operations += 1;
				operation_details.push(json!({
					"operation_index": index,
					"operation": op_type_str,
					"status": "failed",
					"error": "Missing 'line_range' field"
				}));
				continue;
			}
		};

		// Extract content
		let content = match operation_obj.get("content").and_then(|v| v.as_str()) {
			Some(c) => c.to_string(),
			None => {
				failed_operations += 1;
				operation_details.push(json!({
					"operation_index": index,
					"operation": op_type_str,
					"status": "failed",
					"error": "Missing 'content' field"
				}));
				continue;
			}
		};

		// Create unresolved batch operation
		let unresolved_op = UnresolvedBatchOperation {
			operation_type,
			line_range: line_range.clone(),
			content,
			operation_index: index,
		};

		unresolved_operations.push(unresolved_op);

		operation_details.push(json!({
			"operation_index": index,
			"operation": op_type_str,
			"status": "parsed",
			"line_range": match &line_range {
				UnresolvedLineRange::Single(line) => json!(line),
				UnresolvedLineRange::Range(start, end) => json!([start, end]),
				UnresolvedLineRange::Hash(h) => json!(h),
				UnresolvedLineRange::HashRange(s, e) => json!([s, e]),
			}
		}));
	}

	// If all operations failed during parsing, return error
	if unresolved_operations.is_empty() {
		bail!(
			"No valid operations found. {} operations failed during parsing.",
			failed_operations
		);
	}

	// Resolve negative line indices (and hash identifiers) now that we have the file content
	let total_lines = original_content.lines().count();
	let original_lines_for_resolve: Vec<&str> = original_content.lines().collect();
	let mut batch_operations = Vec::new();

	for unresolved_op in unresolved_operations {
		match resolve_unresolved_line_range(
			&unresolved_op.line_range,
			total_lines,
			&original_lines_for_resolve,
		) {
			Ok(resolved_range) => {
				batch_operations.push(BatchOperation {
					operation_type: unresolved_op.operation_type,
					line_range: resolved_range,
					content: unresolved_op.content,
					operation_index: unresolved_op.operation_index,
				});
			}
			Err(err) => {
				bail!(
					"Invalid line range in operation {}: {}",
					unresolved_op.operation_index,
					err
				);
			}
		}
	}

	// Compute hashes once for all validation and output (used in hash mode)
	let original_lines: Vec<&str> = original_content.lines().collect();
	let use_hashes = crate::utils::line_hash::is_hash_mode();
	let orig_hashes: Vec<String> = if use_hashes {
		crate::utils::line_hash::compute_line_hashes(&original_lines)
	} else {
		Vec::new()
	};

	// Check for conflicts between operations
	if let Err(conflict_error) = detect_conflicts(&batch_operations, &orig_hashes, use_hashes) {
		bail!("{}", conflict_error);
	}

	// Duplicate-line detection: validate operations against original content before applying.
	// Catches the #1 AI mistake of including surrounding/already-existing lines.
	// Helper: format a line reference as hash or number for error messages
	let orig_line_id = |line_1idx: usize| -> String {
		if use_hashes {
			orig_hashes[line_1idx - 1].clone()
		} else {
			format!("line {}", line_1idx)
		}
	};
	for op in &batch_operations {
		let content_lines: Vec<&str> = op.content.lines().collect();
		if content_lines.is_empty() {
			continue;
		}
		match op.operation_type {
			OperationType::Replace => {
				let (start, end) = match op.line_range {
					LineRange::Range(s, e) => (s, e),
					LineRange::Single(line) => (line, line),
				};
				if let Err(e) = check_replace_duplicates(
					&content_lines,
					&original_lines,
					start,
					end,
					op.operation_index,
					&orig_hashes,
					use_hashes,
				) {
					bail!("{}", e);
				}
			}
			OperationType::Insert => {
				// insert_after=N means content goes between line N and line N+1.
				let insert_after = match op.line_range {
					LineRange::Single(line) => line,
					_ => continue, // malformed; apply_batch_operations will catch it
				};
				// Single-line insert: content[0] must not duplicate the line right after
				// the insert point, unless it is structural noise.
				if content_lines.len() == 1 {
					if insert_after < original_lines.len() {
						let line_after = original_lines[insert_after];
						if content_lines[0] == line_after && !is_structural_noise(line_after) {
							bail!(
								"Duplicate line detected in operation {}: inserting after {} would duplicate {} which already reads {:?}. Do NOT re-insert content that already exists in the file.",
								op.operation_index, orig_line_id(insert_after), orig_line_id(insert_after + 1), line_after
							);
						}
					}
				} else {
					// Multi-line insert (>=2 lines): full block match is unambiguous duplication — no noise exemption.
					let available = original_lines.len().saturating_sub(insert_after);
					let check_len = content_lines.len().min(available);
					if check_len >= 2
						&& content_lines[..check_len]
							== original_lines[insert_after..insert_after + check_len]
					{
						bail!(
							"Duplicate block detected in operation {}: the {} inserted lines starting after {} already exist verbatim at {}-{}. Do NOT re-insert content that already exists in the file.",
							op.operation_index, check_len, orig_line_id(insert_after), orig_line_id(insert_after + 1), orig_line_id(insert_after + check_len)
						);
					}
				}
			}
		}
	}

	// Apply all operations to the original content
	let final_content = apply_batch_operations(&original_content, &batch_operations)
		.await
		.map_err(|e| anyhow!("Failed to apply operations: {}", e))?;

	// Save file history for undo functionality
	save_file_history(&path).await?;

	atomic_write(&path, &final_content)
		.await
		.map_err(|e| anyhow!("Atomic write failed for '{}': {}", path_str, e))?;

	// Update operation details with success status
	for detail in &mut operation_details {
		if detail["status"] == "parsed" {
			detail["status"] = json!("success");
		}
	}

	// Build annotated diff for each operation so the AI can verify edits landed correctly.
	// In hash mode: uses content-based hashes as line prefixes.
	// In number mode: uses sequential line numbers as before.
	const CONTEXT: usize = 2;
	let new_lines: Vec<&str> = final_content.lines().collect();
	let new_hashes: Vec<String> = if use_hashes {
		crate::utils::line_hash::compute_line_hashes(&new_lines)
	} else {
		Vec::new()
	};

	// Helper closures: produce a line prefix from index (0-based for hashes, 1-based number for display)
	let orig_prefix = |line_1idx: usize| -> String {
		if use_hashes {
			orig_hashes[line_1idx - 1].clone()
		} else {
			format!("{}", line_1idx)
		}
	};
	let new_prefix = |line_1idx: usize| -> String {
		if use_hashes {
			new_hashes[line_1idx - 1].clone()
		} else {
			format!("{}", line_1idx)
		}
	};

	let mut diffs: Vec<String> = Vec::new();

	// Sort ops by original start line (ascending) for readable diff output
	let mut display_ops = batch_operations.clone();
	display_ops.sort_by_key(|op| match &op.line_range {
		LineRange::Single(line) => *line,
		LineRange::Range(start, _) => *start,
	});

	for op in &display_ops {
		match op.operation_type {
			OperationType::Replace => {
				let (start, end) = match op.line_range {
					LineRange::Range(s, e) => (s, e),
					LineRange::Single(line) => (line, line),
				};
				let content_lines: Vec<&str> = op.content.lines().collect();
				let removed: Vec<String> = original_lines[start - 1..end]
					.iter()
					.map(|l| l.to_string())
					.collect();

				let mut diff: Vec<String> = Vec::new();
				let ctx_before_start = start.saturating_sub(CONTEXT).max(1);
				if ctx_before_start > 1 {
					diff.push("...".to_string());
				}
				for i in ctx_before_start..start {
					diff.push(format!("{}: {}", orig_prefix(i), original_lines[i - 1]));
				}
				for (i, old_line) in removed.iter().enumerate() {
					diff.push(format!("-{}: {}", orig_prefix(start + i), old_line));
				}
				for (i, new_line) in content_lines.iter().enumerate() {
					let idx = start + i;
					let pfx = if idx <= new_lines.len() {
						new_prefix(idx)
					} else {
						format!("{}", idx)
					};
					diff.push(format!("+{}: {}", pfx, new_line));
				}
				let new_after_start = start + content_lines.len();
				let new_after_end = (new_after_start + CONTEXT - 1).min(new_lines.len());
				for new_i in new_after_start..=new_after_end {
					if new_i >= 1 && new_i <= new_lines.len() {
						diff.push(format!("{}: {}", new_prefix(new_i), new_lines[new_i - 1]));
					}
				}
				if new_after_end < new_lines.len() {
					diff.push("...".to_string());
				}
				diffs.push(diff.join("\n"));
			}
			OperationType::Insert => {
				// For inserts show context lines before and after so the AI can verify placement
				let after = match op.line_range {
					LineRange::Single(line) => line,
					LineRange::Range(start, _) => start,
				};
				let content_lines: Vec<&str> = op.content.lines().collect();
				let insert_at = after + 1; // new line numbers start here
				let mut diff: Vec<String> = Vec::new();

				// Context before: up to CONTEXT lines before the insertion point (in new file)
				let ctx_before_start = insert_at.saturating_sub(CONTEXT).max(1);
				if ctx_before_start > 1 {
					diff.push("...".to_string());
				}
				for new_i in ctx_before_start..insert_at {
					if new_i >= 1 && new_i <= new_lines.len() {
						diff.push(format!("{}: {}", new_prefix(new_i), new_lines[new_i - 1]));
					}
				}

				// The inserted lines
				for (i, new_line) in content_lines.iter().enumerate() {
					let idx = insert_at + i;
					let pfx = if idx <= new_lines.len() {
						new_prefix(idx)
					} else {
						format!("{}", idx)
					};
					diff.push(format!("+{}: {}", pfx, new_line));
				}

				// Context after: up to CONTEXT lines after the inserted block (in new file)
				let after_end = insert_at + content_lines.len();
				let ctx_after_end = (after_end + CONTEXT - 1).min(new_lines.len());
				for new_i in after_end..=ctx_after_end {
					if new_i >= 1 && new_i <= new_lines.len() {
						diff.push(format!("{}: {}", new_prefix(new_i), new_lines[new_i - 1]));
					}
				}
				if ctx_after_end < new_lines.len() {
					diff.push("...".to_string());
				}

				diffs.push(diff.join("\n"));
			}
		}
	}

	// The diff IS the result — plain text, same style as `view` output.
	// LLM reads it to verify edits landed correctly without needing a separate view call.
	let diff_output = diffs.join("\n---\n");

	Ok(diff_output)
}
