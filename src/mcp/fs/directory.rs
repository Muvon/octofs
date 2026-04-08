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

use super::super::{get_thread_working_directory, McpToolCall};
use super::search;
use crate::utils::line_hash::{compute_line_hashes, is_hash_mode};
use anyhow::{bail, Result};
use ignore::WalkBuilder;
use std::path::Path;

// Convert glob pattern to regex pattern for filename filtering
fn convert_glob_to_regex(glob_pattern: &str) -> String {
	let patterns: Vec<&str> = glob_pattern.split('|').collect();

	if patterns.len() > 1 {
		let regex_patterns: Vec<String> = patterns
			.iter()
			.map(|p| convert_single_glob_to_regex(p.trim()))
			.collect();
		format!("({})", regex_patterns.join("|"))
	} else {
		convert_single_glob_to_regex(glob_pattern)
	}
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
			c if "(){}^$+|\\".contains(c) => {
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

	let working_dir = get_thread_working_directory();
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
		let content_pattern_clone = content_pattern.clone();

		let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
			let mut builder = build_walker(&abs_dir_str, max_depth, include_hidden);
			let files = collect_file_paths(&mut builder, &working_dir);

			let hash_mode = is_hash_mode();
			let mut file_results: Vec<String> = Vec::new();

			for rel_path in &files {
				let full_path = working_dir.join(rel_path);
				let file_content = match std::fs::read_to_string(&full_path) {
					Ok(c) => c,
					Err(_) => continue, // skip unreadable files
				};

				// Skip likely binary files
				let sample_size = file_content.len().min(512);
				let null_count = file_content.as_bytes()[..sample_size]
					.iter()
					.filter(|&&b| b == 0)
					.count();
				if null_count > sample_size / 10 {
					continue;
				}

				let blocks =
					search::search_content(&file_content, &content_pattern_clone, context_lines);
				if blocks.is_empty() {
					continue;
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

				file_results.push(format!("{}:\n{}", rel_path, rendered_blocks.join("\n--\n")));
			}

			Ok(file_results.join("\n\n"))
		})
		.await;

		match output {
			Ok(Ok(s)) => Ok(s),
			Ok(Err(e)) => bail!("{}", e),
			Err(join_err) => bail!("Failed to execute content search: {}", join_err),
		}
	} else {
		// File listing mode
		let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
			let mut builder = build_walker(&abs_dir_str, max_depth, include_hidden);
			let mut files = collect_file_paths(&mut builder, &working_dir);

			// Apply glob pattern filter if provided
			if let Some(ref name_pattern) = pattern {
				let regex_pattern = convert_glob_to_regex(name_pattern);
				if let Ok(regex) = regex::Regex::new(&regex_pattern) {
					files.retain(|file| regex.is_match(file));
				}
			}

			Ok(files.join("\n"))
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
	fn test_content_search_with_special_chars() {
		// Verify that special regex characters in patterns are treated as literals
		let content = "line1\nbackward_step()\nline3\n";
		let blocks = search::search_content(content, "backward_step()", 0);
		assert_eq!(blocks.len(), 1);
		assert_eq!(blocks[0].line_numbers, vec![2]);
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
