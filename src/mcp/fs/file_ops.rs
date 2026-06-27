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

use super::search;
use crate::utils::line_hash::{compute_line_hashes, is_hash_mode};
use crate::utils::truncation::format_content_with_line_numbers;
use anyhow::{anyhow, bail, Result};
use std::path::Path;
use tokio::fs as tokio_fs;

// Helper function to format file content with line numbers (or hashes) and smart truncation.
fn format_file_content_with_numbers(lines: &[&str], line_range: Option<(usize, i64)>) -> String {
	format_content_with_line_numbers(lines, 1, line_range)
}

// View the content of a file with line identifiers and an optional line range.
// Directories are dispatched to `directory::list_directory` upstream in `execute_view`,
// so this only ever handles regular files.
pub async fn view_file_spec(path: &Path, line_range: Option<(usize, i64)>) -> Result<String> {
	if !path.exists() {
		bail!("File not found");
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

	// Defensive: the range is pre-clamped in resolve_view_range, but if the formatter ever
	// returns its out-of-range error string, surface it as an error rather than content.
	if content_with_numbers.starts_with("Start line") {
		bail!("{}", content_with_numbers);
	}

	// Return plain text content
	Ok(content_with_numbers)
}

// Search a single file for a pattern and render results using the same
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

// Create a new file.
pub async fn create_file_spec(path: &Path, content: &str) -> Result<String> {
	// Check if file already exists — guide the AI toward the right edit tool instead of retrying create
	if path.exists() {
		bail!(
			"File already exists: {}. Do NOT retry `create` — use `text_editor` str_replace to swap specific content, or `batch_edit` insert/replace operations to edit by line.",
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
