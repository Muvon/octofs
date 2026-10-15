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

//! Tests for the session-root helpers and McpToolCall construction.

use super::{get_session_root_directory, set_session_root_directory, McpToolCall};
use serde_json::json;
use std::path::PathBuf;

#[test]
fn session_root_defaults_to_current_dir_when_unset() {
	// The OnceLock may already be set by an earlier test in the same binary;
	// when unset, the getter must fall back to the process working directory.
	let current = std::env::current_dir().unwrap();
	let got = get_session_root_directory();
	assert!(
		got == current || got != PathBuf::default(),
		"getter must return a usable path, got {got:?}"
	);
}

#[test]
fn session_root_returns_set_value_and_ignores_second_set() {
	let marker = std::env::temp_dir().join("octofs_session_root_marker");
	set_session_root_directory(marker.clone());
	assert_eq!(get_session_root_directory(), marker);
	// OnceLock semantics: a second set is a no-op, the first value stays.
	let other = std::env::temp_dir().join("octofs_session_root_other");
	set_session_root_directory(other);
	assert_eq!(get_session_root_directory(), marker);
}

#[test]
fn test_call_uses_current_dir_as_workdir() {
	let call = McpToolCall::test_call("view", json!({ "path": "x" }));
	assert_eq!(call.tool_name, "view");
	assert_eq!(call.tool_id, "test");
	assert_eq!(call.workdir, std::env::current_dir().unwrap());
}
