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

//! Structural outline mode of `view` (`flags: "s"`): reads files through the
//! same I/O path and caps as a whole-file view, extracts declarations with
//! `crate::structural`, and renders them as ordinary `N:hh|content` lines so
//! the ids feed `batch_edit` unchanged. Bodies are elided; nesting is conveyed
//! by the original indentation only. Outline reads never register in the
//! delta/view cache.

use super::directory::{
	apply_pattern, build_pattern_matcher, build_walker, collect_file_paths, collect_remote_files, join_bounded,
	read_capped, remote_child,
};
use super::file_ops::MAX_VIEW_FILE_BYTES;
use super::remote::{io_is_dir, io_metadata, io_read, io_read_capped, IoMetadata, PathSource};
use crate::mcp::McpToolCall;
use crate::structural::{self, OutlineItem};
use crate::utils::line_hash::line_id;
use crate::utils::truncation::MAX_RESULT_BYTES;
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Bytes kept free at the end of a truncated result so the truncation notice
/// itself always fits.
pub(super) const TRUNCATION_NOTICE_RESERVE: usize = 256;

/// Render the outline of a file, or of every supported file under a directory.
/// Flag exclusivity (`s` alone, no `content`, no `start`/`end`) is validated
/// upstream in `ViewFlags::parse` and `view_existing_path`.
pub async fn view_outline(call: &McpToolCall, path: &str, source: &PathSource) -> Result<String> {
	if io_is_dir(source).await? {
		return outline_directory(call, path, source).await;
	}
	outline_file(path, source).await
}

async fn outline_file(path: &str, source: &PathSource) -> Result<String> {
	// Same caps as a whole-file view: refuse oversized files instead of
	// parsing megabytes of declarations.
	let metadata = io_metadata(source)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {e}"))?;
	if metadata.size > MAX_VIEW_FILE_BYTES {
		bail!("File is too large (>5MB)");
	}
	let bytes = io_read_capped(source, MAX_VIEW_FILE_BYTES)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {e}"))?
		.ok_or_else(|| anyhow!("File is too large (>5MB)"))?;
	// Lossy decode: an outline of a mostly-binary file is useless but must not
	// error the way a text view does — tree-sitter recovers what it can.
	let content = String::from_utf8_lossy(&bytes).into_owned();
	let label = path.to_string();
	// Parsing is CPU-bound: keep it off the async reactor; the content stays
	// here for rendering, so the closure parses a clone.
	let parse_content = content.clone();
	let items = tokio::task::spawn_blocking(move || structural::outline(&label, &parse_content))
		.await
		.map_err(|e| anyhow!("Failed to extract outline: {e}"))?
		.map_err(|e| anyhow!("{e}"))?;
	Ok(render_file_outline(path, &content, &items, MAX_RESULT_BYTES))
}

async fn outline_directory(call: &McpToolCall, path: &str, source: &PathSource) -> Result<String> {
	let pattern = call
		.parameters
		.get("pattern")
		.and_then(|v| v.as_str())
		.map(str::to_string);
	let max_depth = call
		.parameters
		.get("max_depth")
		.and_then(|v| v.as_u64())
		.map(|n| n as usize);
	let include_hidden = call
		.parameters
		.get("include_hidden")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	if source.is_remote() {
		return outline_directory_remote(path, source, pattern, max_depth, include_hidden).await;
	}

	let abs_dir = if Path::new(path).is_absolute() {
		PathBuf::from(path)
	} else {
		call.workdir.join(path)
	};
	let abs_dir_str = abs_dir.to_string_lossy().to_string();
	let working_dir = call.workdir.clone();
	let label = path.to_string();

	// The walk, read, parse and render are all CPU/IO-bound blocking work —
	// one spawn_blocking, files in parallel like the tree content search.
	let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
		let files = match supported_files_under(
			&abs_dir_str,
			&working_dir,
			pattern.as_deref(),
			max_depth,
			include_hidden,
		)? {
			Some(files) => files,
			None => return Ok(format!("No files matched pattern \"{}\".", pattern.unwrap_or_default())),
		};

		let mut blocks: Vec<(usize, String)> = files
			.par_iter()
			.enumerate()
			.filter_map(|(i, rel_path)| {
				let full_path = working_dir.join(rel_path);
				// Files over the view cap are not loaded — same OOM bound as
				// the content search; they are skipped silently like files
				// with no grammar or no items.
				let meta = std::fs::metadata(&full_path).ok()?;
				if meta.len() > MAX_VIEW_FILE_BYTES {
					return None;
				}
				let bytes = read_capped(&full_path)?;
				let content = String::from_utf8_lossy(&bytes);
				let items = structural::outline(rel_path, &content).ok()?;
				(!items.is_empty()).then(|| (i, render_file_block(rel_path, &content, &items, MAX_RESULT_BYTES)))
			})
			.collect();
		blocks.sort_by_key(|(i, _)| *i);
		let blocks: Vec<String> = blocks.into_iter().map(|(_, block)| block).collect();
		Ok(join_file_blocks(&label, blocks, MAX_RESULT_BYTES))
	})
	.await;

	match output {
		Ok(Ok(s)) => Ok(s),
		Ok(Err(e)) => bail!("{e}"),
		Err(join_err) => bail!("Failed to extract outline: {join_err}"),
	}
}

async fn outline_directory_remote(
	path: &str,
	source: &PathSource,
	pattern: Option<String>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<String> {
	let files = match supported_remote_files(source, pattern.as_deref(), max_depth, include_hidden).await? {
		Some(files) => files,
		None => return Ok(format!("No files matched pattern \"{}\".", pattern.unwrap_or_default())),
	};

	// Read concurrently like the remote content search (oversized files are
	// not downloaded); parse and render afterwards, off the reactor.
	let reads: Vec<_> = files
		.iter()
		.enumerate()
		.filter(|(_, (_, meta))| meta.size <= MAX_VIEW_FILE_BYTES)
		.map(|(i, (rel_path, _))| {
			let file_source = remote_child(source, rel_path);
			let rel_path = rel_path.clone();
			async move {
				let content = io_read(&file_source)
					.await
					.ok()
					.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
				(i, rel_path, content)
			}
		})
		.collect();
	let mut reads = join_bounded(reads).await?;
	reads.sort_by_key(|(i, _, _)| *i);

	let label = path.to_string();
	let output = tokio::task::spawn_blocking(move || {
		let blocks: Vec<String> = reads
			.into_iter()
			.filter_map(|(_, rel_path, content)| {
				let content = content?;
				let items = structural::outline(&rel_path, &content).ok()?;
				(!items.is_empty()).then(|| render_file_block(&rel_path, &content, &items, MAX_RESULT_BYTES))
			})
			.collect();
		join_file_blocks(&label, blocks, MAX_RESULT_BYTES)
	})
	.await
	.map_err(|e| anyhow!("Failed to extract outline: {e}"))?;
	Ok(output)
}

/// Grammar-supported files under a local directory, glob-filtered. `None`
/// when the glob matched no file at all — callers report that distinctly from
/// "matched, but nothing supported".
pub(super) fn supported_files_under(
	abs_dir_str: &str,
	working_dir: &Path,
	pattern: Option<&str>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<Option<Vec<String>>, String> {
	let pattern_matcher = pattern.map(build_pattern_matcher).transpose()?;
	let mut builder = build_walker(abs_dir_str, max_depth, include_hidden);
	let mut files = collect_file_paths(&mut builder, working_dir);
	if let Some(matcher) = pattern_matcher {
		apply_pattern(&mut files, &matcher);
		if files.is_empty() {
			return Ok(None);
		}
	}
	files.retain(|file| structural::is_supported(file));
	Ok(Some(files))
}

/// Grammar-supported files under a remote directory, glob-filtered. `None`
/// when the glob matched no file at all.
pub(super) async fn supported_remote_files(
	source: &PathSource,
	pattern: Option<&str>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<Option<Vec<(String, IoMetadata)>>> {
	let pattern_matcher = pattern
		.map(build_pattern_matcher)
		.transpose()
		.map_err(|e| anyhow!("{e}"))?;
	let mut files = collect_remote_files(source, max_depth, include_hidden).await?;
	if let Some(matcher) = pattern_matcher {
		let mut names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
		apply_pattern(&mut names, &matcher);
		if names.is_empty() {
			return Ok(None);
		}
		let keep: HashSet<String> = names.into_iter().collect();
		files.retain(|(name, _)| keep.contains(name));
	}
	files.retain(|(name, _)| structural::is_supported(name));
	Ok(Some(files))
}

/// Join per-file blocks under the result budget, dropping whole files from
/// the end when they do not fit. Returns the joined output and how many
/// blocks were dropped; the caller appends its own truncation notice.
pub(super) fn join_blocks_budgeted(blocks: &[String], budget: usize) -> (String, usize) {
	let mut used = 0usize;
	let mut kept: Vec<&String> = Vec::new();
	for block in blocks {
		if used + 2 + block.len() > budget.saturating_sub(TRUNCATION_NOTICE_RESERVE) {
			break;
		}
		used += 2 + block.len();
		kept.push(block);
	}
	let omitted = blocks.len() - kept.len();
	(kept.into_iter().cloned().collect::<Vec<_>>().join("\n\n"), omitted)
}

/// Render one file's item lines under `budget`, stopping before the
/// truncation notice reserve. Returns the rendered lines and how many items
/// were dropped.
fn render_item_lines(contents: &str, items: &[OutlineItem], budget: usize) -> (String, usize) {
	let lines: Vec<&str> = contents.lines().collect();
	let mut used = 0usize;
	let mut kept: Vec<String> = Vec::new();
	for item in items {
		let mut block = Vec::with_capacity(item.sig_lines);
		for n in item.start_line..item.start_line + item.sig_lines {
			// A broken parse can name a line past the file's end; render what
			// exists rather than failing.
			if let Some(line) = lines.get(n - 1) {
				block.push(format!("{}|{}", line_id(n, line), line));
			}
		}
		let block = block.join("\n");
		if block.is_empty() {
			continue;
		}
		if used + 1 + block.len() > budget.saturating_sub(TRUNCATION_NOTICE_RESERVE) {
			break;
		}
		used += 1 + block.len();
		kept.push(block);
	}
	let omitted = items.len() - kept.len();
	(kept.join("\n"), omitted)
}

/// The outline truncation notices, built by one helper so the item and file
/// variants cannot drift apart.
fn outline_truncation_notice(omitted: usize, unit: &str, remedy: &str) -> String {
	format!("[outline truncated: {omitted} {unit} omitted — {remedy}]")
}

/// Notice for items dropped from one file's outline.
fn items_truncation_notice(omitted: usize) -> String {
	outline_truncation_notice(omitted, "items", "narrow with `symbol` or a ranged view")
}

/// Notice for whole files dropped from a directory outline.
fn files_truncation_notice(omitted: usize) -> String {
	outline_truncation_notice(omitted, "files", "narrow `path`, `pattern` or `max_depth`")
}

/// Render one file's outline: a `path\tNL` header (the total line count bounds
/// the last item's body), then each item's signature lines as `N:hh|content`.
/// When the budget cuts the result, the dropped items are counted in a final
/// notice.
pub(super) fn render_file_outline(path: &str, contents: &str, items: &[OutlineItem], budget: usize) -> String {
	let lines_count = contents.lines().count();
	let mut out = format!("{path}\t{lines_count}");
	if items.is_empty() {
		out.push_str("\nNo declarations found.");
		return out;
	}
	let (body, omitted) = render_item_lines(contents, items, budget.saturating_sub(out.len() + 1));
	if !body.is_empty() {
		out.push('\n');
		out.push_str(&body);
	}
	if omitted > 0 {
		out.push('\n');
		out.push_str(&items_truncation_notice(omitted));
	}
	out
}

/// Render one file's block for a directory outline: the same `path:` header
/// style the tree content search uses for multi-file results, then the item
/// lines.
fn render_file_block(rel_path: &str, contents: &str, items: &[OutlineItem], budget: usize) -> String {
	let (body, omitted) = render_item_lines(contents, items, budget);
	let mut out = format!("{rel_path}:\n{body}");
	if omitted > 0 {
		out.push('\n');
		out.push_str(&items_truncation_notice(omitted));
	}
	out
}

/// Join per-file blocks under the result budget, dropping whole files from
/// the end when they do not fit, and say how many were dropped. Empty input
/// means nothing under the directory qualified.
fn join_file_blocks(path: &str, blocks: Vec<String>, budget: usize) -> String {
	if blocks.is_empty() {
		return format!("No supported files with declarations found under {path}.");
	}
	let (mut out, omitted) = join_blocks_budgeted(&blocks, budget);
	if out.is_empty() {
		return files_truncation_notice(omitted);
	}
	if omitted > 0 {
		out.push_str("\n\n");
		out.push_str(&files_truncation_notice(omitted));
	}
	out
}

#[cfg(test)]
#[path = "structure_tests.rs"]
mod structure_tests;
