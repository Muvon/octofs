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
use super::remote::{
	io_exists, io_metadata, io_read_to_string, io_remove_file, io_write, PathSource,
};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
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
	if let Ok(canon) = path.canonicalize() {
		return canon.to_string_lossy().to_string();
	}
	// File may not exist yet (create) or has been removed (delete+undo).
	// Canonicalize the parent and append the file name so the key stays
	// consistent across the file's lifetime.
	if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
		if let Ok(canon_parent) = parent.canonicalize() {
			return canon_parent.join(name).to_string_lossy().to_string();
		}
	}
	path.to_string_lossy().to_string()
}

/// Build the lock-map key for a PathSource. For local paths, delegates to
/// `lock_key_for` (canonicalize when possible). For remote paths, uses
/// `host:port/path` to avoid cross-host collisions.
pub fn lock_key_for_source(source: &PathSource) -> String {
	match source {
		PathSource::Local(p) => lock_key_for(p),
		PathSource::Remote { .. } => source.lock_key(),
	}
}

// Sweep threshold for the lock map — far above any realistic in-flight file count.
const LOCK_MAP_SWEEP_THRESHOLD: usize = 1024;

// Acquire a file-specific lock to prevent concurrent writes to the same file
pub(crate) async fn acquire_file_lock(source: &PathSource) -> Result<Arc<AsyncMutex<()>>> {
	let key = lock_key_for_source(source);

	let file_lock = {
		let mut locks = get_file_locks().lock().expect("file locks poisoned");
		// Bound the map for long-lived sessions: an entry whose Arc strong_count is 1
		// is held ONLY by the map itself — we hold the map mutex, so no task can be
		// cloning it concurrently, hence nobody owns or awaits that lock. Dropping it
		// is safe; the next acquire of that path simply creates a fresh one.
		if locks.len() > LOCK_MAP_SWEEP_THRESHOLD {
			locks.retain(|_, lock| Arc::strong_count(lock) > 1);
		}
		locks
			.entry(key)
			.or_insert_with(|| Arc::new(AsyncMutex::new(())))
			.clone()
	};

	Ok(file_lock)
}

/// Test-only visibility into the lock map size (sweep behavior assertions).
#[cfg(test)]
pub(crate) fn file_lock_map_len() -> usize {
	get_file_locks().lock().expect("file locks poisoned").len()
}

/// Delete a file. Saves history first so `undo_edit` can restore it.
/// Refuses directories — use shell `rm -r` for that.
pub async fn delete_file_spec(source: &PathSource) -> Result<String> {
	if !io_exists(source).await? {
		bail!("File does not exist: {}", source.display());
	}
	let meta = io_metadata(source).await?;
	if meta.is_dir {
		bail!(
			"Path is a directory, not a file: {}. Use the shell tool with `rm -r` to remove directories.",
			source.display()
		);
	}

	let file_lock = acquire_file_lock(source).await?;
	let _lock_guard = file_lock.lock().await;

	// Snapshot for undo before unlinking.
	save_file_history(source).await?;

	io_remove_file(source)
		.await
		.map_err(|e| anyhow!("Failed to delete '{}': {}", source.display(), e))?;

	Ok(format!("Successfully deleted {}", source.display()))
}

// Line-id helpers are shared from utils::line_hash.
use crate::utils::line_hash::{line_id_at, verify_line_id};

// Batch operation structures for the new single-file, multi-operation approach
#[derive(Debug, Clone)]
struct BatchOperation {
	operation_type: OperationType,
	line_range: LineRange,
	content: String,
	operation_index: usize,
}

// Unresolved batch operation with endpoints not yet verified against the file
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
	/// Insert anchor without content to verify: 0 = file start, -1 = after last line.
	Anchor(i64),
	/// Insert after this line id ("N:hh", verified against the file).
	IdAnchor { line: usize, hash: String },
	/// Replace this id range (inclusive; both endpoints verified).
	IdRange {
		start: (usize, String),
		end: (usize, String),
	},
}

// Resolve an unresolved range to concrete line positions, verifying every line id
// against the current file content. A stale id fails with a self-healing error
// (fresh context + relocation candidates) built by `verify_line_id`.
fn resolve_unresolved_line_range(
	unresolved: &UnresolvedLineRange,
	total_lines: usize,
	lines: &[&str],
) -> Result<LineRange, String> {
	match unresolved {
		UnresolvedLineRange::Anchor(0) => Ok(LineRange::Single(0)),
		UnresolvedLineRange::Anchor(-1) => Ok(LineRange::Single(total_lines)),
		UnresolvedLineRange::Anchor(n) => Err(format!(
			"Invalid insert anchor {n}: use 0 (file start), -1 (after last line), or a line id like \"12:a3\" from view output"
		)),
		UnresolvedLineRange::IdAnchor { line, hash } => {
			let resolved = verify_line_id(*line, hash, lines)?;
			Ok(LineRange::Single(resolved))
		}
		UnresolvedLineRange::IdRange { start, end } => {
			// Verify both endpoints before failing: a range with two bad ids must
			// surface both, or the caller fixes one, retries, and only then sees
			// the other.
			let (s, e) = match (
				verify_line_id(start.0, &start.1, lines),
				verify_line_id(end.0, &end.1, lines),
			) {
				(Ok(s), Ok(e)) => (s, e),
				(s, e) => {
					let errs: Vec<String> = [s.err(), e.err()].into_iter().flatten().collect();
					return Err(errs.join("\n"));
				}
			};
			if s > e {
				return Err(format!(
					"Range is reversed: start \"{}:{}\" is after end \"{}:{}\". Did you mean start: \"{}:{}\", end: \"{}:{}\"?",
					start.0, start.1, end.0, end.1, end.0, end.1, start.0, start.1
				));
			}
			Ok(LineRange::Range(s, e))
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

/// Build a unified-style diff for a str_replace operation showing CONTEXT lines before/after.
/// Context and added lines carry FRESH line ids from the final file, so the model can
/// chain follow-up edits without re-viewing; removed lines carry their old ids.
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

	// Context before (identical in both files — render with final-file ids)
	let ctx_before_start = start.saturating_sub(CONTEXT);
	if ctx_before_start > 0 {
		diff.push("...".to_string());
	}
	for i in ctx_before_start..start {
		diff.push(format!("{}|{}", line_id_at(new_lines, i + 1), new_lines[i]));
	}

	// Removed lines (old ids)
	for (i, line) in orig_lines
		.iter()
		.enumerate()
		.skip(start)
		.take(old_line_count)
	{
		diff.push(format!("-{}|{}", line_id_at(orig_lines, i + 1), line));
	}

	// Added lines — at their final positions with fresh ids.
	// In the new file the inserted block starts at `start + 1` (1-indexed)
	for (i, line) in new_text_lines.iter().enumerate() {
		diff.push(format!(
			"+{}|{}",
			line_id_at(new_lines, start + 1 + i),
			line
		));
	}

	// Context after: read from new_lines (already has the replacement applied)
	let new_after_start = start + new_text_lines.len(); // 0-indexed in new_lines
	let ctx_after_end = (new_after_start + CONTEXT).min(new_lines.len());
	for i in new_after_start..ctx_after_end {
		diff.push(format!("{}|{}", line_id_at(new_lines, i + 1), new_lines[i]));
	}
	if ctx_after_end < new_lines.len() {
		diff.push("...".to_string());
	}

	diff.join("\n")
}

/// Atomic write: write to a temp file in the same directory, then rename over the target.
/// Guarantees the file is never in a partial/corrupt state if the process is interrupted.
/// Preserves the original file's permissions (including the executable bit) — without this,
/// the rename would replace the file with the temp file's default mode and silently strip
/// permission bits the user set deliberately.
///
/// For remote paths, falls back to direct write (SFTP has no atomic rename-to-temp in our
/// abstraction layer; the connection-level lock serializes concurrent writes anyway).
pub async fn atomic_write(source: &PathSource, content: &str) -> Result<()> {
	match source {
		PathSource::Local(path) => {
			let parent_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
			let tmp_path = parent_dir.join(format!(
				".octofs_tmp_{}.tmp",
				path.file_name().unwrap_or_default().to_string_lossy()
			));

			// Snapshot existing permissions before we overwrite, so we can re-apply them to the temp
			// file before the rename swaps inodes. None means the target didn't exist yet.
			let original_perms = match tokio::fs::metadata(path).await {
				Ok(meta) => Some(meta.permissions()),
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
				Err(e) => {
					return Err(anyhow!(
						"Failed to read metadata for '{}': {}",
						path.display(),
						e
					))
				}
			};

			tokio::fs::write(&tmp_path, content).await.map_err(|e| {
				anyhow!("Failed to write temp file for '{}': {}", path.display(), e)
			})?;

			if let Some(perms) = original_perms {
				if let Err(e) = tokio::fs::set_permissions(&tmp_path, perms).await {
					let _ = tokio::fs::remove_file(&tmp_path).await;
					return Err(anyhow!(
						"Failed to preserve permissions on '{}': {}",
						path.display(),
						e
					));
				}
			}

			if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
				// Clean up temp file on rename failure
				let _ = tokio::fs::remove_file(&tmp_path).await;
				return Err(anyhow!(
					"Failed to atomically replace '{}': {}",
					path.display(),
					e
				));
			}
			Ok(())
		}
		PathSource::Remote { .. } => {
			io_write(source, content.as_bytes()).await?;
			Ok(())
		}
	}
}
/// Restore CRLF line endings on the outgoing content when the original file used
/// them. All matching/replacement happens in LF space; without this, edited CRLF
/// files would be silently rewritten to LF.
pub(crate) fn restore_endings(uses_crlf: bool, s: String) -> String {
	if uses_crlf {
		s.replace('\n', "\r\n")
	} else {
		s
	}
}

/// Interpret double-escaped whitespace sequences (`\n`, `\t`, `\r` arriving as
/// backslash + letter) as the real characters. Recovery heuristic only — applied
/// when the raw form found no match and this form matches uniquely.
fn unescape_literals(s: &str) -> String {
	s.replace("\\n", "\n")
		.replace("\\t", "\t")
		.replace("\\r", "\r")
}

/// Apply a unique exact replacement and return the annotated diff.
/// Shared by stage 1 (exact match) and stage 1.5 (escaped-literal recovery).
async fn apply_unique_replacement(
	source: &PathSource,
	content: &str,
	old_text: &str,
	new_text: &str,
	uses_crlf: bool,
) -> Result<String> {
	let orig_lines: Vec<&str> = content.lines().collect();
	let old_line_count = old_text.lines().count();
	// Find the 0-indexed start line of the match
	let match_offset = content.find(old_text).unwrap_or(0);
	let match_start = byte_offset_to_line(content, match_offset) - 1;

	save_file_history(source).await?;
	let new_content = content.replace(old_text, new_text);
	atomic_write(source, &restore_endings(uses_crlf, new_content.clone())).await?;

	if old_line_count > 1 {
		crate::mcp::request_ctx::push_hint(&format!(
			"`str_replace` matched {} lines. Prefer `batch_edit` when you know the line ids — it's faster and avoids content-search ambiguity.",
			old_line_count
		));
	}

	let new_lines: Vec<&str> = new_content.lines().collect();
	let new_text_lines: Vec<&str> = new_text.lines().collect();
	Ok(build_str_replace_diff(
		&orig_lines,
		&new_lines,
		match_start,
		old_line_count,
		&new_text_lines,
	))
}

/// Replace every occurrence of `old_text` and report where the replacements
/// landed (fresh line ids in the final file).
async fn apply_replace_all(
	source: &PathSource,
	content: &str,
	old_text: &str,
	new_text: &str,
	uses_crlf: bool,
) -> Result<String> {
	let positions = find_all_positions(content, old_text);
	save_file_history(source).await?;
	let new_content = content.replace(old_text, new_text);
	atomic_write(source, &restore_endings(uses_crlf, new_content.clone())).await?;

	let new_lines: Vec<&str> = new_content.lines().collect();
	// Each occurrence shifts later ones by the length delta; map original offsets
	// to final-file offsets to name the landing line of every replacement.
	let delta = new_text.len() as i64 - old_text.len() as i64;
	let sites: Vec<String> = positions
		.iter()
		.enumerate()
		.map(|(i, &pos)| {
			let new_pos = (pos as i64 + delta * i as i64).max(0) as usize;
			let line = byte_offset_to_line(&new_content, new_pos.min(new_content.len()))
				.min(new_lines.len().max(1));
			if new_lines.is_empty() {
				format!("line {line}")
			} else {
				line_id_at(&new_lines, line)
			}
		})
		.collect();

	Ok(format!(
		"Replaced {} occurrences. Replacements start at: {}",
		positions.len(),
		sites.join(", ")
	))
}

// Replace a string in a file with progressive matching strategy:
// 1. Exact match (unique, or all occurrences with `replace_all`)
// 1.5. Escaped-literal recovery (double-escaped \n/\t interpreted, unique match)
// 2. Whitespace-normalized fuzzy match with indentation adjustment
// 3. Rich diagnostics with closest candidates on failure
// CRLF files are matched and edited in LF space; original endings are restored on write.
pub async fn str_replace_spec(
	source: &PathSource,
	old_text: &str,
	new_text: &str,
	replace_all: bool,
) -> Result<String> {
	if !io_exists(source).await? {
		bail!("File not found");
	}

	// Acquire file lock to prevent concurrent writes
	let file_lock = acquire_file_lock(source).await?;
	let _lock_guard = file_lock.lock().await;

	// Read the file content. Content-addressed matching is its own staleness check:
	// old_text either still matches uniquely or the error below explains why not.
	let raw = io_read_to_string(source)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;

	// Normalize CRLF for matching (and in the inputs, in case the model echoed
	// CRLF back); `restore_endings` puts the file's endings back on write.
	let uses_crlf = raw.contains("\r\n");
	let content = if uses_crlf {
		raw.replace("\r\n", "\n")
	} else {
		raw
	};
	let old_text = old_text.replace("\r\n", "\n");
	let new_text = new_text.replace("\r\n", "\n");
	let old_text = old_text.as_str();
	let new_text = new_text.as_str();

	// === Stage 1: Exact match ===
	let occurrences = content.matches(old_text).count();

	if replace_all && occurrences >= 1 {
		return apply_replace_all(source, &content, old_text, new_text, uses_crlf).await;
	}

	if occurrences == 1 {
		return apply_unique_replacement(source, &content, old_text, new_text, uses_crlf).await;
	}

	if occurrences > 1 {
		// Multiple exact matches — show locations with line ids so the model can
		// switch to `batch_edit` without another view.
		let positions = find_all_positions(&content, old_text);
		let file_lines: Vec<&str> = content.lines().collect();
		let locations: Vec<String> = positions
			.iter()
			.enumerate()
			.map(|(i, &offset)| {
				let line = byte_offset_to_line(&content, offset);
				format!("  {}. {}", i + 1, line_id_at(&file_lines, line))
			})
			.collect();

		bail!(
			"Found {} matches for replacement text at:\n{}\nAdd more surrounding context to make a unique match, pass `replace_all: true` to replace all {} occurrences, or use `batch_edit` with the specific line ids.",
			occurrences,
			locations.join("\n"),
			occurrences
		);
	}

	// === Stage 1.5: Escaped-literal recovery ===
	// A double-escaped old_text (JSON "\\n" arriving as backslash-n) never matches.
	// If interpreting the escapes yields a match, use it instead of bouncing an
	// error back — new_text gets the same interpretation for consistency.
	if old_text.contains("\\n") || old_text.contains("\\t") || old_text.contains("\\r") {
		let un_old = unescape_literals(old_text);
		if un_old != old_text {
			let un_occurrences = content.matches(&un_old).count();
			let un_new = unescape_literals(new_text);
			if replace_all && un_occurrences >= 1 {
				crate::mcp::request_ctx::push_hint(
					"old_text contained literal \\n/\\t escapes; they were interpreted as real newlines/tabs.",
				);
				return apply_replace_all(source, &content, &un_old, &un_new, uses_crlf).await;
			}
			if un_occurrences == 1 {
				crate::mcp::request_ctx::push_hint(
					"old_text contained literal \\n/\\t escapes; they were interpreted as real newlines/tabs (unique match).",
				);
				return apply_unique_replacement(source, &content, &un_old, &un_new, uses_crlf)
					.await;
			}
		}
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

			save_file_history(source).await?;
			let new_content = content.replace(&actual_old, &adjusted_new);
			atomic_write(source, &restore_endings(uses_crlf, new_content.clone())).await?;

			crate::mcp::request_ctx::push_hint(
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
		let diag_lines: Vec<&str> = content.lines().collect();

		msg.push_str("Closest matches:\n");
		let old_line_count = old_text.lines().count();
		for (i, (line_num, window, sim)) in candidates.iter().enumerate() {
			let diagnosis = diagnose_mismatch(old_text, window);
			let end_line = line_num + old_line_count - 1;
			msg.push_str(&format!(
				"\n  {}. {} .. {} ({:.0}% similar, {})\n",
				i + 1,
				line_id_at(&diag_lines, *line_num),
				line_id_at(&diag_lines, end_line),
				sim * 100.0,
				diagnosis
			));
			// Show first 3 lines of the candidate as preview
			for (j, line) in window.lines().take(3).enumerate() {
				msg.push_str(&format!(
					"     {}|{}\n",
					line_id_at(&diag_lines, line_num + j),
					line
				));
			}
			if old_line_count > 3 {
				msg.push_str(&format!("     ... ({} more lines)\n", old_line_count - 3));
			}
		}
		msg.push_str(
			"\nTip: use `batch_edit` with the line ids shown above, or fix the `old_text` content.",
		);
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
) -> Result<(), String> {
	if content_lines.is_empty() {
		return Ok(());
	}

	let id = |line_1idx: usize| -> String { line_id_at(file_lines, line_1idx) };
	let range_id = |s: usize, e: usize| -> String { format!("\"{}\"-\"{}\"", id(s), id(e)) };

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
				id(start_line - 1),
				range_id(start_line, end_line),
				id(start_line - 1),
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
				id(end_line + 1),
				range_id(start_line, end_line),
				id(end_line + 1),
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
fn detect_conflicts(operations: &[BatchOperation], file_lines: &[&str]) -> Result<(), String> {
	let id = |line_1idx: usize| -> String {
		// Anchor 0 means "file start" — there is no line to identify, and `0 - 1`
		// would underflow the usize index.
		if line_1idx == 0 {
			"the start of the file".to_string()
		} else {
			line_id_at(file_lines, line_1idx)
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
							"Conflicting operations: operation {} (replace \"{}-{}\") and {} (replace \"{}-{}\") have overlapping ranges",
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

// Parse an operation's `start` (+ optional `end`) into an unresolved line range.
//   insert:  start is the anchor — a line id "N:hh", or the integers 0 (file start) /
//            -1 (after last line). Other plain numbers are rejected: they carry no
//            content hash, so a stale position could not be detected.
//   replace: start..end line ids (end omitted → single line). Ids are mandatory —
//            every replaced boundary is verified against the file before applying.
fn parse_line_range(
	start_val: &Value,
	end_val: Option<&Value>,
	operation_type: &OperationType,
) -> Result<UnresolvedLineRange, String> {
	use crate::utils::line_hash::{parse_endpoint, Endpoint};

	let start = parse_endpoint(start_val).map_err(|e| format!("invalid `start`: {e}"))?;

	match operation_type {
		OperationType::Insert => match start {
			Endpoint::Number(n @ (0 | -1)) => Ok(UnresolvedLineRange::Anchor(n)),
			Endpoint::Number(n) => Err(format!(
				"insert anchor {n} is not verifiable: use a line id like \"12:a3\" from view output, or 0 (file start) / -1 (after last line)"
			)),
			Endpoint::Id { line, hash } => Ok(UnresolvedLineRange::IdAnchor { line, hash }),
		},
		OperationType::Replace => {
			let end = match end_val {
				Some(v) => parse_endpoint(v).map_err(|e| format!("invalid `end`: {e}"))?,
				None => start.clone(), // single-line replace
			};
			match (start, end) {
				(Endpoint::Id { line: sl, hash: sh }, Endpoint::Id { line: el, hash: eh }) => {
					Ok(UnresolvedLineRange::IdRange {
						start: (sl, sh),
						end: (el, eh),
					})
				}
				_ => Err(
					"replace targets must be line ids like \"12:a3\" copied from view output — plain line numbers are not verified against the file"
						.to_string(),
				),
			}
		}
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

	let source = super::remote::resolve_path_source(path_str, &call.workdir);
	// Check if file exists
	if !io_exists(&source).await? {
		bail!("File not found: {}", path_str);
	}

	// Acquire file lock to prevent concurrent writes
	let file_lock = acquire_file_lock(&source).await?;
	let _lock_guard = file_lock.lock().await;

	// Read original file content. Line splitting/hashing strips `\r` everywhere, so
	// ids and diffs are ending-agnostic; `uses_crlf` restores the endings on write.
	let original_content = io_read_to_string(&source)
		.await
		.map_err(|e| anyhow!("Failed to read file '{}': {}", path_str, e))?;
	let uses_crlf = original_content.contains("\r\n");

	// Parse and validate all operations (with unresolved line ranges)
	let mut unresolved_operations = Vec::new();
	let mut parse_failures: Vec<String> = Vec::new();

	for (index, operation) in operations.iter().enumerate() {
		let operation_obj = match operation.as_object() {
			Some(obj) => obj,
			None => {
				parse_failures.push(format!("  op {index}: Operation must be an object"));
				continue;
			}
		};

		// Extract operation type
		let op_type_str = match operation_obj.get("operation").and_then(|v| v.as_str()) {
			Some(op) => op,
			None => {
				parse_failures.push(format!("  op {index}: Missing 'operation' field"));
				continue;
			}
		};

		// Parse operation type
		let operation_type = match op_type_str {
			"insert" => OperationType::Insert,
			"replace" => OperationType::Replace,
			_ => {
				parse_failures.push(format!(
					"  op {index}: Unsupported operation type: '{op_type_str}'. Supported operations: insert, replace"
				));
				continue;
			}
		};

		// Extract start (required) and optional end endpoints. JSON null counts as
		// omitted: the server round-trips ops through the typed struct, which
		// re-serializes an absent `end` as null (and clients send explicit nulls too).
		let line_range = match operation_obj.get("start").filter(|v| !v.is_null()) {
			Some(start_value) => {
				match parse_line_range(
					start_value,
					operation_obj.get("end").filter(|v| !v.is_null()),
					&operation_type,
				) {
					Ok(range) => range,
					Err(e) => {
						parse_failures.push(format!("  op {index}: Invalid line target: {e}"));
						continue;
					}
				}
			}
			None => {
				parse_failures.push(format!("  op {index}: Missing 'start' field"));
				continue;
			}
		};

		// Extract content
		let content = match operation_obj.get("content").and_then(|v| v.as_str()) {
			Some(c) => c.to_string(),
			None => {
				parse_failures.push(format!("  op {index}: Missing 'content' field"));
				continue;
			}
		};

		unresolved_operations.push(UnresolvedBatchOperation {
			operation_type,
			line_range,
			content,
			operation_index: index,
		});
	}

	// Atomic contract: if ANY operation is malformed, apply nothing. Silently dropping
	// a bad op while applying the rest would half-execute the caller's intent.
	if !parse_failures.is_empty() {
		bail!(
			"No operations were applied — {} of {} operations failed during parsing:\n{}",
			parse_failures.len(),
			operations.len(),
			parse_failures.join("\n")
		);
	}

	// Resolve anchors and verify every line id against the current file content.
	// Atomic contract mirrors parsing: if ANY id is stale, apply nothing and report
	// all failures at once — each failure carries the fresh context to retarget from.
	let total_lines = original_content.lines().count();
	let original_lines: Vec<&str> = original_content.lines().collect();
	let mut batch_operations = Vec::new();
	let mut resolve_failures: Vec<String> = Vec::new();

	for unresolved_op in unresolved_operations {
		match resolve_unresolved_line_range(&unresolved_op.line_range, total_lines, &original_lines)
		{
			Ok(resolved_range) => {
				batch_operations.push(BatchOperation {
					operation_type: unresolved_op.operation_type,
					line_range: resolved_range,
					content: unresolved_op.content,
					operation_index: unresolved_op.operation_index,
				});
			}
			Err(err) => {
				resolve_failures.push(format!("op {}: {}", unresolved_op.operation_index, err));
			}
		}
	}

	if !resolve_failures.is_empty() {
		bail!(
			"No operations were applied to {path_str} — {} of {} operations have invalid or stale targets:\n{}",
			resolve_failures.len(),
			operations.len(),
			resolve_failures.join("\n---\n")
		);
	}

	// Check for conflicts between operations
	if let Err(conflict_error) = detect_conflicts(&batch_operations, &original_lines) {
		bail!("{}", conflict_error);
	}

	// Duplicate-line detection: validate operations against original content before applying.
	// Catches the #1 AI mistake of including surrounding/already-existing lines.
	let orig_line_id = |line_1idx: usize| -> String {
		// Anchor 0 means "file start" — no line to identify, and `0 - 1` underflows.
		if line_1idx == 0 {
			"the start of the file".to_string()
		} else {
			line_id_at(&original_lines, line_1idx)
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

	// Apply all operations to the original content (LF-joined by construction)
	let final_content = apply_batch_operations(&original_content, &batch_operations)
		.await
		.map_err(|e| anyhow!("Failed to apply operations: {}", e))?;

	// Save file history for undo functionality
	save_file_history(&source).await?;

	atomic_write(&source, &restore_endings(uses_crlf, final_content.clone()))
		.await
		.map_err(|e| anyhow!("Atomic write failed for '{}': {}", path_str, e))?;

	// Build annotated diff for each operation so the AI can verify edits landed correctly.
	// Context and added lines carry FRESH ids from the final file — usable directly as
	// targets for follow-up edits, no re-view needed. Removed lines carry their old ids.
	const CONTEXT: usize = 2;
	let new_lines: Vec<&str> = final_content.lines().collect();

	let orig_prefix = |line_1idx: usize| -> String { line_id_at(&original_lines, line_1idx) };
	let new_prefix = |line_1idx: usize| -> String { line_id_at(&new_lines, line_1idx) };

	let mut diffs: Vec<String> = Vec::new();

	// Sort ops by original start line (ascending) for readable diff output
	let mut display_ops = batch_operations.clone();
	display_ops.sort_by_key(|op| match &op.line_range {
		LineRange::Single(line) => *line,
		LineRange::Range(start, _) => *start,
	});

	// Each op references ORIGINAL line numbers, but the diff is rendered against the FINAL
	// file (`new_lines`/`new_prefix`). Walking ops ascending and accumulating each op's
	// line-count delta keeps every later op's rendered positions (and hash prefixes) aligned
	// with where its content actually landed — without this, every op after the first
	// length-changing one shows wrong line numbers/hashes.
	// ponytail: assumes non-overlapping ops (guaranteed by detect_conflicts for replaces and
	// equal-anchor inserts); an insert anchored strictly inside a replaced range renders
	// approximately. The on-disk write is always correct regardless.
	let mut offset: i64 = 0;

	// 1-indexed position in the final file = original position + offset of prior ops.
	let shift =
		|orig_1idx: usize, offset: i64| -> usize { (orig_1idx as i64 + offset).max(1) as usize };
	// Render context line `new_i` from the final file, if in range.
	let ctx_line = |new_i: usize| -> Option<String> {
		(new_i >= 1 && new_i <= new_lines.len())
			.then(|| format!("{}|{}", new_prefix(new_i), new_lines[new_i - 1]))
	};

	for op in &display_ops {
		match op.operation_type {
			OperationType::Replace => {
				let (start, end) = match op.line_range {
					LineRange::Range(s, e) => (s, e),
					LineRange::Single(line) => (line, line),
				};
				let content_lines: Vec<&str> = op.content.lines().collect();
				let old_count = end - start + 1;
				let new_count = content_lines.len();
				let new_start = shift(start, offset);

				let mut diff: Vec<String> = Vec::new();
				// Context before — from the final file, at shifted positions.
				let ctx_before_start = new_start.saturating_sub(CONTEXT).max(1);
				if ctx_before_start > 1 {
					diff.push("...".to_string());
				}
				for new_i in ctx_before_start..new_start {
					diff.extend(ctx_line(new_i));
				}
				// Removed lines — original content at ORIGINAL coordinates.
				for (i, old_line) in original_lines[start - 1..end].iter().enumerate() {
					diff.push(format!("-{}|{}", orig_prefix(start + i), old_line));
				}
				// Added lines — at their FINAL positions, ids computed from the
				// added content itself (always correct, no bounds concern).
				for (i, new_line) in content_lines.iter().enumerate() {
					let idx = new_start + i;
					diff.push(format!(
						"+{}|{}",
						crate::utils::line_hash::line_id(idx, new_line),
						new_line
					));
				}
				// Context after — from the final file.
				let new_after_start = new_start + new_count;
				let new_after_end = (new_after_start + CONTEXT - 1).min(new_lines.len());
				for new_i in new_after_start..=new_after_end {
					diff.extend(ctx_line(new_i));
				}
				if new_after_end < new_lines.len() {
					diff.push("...".to_string());
				}
				diffs.push(diff.join("\n"));
				offset += new_count as i64 - old_count as i64;
			}
			OperationType::Insert => {
				// For inserts show context lines before and after so the AI can verify placement
				let after = match op.line_range {
					LineRange::Single(line) => line,
					LineRange::Range(start, _) => start,
				};
				let content_lines: Vec<&str> = op.content.lines().collect();
				// First inserted line's position in the final file.
				let insert_at = shift(after, offset) + if after == 0 { 0 } else { 1 };
				let mut diff: Vec<String> = Vec::new();

				let ctx_before_start = insert_at.saturating_sub(CONTEXT).max(1);
				if ctx_before_start > 1 {
					diff.push("...".to_string());
				}
				for new_i in ctx_before_start..insert_at {
					diff.extend(ctx_line(new_i));
				}

				for (i, new_line) in content_lines.iter().enumerate() {
					let idx = insert_at + i;
					diff.push(format!(
						"+{}|{}",
						crate::utils::line_hash::line_id(idx, new_line),
						new_line
					));
				}

				let after_end = insert_at + content_lines.len();
				let ctx_after_end = (after_end + CONTEXT - 1).min(new_lines.len());
				for new_i in after_end..=ctx_after_end {
					diff.extend(ctx_line(new_i));
				}
				if ctx_after_end < new_lines.len() {
					diff.push("...".to_string());
				}

				diffs.push(diff.join("\n"));
				offset += content_lines.len() as i64;
			}
		}
	}

	// The diff IS the result — plain text, same style as `view` output.
	// LLM reads it to verify edits landed correctly without needing a separate view call.
	let diff_output = diffs.join("\n---\n");

	Ok(diff_output)
}
