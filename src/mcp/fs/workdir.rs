// Working directory management for the Filesystem MCP provider

use super::super::McpToolCall;
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Execute working directory command
pub async fn execute_workdir_command(call: &McpToolCall) -> Result<String> {
	let reset = call
		.parameters
		.get("reset")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// Reset to original session directory
	// Note: The reset is handled by returning success - the caller
	// (server.rs) will update the workdir state using self.workdir.root
	if reset {
		return Ok(json!({
			"success": true,
			"action": "reset"
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
				call.workdir.join(path_str)
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

			let old_dir = call.workdir.clone();
			// Note: The set is handled by returning success - the caller
			// (server.rs) will update the workdir state

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
			let current_dir = call.workdir.clone();

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
