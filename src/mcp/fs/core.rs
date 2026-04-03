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

use super::super::{get_thread_working_directory, McpToolCall};
use crate::mcp::fs::{directory, file_ops, text_editing};
use crate::utils::truncation::format_extracted_content_smart;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use tokio::fs as tokio_fs;

/// Resolve a path relative to the thread working directory
/// If the path is absolute, returns it as-is
/// If the path is relative, resolves it relative to the thread working directory
pub fn resolve_path(path_str: &str) -> std::path::PathBuf {
	let path = Path::new(path_str);
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		get_thread_working_directory().join(path)
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
			file_ops::create_file_spec(&resolve_path(&path), &content).await
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
			text_editing::str_replace_spec(&resolve_path(&path), &old_text, &new_text).await
		}
		"undo_edit" => {
			let path = match call.parameters.get("path") {
				Some(Value::String(p)) => p.clone(),
				_ => {
					bail!("Missing or invalid 'path' parameter for undo_edit command");
				}
			};
			undo_edit(&resolve_path(&path)).await
		}
		_ => bail!(
			"Invalid command: {command}. Allowed commands are: create, str_replace, undo_edit"
		),
	}
}

// Execute view command - unified read-only tool for files, directories, and content search
pub async fn execute_view(call: &McpToolCall) -> Result<String> {
	// Multi-file view: paths array takes priority
	if let Some(Value::Array(arr)) = call.parameters.get("paths") {
		let path_strings: Result<Vec<String>, _> = arr
			.iter()
			.map(|p| p.as_str().ok_or_else(|| anyhow!("Invalid path in array")))
			.map(|r| r.map(|s| s.to_string()))
			.collect();
		let paths = path_strings?;
		if paths.len() > 50 {
			bail!("Too many files requested. Maximum 50 files per request.");
		}
		return file_ops::view_many_files_spec(&paths).await;
	}

	// Single path required
	let path = match call.parameters.get("path") {
		Some(Value::String(p)) => p.clone(),
		_ => {
			bail!("Missing or invalid 'path' parameter. Provide 'path' for a file/directory or 'paths' for multiple files.");
		}
	};

	let resolved = resolve_path(&path);

	// Directory: dispatch directly with the resolved path string
	if resolved.is_dir() {
		return directory::list_directory(call, &path).await;
	}

	// File + content: search the file with ripgrep and render with the same hash/number format
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

	// File: resolve optional line range with negative-index support
	// Accepts both line numbers [10, 20] and hash identifiers ["a3bd", "c7f2"]
	let lines = match call.parameters.get("lines") {
		Some(Value::Array(arr)) if arr.len() == 2 => {
			// Try numbers first, then hash strings
			if let (Some(start), Some(end)) = (arr[0].as_i64(), arr[1].as_i64()) {
				// Numeric line range
				let total_lines = match tokio_fs::read_to_string(&resolved).await {
					Ok(c) => c.lines().count(),
					Err(_) => 0,
				};
				if total_lines > 0 {
					match resolve_line_range(start, end, total_lines) {
						Ok((s, e)) => Some((s, e as i64)),
						Err(err) => {
							bail!("Invalid lines parameter: {err}");
						}
					}
				} else {
					Some((start as usize, end))
				}
			} else if let (Some(start_hash), Some(end_hash)) = (arr[0].as_str(), arr[1].as_str()) {
				// Hash-based line range — resolve to line numbers
				let content = tokio_fs::read_to_string(&resolved)
					.await
					.map_err(|e| anyhow!("Cannot read file for hash resolution: {}", e))?;
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
				Some((start, end as i64))
			} else {
				bail!("lines array elements must be integers or hash strings");
			}
		}
		Some(Value::Array(_)) => {
			bail!("lines must be an array with exactly 2 elements");
		}
		Some(_) => {
			bail!("lines must be an array");
		}
		None => None,
	};

	file_ops::view_file_spec(&resolved, lines).await
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

	// Validate and extract from_range parameter (defer negative index resolution until after file read)
	let (from_range_start_raw, from_range_end_raw) = match call.parameters.get("from_range") {
		Some(Value::Array(arr)) => {
			if arr.len() != 2 {
				bail!("Parameter 'from_range' must be an array with exactly 2 elements");
			}

			let start = match arr[0].as_i64() {
				Some(0) => {
					bail!("Line numbers are 1-indexed, use 1 for first line");
				}
				Some(n) => n,
				None => {
					bail!("Start line number must be an integer");
				}
			};

			let end = match arr[1].as_i64() {
				Some(0) => {
					bail!("Line numbers are 1-indexed, use 1 for first line");
				}
				Some(n) => n,
				None => {
					bail!("End line number must be an integer");
				}
			};

			(start, end)
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

	// Validate and extract append_line parameter
	let append_line = match call.parameters.get("append_line") {
		Some(Value::Number(n)) => match n.as_i64() {
			Some(line) => line,
			None => {
				bail!("Parameter 'append_line' must be an integer");
			}
		},
		Some(_) => {
			bail!("Parameter 'append_line' must be an integer");
		}
		None => {
			bail!("Missing required parameter 'append_line'");
		}
	};

	// Read source file
	let from_path_obj = resolve_path(&from_path);
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

	// Resolve negative indices now that we know the file length
	let from_range = match resolve_line_range(from_range_start_raw, from_range_end_raw, total_lines)
	{
		Ok(range) => range,
		Err(err) => {
			bail!("Invalid from_range: {err}");
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
	let append_path_obj = resolve_path(&append_path);
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
