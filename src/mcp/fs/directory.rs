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
use ignore::WalkBuilder;
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
// Convert glob pattern to regex pattern for filename filtering
fn convert_glob_to_regex(glob_pattern: &str) -> String {
	let patterns: Vec<&str> = glob_pattern.split('|').collect();

	let body = if patterns.len() > 1 {
		let regex_patterns: Vec<String> = patterns
			.iter()
			.map(|p| convert_single_glob_to_regex(p.trim()))
			.collect();
		format!("({})", regex_patterns.join("|"))
	} else {
		convert_single_glob_to_regex(glob_pattern)
	};
	// Anchored: the glob must match the whole relative path, not a substring —
	// unanchored, `*.rs` also matched `main.rsx`.
	format!("^(?:{body})$")
}

fn convert_single_glob_to_regex(pattern: &str) -> String {
	let mut regex = String::new();
	let chars: Vec<char> = pattern.chars().collect();
	let mut i = 0;

	while i < chars.len() {
		match chars[i] {
			'*' => regex.push_str(".*?"),
			'?' => regex.push('.'),
			'[' => {
				regex.push('[');
				i += 1;
				while i < chars.len() && chars[i] != ']' {
					regex.push(chars[i]);
					i += 1;
				}
				if i < chars.len() {
					regex.push(']');
				}
			}
			c if "(){}^$+|\\.".contains(c) => {
				regex.push('\\');
				regex.push(c);
			}
			c => regex.push(c),
		}
		i += 1;
	}

	regex
}

// A glob with no '/' matches the file name at any depth (gitignore semantics) —
// `Model.php` finds `app/src/Plugin/Foo/Model.php`. A glob containing '/' matches
// the workdir-relative path.
fn filter_by_pattern(files: &mut Vec<String>, glob: &str) -> Result<(), String> {
	let regex = regex::Regex::new(&convert_glob_to_regex(glob))
		.map_err(|e| format!("Invalid `pattern` glob '{}': {}", glob, e))?;
	if glob.contains('/') {
		files.retain(|file| regex.is_match(file));
	} else {
		files.retain(|file| {
			Path::new(file)
				.file_name()
				.is_some_and(|n| regex.is_match(&n.to_string_lossy()))
		});
	}
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
	current_depth: usize,
) -> Result<Vec<(String, IoMetadata)>> {
	let mut files = Vec::new();
	let entries = io_list_dir(source).await?;

	for (name, meta) in entries {
		if !include_hidden && name.starts_with('.') {
			continue;
		}

		let rel_path = if base_rel.is_empty() {
			name.clone()
		} else {
			format!("{base_rel}/{name}")
		};

		if let Some(gi) = gitignore {
			if gi
				.matched_path_or_any_parents(&rel_path, meta.is_dir)
				.is_ignore()
			{
				continue;
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

	let mut files =
		collect_remote_files(source, "", max_depth, include_hidden, gitignore.as_ref(), 0).await?;

	if let Some(ref name_pattern) = pattern {
		let mut names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
		filter_by_pattern(&mut names, name_pattern).map_err(|e| anyhow::anyhow!("{e}"))?;
		if names.is_empty() {
			return Ok(format!("No files matched pattern \"{name_pattern}\"."));
		}
		let keep: std::collections::HashSet<String> = names.into_iter().collect();
		files.retain(|(n, _)| keep.contains(n));
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
			let mut files = collect_file_paths(&mut builder, &working_dir);

			// The `pattern` glob narrows content search the same way it narrows listing —
			// silently ignoring it here would search files the caller explicitly excluded.
			if let Some(ref name_pattern) = pattern {
				filter_by_pattern(&mut files, name_pattern)?;
				if files.is_empty() {
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
			let mut files = collect_file_paths(&mut builder, &working_dir);

			// Apply glob pattern filter if provided — an unparseable pattern is a caller
			// error, not a reason to silently return the unfiltered listing.
			if let Some(ref name_pattern) = pattern {
				filter_by_pattern(&mut files, name_pattern)?;
				if files.is_empty() {
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
	fn test_glob_regex_is_anchored_and_escapes_dot() {
		let re = regex::Regex::new(&convert_glob_to_regex("*.rs")).unwrap();
		assert!(re.is_match("main.rs"));
		assert!(re.is_match("src/main.rs"));
		assert!(!re.is_match("main.rsx"), "unanchored regex matched suffix");
		assert!(!re.is_match("mainxrs"), "unescaped '.' matched any char");

		let multi = regex::Regex::new(&convert_glob_to_regex("*.rs|*.toml")).unwrap();
		assert!(multi.is_match("Cargo.toml"));
		assert!(!multi.is_match("Cargo.toml.bak"));
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
