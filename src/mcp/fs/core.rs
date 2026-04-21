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
				"Line {index} exceeds file length ({total_lines} lines)"
			));
		}
		Ok(pos_index)
	} else {
		// Negative indexing: -1 = last line, -2 = second-to-last, etc.
		let from_end = (-index) as usize;
		if from_end > total_lines {
			return Err(format!(
				"Negative index {index} exceeds file length ({total_lines} lines)"
			));
		}
		Ok(total_lines - from_end + 1)
	}
}

// Helper function to resolve line range with negative indexing support
fn resolve_line_range(start: i64, end: i64, total_lines: usize) -> Result<(usize, usize), String> {
	let resolved_start = resolve_line_index(start, total_lines)?;
	let resolved_end = resolve_line_index(end, total_lines)?;

	if resolved_start > resolved_end {
		return Err(format!(
			"Start line ({start}) cannot be greater than end line ({end})"
		));
	}

	Ok((resolved_start, resolved_end))
}

/// View-only: resolve line index while clamping out-of-bounds values to the
/// nearest valid line. Returns (resolved_index, was_clamped).
/// Line 0 is still rejected — it's a spec violation, not out-of-bounds.
fn resolve_line_index_clamped(index: i64, total_lines: usize) -> Result<(usize, bool), String> {
	if index == 0 {
		return Err("Line numbers are 1-indexed, use 1 for first line".to_string());
	}
	if index > 0 {
		let pos = index as usize;
		if pos > total_lines {
			Ok((total_lines, true))
		} else {
			Ok((pos, false))
		}
	} else {
		let from_end = (-index) as usize;
		if from_end > total_lines {
			// Negative index past the beginning — clamp to first line
			Ok((1, true))
		} else {
			Ok((total_lines - from_end + 1, false))
		}
	}
}

/// View-only: resolve a line range, clamping out-of-bounds to file limits.
/// Returns (start, end, hint_message_if_clamped).
fn resolve_line_range_clamped(
	start: i64,
	end: i64,
	total_lines: usize,
) -> Result<(usize, usize, Option<String>), String> {
	let (resolved_start, start_clamped) = resolve_line_index_clamped(start, total_lines)?;
	let (resolved_end, end_clamped) = resolve_line_index_clamped(end, total_lines)?;

	if resolved_start > resolved_end {
		return Err(format!(
			"Start line ({start}) cannot be greater than end line ({end})"
		));
	}

	let hint = if start_clamped || end_clamped {
		Some(format!(
			"Requested line range [{start}, {end}] was out of bounds for a {total_lines}-line file; clamped to [{resolved_start}, {resolved_end}]. Use line numbers within 1..={total_lines} (negative indices count from the end)."
		))
	} else {
		None
	};

	Ok((resolved_start, resolved_end, hint))
}

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
		let path_str = path.to_string_lossy().to_string();

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
	let path_str = path.to_string_lossy().to_string();

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
		_ => bail!(
			"Invalid command: {command}. Allowed commands are: create, str_replace, undo_edit"
		),
	}
}

/// Resolve a single line range value `[start, end]` (numbers or hash strings)
/// into a resolved `(start, end)` tuple with 1-indexed line numbers.
///
/// `cached_content` is the file's content if already read (None = read lazily).
/// Returns the resolved range plus the content (read or passed in) so the caller
/// can reuse it for subsequent ranges on the same file.
async fn resolve_single_line_range(
	arr: &[Value],
	resolved_path: &Path,
	cached_content: Option<&str>,
) -> Result<Option<(usize, i64)>> {
	if arr.len() != 2 {
		bail!(
			"Each line range must have exactly 2 elements [start, end], got {}. Example: [1, 50].",
			arr.len()
		);
	}

	// Try numbers first, then hash strings
	if let (Some(start), Some(end)) = (arr[0].as_i64(), arr[1].as_i64()) {
		let total_lines = if let Some(content) = cached_content {
			content.lines().count()
		} else {
			match tokio_fs::read_to_string(resolved_path).await {
				Ok(c) => c.lines().count(),
				Err(_) => 0,
			}
		};
		if total_lines > 0 {
			match resolve_line_range_clamped(start, end, total_lines) {
				Ok((s, e, hint)) => {
					if let Some(msg) = hint {
						crate::mcp::hint_accumulator::push_hint(&msg);
					}
					Ok(Some((s, e as i64)))
				}
				Err(err) => bail!("Invalid lines parameter: {err}"),
			}
		} else {
			Ok(Some((start as usize, end)))
		}
	} else if let (Some(start_hash), Some(end_hash)) = (arr[0].as_str(), arr[1].as_str()) {
		let content_owned;
		let content: &str = if let Some(c) = cached_content {
			c
		} else {
			content_owned = tokio_fs::read_to_string(resolved_path)
				.await
				.map_err(|e| anyhow!("Cannot read file for hash resolution: {}", e))?;
			&content_owned
		};
		let file_lines: Vec<&str> = content.lines().collect();
		let start = crate::utils::line_hash::resolve_hash_to_line(start_hash, &file_lines)
			.map_err(|e| anyhow!("Invalid start hash: {}", e))?;
		let end = crate::utils::line_hash::resolve_hash_to_line(end_hash, &file_lines)
			.map_err(|e| anyhow!("Invalid end hash: {}", e))?;
		if start > end {
			bail!(
				"Start hash '{}' (line {}) is after end hash '{}' (line {}) — range must go forward",
				start_hash, start, end_hash, end
			);
		}
		Ok(Some((start, end as i64)))
	} else {
		bail!("Line range elements must both be integers or both be hash strings. Example: [1, 50] or [\"a1b2c3\", \"d4e5f6\"].");
	}
}

/// Parsed `lines` parameter.
enum ParsedLines {
	/// No `lines` parameter — show full content.
	None,
	/// Single range `[start, end]` — applied to the single file or all files.
	Single(Option<(usize, i64)>),
	/// Multiple ranges on a single file — only valid when exactly one path is given.
	MultiRangeSingleFile(Vec<(usize, i64)>),
	/// Per-file ranges `[[start, end], ...]` — one per path (paths.len() > 1).
	PerFile(Vec<Option<(usize, i64)>>),
}

/// Parse the `lines` parameter from the tool call.
///
/// Supported shapes:
/// - `[start, end]` — single range (numbers or hash strings)
/// - `[[start, end], [start, end], ...]` with **one** path → multiple ranges from that file
/// - `[[start, end], [start, end], ...]` with **multiple** paths → per-file ranges (one per path)
async fn parse_lines_param(
	lines_value: Option<&Value>,
	paths: &[String],
	workdir: &Path,
) -> Result<ParsedLines> {
	let Some(lines_value) = lines_value else {
		return Ok(ParsedLines::None);
	};

	// Treat explicit null the same as absent
	if lines_value.is_null() {
		return Ok(ParsedLines::None);
	}

	let Value::Array(arr) = lines_value else {
		bail!("`lines` must be an array. Examples: [1, 50] (single range), [[1,50],[200,250]] (multiple ranges on one file or per-file ranges when multiple paths).");
	};

	if arr.is_empty() {
		return Ok(ParsedLines::None);
	}

	// Detect array-of-arrays shape
	let is_nested = matches!(arr.first(), Some(Value::Array(_)));

	if !is_nested {
		// Flat array → must be [start, end]
		if arr.len() != 2 {
			bail!(
				"Single line range must have exactly 2 elements [start, end], got {}. For multiple ranges, wrap each in its own array: [[1,50],[200,250]].",
				arr.len()
			);
		}
		let resolved_path = resolve_path(&paths[0], workdir);
		let range = resolve_single_line_range(arr, &resolved_path, None).await?;
		return Ok(ParsedLines::Single(range));
	}

	// Nested array → interpretation depends on path count
	if paths.len() == 1 {
		// Multiple ranges on a single file — read content once, resolve all ranges against it.
		let resolved_path = resolve_path(&paths[0], workdir);
		let cached = tokio_fs::read_to_string(&resolved_path).await.ok();
		let mut ranges = Vec::with_capacity(arr.len());
		for (i, range_val) in arr.iter().enumerate() {
			let Value::Array(range_arr) = range_val else {
				bail!("lines[{}] must be an array [start, end]", i);
			};
			let range =
				resolve_single_line_range(range_arr, &resolved_path, cached.as_deref()).await?;
			if let Some(r) = range {
				ranges.push(r);
			}
		}
		if ranges.is_empty() {
			return Ok(ParsedLines::None);
		}
		return Ok(ParsedLines::MultiRangeSingleFile(ranges));
	}

	// Multiple paths → per-file ranges (positional)
	if arr.len() > paths.len() {
		bail!(
			"`lines` has {} range pairs but only {} paths provided. For multiple ranges on a single file, pass exactly one path. For per-file ranges, range count must not exceed path count.",
			arr.len(),
			paths.len()
		);
	}
	let mut ranges = Vec::with_capacity(paths.len());
	// Cache per-path content keyed by path string to avoid re-reading on hash resolution
	let mut content_cache: std::collections::HashMap<String, Option<String>> =
		std::collections::HashMap::new();
	for (i, range_val) in arr.iter().enumerate() {
		let Value::Array(range_arr) = range_val else {
			bail!("lines[{}] must be an array [start, end]", i);
		};
		let resolved_path = resolve_path(&paths[i], workdir);
		let key = paths[i].clone();
		let cached = content_cache
			.entry(key)
			.or_insert_with(|| {
				// Synchronously unavailable in async context; we'll populate below.
				None
			})
			.clone();
		let cached_ref = if cached.is_some() {
			cached.as_deref()
		} else {
			// Lazy read now
			let c = tokio_fs::read_to_string(&resolved_path).await.ok();
			content_cache.insert(paths[i].clone(), c.clone());
			// SAFETY: re-borrow from the map
			content_cache.get(&paths[i]).and_then(|o| o.as_deref())
		};
		let range = resolve_single_line_range(range_arr, &resolved_path, cached_ref).await?;
		ranges.push(range);
	}
	// Fill remaining paths with None (no range)
	while ranges.len() < paths.len() {
		ranges.push(None);
	}
	Ok(ParsedLines::PerFile(ranges))
}

// Execute view command - unified read-only tool for files, directories, and content search
pub async fn execute_view(call: &McpToolCall) -> Result<String> {
	// Extract paths array (required, one or more elements)
	let paths: Vec<String> = match call.parameters.get("paths") {
		Some(Value::Array(arr)) => {
			let path_strings: Result<Vec<String>, _> = arr
				.iter()
				.map(|p| {
					p.as_str()
						.ok_or_else(|| anyhow!("Invalid path in array"))
						.map(|s| s.to_string())
				})
				.collect();
			path_strings?
		}
		Some(Value::String(s)) => vec![s.clone()],
		_ => {
			bail!("Missing or invalid 'paths' parameter.");
		}
	};

	if paths.is_empty() {
		bail!("'paths' must contain at least one element.");
	}
	if paths.len() > 50 {
		bail!("Too many files requested. Maximum 50 files per request.");
	}

	// Parse the lines parameter (single range or per-file ranges)
	let parsed_lines =
		parse_lines_param(call.parameters.get("lines"), &paths, &call.workdir).await?;

	// Multi-file view: more than one path
	if paths.len() > 1 {
		let per_file_ranges = match parsed_lines {
			ParsedLines::None => vec![None; paths.len()],
			ParsedLines::Single(range) => vec![range; paths.len()],
			ParsedLines::PerFile(ranges) => ranges,
			ParsedLines::MultiRangeSingleFile(_) => {
				// Unreachable: parse_lines_param only returns this variant when paths.len() == 1.
				unreachable!("MultiRangeSingleFile is only produced for a single path");
			}
		};
		return file_ops::view_many_files_spec(&paths, &call.workdir, &per_file_ranges).await;
	}

	// Single path
	let path = &paths[0];
	let resolved = resolve_path(path, &call.workdir);
	// Directory: dispatch directly with the resolved path string
	if resolved.is_dir() {
		return directory::list_directory(call, path).await;
	}

	// File + content: search the file for a literal pattern and render with the same hash/number format
	if let Some(content_pattern) = call.parameters.get("content").and_then(|v| v.as_str()) {
		if !content_pattern.trim().is_empty() {
			let context_lines = call
				.parameters
				.get("context")
				.and_then(|v| v.as_i64())
				.unwrap_or(0) as usize;
			return file_ops::view_file_with_content_search(
				&resolved,
				content_pattern,
				context_lines,
			)
			.await;
		}
	}

	// File: resolve to the appropriate renderer based on parsed lines shape
	match parsed_lines {
		ParsedLines::None => file_ops::view_file_spec(&resolved, None).await,
		ParsedLines::Single(range) => file_ops::view_file_spec(&resolved, range).await,
		ParsedLines::MultiRangeSingleFile(ranges) => {
			file_ops::view_file_multi_ranges(&resolved, &ranges).await
		}
		ParsedLines::PerFile(ranges) => {
			// paths.len() == 1 here, so PerFile should really not occur, but handle defensively
			let first = ranges.into_iter().next().flatten();
			file_ops::view_file_spec(&resolved, first).await
		}
	}
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

	// Validate and extract from_range parameter.
	// Accepts [int, int] (line numbers) or [string, string] (hash identifiers).
	// Hash resolution is deferred until after the source file is read.
	enum FromRange {
		Lines(i64, i64),
		Hashes(String, String),
	}
	let from_range_raw = match call.parameters.get("from_range") {
		Some(Value::Array(arr)) => {
			if arr.len() != 2 {
				bail!("Parameter 'from_range' must be an array with exactly 2 elements");
			}
			if arr[0].is_string() || arr[1].is_string() {
				// Hash mode
				let start = arr[0]
					.as_str()
					.ok_or_else(|| {
						anyhow::anyhow!("from_range elements must both be hash strings")
					})?
					.to_string();
				let end = arr[1]
					.as_str()
					.ok_or_else(|| {
						anyhow::anyhow!("from_range elements must both be hash strings")
					})?
					.to_string();
				FromRange::Hashes(start, end)
			} else {
				// Number mode
				let start = match arr[0].as_i64() {
					Some(0) => bail!("Line numbers are 1-indexed, use 1 for first line"),
					Some(n) => n,
					None => bail!("Start line number must be an integer"),
				};
				let end = match arr[1].as_i64() {
					Some(0) => bail!("Line numbers are 1-indexed, use 1 for first line"),
					Some(n) => n,
					None => bail!("End line number must be an integer"),
				};
				FromRange::Lines(start, end)
			}
		}
		Some(_) => {
			bail!("Parameter 'from_range' must be an array");
		}
		None => {
			bail!("Missing required parameter 'from_range'");
		}
	};

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

	// Validate and extract append_line parameter.
	// Accepts integer (line number or 0/-1 special) or string (hash identifier).
	// Hash resolution is deferred until after the target file is read.
	enum AppendLine {
		Position(i64),
		Hash(String),
	}
	let append_line_raw = match call.parameters.get("append_line") {
		Some(Value::Number(n)) => match n.as_i64() {
			Some(line) => AppendLine::Position(line),
			None => bail!("Parameter 'append_line' must be an integer"),
		},
		Some(Value::String(h)) => AppendLine::Hash(h.clone()),
		Some(_) => {
			bail!("Parameter 'append_line' must be an integer or hash string");
		}
		None => {
			bail!("Missing required parameter 'append_line'");
		}
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

	// Resolve from_range: hashes → line numbers, or resolve negative indices
	let from_range = match from_range_raw {
		FromRange::Hashes(start_hash, end_hash) => {
			let start = crate::utils::line_hash::resolve_hash_to_line(&start_hash, &source_lines)
				.map_err(|e| anyhow::anyhow!("Invalid from_range start: {e}"))?;
			let end = crate::utils::line_hash::resolve_hash_to_line(&end_hash, &source_lines)
				.map_err(|e| anyhow::anyhow!("Invalid from_range end: {e}"))?;
			if start > end {
				bail!(
					"Hash range is reversed: '{}' is line {} but '{}' is line {} (which comes before it). Did you mean from_range: [\"{}\", \"{}\"]?",
					start_hash, start, end_hash, end, end_hash, start_hash
				);
			}
			(start, end)
		}
		FromRange::Lines(start_raw, end_raw) => {
			match resolve_line_range(start_raw, end_raw, total_lines) {
				Ok(range) => range,
				Err(err) => bail!("Invalid from_range: {err}"),
			}
		}
	};

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

	// Resolve append_line: hash → line number, or keep integer as-is
	let append_line: i64 = match append_line_raw {
		AppendLine::Position(n) => n,
		AppendLine::Hash(hash) => {
			if target_content.is_empty() {
				bail!("Cannot use hash identifier for append_line on an empty or non-existent target file");
			}
			let target_lines: Vec<&str> = target_content.lines().collect();
			let line = crate::utils::line_hash::resolve_hash_to_line(&hash, &target_lines)
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
				"Insert position {insert_after} exceeds target file length ({}) lines) in '{append_path}'",
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

// Execute batch_edit operations on a single file
pub async fn execute_batch_edit(call: &McpToolCall) -> Result<String> {
	let (operations_vec, ai_format_warning) = match call.parameters.get("operations") {
		Some(Value::Array(ops)) => {
			// Correct format - AI passed array directly
			if ops.len() > 50 {
				bail!("Too many operations in batch. Maximum 50 operations allowed.");
			}
			(ops.clone(), false)
		}
		Some(Value::String(ops_str)) => {
			// AI incorrectly passed operations as JSON string - try to parse it
			match serde_json::from_str::<Vec<Value>>(ops_str) {
				Ok(parsed_ops) => {
					if parsed_ops.len() > 50 {
						bail!("Too many operations in batch. Maximum 50 operations allowed.");
					}
					tracing::debug!("AI passed operations as JSON string instead of array - parsing defensively");
					(parsed_ops, true)
				}
				Err(_) => {
					bail!("Invalid 'operations' parameter for batch_edit - must be an array or valid JSON array string");
				}
			}
		}
		_ => {
			bail!("Missing or invalid 'operations' parameter for batch_edit - must be an array");
		}
	};

	// Create a modified call with the AI format warning flag
	let mut modified_call = call.clone();
	if ai_format_warning {
		modified_call
			.parameters
			.as_object_mut()
			.unwrap()
			.insert("_ai_format_warning".to_string(), Value::Bool(true));
	}

	text_editing::batch_edit_spec(&modified_call, &operations_vec).await
}
