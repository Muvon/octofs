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

use super::remote::{io_create_dir_all, io_exists, io_is_file, io_metadata, io_read_capped, io_write, PathSource};
use super::search;
use crate::utils::line_hash::line_id_at;
use crate::utils::truncation::format_content_with_line_numbers;
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::path::Path;

/// Refuse to load files larger than this into memory for viewing.
pub(crate) const MAX_VIEW_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// An image file served by `view` as an MCP image block instead of text.
#[derive(Debug)]
pub struct ImageView {
	pub base64: String,
	pub mime: &'static str,
}

/// The image formats `view` serves as an image block, chosen by extension so a
/// text view never has to read the bytes first. Every major provider accepts
/// these four.
fn image_mime_by_extension(path: &str) -> Option<&'static str> {
	let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
	match extension.as_str() {
		"png" => Some("image/png"),
		"jpg" | "jpeg" => Some("image/jpeg"),
		"gif" => Some("image/gif"),
		"webp" => Some("image/webp"),
		_ => None,
	}
}

/// The format the leading bytes declare. The extension alone is not trusted:
/// a provider rejects an image block whose data is not the declared type.
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
	if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
		Some("image/png")
	} else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
		Some("image/jpeg")
	} else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
		Some("image/gif")
	} else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
		Some("image/webp")
	} else {
		None
	}
}

/// Serve an image file as an image block. None when `path` does not name an image
/// by extension or is not a regular file (the text path then reports it); an
/// image-named file whose bytes are not that format is an error, not a text view.
pub async fn view_image_spec(source: &PathSource, path: &str) -> Result<Option<ImageView>> {
	let Some(mime) = image_mime_by_extension(path) else {
		return Ok(None);
	};
	if !io_exists(source).await? || !io_is_file(source).await? {
		return Ok(None);
	}
	let metadata = io_metadata(source)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	if metadata.size > MAX_VIEW_FILE_BYTES {
		bail!("Image is too large (>5MB)");
	}
	let bytes = io_read_capped(source, MAX_VIEW_FILE_BYTES)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?
		.ok_or_else(|| anyhow!("Image is too large (>5MB)"))?;
	if sniff_image(&bytes) != Some(mime) {
		bail!("{path} is named as {mime} but its bytes are not that format");
	}
	Ok(Some(ImageView {
		base64: BASE64.encode(&bytes),
		mime,
	}))
}

// Helper function to format file content with line numbers (or hashes) and smart truncation.
fn format_file_content_with_numbers(lines: &[&str], line_range: Option<(usize, i64)>) -> String {
	format_content_with_line_numbers(lines, 1, line_range)
}

// View the content of a file with line identifiers and an optional line range.
// Directories are dispatched to `directory::list_directory` upstream in `execute_view`,
// so this only ever handles regular files.
pub async fn view_file_spec(source: &PathSource, line_range: Option<(usize, i64)>, full: bool) -> Result<String> {
	if !io_exists(source).await? {
		bail!("File not found");
	}

	if !io_is_file(source).await? {
		bail!("Path is not a file");
	}

	// Check file size to avoid loading very large files
	let metadata = io_metadata(source)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;
	if metadata.size > MAX_VIEW_FILE_BYTES {
		bail!("File is too large (>5MB)");
	}

	// Read the file content
	let bytes = io_read_capped(source, MAX_VIEW_FILE_BYTES)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?
		.ok_or_else(|| anyhow!("File is too large (>5MB)"))?;
	let content = String::from_utf8(bytes).map_err(|e| anyhow!("Permission denied. Cannot read file: {}", e))?;

	// Whole-file views go through the session delta cache; ranged views render
	// exactly what was asked and leave the cache alone.
	let Some(range) = line_range else {
		return Ok(super::delta::render_whole_file(source, &content, full));
	};
	let lines: Vec<&str> = content.lines().collect();

	let content_with_numbers = format_file_content_with_numbers(&lines, Some(range));

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
	source: &PathSource,
	pattern: &str,
	context_lines: usize,
	regex: bool,
) -> Result<String> {
	if !io_exists(source).await? {
		bail!("File not found");
	}
	if !io_is_file(source).await? {
		bail!("Path is not a file");
	}

	// Same cap as view — searching loads the whole file (plus a lossy copy).
	let metadata = io_metadata(source)
		.await
		.map_err(|e| anyhow!("Cannot read file: {}", e))?;
	if metadata.size > MAX_VIEW_FILE_BYTES {
		bail!("File is too large to search in memory (>5MB).");
	}

	// Lossy UTF-8 read so non-UTF-8 files (UTF-16 BOM, Latin-1, etc.) still match.
	let bytes = io_read_capped(source, MAX_VIEW_FILE_BYTES)
		.await
		.map_err(|e| anyhow!("Cannot read file: {}", e))?
		.ok_or_else(|| {
			anyhow!("File is too large to search (>5MB). Use the shell tool (`grep -n ...`) for files this size.")
		})?;
	let content = String::from_utf8_lossy(&bytes).into_owned();
	let file_lines: Vec<&str> = content.lines().collect();
	let total = file_lines.len();

	if total == 0 {
		return Ok(format!("File is empty — no content to match pattern \"{pattern}\"."));
	}

	let matcher = search::Matcher::new(pattern, regex)?;
	let blocks = search::search_lines(&content, &matcher, context_lines);
	if blocks.is_empty() {
		return Ok(format!("No matches for pattern \"{pattern}\" in this file."));
	}

	// Render each block. Blocks follow each other directly: every line carries its
	// number, so a jump in numbering already marks a gap, and a separator line would
	// cost 2 tokens per gap.
	let mut parts: Vec<String> = Vec::new();
	for block in &blocks {
		let mut rendered = Vec::new();
		for &n in &block.line_numbers {
			rendered.push(format!("{}|{}", line_id_at(&file_lines, n), file_lines[n - 1]));
		}
		parts.push(rendered.join("\n"));
	}

	Ok(parts.join("\n"))
}

// Create a new file.
pub async fn create_file_spec(source: &PathSource, content: &str) -> Result<String> {
	// Check if file already exists — guide the AI toward the right edit tool instead of retrying create
	if io_exists(source).await? {
		bail!(
			"File already exists: {}. Edit it with str_replace or batch_edit instead of create.",
			source.display()
		);
	}

	// Create parent directories if they don't exist
	if let Some(parent_source) = source.parent() {
		if !io_exists(&parent_source).await? {
			io_create_dir_all(&parent_source)
				.await
				.map_err(|e| anyhow!("Permission denied. Cannot create directories: {}", e))?;
		}
	}

	// Write the content to the file
	io_write(source, content.as_bytes())
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot write to file: {}", e))?;
	super::delta::note_create(source, content);

	Ok(format!("File created successfully with {} bytes", content.len()))
}

#[cfg(test)]
#[path = "file_ops_tests.rs"]
mod file_ops_tests;
