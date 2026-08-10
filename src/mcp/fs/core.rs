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

// Core functionality and shared utilities for file system operations

use super::super::McpToolCall;
use super::remote::{
	io_create_dir_all, io_exists, io_is_dir, io_metadata, io_read_to_string, resolve_path_source,
	PathSource,
};
use crate::mcp::fs::{directory, file_ops, text_editing};
use crate::utils::line_hash::{self, Endpoint};
use crate::utils::truncation::format_extracted_content_smart;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Resolve a path relative to the session working directory
/// If the path is absolute, returns it as-is
/// If the path is relative, resolves it relative to the session working directory
pub fn resolve_path(path_str: &str, workdir: &Path) -> std::path::PathBuf {
	let path = Path::new(path_str);
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		workdir.join(path)
	}
}

// Line-index resolution (negative indexing, clamping) lives in utils::line_hash so it is
// shared with text_editing — see line_hash::resolve_line_index / resolve_line_index_clamped.
use crate::utils::line_hash::{resolve_line_index, resolve_line_index_clamped};

// Undo history: per-file snapshots with a global byte budget so a long-lived
// server (HTTP mode, days of edits) cannot grow without bound.
const MAX_HISTORY_PER_FILE: usize = 10;
const MAX_HISTORY_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct HistoryStore {
	files: HashMap<String, Vec<String>>,
	// One entry per snapshot push, oldest first. May reference paths whose oldest
	// snapshot is already gone (undo pops, per-file cap) — eviction skips those.
	order: std::collections::VecDeque<String>,
	total_bytes: usize,
}

impl HistoryStore {
	fn push(&mut self, key: String, content: String, byte_cap: usize) {
		self.total_bytes += content.len();
		let history = self.files.entry(key.clone()).or_default();
		if history.len() >= MAX_HISTORY_PER_FILE {
			let removed = history.remove(0);
			self.total_bytes -= removed.len();
		}
		history.push(content);
		self.order.push_back(key);

		// Over budget: drop the globally oldest snapshots. `len() > 1` keeps the
		// snapshot just pushed even if it alone exceeds the cap — evicting it
		// would silently break undo for the file being edited right now.
		while self.total_bytes > byte_cap && self.order.len() > 1 {
			let Some(oldest) = self.order.pop_front() else {
				break;
			};
			let Some(history) = self.files.get_mut(&oldest) else {
				continue;
			};
			if history.is_empty() {
				self.files.remove(&oldest);
				continue;
			}
			let removed = history.remove(0);
			self.total_bytes -= removed.len();
			if history.is_empty() {
				self.files.remove(&oldest);
			}
		}
	}

	fn pop(&mut self, key: &str) -> Option<String> {
		let history = self.files.get_mut(key)?;
		let content = history.pop()?;
		self.total_bytes -= content.len();
		if history.is_empty() {
			self.files.remove(key);
		}
		Some(content)
	}
}

static FILE_HISTORY: OnceLock<Mutex<HistoryStore>> = OnceLock::new();

fn file_history() -> &'static Mutex<HistoryStore> {
	FILE_HISTORY.get_or_init(|| Mutex::new(HistoryStore::default()))
}

// Save the current content of a file for undo
pub async fn save_file_history(source: &PathSource) -> Result<()> {
	if io_exists(source).await? {
		let content = io_read_to_string(source).await?;
		// Use the same canonicalized key as the file lock map so undo history
		// follows the file even when callers pass aliased paths.
		let path_str = text_editing::lock_key_for_source(source);
		file_history()
			.lock()
			.map_err(|_| anyhow!("Failed to acquire lock on file history"))?
			.push(path_str, content, MAX_HISTORY_TOTAL_BYTES);
	}
	Ok(())
}

// Undo the last edit to a file
pub async fn undo_edit(source: &PathSource) -> Result<String> {
	// Serialize with other writers on the same file — without this, an undo racing a
	// concurrent str_replace/batch_edit could interleave the pop and write and lose data.
	let file_lock = text_editing::acquire_file_lock(source).await?;
	let _lock_guard = file_lock.lock().await;

	let path_str = text_editing::lock_key_for_source(source);
	let previous_content = file_history()
		.lock()
		.map_err(|_| anyhow!("Failed to acquire lock on file history"))?
		.pop(&path_str);

	if let Some(prev_content) = previous_content {
		text_editing::atomic_write(source, &prev_content).await?;

		Ok(format!(
			"Successfully undid the last edit to {}",
			source.display()
		))
	} else {
		bail!("No more undo history for this file (up to 10 levels are stored per file).");
	}
}

// Main execution functions

// Execute a text editor command following modern text editor specifications
pub async fn execute_text_editor(call: &McpToolCall) -> Result<String> {
	// Extract command parameter
	let command = match call.parameters.get("command") {
		Some(Value::String(cmd)) => cmd.clone(),
		Some(_) => {
			bail!("Command parameter must be a string");
		}
		None => {
			bail!("Missing required 'command' parameter");
		}
	};

	// Execute the appropriate command with cancellation checks
	match command.as_str() {
		"create" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for create command");
				}
			};
			let content = match call.parameters.get("content") {
				Some(Value::String(txt)) => txt.clone(),
				_ => {
					bail!("Missing or invalid 'content' parameter for create command");
				}
			};
			let source = resolve_path_source(&path, &call.workdir);
			file_ops::create_file_spec(&source, &content).await
		}
		"str_replace" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for str_replace command");
				}
			};
			let old_text = match call.parameters.get("old_text") {
				Some(Value::String(s)) => s.clone(),
				_ => {
					bail!("Missing or invalid 'old_text' parameter");
				}
			};
			let new_text = match call.parameters.get("new_text") {
				Some(Value::String(s)) => s.clone(),
				_ => {
					bail!("Missing or invalid 'new_text' parameter");
				}
			};
			let source = resolve_path_source(&path, &call.workdir);
			text_editing::str_replace_spec(&source, &old_text, &new_text).await
		}
		"undo_edit" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for undo_edit command");
				}
			};
			undo_edit(&resolve_path_source(&path, &call.workdir)).await
		}
		"delete" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for delete command");
				}
			};
			text_editing::delete_file_spec(&resolve_path_source(&path, &call.workdir)).await
		}
		_ => bail!(
			"Invalid command: {command}. Allowed commands are: create, str_replace, delete, undo_edit"
		),
	}
}

/// Parse an optional `start`/`end` endpoint parameter from the tool call.
fn parse_optional_endpoint(value: Option<&Value>, name: &str) -> Result<Option<Endpoint>> {
	match value {
		Some(v) if !v.is_null() => Ok(Some(
			line_hash::parse_endpoint(v).map_err(|e| anyhow!("Invalid '{name}': {e}"))?,
		)),
		_ => Ok(None),
	}
}

/// Resolve one endpoint to a 1-indexed line for viewing. Both numbers and line ids
/// clamp to file bounds (setting `clamped`) — a view is self-correcting because it
/// returns fresh ids, so a stale id's position part is still the best answer.
fn resolve_endpoint_to_line(
	ep: &Endpoint,
	total_lines: usize,
	clamped: &mut bool,
) -> Result<usize> {
	let n = match ep {
		Endpoint::Number(n) => *n,
		Endpoint::Id { line, .. } => *line as i64,
	};
	let (line, was) = resolve_line_index_clamped(n, total_lines)
		.map_err(|e| anyhow!("Invalid lines parameter: {e}"))?;
	*clamped |= was;
	Ok(line)
}

/// Resolve a `view` line range from optional start/end endpoints into a clamped
/// `(start, end)` 1-indexed tuple. Returns None when both endpoints are absent
/// (whole file). Missing `start` defaults to line 1, missing `end` to the last line.
async fn resolve_view_range(
	start_ep: Option<Endpoint>,
	end_ep: Option<Endpoint>,
	source: &PathSource,
) -> Result<Option<(usize, i64)>> {
	if start_ep.is_none() && end_ep.is_none() {
		return Ok(None);
	}

	// Same limit view_file_spec enforces — check before loading so a huge file isn't
	// read fully here just to resolve a range and then be rejected downstream.
	if let Ok(meta) = io_metadata(source).await {
		if meta.size > file_ops::MAX_VIEW_FILE_BYTES {
			bail!("File is too large (>5MB)");
		}
	}

	let total_lines = io_read_to_string(source)
		.await
		.map(|c| c.lines().count())
		.unwrap_or(0);

	if total_lines == 0 {
		// Empty/unreadable file: there is nothing to clamp a range against, so render the
		// (empty) whole file rather than erroring on an out-of-bounds line.
		return Ok(None);
	}

	let start_ep = start_ep.unwrap_or(Endpoint::Number(1));
	let end_ep = end_ep.unwrap_or(Endpoint::Number(-1));

	let mut clamped = false;
	let start = resolve_endpoint_to_line(&start_ep, total_lines, &mut clamped)?;
	let end = resolve_endpoint_to_line(&end_ep, total_lines, &mut clamped)?;
	if start > end {
		bail!("Invalid lines parameter: start line {start} is after end line {end}");
	}
	if clamped {
		crate::mcp::request_ctx::push_hint(&format!(
			"Requested line range was out of bounds for a {total_lines}-line file; clamped to [{start}, {end}]. Use line numbers within 1..={total_lines} (negative indices count from the end)."
		));
	}
	Ok(Some((start, end as i64)))
}

// Execute view command - unified read-only tool for a single file, directory, or content search.
// To view multiple files, the caller makes multiple `view` calls (they run in parallel).
pub async fn execute_view(call: &McpToolCall) -> Result<String> {
	// Single path (the common case). An array is rejected with a pointer to parallel calls.
	let path = match call.parameters.get("path") {
		Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
		Some(Value::Array(_)) => bail!(
			"`path` must be a single path string. To view multiple files, make separate `view` calls — they run in parallel."
		),
		_ => bail!(
			"Missing or invalid 'path' parameter. Pass a single path string, e.g. \"src/main.rs\"."
		),
	};

	let source = resolve_path_source(&path, &call.workdir);

	// Name the path and where it resolved to — a bare "File not found" hides
	// workdir mismatches (relative path resolved against an unexpected root).
	if !io_exists(&source).await? {
		bail!(
			"Path not found: {} (resolved to {})",
			path,
			source.display()
		);
	}

	// Directory: dispatch directly with the path string
	if io_is_dir(&source).await? {
		return directory::list_directory(call, &path).await;
	}

	// File + content: search the file for a literal/regex pattern and render with the same hash/number format
	if let Some(content_pattern) = call.parameters.get("content").and_then(|v| v.as_str()) {
		if !content_pattern.trim().is_empty() {
			let context_lines = call
				.parameters
				.get("context")
				.and_then(|v| v.as_i64())
				.unwrap_or(0) as usize;
			let regex_flag = call
				.parameters
				.get("regex")
				.and_then(|v| v.as_bool())
				.unwrap_or(false);
			return file_ops::view_file_with_content_search(
				&source,
				content_pattern,
				context_lines,
				regex_flag,
			)
			.await;
		}
	}

	// File: optional start/end line range (both omitted → whole file).
	let start_ep = parse_optional_endpoint(call.parameters.get("start"), "start")?;
	let end_ep = parse_optional_endpoint(call.parameters.get("end"), "end")?;
	let range = resolve_view_range(start_ep, end_ep, &source).await?;
	file_ops::view_file_spec(&source, range).await
}

// Execute extract_lines command - MCP compliant implementation
pub async fn execute_extract_lines(call: &McpToolCall) -> Result<String> {
	// Validate and extract from_path parameter
	let from_path = match call.parameters.get("from_path") {
		Some(Value::String(p)) => {
			if p.trim().is_empty() {
				bail!("Parameter 'from_path' cannot be empty");
			}
			p.clone()
		}
		Some(_) => {
			bail!("Parameter 'from_path' must be a string");
		}
		None => {
			bail!("Missing required parameter 'from_path'");
		}
	};

	// Validate and extract from_start / from_end endpoints (line number or line id).
	// from_end omitted → single line (defaults to from_start). Resolution is deferred
	// until after the source file is read.
	let from_start_ep = match call.parameters.get("from_start") {
		Some(v) if !v.is_null() => {
			line_hash::parse_endpoint(v).map_err(|e| anyhow!("Invalid 'from_start': {e}"))?
		}
		_ => bail!("Missing required parameter 'from_start' (line number or line id)"),
	};
	let from_end_ep = parse_optional_endpoint(call.parameters.get("from_end"), "from_end")?
		.unwrap_or_else(|| from_start_ep.clone());

	// Validate and extract append_path parameter
	let append_path = match call.parameters.get("append_path") {
		Some(Value::String(p)) => {
			if p.trim().is_empty() {
				bail!("Parameter 'append_path' cannot be empty");
			}
			p.clone()
		}
		Some(_) => {
			bail!("Parameter 'append_path' must be a string");
		}
		None => {
			bail!("Missing required parameter 'append_path'");
		}
	};

	// Validate and extract append_line parameter: 0 (start), -1 (end), N (after line N),
	// or a line id. Id verification is deferred until after the target file is read.
	let append_line_ep = match call.parameters.get("append_line") {
		Some(v) if !v.is_null() => {
			line_hash::parse_endpoint(v).map_err(|e| anyhow!("Invalid 'append_line': {e}"))?
		}
		_ => {
			bail!("Missing required parameter 'append_line' (0 = start, -1 = end, N, or a line id)")
		}
	};

	// Read source file
	let from_source = resolve_path_source(&from_path, &call.workdir);
	if !io_exists(&from_source).await? {
		bail!("Source file does not exist: {from_path}");
	}

	let source_content = match io_read_to_string(&from_source).await {
		Ok(content) => content,
		Err(e) => {
			bail!("Failed to read source file '{from_path}': {e}");
		}
	};

	// Split content into lines and resolve negative indices
	let source_lines: Vec<&str> = source_content.lines().collect();
	let total_lines = source_lines.len();

	// Resolve from_start / from_end endpoints to 1-indexed line numbers (strict — no
	// clamping). Line ids are verified against the source content; a stale id fails
	// with the current content instead of silently copying the wrong lines.
	let resolve_extract_endpoint = |ep: &Endpoint, which: &str| -> Result<usize> {
		match ep {
			Endpoint::Number(n) => resolve_line_index(*n, total_lines)
				.map_err(|e| anyhow!("Invalid from_{which}: {e}")),
			Endpoint::Id { line, hash } => line_hash::verify_line_id(*line, hash, &source_lines)
				.map_err(|e| anyhow!("Invalid from_{which}: {e}")),
		}
	};
	let from_start = resolve_extract_endpoint(&from_start_ep, "start")?;
	let from_end = resolve_extract_endpoint(&from_end_ep, "end")?;
	if from_start > from_end {
		bail!(
			"from_start (line {from_start}) is after from_end (line {from_end}) — the range must go forward"
		);
	}
	let from_range = (from_start, from_end);

	// Extract the specified lines (convert to 0-indexed)
	let extracted_lines: Vec<&str> = source_lines[(from_range.0 - 1)..from_range.1].to_vec();

	// Confirmation echo with SOURCE-position line ids (content hashes stay valid
	// wherever the lines land in the target).
	let extracted_content_display = format_extracted_content_smart(
		&extracted_lines,
		from_range.0, // Start line number (1-indexed)
		// Deliberate 30-line cap: this is a confirmation echo, not data transfer —
		// the model chose this exact content, the marker preserves the hidden count,
		// and the full result is on disk behind `view`.
		Some(30),
	);

	// Preserve original newline structure by checking if source content ends with newline
	// and if we're extracting the last line (for file writing purposes)
	let source_ends_with_newline = source_content.ends_with('\n');
	let extracting_last_line = from_range.1 == total_lines;

	let extracted_content =
		if extracted_lines.len() == 1 && extracting_last_line && !source_ends_with_newline {
			// Single line extraction from end of file without trailing newline
			extracted_lines[0].to_string()
		} else if extracting_last_line && source_ends_with_newline {
			// Extracting from end and source has trailing newline - preserve it
			format!("{}\n", extracted_lines.join("\n"))
		} else {
			// Normal case - join lines with newlines
			extracted_lines.join("\n")
		};

	// Handle target file - create parent directories if needed
	let append_source = resolve_path_source(&append_path, &call.workdir);
	if let Some(parent_source) = append_source.parent() {
		if let Err(e) = io_create_dir_all(&parent_source).await {
			bail!("Failed to create parent directories for '{append_path}': {e}");
		}
	}

	// extract_lines modifies the target just like text_editor/batch_edit do — take the
	// same per-file lock so concurrent writers serialize instead of clobbering each other.
	let file_lock = text_editing::acquire_file_lock(&append_source).await?;
	let _lock_guard = file_lock.lock().await;

	// Read existing target file content or create empty if doesn't exist
	let target_content = if io_exists(&append_source).await? {
		match io_read_to_string(&append_source).await {
			Ok(content) => content,
			Err(e) => {
				bail!("Failed to read target file '{append_path}': {e}");
			}
		}
	} else {
		String::new()
	};

	// Resolve append_line: line id → verified line number, or keep integer as-is (0/-1/N).
	let append_line: i64 =
		match append_line_ep {
			Endpoint::Number(n) => {
				if n < -1 {
					bail!(
					"Invalid append_line {n}: use 0 (beginning), -1 (end), N (after line N), or a line id"
				);
				}
				n
			}
			Endpoint::Id { line, hash } => {
				if target_content.is_empty() {
					bail!("Cannot use a line id for append_line on an empty or non-existent target file");
				}
				let target_lines: Vec<&str> = target_content.lines().collect();
				let line = line_hash::verify_line_id(line, &hash, &target_lines)
					.map_err(|e| anyhow::anyhow!("Invalid append_line: {e}"))?;
				line as i64
			}
		};

	// Determine insertion logic based on append_line
	let final_content = if append_line == 0 {
		// Insert at beginning
		if target_content.is_empty() {
			extracted_content.clone()
		} else {
			// Check if extracted content already ends with newline
			if extracted_content.ends_with('\n') {
				format!("{extracted_content}{target_content}")
			} else {
				format!("{extracted_content}\n{target_content}")
			}
		}
	} else if append_line == -1 {
		// Append at end
		if target_content.is_empty() {
			extracted_content.clone()
		} else if target_content.ends_with('\n') {
			format!("{target_content}{extracted_content}")
		} else {
			format!("{target_content}\n{extracted_content}")
		}
	} else {
		// Insert after specific line
		let target_lines: Vec<&str> = target_content.lines().collect();
		let insert_after = append_line as usize;

		if insert_after > target_lines.len() {
			bail!(
				"Insert position {insert_after} exceeds target file length ({} lines) in '{append_path}'",
				target_lines.len()
			);
		}

		let mut new_lines = Vec::new();

		// Add lines before insertion point
		new_lines.extend(target_lines[..insert_after].iter().map(|s| s.to_string()));

		// Add extracted content
		new_lines.extend(extracted_lines.iter().map(|s| s.to_string()));

		// Add remaining lines after insertion point
		if insert_after < target_lines.len() {
			new_lines.extend(target_lines[insert_after..].iter().map(|s| s.to_string()));
		}

		// Preserve target file's newline structure
		let target_ends_with_newline = target_content.ends_with('\n');
		if target_ends_with_newline {
			format!("{}\n", new_lines.join("\n"))
		} else {
			new_lines.join("\n")
		}
	};

	// Snapshot the target for undo_edit, then write atomically — same guarantees as
	// every other edit path (no partial file on interruption, permissions preserved).
	save_file_history(&append_source).await?;
	if let Err(e) = text_editing::atomic_write(&append_source, &final_content).await {
		bail!("Failed to write to target file '{append_path}': {e}");
	}

	// Return success result with useful information
	let lines_extracted = from_range.1 - from_range.0 + 1;
	let position_desc = match append_line {
		0 => "beginning of file".to_string(),
		-1 => "end of file".to_string(),
		n => format!("after line {n}"),
	};

	Ok(format!(
		"Successfully extracted {lines_extracted} lines (lines {}-{}) from '{from_path}' and appended to '{append_path}' at {position_desc}.\n\nExtracted content:\n{extracted_content_display}",
		from_range.0,
		from_range.1
	))
}

// Execute batch_edit operations on a single file.
// `operations` is a typed `Vec<BatchEditOperation>` in the tool schema, so rmcp guarantees
// it arrives as a JSON array here; the operation-count limit is enforced in batch_edit_spec.
pub async fn execute_batch_edit(call: &McpToolCall) -> Result<String> {
	let operations_vec = match call.parameters.get("operations") {
		Some(Value::Array(ops)) => ops.clone(),
		_ => bail!("Missing or invalid 'operations' parameter for batch_edit - must be an array"),
	};

	text_editing::batch_edit_spec(call, &operations_vec).await
}

#[cfg(test)]
mod history_tests {
	use super::*;

	#[test]
	fn test_history_store_byte_cap_evicts_globally_oldest() {
		let mut store = HistoryStore::default();
		store.push("a".to_string(), "x".repeat(100), 250);
		store.push("b".to_string(), "y".repeat(100), 250);
		store.push("c".to_string(), "z".repeat(100), 250);
		assert!(store.pop("a").is_none(), "oldest snapshot must be evicted");
		assert!(store.pop("b").is_some());
		assert!(store.pop("c").is_some());
	}

	#[test]
	fn test_history_store_never_evicts_the_snapshot_just_pushed() {
		let mut store = HistoryStore::default();
		store.push("big".to_string(), "x".repeat(1000), 250);
		assert!(
			store.pop("big").is_some(),
			"the newest snapshot survives even when it alone exceeds the cap"
		);
	}

	#[test]
	fn test_history_store_per_file_cap() {
		let mut store = HistoryStore::default();
		for i in 0..12 {
			store.push("f".to_string(), format!("v{i}"), usize::MAX);
		}
		let mut n = 0;
		while store.pop("f").is_some() {
			n += 1;
		}
		assert_eq!(n, 10);
	}
}
