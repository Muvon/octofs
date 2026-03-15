// Copyright 2025 Muvon Un Limited
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

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub mod fs;
pub mod hint_accumulator;
pub mod server;
pub mod shared_utils;

// Thread-local working directory for parallel execution isolation.
// `session` is the anchor set at startup (used by workdir reset).
// `current` tracks mid-session changes via the workdir tool.
struct WorkDir {
	session: PathBuf,
	current: PathBuf,
}

thread_local! {
	static WORKDIR: std::cell::RefCell<Option<WorkDir>> = const { std::cell::RefCell::new(None) };
}

/// Set the session working directory. Call once at startup.
/// Resets both the active directory and the reset anchor to `path`.
pub fn set_session_working_directory(path: PathBuf) {
	WORKDIR.with(|w| {
		*w.borrow_mut() = Some(WorkDir {
			session: path.clone(),
			current: path,
		});
	});
}

/// Override the active directory mid-session (workdir tool). Does not move the reset anchor.
pub fn set_thread_working_directory(path: PathBuf) {
	WORKDIR.with(|w| {
		let mut w = w.borrow_mut();
		if let Some(ref mut wd) = *w {
			wd.current = path;
		}
	});
}

/// Active working directory for the current thread.
pub fn get_thread_working_directory() -> PathBuf {
	WORKDIR.with(|w| {
		w.borrow()
			.as_ref()
			.map(|wd| wd.current.clone())
			.unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
	})
}

/// Session anchor — the directory to return to on workdir reset.
pub fn get_thread_original_working_directory() -> PathBuf {
	WORKDIR.with(|w| {
		w.borrow()
			.as_ref()
			.map(|wd| wd.session.clone())
			.unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
	})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
	pub tool_name: String,
	pub parameters: Value,
	#[serde(default)]
	pub tool_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
	pub tool_name: String,
	pub result: Value,
	#[serde(default)]
	pub tool_id: String,
}

impl McpToolResult {
	pub fn success(tool_name: String, tool_id: String, content: String) -> Self {
		Self {
			tool_name,
			tool_id,
			result: json!({
				"content": [
					{
						"type": "text",
						"text": content
					}
				],
				"isError": false
			}),
		}
	}

	pub fn success_with_metadata(
		tool_name: String,
		tool_id: String,
		content: String,
		metadata: serde_json::Value,
	) -> Self {
		Self {
			tool_name,
			tool_id,
			result: json!({
				"content": [
					{
						"type": "text",
						"text": content
					}
				],
				"isError": false,
				"metadata": metadata
			}),
		}
	}

	pub fn error(tool_name: String, tool_id: String, error_message: String) -> Self {
		Self {
			tool_name,
			tool_id,
			result: json!({
				"content": [
					{
						"type": "text",
						"text": error_message
					}
				],
				"isError": true
			}),
		}
	}

	pub fn is_error(&self) -> bool {
		self.result
			.get("isError")
			.and_then(|v| v.as_bool())
			.unwrap_or(false)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpFunction {
	pub name: String,
	pub description: String,
	pub parameters: Value,
}

/// Extract text content from an MCP-compliant result value.
pub fn extract_mcp_content(result: &Value) -> String {
	if let Some(content_array) = result.get("content") {
		if let Some(content_items) = content_array.as_array() {
			return content_items
				.iter()
				.filter_map(|item| {
					if item.get("type").and_then(|t| t.as_str()) == Some("text") {
						item.get("text").and_then(|t| t.as_str())
					} else {
						None
					}
				})
				.collect::<Vec<_>>()
				.join("\n");
		}
	}
	serde_json::to_string_pretty(result).unwrap_or_default()
}

/// Ensure tool calls have valid IDs.
pub fn ensure_tool_call_ids(calls: &mut [McpToolCall]) {
	for call in calls.iter_mut() {
		if call.tool_id.is_empty() {
			call.tool_id = format!("tool_{}", uuid::Uuid::new_v4().simple());
		}
	}
}
