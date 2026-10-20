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

//! Unit tests for file viewing, in-file search and creation (local paths).

use super::*;
use crate::mcp::fs::remote::PathSource;
use tokio::fs;

fn source(path: &std::path::Path) -> PathSource {
	PathSource::Local(path.to_path_buf())
}

#[tokio::test]
async fn view_missing_file_is_an_error() {
	let src = source(std::path::Path::new("/nonexistent/octofs/view.txt"));
	let err = view_file_spec(&src, None, false).await.unwrap_err().to_string();
	assert_eq!(err, "File not found");
}

#[tokio::test]
async fn view_directory_is_rejected_as_not_a_file() {
	let dir = tempfile::tempdir().unwrap();
	let err = view_file_spec(&source(dir.path()), None, false)
		.await
		.unwrap_err()
		.to_string();
	assert_eq!(err, "Path is not a file");
}

#[tokio::test]
async fn view_oversized_file_is_refused_before_reading() {
	let file = tempfile::NamedTempFile::new().unwrap();
	let zeros = vec![0u8; (MAX_VIEW_FILE_BYTES + 1) as usize];
	fs::write(file.path(), zeros).await.unwrap();
	let err = view_file_spec(&source(file.path()), None, false)
		.await
		.unwrap_err()
		.to_string();
	assert!(err.contains("too large"), "error was: {err}");
}

#[tokio::test]
async fn view_ranged_read_renders_only_the_requested_lines() {
	let file = tempfile::NamedTempFile::new().unwrap();
	fs::write(file.path(), "one\ntwo\nthree\n").await.unwrap();
	let out = view_file_spec(&source(file.path()), Some((2, 2)), false).await.unwrap();
	assert!(out.contains("|two"));
	assert!(!out.contains("|one"));
	assert!(!out.contains("|three"));
}

#[tokio::test]
async fn content_search_missing_file_is_an_error() {
	let src = source(std::path::Path::new("/nonexistent/octofs/search.txt"));
	let err = view_file_with_content_search(&src, "x", 0, false)
		.await
		.unwrap_err()
		.to_string();
	assert_eq!(err, "File not found");
}

#[tokio::test]
async fn content_search_directory_is_rejected() {
	let dir = tempfile::tempdir().unwrap();
	let err = view_file_with_content_search(&source(dir.path()), "x", 0, false)
		.await
		.unwrap_err()
		.to_string();
	assert_eq!(err, "Path is not a file");
}

#[tokio::test]
async fn content_search_oversized_file_points_at_grep() {
	let file = tempfile::NamedTempFile::new().unwrap();
	let zeros = vec![0u8; (MAX_VIEW_FILE_BYTES + 1) as usize];
	fs::write(file.path(), zeros).await.unwrap();
	let err = view_file_with_content_search(&source(file.path()), "x", 0, false)
		.await
		.unwrap_err()
		.to_string();
	assert!(err.contains("grep"), "error was: {err}");
}

#[tokio::test]
async fn content_search_empty_file_says_so_without_matching() {
	let file = tempfile::NamedTempFile::new().unwrap();
	fs::write(file.path(), "").await.unwrap();
	let out = view_file_with_content_search(&source(file.path()), "x", 0, false)
		.await
		.unwrap();
	assert!(out.contains("File is empty"), "out was: {out}");
}

#[tokio::test]
async fn content_search_no_match_names_the_pattern() {
	let file = tempfile::NamedTempFile::new().unwrap();
	fs::write(file.path(), "hello\nworld\n").await.unwrap();
	let out = view_file_with_content_search(&source(file.path()), "zzz", 0, false)
		.await
		.unwrap();
	assert!(out.contains("No matches"), "out was: {out}");
	assert!(out.contains("zzz"), "out was: {out}");
}

#[tokio::test]
async fn content_search_renders_matches_with_context_and_line_ids() {
	let file = tempfile::NamedTempFile::new().unwrap();
	fs::write(file.path(), "a\nb\nneedle\nc\nd\n").await.unwrap();
	let out = view_file_with_content_search(&source(file.path()), "needle", 1, false)
		.await
		.unwrap();
	let lines: Vec<&str> = out.split('\n').collect();
	assert_eq!(lines.len(), 3, "out was: {out}");
	// ids carry the line number: line 2 for "b", 3 for the match, 4 for "c"
	assert!(lines[0].ends_with("|b") && lines[0].starts_with("2:"));
	assert!(lines[1].ends_with("|needle") && lines[1].starts_with("3:"));
	assert!(lines[2].ends_with("|c") && lines[2].starts_with("4:"));
}

#[tokio::test]
async fn create_new_file_with_missing_parents_succeeds() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("deeply/nested/new.txt");
	let out = create_file_spec(&source(&target), "payload").await.unwrap();
	assert!(out.contains("7 bytes"), "out was: {out}");
	let written = fs::read_to_string(&target).await.unwrap();
	assert_eq!(written, "payload");
}

#[tokio::test]
async fn create_existing_file_is_rejected_with_edit_guidance() {
	let file = tempfile::NamedTempFile::new().unwrap();
	let err = create_file_spec(&source(file.path()), "x")
		.await
		.unwrap_err()
		.to_string();
	assert!(err.contains("already exists"), "error was: {err}");
	assert!(err.contains("str_replace"), "error was: {err}");
}
