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

// File operations module - handling file viewing, creation, and basic manipulation

use super::core::resolve_path;
use super::search;
use crate::utils::line_hash::{compute_line_hashes, is_hash_mode};
use crate::utils::truncation::format_content_with_line_numbers;
use anyhow::{anyhow, bail, Result};
use std::path::Path;
use tokio::fs as tokio_fs;

// Helper function to format file content with line numbers and smart truncation
// This is the core logic shared between view and view_many commands
fn format_file_content_with_numbers(lines: &[&str], line_range: Option<(usize, i64)>) -> String {
	format_content_with_line_numbers(lines, 1, line_range)
}

// View the content of a file following Anthropic specification - with line numbers and line_range support
pub async fn view_file_spec(path: &Path, line_range: Option<(usize, i64)>) -> Result<String> {
	if !path.exists() {
		bail!("File not found");
	}

	if path.is_dir() {
		// List directory contents
		let mut entries = Vec::new();
		let read_dir = tokio_fs::read_dir(path)
			.await
			.map_err(|e| anyhow!("Permission denied. Cannot read directory: {}", e))?;
		let mut dir_entries = read_dir;

		while let Some(entry) = dir_entries
			.next_entry()
			.await
			.map_err(|e| anyhow!("Error reading directory: {}", e))?
		{
			let name = entry.file_name().to_string_lossy().to_string();
			let is_dir = entry
				.file_type()
				.await
				.map_err(|e| anyhow!("Error reading file type: {}", e))?
				.is_dir();
			entries.push(if is_dir { format!("{}/", name) } else { name });
		}

		entries.sort();
		let content = entries.join("\n");

		return Ok(content);
	}

	if !path.is_file() {
		bail!("Path is not a file");
	}

	// Check file size to avoid loading very large files
	let metadata = tokio_fs::metadata(path)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	if metadata.len() > 1024 * 1024 * 5 {
		// 5MB limit
		bail!("File is too large (>5MB)");
	}

	// Read the file content
	let content = tokio_fs::read_to_string(path)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	let lines: Vec<&str> = content.lines().collect();

	let content_with_numbers = format_file_content_with_numbers(&lines, line_range);

	// Check if this is an error message from the helper function
	if content_with_numbers.starts_with("Start line")
		|| content_with_numbers.starts_with("Start line")
	{
		bail!("{}", content_with_numbers);
	}

	// Return plain text content
	Ok(content_with_numbers)
}

// View a single file with multiple line ranges, rendered with '--' separators
// between ranges (same convention as content search). File is read once.
// Ranges are applied in the order given (no dedup/merge — caller's responsibility).
pub async fn view_file_multi_ranges(path: &Path, ranges: &[(usize, i64)]) -> Result<String> {
	if !path.exists() {
		bail!("File not found");
	}
	if !path.is_file() {
		bail!("Path is not a file");
	}

	let metadata = tokio_fs::metadata(path)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	if metadata.len() > 1024 * 1024 * 5 {
		bail!("File is too large (>5MB)");
	}

	let content = tokio_fs::read_to_string(path)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	let lines: Vec<&str> = content.lines().collect();

	if ranges.is_empty() {
		return Ok(format_file_content_with_numbers(&lines, None));
	}

	let parts: Vec<String> = ranges
		.iter()
		.map(|r| format_file_content_with_numbers(&lines, Some(*r)))
		.collect();

	Ok(parts.join("\n--\n"))
}

// Search a single file for a literal pattern and render results using the same
// hash/number format as view. No external tools — pure Rust string matching.
pub async fn view_file_with_content_search(
	path: &Path,
	pattern: &str,
	context_lines: usize,
	regex: bool,
) -> Result<String> {
	if !path.exists() {
		bail!("File not found");
	}
	if !path.is_file() {
		bail!("Path is not a file");
	}

	// Lossy UTF-8 read so non-UTF-8 files (UTF-16 BOM, Latin-1, etc.) still match.
	let bytes = tokio_fs::read(path)
		.await
		.map_err(|e| anyhow!("Cannot read file: {}", e))?;
	let content = String::from_utf8_lossy(&bytes).into_owned();
	let file_lines: Vec<&str> = content.lines().collect();
	let total = file_lines.len();

	if total == 0 {
		return Ok(String::new());
	}

	let matcher = search::Matcher::new(pattern, regex)?;
	let blocks = search::search_lines(&content, &matcher, context_lines);
	if blocks.is_empty() {
		return Ok(String::new());
	}

	// Compute prefixes once for the whole file (same as view_file_spec does)
	let prefixes: Vec<String> = if is_hash_mode() {
		compute_line_hashes(&file_lines)
	} else {
		(1..=total).map(|n| n.to_string()).collect()
	};

	// Render each block; separate blocks with "--"
	let mut parts: Vec<String> = Vec::new();
	for block in &blocks {
		let mut rendered = Vec::new();
		for &n in &block.line_numbers {
			let idx = n - 1;
			rendered.push(format!("{}:{}", prefixes[idx], file_lines[idx]));
		}
		parts.push(rendered.join("\n"));
	}

	Ok(parts.join("\n--\n"))
}

// Create a new file following Anthropic specification
pub async fn create_file_spec(path: &Path, content: &str) -> Result<String> {
	// Check if file already exists — guide the AI toward the right edit tool instead of retrying create
	if path.exists() {
		bail!(
			"File already exists: {}. Do NOT retry `create` — use `str_replace` to replace specific content, `line_replace` to replace specific lines, or `insert` to add new content at a position.",
			path.display()
		);
	}

	// Create parent directories if they don't exist
	if let Some(parent) = path.parent() {
		if !parent.exists() {
			tokio_fs::create_dir_all(parent)
				.await
				.map_err(|e| anyhow!("Permission denied. Cannot create directories: {}", e))?;
		}
	}

	// Write the content to the file
	tokio_fs::write(path, content)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot write to file: {}", e))?;

	Ok(format!(
		"File created successfully with {} bytes",
		content.len()
	))
}

// View multiple files simultaneously as part of text_editor tool
pub async fn view_many_files_spec(
	paths: &[String],
	workdir: &Path,
	per_file_ranges: &[Option<(usize, i64)>],
) -> Result<String> {
	let mut result_parts = Vec::new();
	let mut success_count = 0;

	// Process each file in the list
	for (i, path_str) in paths.iter().enumerate() {
		let path = resolve_path(path_str, workdir);
		let path_display = path_str.to_string();
		let line_range = per_file_ranges.get(i).copied().flatten();

		// Add file header
		result_parts.push(path_display.clone());

		// Check if file exists and is a regular file
		if !path.exists() {
			result_parts.push("✗ File does not exist".to_string());
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		if path.is_dir() {
			// Handle directory case - list contents like view command does
			let mut entries = Vec::new();
			if let Ok(mut read_dir) = tokio_fs::read_dir(&path).await {
				while let Ok(Some(entry)) = read_dir.next_entry().await {
					let name = entry.file_name().to_string_lossy().to_string();
					if let Ok(file_type) = entry.file_type().await {
						entries.push(if file_type.is_dir() {
							format!("{}/", name)
						} else {
							name
						});
					}
				}
				entries.sort();
				result_parts.push(entries.join("\n"));
			} else {
				result_parts.push("✗ Permission denied. Cannot read directory".to_string());
			}
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		if !path.is_file() {
			result_parts.push("✗ Path is not a file".to_string());
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		// Check file size - avoid loading very large files
		let _metadata = match tokio_fs::metadata(&path).await {
			Ok(meta) => {
				if meta.len() > 1024 * 1024 * 5 {
					// 5MB limit
					result_parts.push("✗ File is too large (>5MB)".to_string());
					result_parts.push("".to_string()); // Empty line separator
					continue;
				}
				meta
			}
			Err(_) => {
				result_parts.push("✗ Permission denied. Cannot read file".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		};

		// Check if file is binary
		if let Ok(sample) = tokio_fs::read(&path).await {
			let sample_size = sample.len().min(512);
			let null_count = sample[..sample_size].iter().filter(|&&b| b == 0).count();
			if null_count > sample_size / 10 {
				result_parts.push("✗ Binary file skipped".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		}

		// Read file content with error handling
		let content = match tokio_fs::read_to_string(&path).await {
			Ok(content) => content,
			Err(_) => {
				result_parts.push("✗ Permission denied. Cannot read file".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		};

		// Use the same smart truncation logic as view command, with per-file line range
		let lines: Vec<&str> = content.lines().collect();
		let content_with_numbers = format_file_content_with_numbers(&lines, line_range);

		result_parts.push(content_with_numbers);
		result_parts.push("".to_string()); // Empty line separator
		success_count += 1;
	}

	// Remove the last empty separator if it exists
	if result_parts.last() == Some(&"".to_string()) {
		result_parts.pop();
	}

	// Join all parts with newlines to create plain text output
	let final_content = result_parts.join("\n");

	if success_count == 0 {
		bail!("{}", final_content);
	}

	Ok(final_content)
}

// View multiple files simultaneously with optimized token usage
pub async fn view_many_files(
	paths: &[String],
	workdir: &Path,
	per_file_ranges: &[Option<(usize, i64)>],
) -> Result<String> {
	let mut result_parts = Vec::new();
	let mut success_count = 0;

	// Process each file in the list
	for (i, path_str) in paths.iter().enumerate() {
		let path = resolve_path(path_str, workdir);
		let path_display = path_str.to_string();
		let line_range = per_file_ranges.get(i).copied().flatten();

		// Add file header
		result_parts.push(path_display.clone());

		// Check if file exists and is a regular file
		if !path.exists() {
			result_parts.push("✗ File does not exist".to_string());
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		if path.is_dir() {
			// Handle directory case - list contents like view command does
			let mut entries = Vec::new();
			if let Ok(mut read_dir) = tokio_fs::read_dir(&path).await {
				while let Ok(Some(entry)) = read_dir.next_entry().await {
					let name = entry.file_name().to_string_lossy().to_string();
					if let Ok(file_type) = entry.file_type().await {
						entries.push(if file_type.is_dir() {
							format!("{}/", name)
						} else {
							name
						});
					}
				}
				entries.sort();
				result_parts.push(entries.join("\n"));
			} else {
				result_parts.push("✗ Permission denied. Cannot read directory".to_string());
			}
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		if !path.is_file() {
			result_parts.push("✗ Path is not a file".to_string());
			result_parts.push("".to_string()); // Empty line separator
			continue;
		}

		// Check file size - avoid loading very large files
		let _metadata = match tokio_fs::metadata(&path).await {
			Ok(meta) => {
				if meta.len() > 1024 * 1024 * 5 {
					// 5MB limit
					result_parts.push("✗ File is too large (>5MB)".to_string());
					result_parts.push("".to_string()); // Empty line separator
					continue;
				}
				meta
			}
			Err(_) => {
				result_parts.push("✗ Permission denied. Cannot read file".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		};

		// Check if file is binary
		if let Ok(sample) = tokio_fs::read(&path).await {
			let sample_size = sample.len().min(512);
			let null_count = sample[..sample_size].iter().filter(|&&b| b == 0).count();
			if null_count > sample_size / 10 {
				result_parts.push("✗ Binary file skipped".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		}

		// Read file content with error handling
		let content = match tokio_fs::read_to_string(&path).await {
			Ok(content) => content,
			Err(_) => {
				result_parts.push("✗ Permission denied. Cannot read file".to_string());
				result_parts.push("".to_string()); // Empty line separator
				continue;
			}
		};

		// Use the same smart truncation logic as view command, with per-file line range
		let lines: Vec<&str> = content.lines().collect();
		let content_with_numbers = format_file_content_with_numbers(&lines, line_range);

		result_parts.push(content_with_numbers);
		result_parts.push("".to_string()); // Empty line separator
		success_count += 1;
	}

	// Remove the last empty separator if it exists
	if result_parts.last() == Some(&"".to_string()) {
		result_parts.pop();
	}

	// Join all parts with newlines to create plain text output
	let final_content = result_parts.join("\n");

	if success_count == 0 {
		bail!("{}", final_content);
	}

	Ok(final_content)
}
