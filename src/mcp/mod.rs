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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

pub mod fs;
pub mod request_ctx;
pub mod server;

// Session working directory is now stored per-server-instance (see server.rs).
// This module only provides helper functions for the session root directory.

static SESSION_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Set the session root directory (set once at startup from CLI).
/// This is the default workdir for new server instances.
pub fn set_session_root_directory(path: PathBuf) {
	SESSION_ROOT.set(path).ok();
}

/// Get the session root directory (default for new sessions).
pub fn get_session_root_directory() -> PathBuf {
	SESSION_ROOT
		.get()
		.cloned()
		.unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// How tool-misuse hints are enforced: hard error or soft warning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HintMode {
	/// Misuse is rejected with an error; the call fails.
	Hard,
	/// Misuse is allowed; guidance is appended to the response as a hint.
	Soft,
}

static HINT_MODE: OnceLock<HintMode> = OnceLock::new();

/// Set the global hint mode. Call once at startup.
pub fn set_hint_mode(mode: HintMode) {
	HINT_MODE.set(mode).ok();
}

/// Get the current hint mode (defaults to Soft).
pub fn get_hint_mode() -> HintMode {
	HINT_MODE.get().copied().unwrap_or(HintMode::Soft)
}

/// Returns true if misuse should surface as a soft hint instead of an error.
pub fn is_soft_hint_mode() -> bool {
	get_hint_mode() == HintMode::Soft
}

/// MCP tool call with per-session working directory context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
	pub tool_name: String,
	pub parameters: Value,
	#[serde(default)]
	pub tool_id: String,
	/// Per-session working directory for this call.
	pub workdir: PathBuf,
}

impl McpToolCall {
	/// Create a test call with default working directory.
	#[cfg(test)]
	pub fn test_call(tool_name: &str, parameters: Value) -> Self {
		Self {
			tool_name: tool_name.to_string(),
			parameters,
			tool_id: "test".to_string(),
			workdir: std::env::current_dir().unwrap_or_default(),
		}
	}
}
