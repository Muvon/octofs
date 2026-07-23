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
use super::search::{self, Matcher};
use crate::utils::line_hash::{compute_line_hashes, is_hash_mode};
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
		let rel = path
			.strip_prefix(working_dir)
			.unwrap_or(path)
			.to_string_lossy()
			.to_string();
		files.push(rel);
	}
	files.sort();
	files
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
				let regex_pattern = convert_glob_to_regex(name_pattern);
				let regex = regex::Regex::new(&regex_pattern)
					.map_err(|e| format!("Invalid `pattern` glob '{}': {}", name_pattern, e))?;
				files.retain(|file| regex.is_match(file));
			}

			let hash_mode = is_hash_mode();

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
					let prefixes: Vec<String> = if hash_mode {
						compute_line_hashes(&file_lines)
					} else {
						(1..=file_lines.len()).map(|n| n.to_string()).collect()
					};

					let mut rendered_blocks: Vec<String> = Vec::new();
					for block in &blocks {
						let mut rendered = Vec::new();
						for &n in &block.line_numbers {
							let idx = n - 1;
							if idx < file_lines.len() {
								rendered.push(format!("{}:{}", prefixes[idx], file_lines[idx]));
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
				let regex_pattern = convert_glob_to_regex(name_pattern);
				let regex = regex::Regex::new(&regex_pattern)
					.map_err(|e| format!("Invalid `pattern` glob '{}': {}", name_pattern, e))?;
				files.retain(|file| regex.is_match(file));
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
