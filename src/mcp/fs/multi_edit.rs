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

//! Ordered, in-memory edit planning and one atomic commit path for multi-file edits.

use super::remote::{io_exists, io_metadata, io_read_to_string, resolve_path_source, PathSource};
use super::text_editing::{
	acquire_file_lock, atomic_write, lock_key_for_source, replacement_diagnostic, sanitize_incoming_content,
	EditableContent,
};
use crate::mcp::McpToolCall;
use crate::utils::{line_hash::line_id_at, truncation::MAX_RESULT_BYTES};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
	path: Option<String>,
	old_text: String,
	new_text: String,
	#[serde(default)]
	replace_all: bool,
}

struct File {
	first_edit: usize,
	source: PathSource,
	// Path as the caller named it: canonicalizing can rewrite it (Windows `\\?\` prefix,
	// expanded 8.3 names, macOS `/private`), which callers would not recognize.
	display: String,
	before: String,
	after: String,
	permissions: Option<std::fs::Permissions>,
}

pub(super) async fn execute_edits(call: &McpToolCall) -> Result<String> {
	for field in ["old_text", "new_text", "replace_all"] {
		if call.parameters.get(field).is_some_and(|v| !v.is_null()) {
			bail!("Use edits or single old_text/new_text, not both");
		}
	}
	let values = call.parameters["edits"].as_array().context("edits must be an array")?;
	if values.is_empty() {
		bail!("edits must not be empty");
	}
	let default_path = call.parameters.get("path").and_then(|v| v.as_str());
	let mut sources = BTreeMap::new();
	let mut steps = Vec::new();
	for (index, value) in values.iter().enumerate() {
		let edit: Edit =
			serde_json::from_value(value.clone()).with_context(|| format!("edit index {index} (zero-based)"))?;
		let path = edit
			.path
			.as_deref()
			.or(default_path)
			.with_context(|| format!("edit index {index}: path is required"))?;
		let source = resolve(path, call).with_context(|| format!("edit index {index}"))?;
		let key = lock_key_for_source(&source);
		sources.entry(key.clone()).or_insert((source, index));
		steps.push((key, edit));
	}
	// A consistent global order prevents deadlock when calls overlap across files.
	let mut guards = Vec::new();
	for (source, _) in sources.values() {
		guards.push(acquire_file_lock(source).await?.lock_owned().await);
	}
	let mut files = BTreeMap::new();
	for (key, (source, index)) in sources {
		let file = load_file(source, index)
			.await
			.with_context(|| format!("edit index {index} (zero-based)"))?;
		files.insert(key, file);
	}

	for (index, (key, edit)) in steps.into_iter().enumerate() {
		let file = files.get_mut(&key).context("Missing staged file")?;
		file.after = replace(&file.after, &edit).with_context(|| format!("edit index {index} (zero-based)"))?;
	}
	// Render before committing: a result-size failure must not leave changes behind.
	let output = render(&files)?;
	// Once writes start, cancellation must not interrupt commit or rollback.
	let cache = crate::mcp::request_ctx::view_cache();
	tokio::spawn(crate::mcp::request_ctx::with_request_context(cache, async move {
		let _guards = guards;
		commit(files).await
	}))
	.await
	.context("Edit commit task failed")??;
	Ok(output)
}

fn resolve(path: &str, call: &McpToolCall) -> Result<PathSource> {
	if path.trim().is_empty() {
		bail!("File path must not be empty");
	}
	Ok(resolve_path_source(path, &call.workdir))
}

async fn load_file(source: PathSource, first_edit: usize) -> Result<File> {
	if !io_exists(&source).await? {
		bail!("File not found: {}", source.display());
	}
	if !io_metadata(&source).await?.is_file {
		bail!("Not a regular file: {}", source.display());
	}
	let before = io_read_to_string(&source)
		.await
		.with_context(|| format!("Cannot read {}", source.display()))?;
	let display = source.display().to_string();
	// Canonical targets keep aliases under one lock and preserve existing symlinks.
	let source = match source {
		PathSource::Local(path) => PathSource::Local(tokio::fs::canonicalize(path).await?),
		source => source,
	};
	let permissions = match &source {
		PathSource::Local(path) => Some(tokio::fs::metadata(path).await?.permissions()),
		PathSource::Remote { .. } => None,
	};
	Ok(File {
		first_edit,
		source,
		display,
		after: before.clone(),
		before,
		permissions,
	})
}

async fn commit(files: BTreeMap<String, File>) -> Result<()> {
	let mut attempted = Vec::new();
	for (key, file) in &files {
		if file.before == file.after {
			continue;
		}
		attempted.push(key);
		if let Err(error) = atomic_write(&file.source, &file.after).await {
			let mut failures = Vec::new();
			for key in attempted.into_iter().rev() {
				let file = &files[key];
				if let Err(rollback) = restore(file).await {
					failures.push(format!("{}: {rollback:#}", file.display));
				}
			}
			if failures.is_empty() {
				bail!(
					"edit index {}: commit failed; all file contents restored: {error:#}",
					file.first_edit
				);
			}
			bail!(
				"edit index {}: commit failed: {error:#}; rollback failed: {}",
				file.first_edit,
				failures.join("; ")
			);
		}
	}
	super::core::record_transaction_history(
		files
			.iter()
			.filter(|(_, f)| f.before != f.after)
			.map(|(key, f)| (key.clone(), Some(f.before.clone())))
			.collect(),
	);
	for file in files.values().filter(|f| f.before != f.after) {
		super::delta::note_write(&file.source, &file.before, &file.after);
	}
	Ok(())
}

fn replace(raw: &str, edit: &Edit) -> Result<String> {
	if edit.old_text.is_empty() {
		bail!("old_text must not be empty");
	}
	let mut content = EditableContent::from_raw(raw);
	let normalized = content.normalized();
	let old = edit.old_text.replace("\r\n", "\n");
	let new = sanitize_incoming_content(&edit.new_text);
	let positions: Vec<usize> = normalized.match_indices(&old).map(|(i, _)| i).collect();
	if positions.is_empty() {
		bail!("{}", replacement_diagnostic(&normalized, &old));
	}
	if positions.len() > 1 && !edit.replace_all {
		let lines: Vec<&str> = normalized.lines().collect();
		let ids: Vec<String> = positions
			.iter()
			.map(|&offset| line_id_at(&lines, normalized[..offset].bytes().filter(|&b| b == b'\n').count() + 1))
			.collect();
		bail!(
			"Found {} matches at: {}. Add context to make old_text unique or set replace_all: true.",
			positions.len(),
			ids.join(", ")
		);
	}
	for position in positions.into_iter().rev() {
		content.replace_normalized_range(position, position + old.len(), &new)?;
	}
	content.preserve_trailing_newline();
	Ok(content.rendered())
}

async fn restore(file: &File) -> Result<()> {
	if io_read_to_string(&file.source)
		.await
		.is_ok_and(|current| current == file.before)
	{
		return Ok(());
	}
	atomic_write(&file.source, &file.before).await?;
	if let (PathSource::Local(path), Some(permissions)) = (&file.source, &file.permissions) {
		tokio::fs::set_permissions(path, permissions.clone()).await?;
	}
	Ok(())
}

fn render(files: &BTreeMap<String, File>) -> Result<String> {
	let mut output = Vec::new();
	for file in files.values() {
		let old: Vec<&str> = file.before.lines().collect();
		let new: Vec<&str> = file.after.lines().collect();
		let diff = super::delta::render_edit_diff(&old, &new).unwrap_or_else(|| {
			let mut lines = Vec::new();
			if !old.is_empty() {
				lines.push(super::delta::removed_marker(&old, 0, old.len() - 1));
			}
			lines.extend(
				new.iter()
					.enumerate()
					.map(|(i, line)| format!("+{}|{line}", line_id_at(&new, i + 1))),
			);
			lines.join("\n")
		});
		output.push(format!("{}\n{}", file.display, diff));
	}
	let result = output.join("\n\n");
	if result.len() > MAX_RESULT_BYTES {
		bail!("Edit result too large; split into smaller calls");
	}
	Ok(result)
}

#[cfg(test)]
#[path = "multi_edit_tests.rs"]
mod tests;
