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

// Working directory management for the Filesystem MCP provider

use super::super::{
	get_thread_original_working_directory, get_thread_working_directory,
	set_thread_working_directory, McpToolCall,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Execute working directory command
pub async fn execute_workdir_command(call: &McpToolCall) -> Result<String> {
	let reset = call
		.parameters
		.get("reset")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// Reset to original session directory (set at session creation, not process cwd)
	if reset {
		let original_dir = get_thread_original_working_directory();
		set_thread_working_directory(original_dir.clone());

		return Ok(json!({
			"success": true,
			"action": "reset",
			"working_directory": original_dir.to_string_lossy(),
			"message": format!("Working directory reset to: {}", original_dir.display())
		})
		.to_string());
	}

	// Get or set working directory
	match call.parameters.get("path") {
		Some(Value::String(path_str)) if !path_str.trim().is_empty() => {
			let path_str = path_str.trim();

			// Resolve the path (handle relative paths)
			let new_path = if std::path::Path::new(path_str).is_absolute() {
				std::path::PathBuf::from(path_str)
			} else {
				// Relative to current working directory
				let current = get_thread_working_directory();
				current.join(path_str)
			};

			// Canonicalize to resolve .. and symlinks
			let canonical_path = match new_path.canonicalize() {
				Ok(p) => p,
				Err(e) => {
					bail!(
						"Path does not exist or is not accessible: {} (error: {})",
						new_path.display(),
						e
					);
				}
			};

			// Verify it's a directory
			if !canonical_path.is_dir() {
				bail!("Path is not a directory: {}", canonical_path.display());
			}

			let old_dir = get_thread_working_directory();
			set_thread_working_directory(canonical_path.clone());

			Ok(json!({
				"success": true,
				"action": "set",
				"previous_directory": old_dir.to_string_lossy(),
				"working_directory": canonical_path.to_string_lossy(),
				"message": format!("Working directory changed from {} to {}", old_dir.display(), canonical_path.display())
			}).to_string())
		}
		Some(_) => bail!("Parameter 'path' must be a non-empty string"),
		None => {
			// Get current working directory
			let current_dir = get_thread_working_directory();

			Ok(json!({
				"success": true,
				"action": "get",
				"working_directory": current_dir.to_string_lossy(),
				"message": format!("Current working directory: {}", current_dir.display())
			})
			.to_string())
		}
	}
}
