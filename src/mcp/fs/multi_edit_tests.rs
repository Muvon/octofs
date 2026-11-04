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

//! Multi-file transaction contracts exercised through the public command executor.

use super::super::core::{execute_text_editor, undo_edit};
use super::*;
use serde_json::{json, Value};
use tempfile::TempDir;

async fn run(root: &TempDir, parameters: Value) -> Result<String> {
	let mut call = McpToolCall::test_call("text_editor", parameters);
	call.workdir = root.path().to_path_buf();
	execute_text_editor(&call).await
}

async fn read(root: &TempDir, path: &str) -> String {
	tokio::fs::read_to_string(root.path().join(path)).await.unwrap()
}

async fn write(root: &TempDir, path: &str, text: &str) {
	tokio::fs::write(root.path().join(path), text).await.unwrap();
}

#[tokio::test]
async fn ordered_edits_share_defaults_and_one_undo_per_file() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "alpha\r\nbeta\r\n").await;
	write(&root, "b", "gamma\n").await;
	let result = run(
		&root,
		json!({"command":"str_replace", "path":"a", "edits":[
			{"old_text":"alpha", "new_text":"first"},
			{"old_text":"first", "new_text":"last\nextra"},
			{"path":"b", "old_text":"gamma", "new_text":"delta"}
		]}),
	)
	.await
	.unwrap();
	assert_eq!(read(&root, "a").await, "last\r\nextra\r\nbeta\r\n");
	assert_eq!(read(&root, "b").await, "delta\n");
	assert_eq!(result.matches(&root.path().join("a").display().to_string()).count(), 1);
	assert!(result.contains(&format!("+{}|extra", line_id_at(&["last", "extra", "beta"], 2))));
	for path in ["a", "b"] {
		undo_edit(&PathSource::Local(root.path().join(path))).await.unwrap();
	}
	assert_eq!(read(&root, "a").await, "alpha\r\nbeta\r\n");
	assert_eq!(read(&root, "b").await, "gamma\n");
}

#[tokio::test]
async fn exact_once_and_replace_all_are_checked_against_staged_content() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "one one\n").await;
	let error = run(
		&root,
		json!({"command":"str_replace", "path":"a", "edits":[
			{"old_text":"one", "new_text":"two"}
		]}),
	)
	.await
	.unwrap_err();
	assert!(format!("{error:#}").contains("Found 2 matches"));
	assert_eq!(read(&root, "a").await, "one one\n");
	run(
		&root,
		json!({"command":"str_replace", "path":"a", "edits":[
			{"old_text":"one", "new_text":"two", "replace_all":true},
			{"old_text":"two two", "new_text":"done"}
		]}),
	)
	.await
	.unwrap();
	assert_eq!(read(&root, "a").await, "done\n");
}

#[tokio::test]
async fn failed_later_edit_leaves_all_files_history_and_cache_unchanged() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "alpha\n").await;
	write(&root, "b", "  beta\n").await;
	let error = run(
		&root,
		json!({"command":"str_replace", "edits":[
			{"path":"a", "old_text":"alpha", "new_text":"changed"},
			{"path":"b", "old_text":"beta\nmissing", "new_text":"no"}
		]}),
	)
	.await
	.unwrap_err();
	assert!(format!("{error:#}").contains("edit index 1"));
	assert_eq!(read(&root, "a").await, "alpha\n");
	assert_eq!(read(&root, "b").await, "  beta\n");
	assert!(undo_edit(&PathSource::Local(root.path().join("a"))).await.is_err());
	let error = run(
		&root,
		json!({"command":"str_replace", "path":"b", "edits":[
			{"old_text":" beta", "new_text":"no"},
			{"old_text":" beta", "new_text":"no"}
		]}),
	)
	.await
	.unwrap_err();
	assert!(format!("{error:#}").contains("edit index 1"));
	let error = run(
		&root,
		json!({"command":"str_replace", "path":"b", "edits":[
			{"old_text":"    beta", "new_text":"no"}
		]}),
	)
	.await
	.unwrap_err();
	assert!(format!("{error:#}").contains("Closest matches"));
	assert_eq!(read(&root, "b").await, "  beta\n");
}

#[tokio::test]
async fn malformed_edits_fail_before_any_write() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "alpha\n").await;
	for edits in [
		json!([]),
		json!({}),
		json!([{"old_text":"", "new_text":"x"}]),
		json!([{"old_text":"alpha", "new_text":"x", "replace_all":"true"}]),
	] {
		assert!(run(&root, json!({"command":"str_replace", "path":"a", "edits":edits}))
			.await
			.is_err());
		assert_eq!(read(&root, "a").await, "alpha\n");
	}
}

#[tokio::test]
async fn commit_io_failure_restores_previously_written_files() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "original\n").await;
	write(&root, "z", "original z\n").await;
	tokio::fs::create_dir(root.path().join(".octofs_tmp_z.tmp"))
		.await
		.unwrap();
	assert!(run(
		&root,
		json!({"command":"str_replace", "edits":[
			{"path":"a", "old_text":"original", "new_text":"changed"},
			{"path":"z", "old_text":"original z", "new_text":"changed z"}
		]})
	)
	.await
	.is_err());
	assert_eq!(read(&root, "a").await, "original\n");
	assert_eq!(read(&root, "z").await, "original z\n");
	assert!(undo_edit(&PathSource::Local(root.path().join("a"))).await.is_err());
}

#[tokio::test]
async fn missing_file_fails_before_any_write() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "alpha\n").await;
	let error = run(
		&root,
		json!({"command":"str_replace", "edits":[
			{"path":"a", "old_text":"alpha", "new_text":"changed"},
			{"path":"missing", "old_text":"x", "new_text":"y"}
		]}),
	)
	.await
	.unwrap_err();
	assert!(format!("{error:#}").contains("File not found"));
	assert_eq!(read(&root, "a").await, "alpha\n");
	assert!(!root.path().join("missing").exists());
}

#[test]
fn edits_use_remote_workdir_resolution() {
	let mut call = McpToolCall::test_call("text_editor", json!({}));
	call.workdir = std::path::PathBuf::from("ssh://dev@example.test:2222/repo");
	let source = resolve("src/a.rs", &call).unwrap();
	assert!(matches!(source, PathSource::Remote { .. }));
	assert!(source.display().contains("/repo/src/a.rs"));
}

#[tokio::test]
async fn overlapping_calls_lock_in_one_order_and_keep_complete_transactions() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "one\n").await;
	write(&root, "b", "one\n").await;
	let first = json!({"command":"str_replace", "edits":[
		{"path":"a", "old_text":"one", "new_text":"two"},
		{"path":"b", "old_text":"one", "new_text":"two"}
	]});
	let second = json!({"command":"str_replace", "edits":[
		{"path":"b", "old_text":"one", "new_text":"three"},
		{"path":"./a", "old_text":"one", "new_text":"three"}
	]});
	let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
		tokio::join!(run(&root, first), run(&root, second))
	})
	.await
	.unwrap();
	assert_ne!(first.is_ok(), second.is_ok());
	assert_eq!(read(&root, "a").await, read(&root, "b").await);
}

#[tokio::test]
async fn successful_multi_edit_updates_the_view_cache() {
	let root = tempfile::tempdir().unwrap();
	write(&root, "a", "one\n").await;
	let source = PathSource::Local(root.path().join("a"));
	let cache = std::sync::Arc::new(super::super::delta::ViewCache::default());
	crate::mcp::request_ctx::with_request_context(Some(cache), async {
		super::super::delta::render_whole_file(&source, "one\n", false);
		run(
			&root,
			json!({"command":"str_replace", "path":"a", "edits":[
				{"old_text":"one", "new_text":"two"}
			]}),
		)
		.await
		.unwrap();
		let view = super::super::delta::render_whole_file(&source, "two\n", false);
		assert!(view.contains("unchanged"), "{view}");
	})
	.await;
}
