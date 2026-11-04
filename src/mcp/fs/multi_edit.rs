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

//! Ordered, in-memory edit planning and one commit path for all multi-file edits.

use super::patch::{
	file_update::compute_replacements,
	parser::{parse_patch, Hunk, UpdateFileChunk},
};
use super::remote::{
	io_create_dir_all, io_exists, io_metadata, io_read_to_string, io_remove_dir, io_remove_file, resolve_path_source,
	PathSource,
};
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

enum Operation {
	Replace(Edit),
	Add(String),
	Delete,
	Update {
		chunks: Vec<UpdateFileChunk>,
		destination: Option<String>,
	},
}

struct Step {
	key: String,
	operation: Operation,
}

struct File {
	first_edit: usize,
	source: PathSource,
	before: Option<String>,
	after: Option<String>,
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
	let mut operations = Vec::new();
	for (index, value) in values.iter().enumerate() {
		let edit: Edit =
			serde_json::from_value(value.clone()).with_context(|| format!("edit index {index} (zero-based)"))?;
		let path = edit
			.path
			.as_deref()
			.or(default_path)
			.with_context(|| format!("edit index {index}: path is required"))?
			.to_string();
		operations.push((path, Operation::Replace(edit)));
	}
	apply(call, operations).await
}

pub(super) async fn execute_patch(call: &McpToolCall) -> Result<String> {
	let patch = call
		.parameters
		.get("patch")
		.and_then(|v| v.as_str())
		.context("patch must be a string")?;
	let hunks = parse_patch(patch)?;
	if hunks.is_empty() {
		bail!("Patch contains no file sections");
	}
	let operations = hunks
		.into_iter()
		.map(|hunk| match hunk {
			Hunk::AddFile { path, contents } => (path, Operation::Add(contents)),
			Hunk::DeleteFile { path } => (path, Operation::Delete),
			Hunk::UpdateFile {
				path,
				move_path,
				chunks,
			} => (
				path,
				Operation::Update {
					chunks,
					destination: move_path,
				},
			),
		})
		.collect();
	apply(call, operations).await
}

fn resolve(path: &str, call: &McpToolCall) -> Result<PathSource> {
	if path.trim().is_empty() {
		bail!("File path must not be empty");
	}
	Ok(resolve_path_source(path, &call.workdir))
}

async fn apply(call: &McpToolCall, operations: Vec<(String, Operation)>) -> Result<String> {
	let mut sources = BTreeMap::new();
	let mut steps = Vec::new();
	for (index, (path, mut operation)) in operations.into_iter().enumerate() {
		let source = resolve(&path, call).with_context(|| format!("edit index {index}"))?;
		let key = lock_key_for_source(&source);
		sources.entry(key.clone()).or_insert((source, index));
		if let Operation::Update {
			destination: Some(destination),
			..
		} = &mut operation
		{
			let source = resolve(destination, call).with_context(|| format!("edit index {index}"))?;
			*destination = lock_key_for_source(&source);
			sources.entry(destination.clone()).or_insert((source, index));
		}
		steps.push(Step { key, operation });
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

	for (index, step) in steps.into_iter().enumerate() {
		plan_step(&mut files, step).with_context(|| format!("edit index {index} (zero-based)"))?;
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

async fn load_file(source: PathSource, first_edit: usize) -> Result<File> {
	let before = if io_exists(&source).await? {
		if !io_metadata(&source).await?.is_file {
			bail!("Not a regular file: {}", source.display());
		}
		Some(
			io_read_to_string(&source)
				.await
				.with_context(|| format!("Cannot read {}", source.display()))?,
		)
	} else {
		None
	};
	// Canonical targets keep aliases under one lock and preserve existing symlinks.
	let source = match source {
		PathSource::Local(path) if before.is_some() => PathSource::Local(tokio::fs::canonicalize(path).await?),
		source => source,
	};
	let permissions = match &source {
		PathSource::Local(path) if before.is_some() => Some(tokio::fs::metadata(path).await?.permissions()),
		_ => None,
	};
	Ok(File {
		first_edit,
		source,
		after: before.clone(),
		before,
		permissions,
	})
}

async fn commit(files: BTreeMap<String, File>) -> Result<()> {
	let directories = missing_parents(&files).await?;
	let mut attempted = Vec::new();
	for (key, file) in &files {
		if file.before == file.after {
			continue;
		}
		attempted.push(key);
		if let Err(error) = write_state(&file.source, file.after.as_deref()).await {
			let mut failures = Vec::new();
			for key in attempted.into_iter().rev() {
				let file = &files[key];
				if let Err(rollback) = restore(file).await {
					failures.push(format!("{}: {rollback:#}", file.source.display()));
				}
			}
			for directory in &directories {
				if io_exists(directory).await.unwrap_or(false) {
					if let Err(error) = io_remove_dir(directory).await {
						failures.push(format!("{}: {error:#}", directory.display()));
					}
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
			.map(|(key, f)| (key.clone(), f.before.clone()))
			.collect(),
	);
	for file in files.values() {
		if file.before == file.after {
			continue;
		}
		match (&file.before, &file.after) {
			(Some(before), Some(after)) => super::delta::note_write(&file.source, before, after),
			(_, Some(after)) => super::delta::note_create(&file.source, after),
			(_, None) => super::delta::forget(&file.source),
		}
	}
	Ok(())
}

fn plan_step(files: &mut BTreeMap<String, File>, step: Step) -> Result<()> {
	let file = files.get_mut(&step.key).context("Missing staged file")?;
	match step.operation {
		Operation::Replace(edit) => {
			let content = file.after.as_ref().context("File not found")?;
			file.after = Some(replace(content, &edit)?);
		}
		Operation::Add(contents) => {
			if file.after.is_some() {
				bail!("File already exists: {}", file.source.display());
			}
			file.after = Some(contents);
		}
		Operation::Delete => {
			if file.after.take().is_none() {
				bail!("File not found: {}", file.source.display());
			}
		}
		Operation::Update { chunks, destination } => {
			let raw = file.after.as_ref().context("File not found")?;
			let mut content = EditableContent::from_raw(raw);
			let normalized = content.normalized();
			let lines: Vec<String> = normalized.lines().map(str::to_string).collect();
			let replacements = compute_replacements(&lines, &file.source.display(), &chunks)?;
			// Byte ranges refer to the original snapshot, so apply from the end.
			let mut offsets = vec![0];
			for line in normalized.split_inclusive('\n') {
				offsets.push(offsets.last().copied().unwrap_or(0) + line.len());
			}
			for (start, count, new_lines) in replacements.into_iter().rev() {
				let mut replacement = new_lines.join("\n");
				if !new_lines.is_empty() {
					replacement.push('\n');
					if start == lines.len() && !normalized.is_empty() && !normalized.ends_with('\n') {
						replacement.insert(0, '\n');
					}
				}
				content.replace_normalized_range(offsets[start], offsets[start + count], &replacement)?;
			}
			content.preserve_trailing_newline();
			let updated = content.rendered();
			if let Some(destination) = destination.filter(|d| d != &step.key) {
				file.after = None;
				let target = files.get_mut(&destination).context("Missing move destination")?;
				if target.after.is_some() {
					bail!("Move destination already exists: {}", target.source.display());
				}
				target.after = Some(updated);
			} else {
				file.after = Some(updated);
			}
		}
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

async fn missing_parents(files: &BTreeMap<String, File>) -> Result<Vec<PathSource>> {
	let mut directories = BTreeMap::new();
	for file in files.values().filter(|f| f.after.is_some() && f.before != f.after) {
		let mut parent = file.source.parent();
		while let Some(source) = parent {
			let key = lock_key_for_source(&source);
			if files.get(&key).is_some_and(|f| f.after.is_some()) {
				bail!("File is also used as a parent directory: {}", source.display());
			}
			if io_exists(&source).await? {
				if !io_metadata(&source).await?.is_dir {
					bail!("Parent is not a directory: {}", source.display());
				}
				break;
			}
			parent = source.parent();
			directories.insert(key, source);
		}
	}
	// Children must be removed before their parents during rollback.
	let mut directories: Vec<_> = directories.into_values().collect();
	directories.sort_by_key(|source| std::cmp::Reverse(source.display().len()));
	Ok(directories)
}

async fn write_state(source: &PathSource, content: Option<&str>) -> Result<()> {
	match content {
		Some(content) => {
			if let Some(parent) = source.parent() {
				io_create_dir_all(&parent).await?;
			}
			atomic_write(source, content).await
		}
		None if io_exists(source).await? => io_remove_file(source).await,
		None => Ok(()),
	}
}

async fn restore(file: &File) -> Result<()> {
	if let Some(before) = &file.before {
		if io_read_to_string(&file.source).await.ok().as_ref() == Some(before) {
			return Ok(());
		}
	}
	write_state(&file.source, file.before.as_deref()).await?;
	if let (PathSource::Local(path), Some(permissions)) = (&file.source, &file.permissions) {
		tokio::fs::set_permissions(path, permissions.clone()).await?;
	}
	Ok(())
}

fn render(files: &BTreeMap<String, File>) -> Result<String> {
	let mut output = Vec::new();
	for file in files.values() {
		let old: Vec<&str> = file.before.as_deref().unwrap_or("").lines().collect();
		let new: Vec<&str> = file.after.as_deref().unwrap_or("").lines().collect();
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
		output.push(format!("{}\n{}", file.source.display(), diff));
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
