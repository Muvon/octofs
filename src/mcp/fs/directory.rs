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

// Directory operations module — file listing and content search using ignore + pure-Rust matching.

use super::super::McpToolCall;
use super::remote::{
	io_list_dir, io_read, io_read_to_string, resolve_path_source, IoMetadata, PathSource,
};
use super::search::{self, Matcher};
use crate::utils::line_hash::line_id_at;
use crate::utils::truncation::estimate_tokens;
use anyhow::{bail, Result};
use ignore::{overrides::Override, overrides::OverrideBuilder, WalkBuilder};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

// Listing annotations are a pure function of file content; keying on (mtime, len)
// makes a repeat listing of an unchanged tree stat-only instead of re-reading every
// file. Same fingerprint editors use for external-change detection.
type AnnotationCache = HashMap<PathBuf, (SystemTime, u64, String)>;
static ANNOTATIONS: OnceLock<Mutex<AnnotationCache>> = OnceLock::new();
const ANNOTATION_CACHE_MAX: usize = 100_000;

fn annotation_cache() -> &'static Mutex<AnnotationCache> {
	ANNOTATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

// Annotation suffix for a listed file: "NL\t~Nt" or "(binary)". None = unreadable.
fn annotation_suffix(full_path: &Path, mtime: Option<SystemTime>, len: u64) -> Option<String> {
	if let Some(mt) = mtime {
		if let Some((c_mt, c_len, suffix)) = annotation_cache()
			.lock()
			.expect("annotation cache poisoned")
			.get(full_path)
		{
			if *c_mt == mt && *c_len == len {
				return Some(suffix.clone());
			}
		}
	}

	let bytes = std::fs::read(full_path).ok()?;
	// Skip likely binary files: NUL-density check on a leading sample.
	let sample_size = bytes.len().min(512);
	let null_count = bytes[..sample_size].iter().filter(|&&b| b == 0).count();
	let suffix = if null_count > sample_size / 10 {
		"(binary)".to_string()
	} else {
		let text = String::from_utf8_lossy(&bytes);
		format!("{}L\t~{}t", text.lines().count(), estimate_tokens(&text))
	};

	if let Some(mt) = mtime {
		let mut cache = annotation_cache()
			.lock()
			.expect("annotation cache poisoned");
		// ponytail: crude bound — clear and rebuild lazily instead of tracking LRU.
		if cache.len() >= ANNOTATION_CACHE_MAX {
			cache.clear();
		}
		cache.insert(full_path.to_path_buf(), (mt, len, suffix.clone()));
	}
	Some(suffix)
}
// `|` predates the rg-compatible matcher and remains a convenient way to pass
// several ordered -g-style globs through one MCP string. Do not split escaped
// pipes or pipes inside character classes, where `|` names a literal character.
fn split_pattern_alternatives(pattern: &str) -> Result<Vec<&str>, String> {
	let mut alternatives = Vec::new();
	let mut start = 0;
	let mut escaped = false;
	let mut in_class = false;

	for (index, ch) in pattern.char_indices() {
		if escaped {
			escaped = false;
			continue;
		}
		match ch {
			'\\' => escaped = true,
			'[' if !in_class => in_class = true,
			']' if in_class => in_class = false,
			'|' if !in_class => {
				let alternative = pattern[start..index].trim();
				if alternative.is_empty() {
					return Err(format!(
						"Invalid `pattern` glob `{pattern}`: every `|`-separated glob must be non-empty"
					));
				}
				alternatives.push(alternative);
				start = index + ch.len_utf8();
			}
			_ => {}
		}
	}

	let alternative = pattern[start..].trim();
	if alternative.is_empty() {
		return Err(format!(
			"Invalid `pattern` glob `{pattern}`: every `|`-separated glob must be non-empty"
		));
	}
	alternatives.push(alternative);
	Ok(alternatives)
}

// Build with the same gitignore-style override engine used for ripgrep's -g/--glob.
// Positive globs include, leading-! globs exclude, and later alternatives take precedence.
fn build_pattern_matcher(pattern: &str, root: &Path) -> Result<Override, String> {
	let alternatives = split_pattern_alternatives(pattern)?;
	let mut builder = OverrideBuilder::new(root);
	for glob in alternatives {
		builder
			.add(glob)
			.map_err(|e| format!("Invalid `pattern` glob `{glob}`: {e}"))?;
	}
	builder
		.build()
		.map_err(|e| format!("Invalid `pattern` glob `{pattern}`: {e}"))
}

#[cfg(test)]
fn filter_by_pattern(files: &mut Vec<String>, pattern: &str) -> Result<(), String> {
	let matcher = build_pattern_matcher(pattern, Path::new(""))?;
	files.retain(|file| !matcher.matched(file, false).is_ignore());
	Ok(())
}

// Build an ignore::WalkBuilder with the given options
fn build_walker(directory: &str, max_depth: Option<usize>, include_hidden: bool) -> WalkBuilder {
	let mut builder = WalkBuilder::new(directory);
	builder
		.git_ignore(true)
		.git_global(true)
		.git_exclude(true)
		.require_git(false)
		.follow_links(false)
		.hidden(!include_hidden);
	if let Some(depth) = max_depth {
		builder.max_depth(Some(depth));
	}
	builder
}

// Collect file paths from walker, relative to working_dir
fn collect_file_paths(builder: &mut WalkBuilder, working_dir: &Path) -> Vec<String> {
	let walker = builder.build();
	let mut files: Vec<String> = Vec::new();
	for entry in walker.flatten() {
		let path = entry.path();
		if !path.is_file() {
			continue;
		}
		let mut rel = path
			.strip_prefix(working_dir)
			.unwrap_or(path)
			.to_string_lossy()
			.to_string();
		// Normalize Windows separators so listings and `/`-containing glob patterns
		// behave identically on all platforms ('\' is a legal filename char on Unix,
		// so only rewrite it where it is a separator).
		if cfg!(windows) {
			rel = rel.replace('\\', "/");
		}
		files.push(rel);
	}
	files.sort();
	files
}

// --- Remote directory listing (SFTP) ---

// Construct a child PathSource for a remote directory entry.
fn remote_child(source: &PathSource, name: &str) -> PathSource {
	match source {
		PathSource::Remote {
			host,
			port,
			user,
			path,
		} => {
			let new_path = if path.ends_with('/') {
				format!("{path}{name}")
			} else {
				format!("{path}/{name}")
			};
			PathSource::Remote {
				host: host.clone(),
				port: *port,
				user: user.clone(),
				path: new_path,
			}
		}
		_ => unreachable!("remote_child called on non-remote source"),
	}
}

// Recursively list remote files, returning relative paths with the metadata
// the directory listing already provided — one read_dir round trip per
// directory and NOTHING per file. Directories matching the root .gitignore
// are pruned BEFORE recursing: without this a repo listing walks target/,
// node_modules/ etc, thousands of round trips.
async fn collect_remote_files(
	source: &PathSource,
	base_rel: &str,
	max_depth: Option<usize>,
	include_hidden: bool,
	gitignore: Option<&ignore::gitignore::Gitignore>,
	overrides: Option<&Override>,
	current_depth: usize,
) -> Result<Vec<(String, IoMetadata)>> {
	let mut files = Vec::new();
	let entries = io_list_dir(source).await?;

	for (name, meta) in entries {
		let rel_path = if base_rel.is_empty() {
			name.clone()
		} else {
			format!("{base_rel}/{name}")
		};

		// rg-style -g overrides have higher precedence than hidden and ignore rules.
		let override_match = overrides.map(|matcher| matcher.matched(&rel_path, meta.is_dir));
		if override_match.as_ref().is_some_and(|m| m.is_ignore()) {
			continue;
		}
		let explicitly_included = override_match.as_ref().is_some_and(|m| m.is_whitelist());

		if !explicitly_included && !include_hidden && name.starts_with('.') {
			continue;
		}

		if !explicitly_included {
			if let Some(gi) = gitignore {
				if gi
					.matched_path_or_any_parents(&rel_path, meta.is_dir)
					.is_ignore()
				{
					continue;
				}
			}
		}

		if meta.is_dir {
			if let Some(max_d) = max_depth {
				if current_depth >= max_d {
					continue;
				}
			}
			let sub_source = remote_child(source, &name);
			let sub_files = Box::pin(collect_remote_files(
				&sub_source,
				&rel_path,
				max_depth,
				include_hidden,
				gitignore,
				overrides,
				current_depth + 1,
			))
			.await?;
			files.extend(sub_files);
		} else {
			files.push((rel_path, meta));
		}
	}

	files.sort_by(|a, b| a.0.cmp(&b.0));
	Ok(files)
}

// Annotation suffix for a remote file, from metadata ONLY — downloading each
// file for line/token counts made listing a large tree take minutes.
fn annotation_suffix_remote(meta: &IoMetadata) -> String {
	if meta.size > super::file_ops::MAX_VIEW_FILE_BYTES {
		format!("(large: {}MB)", meta.size / (1024 * 1024))
	} else if meta.size >= 1024 {
		format!("{}KB", meta.size / 1024)
	} else {
		format!("{}B", meta.size)
	}
}

// List a remote directory — file listing or content search via SFTP.
// Honors the .gitignore at the listed root (nested ones are not consulted);
// hidden files are controlled by `include_hidden` like the local path.
async fn list_directory_remote(
	source: &PathSource,
	pattern: Option<String>,
	content: Option<String>,
	max_depth: Option<usize>,
	include_hidden: bool,
	context_lines: usize,
	regex_flag: bool,
) -> Result<String> {
	let has_content = content.as_ref().is_some_and(|c| !c.trim().is_empty());

	// Root .gitignore (repo checkouts always have one) — parsed with the same
	// `ignore` crate the local walker uses. ponytail: root file only; nested
	// .gitignore support if someone actually hits it.
	let gitignore = match io_read_to_string(&remote_child(source, ".gitignore")).await {
		Ok(text) => {
			let mut builder = ignore::gitignore::GitignoreBuilder::new("");
			for line in text.lines() {
				let _ = builder.add_line(None, line);
			}
			builder.build().ok()
		}
		Err(_) => None,
	};
	let pattern_matcher = pattern
		.as_deref()
		.map(|glob| build_pattern_matcher(glob, Path::new("")))
		.transpose()
		.map_err(|e| anyhow::anyhow!(e))?;

	let files = collect_remote_files(
		source,
		"",
		max_depth,
		include_hidden,
		gitignore.as_ref(),
		pattern_matcher.as_ref(),
		0,
	)
	.await?;

	if files.is_empty() {
		if let Some(ref name_pattern) = pattern {
			return Ok(format!("No files matched pattern \"{name_pattern}\"."));
		}
	}

	if has_content {
		let content_pattern = content.unwrap();
		let matcher = Matcher::new(&content_pattern, regex_flag)?;

		let mut results: Vec<String> = Vec::new();
		for (rel_path, meta) in &files {
			if meta.size > super::file_ops::MAX_VIEW_FILE_BYTES {
				continue;
			}
			let file_source = remote_child(source, rel_path);

			let bytes = match io_read(&file_source).await {
				Ok(b) => b,
				Err(_) => continue,
			};

			let sample_size = bytes.len().min(512);
			let null_count = bytes[..sample_size].iter().filter(|&&b| b == 0).count();
			if null_count > sample_size / 10 {
				continue;
			}

			let file_content = String::from_utf8_lossy(&bytes);
			let blocks = search::search_lines(&file_content, &matcher, context_lines);
			if blocks.is_empty() {
				continue;
			}

			let file_lines: Vec<&str> = file_content.lines().collect();

			let mut rendered_blocks: Vec<String> = Vec::new();
			for block in &blocks {
				let mut rendered = Vec::new();
				for &n in &block.line_numbers {
					if n <= file_lines.len() {
						rendered.push(format!(
							"{}|{}",
							line_id_at(&file_lines, n),
							file_lines[n - 1]
						));
					}
				}
				rendered_blocks.push(rendered.join("\n"));
			}

			results.push(format!("{}:\n{}", rel_path, rendered_blocks.join("\n--\n")));
		}

		Ok(results.join("\n\n"))
	} else {
		let mut lines: Vec<String> = Vec::new();
		for (rel_path, meta) in &files {
			lines.push(format!("{}\t{}", rel_path, annotation_suffix_remote(meta)));
		}
		Ok(lines.join("\n"))
	}
}

// Execute list_directory — file listing or content search
pub async fn list_directory(call: &McpToolCall, directory: &str) -> Result<String> {
	let pattern = call
		.parameters
		.get("pattern")
		.and_then(|v| v.as_str())
		.map(|s| s.to_string());
	let content = call
		.parameters
		.get("content")
		.and_then(|v| v.as_str())
		.map(|s| s.to_string());
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
	let context_lines = call
		.parameters
		.get("context")
		.and_then(|v| v.as_i64())
		.unwrap_or(0) as usize;

	let working_dir = call.workdir.clone();

	// Remote paths (ssh://, sftp://) dispatch to the SFTP listing path.
	let source = resolve_path_source(directory, &working_dir);
	if source.is_remote() {
		let regex_flag = call
			.parameters
			.get("regex")
			.and_then(|v| v.as_bool())
			.unwrap_or(false);
		return list_directory_remote(
			&source,
			pattern,
			content,
			max_depth,
			include_hidden,
			context_lines,
			regex_flag,
		)
		.await;
	}

	let abs_dir = if Path::new(directory).is_absolute() {
		std::path::PathBuf::from(directory)
	} else {
		working_dir.join(directory)
	};
	let abs_dir_str = abs_dir.to_string_lossy().to_string();

	let has_content = content.as_ref().is_some_and(|c| !c.trim().is_empty());

	if has_content {
		// Content search mode
		let content_pattern = content.unwrap();
		let regex_flag = call
			.parameters
			.get("regex")
			.and_then(|v| v.as_bool())
			.unwrap_or(false);

		// Compile matcher up front so invalid regex fails fast with a clear error.
		let matcher = Matcher::new(&content_pattern, regex_flag)?;

		let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
			let mut builder = build_walker(&abs_dir_str, max_depth, include_hidden);
			// Install the matcher on the walker so -g-style rules override .gitignore and
			// hidden-file filtering with the same precedence as ripgrep.
			if let Some(ref name_pattern) = pattern {
				builder.overrides(build_pattern_matcher(name_pattern, &working_dir)?);
			}
			let files = collect_file_paths(&mut builder, &working_dir);
			if files.is_empty() {
				if let Some(ref name_pattern) = pattern {
					return Ok(format!("No files matched pattern \"{name_pattern}\"."));
				}
			}

			// Parallel per-file scan. Each thread reads + searches independently;
			// results carry the original index so the final output preserves
			// the deterministic alphabetic order of `files`.
			let mut indexed: Vec<(usize, String)> = files
				.par_iter()
				.enumerate()
				.filter_map(|(i, rel_path)| {
					let full_path = working_dir.join(rel_path);

					// ponytail: files over the view cap are skipped like binaries — loading
					// a multi-GB artifact into memory (twice, with the lossy copy) is an OOM
					// hazard, and no real source file is that large.
					let meta = std::fs::metadata(&full_path).ok()?;
					if meta.len() > super::file_ops::MAX_VIEW_FILE_BYTES {
						return None;
					}

					let bytes = std::fs::read(&full_path).ok()?;

					// Skip likely binary files: NUL-density check on a leading sample.
					let sample_size = bytes.len().min(512);
					let null_count = bytes[..sample_size].iter().filter(|&&b| b == 0).count();
					if null_count > sample_size / 10 {
						return None;
					}

					// Lossy UTF-8 conversion lets us search Latin-1, mixed encodings,
					// and BOM-prefixed UTF-8 files without panicking. Invalid byte
					// sequences become U+FFFD; line structure is preserved.
					let file_content = String::from_utf8_lossy(&bytes);

					let blocks = search::search_lines(&file_content, &matcher, context_lines);
					if blocks.is_empty() {
						return None;
					}

					let file_lines: Vec<&str> = file_content.lines().collect();

					let mut rendered_blocks: Vec<String> = Vec::new();
					for block in &blocks {
						let mut rendered = Vec::new();
						for &n in &block.line_numbers {
							if n <= file_lines.len() {
								rendered.push(format!(
									"{}|{}",
									line_id_at(&file_lines, n),
									file_lines[n - 1]
								));
							}
						}
						rendered_blocks.push(rendered.join("\n"));
					}

					Some((
						i,
						format!("{}:\n{}", rel_path, rendered_blocks.join("\n--\n")),
					))
				})
				.collect();

			indexed.sort_by_key(|(i, _)| *i);
			let file_results: Vec<String> = indexed.into_iter().map(|(_, s)| s).collect();
			Ok(file_results.join("\n\n"))
		})
		.await;

		match output {
			Ok(Ok(s)) => Ok(s),
			Ok(Err(e)) => bail!("{}", e),
			Err(join_err) => bail!("Failed to execute content search: {}", join_err),
		}
	} else {
		// File listing mode — annotate each file with line count + estimated tokens.
		let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
			let mut builder = build_walker(&abs_dir_str, max_depth, include_hidden);
			if let Some(ref name_pattern) = pattern {
				builder.overrides(build_pattern_matcher(name_pattern, &working_dir)?);
			}
			let files = collect_file_paths(&mut builder, &working_dir);
			if files.is_empty() {
				if let Some(ref name_pattern) = pattern {
					return Ok(format!("No files matched pattern \"{name_pattern}\"."));
				}
			}

			// Parallel annotation (cached by mtime+len — see annotation_suffix).
			// Order is preserved via the carried index so output stays alphabetic.
			let mut indexed: Vec<(usize, String)> = files
				.par_iter()
				.enumerate()
				.map(|(i, rel_path)| {
					let full_path = working_dir.join(rel_path);
					let line = match std::fs::metadata(&full_path) {
						// Annotate oversized files from metadata alone — reading a huge
						// artifact just to count its lines wastes I/O and memory.
						Ok(meta) if meta.len() > super::file_ops::MAX_VIEW_FILE_BYTES => {
							format!("{}\t(large: {}MB)", rel_path, meta.len() / (1024 * 1024))
						}
						Ok(meta) => {
							match annotation_suffix(&full_path, meta.modified().ok(), meta.len()) {
								Some(suffix) => format!("{}\t{}", rel_path, suffix),
								None => rel_path.clone(),
							}
						}
						Err(_) => rel_path.clone(),
					};
					(i, line)
				})
				.collect();

			indexed.sort_by_key(|(i, _)| *i);
			let lines: Vec<String> = indexed.into_iter().map(|(_, s)| s).collect();
			Ok(lines.join("\n"))
		})
		.await;

		match output {
			Ok(Ok(s)) => Ok(s),
			Ok(Err(e)) => bail!("{}", e),
			Err(join_err) => bail!("Failed to execute directory listing: {}", join_err),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn test_pattern_uses_ripgrep_glob_semantics() {
		let all = vec![
			"Cargo.toml".to_string(),
			"src/main.rs".to_string(),
			"src/nested/lib.rs".to_string(),
			"src/generated/code.rs".to_string(),
			"src/main.rsx".to_string(),
			"notes.txt".to_string(),
		];

		let mut brace_alternation = all.clone();
		filter_by_pattern(&mut brace_alternation, "*.{rs,toml}").unwrap();
		assert!(brace_alternation.contains(&"Cargo.toml".to_string()));
		assert!(brace_alternation.contains(&"src/nested/lib.rs".to_string()));
		assert!(!brace_alternation.contains(&"src/main.rsx".to_string()));

		let mut single_star = all.clone();
		filter_by_pattern(&mut single_star, "src/*.rs").unwrap();
		assert_eq!(single_star, vec!["src/main.rs"]);

		let mut double_star_with_exclusion = all;
		filter_by_pattern(
			&mut double_star_with_exclusion,
			"src/**/*.rs|!src/generated/**",
		)
		.unwrap();
		assert_eq!(
			double_star_with_exclusion,
			vec!["src/main.rs", "src/nested/lib.rs"]
		);

		let mut later_include_wins = vec![
			"src/generated/drop.rs".to_string(),
			"src/generated/keep.rs".to_string(),
		];
		filter_by_pattern(
			&mut later_include_wins,
			"src/**/*.rs|!src/generated/**|src/generated/keep.rs",
		)
		.unwrap();
		assert_eq!(later_include_wins, vec!["src/generated/keep.rs"]);
	}

	#[test]
	fn test_pattern_pipe_escaping_and_validation() {
		let mut files = vec!["report|final.md".to_string(), "report.md".to_string()];
		filter_by_pattern(&mut files, r"report[|]final.md").unwrap();
		assert_eq!(files, vec!["report|final.md"]);

		let mut files = vec!["a.rs".to_string()];
		let err = filter_by_pattern(&mut files, "*.rs|").unwrap_err();
		assert!(err.contains("non-empty"), "got: {err}");

		let err = filter_by_pattern(&mut files, "[unclosed").unwrap_err();
		assert!(err.contains("Invalid `pattern` glob"), "got: {err}");
	}

	#[test]
	fn test_content_search_with_special_chars() {
		// Verify that special regex characters in patterns are treated as literals
		let content = "line1\nbackward_step()\nline3\n";
		let blocks = search::search_content(content, "backward_step()", 0);
		assert_eq!(blocks.len(), 1);
		assert_eq!(blocks[0].line_numbers, vec![2]);
	}

	#[tokio::test]
	async fn test_listing_annotation_updates_after_modification() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		let file = temp_path.join("data.txt");
		fs::write(&file, "a\nb\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let first = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();
		assert!(first.contains("2L"), "got: {first}");

		fs::write(&file, "a\nb\nc\nd\n").unwrap();
		let second = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();
		assert!(
			second.contains("4L"),
			"cache must not serve a stale annotation: {second}"
		);
	}

	#[tokio::test]
	async fn test_pattern_matches_filename_in_subdirectories() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		let nested = temp_path.join("app/src/Plugin/Order");
		fs::create_dir_all(&nested).unwrap();
		fs::write(nested.join("Model.php"), "<?php\n").unwrap();
		fs::write(nested.join("Controller.php"), "<?php\n").unwrap();
		fs::write(temp_path.join("Model.php"), "<?php\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({ "pattern": "Model.php" }),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();

		assert!(
			result.contains("app/src/Plugin/Order/Model.php"),
			"bare filename pattern must match in subdirectories: {result}"
		);
		assert!(result.contains("Model.php\t"), "top-level match: {result}");
		assert!(!result.contains("Controller.php"), "got: {result}");
	}

	#[tokio::test]
	async fn test_pattern_no_matches_returns_explicit_message() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		fs::write(temp_path.join("main.rs"), "fn main() {}\n").unwrap();

		for parameters in [
			json!({ "pattern": "*lighthouse*" }),
			json!({ "pattern": "*lighthouse*", "content": "fn main" }),
		] {
			let call = McpToolCall {
				tool_name: "view".to_string(),
				parameters,
				tool_id: "test-call-id".to_string(),
				workdir: temp_path.to_path_buf(),
			};

			let result = list_directory(&call, temp_path.to_str().unwrap())
				.await
				.unwrap();
			assert_eq!(result, "No files matched pattern \"*lighthouse*\".");
		}
	}

	#[tokio::test]
	async fn test_pattern_with_slash_matches_relative_path() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		let sub = temp_path.join("sub");
		fs::create_dir_all(&sub).unwrap();
		fs::write(sub.join("a.rs"), "fn main() {}\n").unwrap();
		fs::write(temp_path.join("b.rs"), "fn main() {}\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({ "pattern": "sub/*.rs" }),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();

		assert!(result.contains("sub/a.rs"), "got: {result}");
		assert!(!result.contains("b.rs"), "got: {result}");
	}

	#[tokio::test]
	async fn test_content_search_pattern_matches_filename_in_subdirectories() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		let nested = temp_path.join("app/src");
		fs::create_dir_all(&nested).unwrap();
		fs::write(nested.join("Model.php"), "class Model { needle }\n").unwrap();
		fs::write(nested.join("Other.php"), "needle here too\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"pattern": "Model.php",
				"content": "needle"
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();

		assert!(
			result.contains("app/src/Model.php"),
			"bare filename pattern must narrow content search in subdirectories: {result}"
		);
		assert!(!result.contains("Other.php"), "got: {result}");
	}

	#[tokio::test]
	async fn test_content_search_respects_pattern_filter() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		fs::write(temp_path.join("code.rs"), "let needle = 1;\n").unwrap();
		fs::write(temp_path.join("notes.txt"), "needle here too\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"pattern": "*.rs",
				"content": "needle"
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, temp_path.to_str().unwrap())
			.await
			.unwrap();

		assert!(result.contains("code.rs"), "got: {result}");
		assert!(
			!result.contains("notes.txt"),
			"pattern must filter content search too: {result}"
		);
	}

	#[tokio::test]
	async fn test_content_search_supports_recursive_glob_and_exclusion() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		fs::create_dir_all(temp_path.join("src/nested")).unwrap();
		fs::create_dir_all(temp_path.join("src/generated")).unwrap();
		fs::write(temp_path.join("src/main.rs"), "needle\n").unwrap();
		fs::write(temp_path.join("src/nested/lib.rs"), "needle\n").unwrap();
		fs::write(temp_path.join("src/generated/code.rs"), "needle\n").unwrap();
		fs::write(temp_path.join("src/nested/readme.md"), "needle\n").unwrap();
		let matcher = build_pattern_matcher("src/**/*.rs|!src/generated/**", temp_path).unwrap();
		assert!(matcher
			.matched(temp_path.join("src/main.rs"), false)
			.is_whitelist());

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"path": ".",
				"content": "needle",
				"pattern": "src/**/*.rs|!src/generated/**"
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, ".").await.unwrap();
		assert!(result.contains("src/main.rs"), "got: {result}");
		assert!(result.contains("src/nested/lib.rs"), "got: {result}");
		assert!(!result.contains("src/generated/code.rs"), "got: {result}");
		assert!(!result.contains("src/nested/readme.md"), "got: {result}");
	}

	#[tokio::test]
	async fn test_pattern_override_reincludes_gitignored_path() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();
		fs::write(temp_path.join(".gitignore"), "keep.rs\n").unwrap();
		fs::write(temp_path.join("keep.rs"), "needle\n").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"path": ".",
				"content": "needle",
				"pattern": "keep.rs"
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(&call, ".").await.unwrap();
		assert!(result.contains("keep.rs"), "got: {result}");
	}

	#[tokio::test]
	async fn test_list_files_empty_content_should_list_files() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();

		for i in 1..=5 {
			let file_path = temp_path.join(format!("test_file_{}.txt", i));
			fs::write(&file_path, format!("Content of file {}", i)).unwrap();
		}

		let config_path = temp_path.join("config.json");
		fs::write(&config_path, "{}").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"directory": temp_path.to_str().unwrap(),
				"pattern": "*.json",
				"content": ""
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(
			&call,
			call.parameters
				.get("directory")
				.and_then(|v| v.as_str())
				.unwrap_or("."),
		)
		.await
		.unwrap();

		assert!(result.contains("config.json"));
	}

	#[tokio::test]
	async fn test_list_files_no_content_parameter_should_list_files() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();

		for i in 1..=5 {
			let file_path = temp_path.join(format!("test_file_{}.txt", i));
			fs::write(&file_path, format!("Content of file {}", i)).unwrap();
		}

		let config_path = temp_path.join("config.json");
		fs::write(&config_path, "{}").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"directory": temp_path.to_str().unwrap(),
				"pattern": "*.json"
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(
			&call,
			call.parameters
				.get("directory")
				.and_then(|v| v.as_str())
				.unwrap_or("."),
		)
		.await
		.unwrap();

		assert!(result.contains("config.json"));
	}

	#[tokio::test]
	async fn test_list_files_whitespace_content_should_list_files() {
		use std::fs;
		use tempfile::TempDir;

		let temp_dir = TempDir::new().unwrap();
		let temp_path = temp_dir.path();

		for i in 1..=5 {
			let file_path = temp_path.join(format!("test_file_{}.txt", i));
			fs::write(&file_path, format!("Content of file {}", i)).unwrap();
		}

		let config_path = temp_path.join("config.json");
		fs::write(&config_path, "{}").unwrap();

		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: json!({
				"directory": temp_path.to_str().unwrap(),
				"pattern": "*.json",
				"content": "   "
			}),
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let result = list_directory(
			&call,
			call.parameters
				.get("directory")
				.and_then(|v| v.as_str())
				.unwrap_or("."),
		)
		.await
		.unwrap();

		assert!(result.contains("config.json"));
	}
}
