// Working directory management for the Filesystem MCP provider

use super::super::McpToolCall;
use super::remote::{io_canonicalize, io_is_dir, resolve_path_source, PathSource};
use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;

/// Structured result from workdir command — avoids fragile JSON round-tripping.
pub enum WorkdirResult {
	/// Current working directory was queried.
	Get { working_directory: PathBuf },
	/// Working directory was changed.
	Set { previous: PathBuf, current: PathBuf },
	/// Working directory was reset to session root.
	Reset,
}

impl WorkdirResult {
	/// Serialize to JSON string for the MCP response.
	pub fn to_json_string(&self) -> String {
		match self {
			Self::Get {
				working_directory: wd,
			} => json!({
				"success": true,
				"action": "get",
				"working_directory": wd.to_string_lossy(),
				"message": format!("Current working directory: {}", wd.display())
			})
			.to_string(),
			Self::Set { previous, current } => json!({
				"success": true,
				"action": "set",
				"previous_directory": previous.to_string_lossy(),
				"working_directory": current.to_string_lossy(),
				"message": format!("Working directory changed from {} to {}", previous.display(), current.display())
			})
			.to_string(),
			Self::Reset => json!({
				"success": true,
				"action": "reset"
			})
			.to_string(),
		}
	}
}

/// Execute working directory command
pub async fn execute_workdir_command(call: &McpToolCall) -> Result<WorkdirResult> {
	let reset = call
		.parameters
		.get("reset")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// Reset to original session directory
	if reset {
		return Ok(WorkdirResult::Reset);
	}

	// Get or set working directory
	match call.parameters.get("path") {
		Some(serde_json::Value::String(path_str)) if !path_str.trim().is_empty() => {
			let path_str = path_str.trim();

			// Resolve against the current workdir (which itself may be remote)
			match resolve_path_source(path_str, &call.workdir) {
				PathSource::Local(new_path) => {
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

					Ok(WorkdirResult::Set {
						previous: call.workdir.clone(),
						current: canonical_path,
					})
				}
				source @ PathSource::Remote { .. } => {
					// Canonicalize server-side to resolve .. and symlinks
					let canonical = match io_canonicalize(&source).await {
						Ok(p) => p,
						Err(e) => {
							bail!(
								"Path does not exist or is not accessible: {} (error: {})",
								source.display(),
								e
							);
						}
					};
					let canonical_source = match &source {
						PathSource::Remote {
							host,
							port,
							user,
							user_explicit,
							port_explicit,
							..
						} => PathSource::Remote {
							host: host.clone(),
							port: *port,
							user: user.clone(),
							path: canonical.to_string_lossy().to_string(),
							user_explicit: *user_explicit,
							port_explicit: *port_explicit,
						},
						PathSource::Local(_) => unreachable!(),
					};

					// Verify it's a directory
					if !io_is_dir(&canonical_source).await? {
						bail!("Path is not a directory: {}", canonical_source.display());
					}

					// The workdir travels as a PathBuf holding the ssh:// URL;
					// display() round-trips through parse_path_source on later calls.
					Ok(WorkdirResult::Set {
						previous: call.workdir.clone(),
						current: PathBuf::from(canonical_source.display()),
					})
				}
			}
		}
		Some(_) => bail!("Parameter 'path' must be a non-empty string"),
		None => {
			// Get current working directory
			Ok(WorkdirResult::Get {
				working_directory: call.workdir.clone(),
			})
		}
	}
}
