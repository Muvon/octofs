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

use super::*;
use serde_json::json;

fn filter_by_pattern(files: &mut Vec<String>, pattern: &str) -> Result<(), String> {
	let matcher = build_pattern_matcher(pattern)?;
	apply_pattern(files, &matcher);
	Ok(())
}

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
	let mut nested_braces = vec![
		"file.rs".to_string(),
		"file.toml".to_string(),
		"file.md".to_string(),
	];
	filter_by_pattern(&mut nested_braces, "*.{rs,{toml,md}}").unwrap();
	assert_eq!(nested_braces.len(), 3);

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

	let mut exclusion_only = vec!["keep.rs".to_string(), "drop.log".to_string()];
	filter_by_pattern(&mut exclusion_only, "!*.log").unwrap();
	assert_eq!(exclusion_only, vec!["keep.rs"]);
}

#[test]
fn test_pattern_literal_escaping_and_shell_characters_are_data() {
	let mut files = vec!["report|final.md".to_string(), "report.md".to_string()];
	filter_by_pattern(&mut files, r"report\|final.md").unwrap();
	assert_eq!(files, vec!["report|final.md"]);

	let mut files = vec!["report|final.md".to_string(), "report.md".to_string()];
	filter_by_pattern(&mut files, r"report[|]final.md").unwrap();
	assert_eq!(files, vec!["report|final.md"]);

	let mut files = vec!["#notes".to_string(), "notes".to_string()];
	filter_by_pattern(&mut files, r"\#notes").unwrap();
	assert_eq!(files, vec!["#notes"]);

	let mut files = vec!["!important".to_string(), "important".to_string()];
	filter_by_pattern(&mut files, r"\!important").unwrap();
	assert_eq!(files, vec!["!important"]);

	let mut files = vec!["$(touch pwned)".to_string(), "safe".to_string()];
	filter_by_pattern(&mut files, "$(touch pwned)").unwrap();
	assert_eq!(files, vec!["$(touch pwned)"]);
}

#[test]
fn test_pattern_rejects_malformed_or_ambiguous_input_without_mutation() {
	for pattern in [
		"",
		"   ",
		"|*.rs",
		"*.rs||*.toml",
		"*.rs|",
		"[unclosed",
		"{unclosed",
		"empty{,branch}",
		"empty{branch,}",
		"empty{branch,,other}",
		"empty{}",
		r"dangling\",
		"!",
		"#silent-comment",
		"*.rs\n!**",
		"*.rs\0",
	] {
		let original = vec!["keep.rs".to_string(), "keep.toml".to_string()];
		let mut files = original.clone();
		let err = filter_by_pattern(&mut files, pattern).unwrap_err();
		assert!(err.contains("Invalid `pattern`"), "{pattern:?}: {err}");
		assert_eq!(
			files, original,
			"invalid pattern mutated files: {pattern:?}"
		);
	}

	let mut files = vec!["#silent-comment".to_string()];
	let err = filter_by_pattern(&mut files, "#silent-comment").unwrap_err();
	assert!(err.contains(r"Use `\#`"), "got: {err}");
}

#[test]
fn test_pattern_resource_limits() {
	let mut files = vec!["keep.rs".to_string()];
	let oversized = "a".repeat(MAX_PATTERN_BYTES + 1);
	let err = filter_by_pattern(&mut files, &oversized).unwrap_err();
	assert!(err.contains("byte limit"), "got: {err}");
	assert!(
		err.len() < 300,
		"error echoed oversized input: {} bytes",
		err.len()
	);

	let allowed: String = (0..MAX_PATTERN_ALTERNATIVES)
		.map(|i| format!("file{i}"))
		.collect::<Vec<_>>()
		.join("|");
	filter_by_pattern(&mut files, &allowed).unwrap();

	let too_many: String = (0..=MAX_PATTERN_ALTERNATIVES)
		.map(|i| format!("file{i}"))
		.collect::<Vec<_>>()
		.join("|");
	let err = filter_by_pattern(&mut files, &too_many).unwrap_err();
	assert!(err.contains("more than 64"), "got: {err}");

	let deeply_nested = format!(
		"{}x{}",
		"{".repeat(MAX_BRACE_NESTING + 1),
		"}".repeat(MAX_BRACE_NESTING + 1)
	);
	let err = filter_by_pattern(&mut files, &deeply_nested).unwrap_err();
	assert!(err.contains("nesting exceeds"), "got: {err}");
}

#[test]
fn test_content_search_with_special_chars() {
	// Verify that special regex characters in patterns are treated as literals
	let content = "line1\nbackward_step()\nline3\n";
	let matcher = search::Matcher::new("backward_step()", false).unwrap();
	let blocks = search::search_lines(content, &matcher, 0);
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
async fn test_view_pattern_rejects_silent_comment_in_listing_and_search() {
	use std::fs;
	use tempfile::TempDir;

	let temp_dir = TempDir::new().unwrap();
	let temp_path = temp_dir.path();
	fs::write(temp_path.join("visible.rs"), "needle\n").unwrap();

	for parameters in [
		json!({ "path": ".", "pattern": "#silently-ignored" }),
		json!({
			"path": ".",
			"pattern": "#silently-ignored",
			"content": "needle"
		}),
	] {
		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters,
			tool_id: "test-call-id".to_string(),
			workdir: temp_path.to_path_buf(),
		};

		let err = list_directory(&call, ".").await.unwrap_err().to_string();
		assert!(err.contains("leading `#`"), "got: {err}");
		assert!(err.contains(r"Use `\#`"), "got: {err}");
		assert!(!err.contains("visible.rs"), "must not leak listing: {err}");
	}
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
