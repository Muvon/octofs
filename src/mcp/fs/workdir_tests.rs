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

//! Unit tests for `execute_workdir_command` and `WorkdirResult` serialization.

use super::*;
use crate::mcp::McpToolCall;
use serde_json::json;
use std::path::PathBuf;

#[tokio::test]
async fn workdir_without_path_returns_current_directory() {
	let call = McpToolCall::test_call("workdir", json!({}));
	let result = execute_workdir_command(&call).await.unwrap();
	match result {
		WorkdirResult::Get { working_directory } => {
			assert_eq!(working_directory, call.workdir);
		}
		other => panic!("expected Get, got {}", other.to_json_string()),
	}
}

#[tokio::test]
async fn workdir_reset_returns_reset_without_touching_the_filesystem() {
	let call = McpToolCall::test_call("workdir", json!({ "reset": true }));
	let result = execute_workdir_command(&call).await.unwrap();
	assert!(matches!(result, WorkdirResult::Reset));
}

#[tokio::test]
async fn workdir_set_to_existing_directory_reports_previous_and_canonical_current() {
	let dir = tempfile::tempdir().unwrap();
	// A symlink is the case where canonicalization observably changes the
	// path: the raw input names the link, the canonical result names the
	// target. (A trailing `.` alone would not — Path equality compares
	// components and drops it.)
	tokio::fs::create_dir(dir.path().join("real"))
		.await
		.unwrap();
	tokio::fs::symlink("real", dir.path().join("alias"))
		.await
		.unwrap();
	let call = McpToolCall::test_call("workdir", json!({ "path": dir.path().join("alias") }));
	let result = execute_workdir_command(&call).await.unwrap();
	match result {
		WorkdirResult::Set { previous, current } => {
			assert_eq!(previous, call.workdir);
			assert_eq!(current, dir.path().join("real").canonicalize().unwrap());
		}
		other => panic!("expected Set, got {}", other.to_json_string()),
	}
}

#[tokio::test]
async fn workdir_set_to_missing_path_is_rejected_with_the_path_in_the_error() {
	let call = McpToolCall::test_call("workdir", json!({ "path": "/nonexistent/octofs/test/dir" }));
	let err = execute_workdir_command(&call)
		.await
		.err()
		.expect("must fail")
		.to_string();
	assert!(err.contains("does not exist"), "error was: {err}");
}

#[tokio::test]
async fn workdir_set_to_a_file_is_rejected_as_not_a_directory() {
	let file = tempfile::NamedTempFile::new().unwrap();
	let call = McpToolCall::test_call("workdir", json!({ "path": file.path().to_str().unwrap() }));
	let err = execute_workdir_command(&call)
		.await
		.err()
		.expect("must fail")
		.to_string();
	assert!(err.contains("not a directory"), "error was: {err}");
}

#[tokio::test]
async fn workdir_set_with_non_string_path_is_rejected() {
	let call = McpToolCall::test_call("workdir", json!({ "path": 42 }));
	let err = execute_workdir_command(&call)
		.await
		.err()
		.expect("must fail")
		.to_string();
	assert!(
		err.contains("must be a non-empty string"),
		"error was: {err}"
	);
}

#[tokio::test]
async fn workdir_set_with_whitespace_only_path_is_rejected() {
	let call = McpToolCall::test_call("workdir", json!({ "path": "   " }));
	let err = execute_workdir_command(&call)
		.await
		.err()
		.expect("must fail")
		.to_string();
	assert!(
		err.contains("must be a non-empty string"),
		"error was: {err}"
	);
}

#[test]
fn workdir_result_json_carries_action_specific_fields() {
	let get = WorkdirResult::Get {
		working_directory: PathBuf::from("/tmp/x"),
	};
	let parsed: serde_json::Value = serde_json::from_str(&get.to_json_string()).unwrap();
	assert_eq!(parsed["action"], "get");
	assert_eq!(parsed["success"], true);
	assert_eq!(parsed["working_directory"], "/tmp/x");
	assert!(parsed["message"].as_str().unwrap().contains("/tmp/x"));

	let set = WorkdirResult::Set {
		previous: PathBuf::from("/a"),
		current: PathBuf::from("/b"),
	};
	let parsed: serde_json::Value = serde_json::from_str(&set.to_json_string()).unwrap();
	assert_eq!(parsed["action"], "set");
	assert_eq!(parsed["previous_directory"], "/a");
	assert_eq!(parsed["working_directory"], "/b");
	assert!(parsed["message"].as_str().unwrap().contains("/a"));
	assert!(parsed["message"].as_str().unwrap().contains("/b"));

	let reset = WorkdirResult::Reset;
	let parsed: serde_json::Value = serde_json::from_str(&reset.to_json_string()).unwrap();
	assert_eq!(parsed["action"], "reset");
	assert_eq!(parsed["success"], true);
}
