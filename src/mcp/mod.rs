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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static TOOL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_tool_id() -> String {
	format!("tool_{}", TOOL_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub mod fs;
pub mod hint_accumulator;
pub mod server;
pub mod shared_utils;

// Global session working directory (set once at startup, never changes).
// This is the anchor for workdir reset operations.
static SESSION_WORKDIR: OnceLock<PathBuf> = OnceLock::new();

// Thread-local current working directory for mid-session changes.
// Defaults to SESSION_WORKDIR if not set.
thread_local! {
	static CURRENT_WORKDIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Set the session working directory. Call once at startup.
/// This sets the global anchor that never changes during the session.
pub fn set_session_working_directory(path: PathBuf) {
	SESSION_WORKDIR.set(path).ok(); // Ignore if already set
}

/// Override the active directory mid-session (workdir tool).
/// Does not affect the session anchor.
pub fn set_thread_working_directory(path: PathBuf) {
	CURRENT_WORKDIR.with(|w| {
		*w.borrow_mut() = Some(path);
	});
}

/// Active working directory for the current thread.
/// Returns CURRENT_WORKDIR if set, otherwise SESSION_WORKDIR.
pub fn get_thread_working_directory() -> PathBuf {
	CURRENT_WORKDIR.with(|w| {
		w.borrow().clone().unwrap_or_else(|| {
			SESSION_WORKDIR
				.get()
				.cloned()
				.unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
		})
	})
}

/// Session anchor — the directory to return to on workdir reset.
pub fn get_thread_original_working_directory() -> PathBuf {
	SESSION_WORKDIR
		.get()
		.cloned()
		.unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
	pub tool_name: String,
	pub parameters: Value,
	#[serde(default)]
	pub tool_id: String,
}
