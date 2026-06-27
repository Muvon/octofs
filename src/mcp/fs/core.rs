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
use crate::mcp::fs::{directory, file_ops, text_editing};
use crate::utils::line_hash::{self, Endpoint};
use crate::utils::truncation::format_extracted_content_smart;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use tokio::fs as tokio_fs;

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

// Thread-safe lazy initialization of file history using OnceLock
static FILE_HISTORY: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

// Thread-safe way to get the file history
pub fn get_file_history() -> &'static Mutex<HashMap<String, Vec<String>>> {
	FILE_HISTORY.get_or_init(|| Mutex::new(HashMap::new()))
}

// Save the current content of a file for undo
pub async fn save_file_history(path: &Path) -> Result<()> {
	if path.exists() {
		// First read the content
		let content = tokio_fs::read_to_string(path).await?;
		// Use the same canonicalized key as the file lock map so undo history
		// follows the file even when callers pass aliased paths.
		let path_str = text_editing::lock_key_for(path);

		// Then update the history with the lock held
		let file_history = get_file_history();
		{
			let mut history_guard = file_history
				.lock()
				.map_err(|_| anyhow!("Failed to acquire lock on file history"))?;

			let history = history_guard.entry(path_str).or_insert_with(Vec::new);

			// Limit history size to avoid excessive memory usage
			if history.len() >= 10 {
				history.remove(0);
			}

			history.push(content);
		} // Lock is released here
	}
	Ok(())
}

// Undo the last edit to a file
pub async fn undo_edit(path: &Path) -> Result<String> {
	let path_str = text_editing::lock_key_for(path);

	// First retrieve the previous content while holding the lock
	let previous_content = {
		let file_history = get_file_history();
		let mut history_guard = file_history
			.lock()
			.map_err(|_| anyhow!("Failed to acquire lock on file history"))?;

		if let Some(history) = history_guard.get_mut(&path_str) {
			history.pop()
		} else {
			None
		}
	}; // Lock is released here when history_guard goes out of scope

	// Now we have the previous content or None, and we've released the lock
	if let Some(prev_content) = previous_content {
		// Atomic write for undo
		text_editing::atomic_write(path, &prev_content).await?;

		Ok(format!(
			"Successfully undid the last edit to {}",
			path.to_string_lossy()
		))
	} else {
		bail!("No more undo history for this file (up to 10 levels are stored per file).");
	}
}

// Helper function to detect language based on file extension
pub fn detect_language(ext: &str) -> &str {
	match ext {
		"rs" => "rust",
		"py" => "python",
		"js" => "javascript",
		"ts" => "typescript",
		"jsx" => "jsx",
		"tsx" => "tsx",
		"html" => "html",
		"css" => "css",
		"json" => "json",
		"md" => "markdown",
		"go" => "go",
		"java" => "java",
		"c" | "h" | "cpp" => "cpp",
		"toml" => "toml",
		"yaml" | "yml" => "yaml",
		"php" => "php",
		"xml" => "xml",
		"sh" => "bash",
		_ => "text",
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
			file_ops::create_file_spec(&resolve_path(&path, &call.workdir), &content).await
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
			text_editing::str_replace_spec(
				&resolve_path(&path, &call.workdir),
				&old_text,
				&new_text,
			)
			.await
		}
		"undo_edit" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for undo_edit command");
				}
			};
			undo_edit(&resolve_path(&path, &call.workdir)).await
		}
		"delete" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for delete command");
				}
			};
			text_editing::delete_file_spec(&resolve_path(&path, &call.workdir)).await
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

/// Resolve one endpoint to a 1-indexed line. Numbers clamp to file bounds (setting
/// `clamped`); hashes must resolve exactly.
fn resolve_endpoint_to_line(
	ep: &Endpoint,
	total_lines: usize,
	file_lines: &[&str],
	clamped: &mut bool,
) -> Result<usize> {
	match ep {
		Endpoint::Number(n) => {
			let (line, was) = resolve_line_index_clamped(*n, total_lines)
				.map_err(|e| anyhow!("Invalid lines parameter: {e}"))?;
			*clamped |= was;
			Ok(line)
		}
		Endpoint::Hash(h) => {
			line_hash::resolve_hash_to_line(h, file_lines).map_err(|e| anyhow!("Invalid hash: {e}"))
		}
	}
}

/// Resolve a `view` line range from optional start/end endpoints into a clamped
/// `(start, end)` 1-indexed tuple. Returns None when both endpoints are absent
/// (whole file). Missing `start` defaults to line 1, missing `end` to the last line.
async fn resolve_view_range(
	start_ep: Option<Endpoint>,
	end_ep: Option<Endpoint>,
	resolved_path: &Path,
) -> Result<Option<(usize, i64)>> {
	if start_ep.is_none() && end_ep.is_none() {
		return Ok(None);
	}

	let content = tokio_fs::read_to_string(resolved_path).await.ok();
	let file_lines: Vec<&str> = content
		.as_deref()
		.map(|c| c.lines().collect())
		.unwrap_or_default();
	let total_lines = file_lines.len();

	if total_lines == 0 {
		// Empty/unreadable file: there is nothing to clamp a range against, so render the
		// (empty) whole file rather than erroring on an out-of-bounds line.
		return Ok(None);
	}

	let start_ep = start_ep.unwrap_or(Endpoint::Number(1));
	let end_ep = end_ep.unwrap_or(Endpoint::Number(-1));

	let mut clamped = false;
	let start = resolve_endpoint_to_line(&start_ep, total_lines, &file_lines, &mut clamped)?;
	let end = resolve_endpoint_to_line(&end_ep, total_lines, &file_lines, &mut clamped)?;
	if start > end {
		bail!("Invalid lines parameter: start line {start} is after end line {end}");
	}
	if clamped {
		crate::mcp::hint_accumulator::push_hint(&format!(
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

	let resolved = resolve_path(&path, &call.workdir);

	// Directory: dispatch directly with the path string
	if resolved.is_dir() {
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
				&resolved,
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
	let range = resolve_view_range(start_ep, end_ep, &resolved).await?;
	file_ops::view_file_spec(&resolved, range).await
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

	// Validate and extract from_start / from_end endpoints (line number or hash).
	// from_end omitted → single line (defaults to from_start). Resolution is deferred
	// until after the source file is read.
	let from_start_ep = match call.parameters.get("from_start") {
		Some(v) if !v.is_null() => {
			line_hash::parse_endpoint(v).map_err(|e| anyhow!("Invalid 'from_start': {e}"))?
		}
		_ => bail!("Missing required parameter 'from_start' (line number or hash)"),
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
	// or a hash. Hash resolution is deferred until after the target file is read.
	let append_line_ep = match call.parameters.get("append_line") {
		Some(v) if !v.is_null() => {
			line_hash::parse_endpoint(v).map_err(|e| anyhow!("Invalid 'append_line': {e}"))?
		}
		_ => bail!("Missing required parameter 'append_line' (0 = start, -1 = end, N, or a hash)"),
	};

	// Read source file
	let from_path_obj = resolve_path(&from_path, &call.workdir);
	if !from_path_obj.exists() {
		bail!("Source file does not exist: {from_path}");
	}

	let source_content = match tokio_fs::read_to_string(&from_path_obj).await {
		Ok(content) => content,
		Err(e) => {
			bail!("Failed to read source file '{from_path}': {e}");
		}
	};

	// Split content into lines and resolve negative indices
	let source_lines: Vec<&str> = source_content.lines().collect();
	let total_lines = source_lines.len();

	// Resolve from_start / from_end endpoints to 1-indexed line numbers (strict — no clamping).
	let resolve_extract_endpoint = |ep: &Endpoint, which: &str| -> Result<usize> {
		match ep {
			Endpoint::Number(n) => resolve_line_index(*n, total_lines)
				.map_err(|e| anyhow!("Invalid from_{which}: {e}")),
			Endpoint::Hash(h) => line_hash::resolve_hash_to_line(h, &source_lines)
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

	// Create smart formatted content with proper line identifiers for display
	// In hash mode, compute hashes from the full source file and slice the relevant range
	let extracted_hashes: Option<Vec<String>> = if crate::utils::line_hash::is_hash_mode() {
		let all_hashes = crate::utils::line_hash::compute_line_hashes(&source_lines);
		Some(all_hashes[(from_range.0 - 1)..from_range.1].to_vec())
	} else {
		None
	};
	let extracted_content_display = format_extracted_content_smart(
		&extracted_lines,
		from_range.0, // Start line number (1-indexed)
		Some(30),     // Limit display to 30 lines with smart truncation
		extracted_hashes.as_deref(),
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
	let append_path_obj = resolve_path(&append_path, &call.workdir);
	if let Some(parent) = append_path_obj.parent() {
		if let Err(e) = tokio_fs::create_dir_all(parent).await {
			bail!("Failed to create parent directories for '{append_path}': {e}");
		}
	}

	// Read existing target file content or create empty if doesn't exist
	let target_content = if append_path_obj.exists() {
		match tokio_fs::read_to_string(&append_path_obj).await {
			Ok(content) => content,
			Err(e) => {
				bail!("Failed to read target file '{append_path}': {e}");
			}
		}
	} else {
		String::new()
	};

	// Resolve append_line: hash → line number, or keep integer as-is (0/-1/N).
	let append_line: i64 = match append_line_ep {
		Endpoint::Number(n) => {
			if n < -1 {
				bail!(
					"Invalid append_line {n}: use 0 (beginning), -1 (end), N (after line N), or a hash"
				);
			}
			n
		}
		Endpoint::Hash(hash) => {
			if target_content.is_empty() {
				bail!("Cannot use a hash for append_line on an empty or non-existent target file");
			}
			let target_lines: Vec<&str> = target_content.lines().collect();
			let line = line_hash::resolve_hash_to_line(&hash, &target_lines)
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

	// Write the final content to target file
	if let Err(e) = tokio_fs::write(&append_path_obj, &final_content).await {
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
