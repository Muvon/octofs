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

// Shell execution functionality for the Filesystem MCP provider

use super::super::{get_thread_working_directory, McpFunction, McpToolCall, McpToolResult};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;

// Track PIDs of in-flight foreground shell children.
// Each child is spawned with process_group(0) so PGID == child PID.
// On shutdown we kill(-pid, SIGKILL) to terminate the entire process group,
// including any grandchildren the command may have spawned.
static SHELL_CHILDREN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn register_child(pid: u32) {
	SHELL_CHILDREN
		.lock()
		.unwrap()
		.get_or_insert_with(HashSet::new)
		.insert(pid);
}

fn unregister_child(pid: u32) {
	if let Some(set) = SHELL_CHILDREN.lock().unwrap().as_mut() {
		set.remove(&pid);
	}
}

/// Kill every tracked in-flight shell child's process group.
/// Called on SIGTERM / EOF so grandchildren don't survive as orphans.
#[cfg(unix)]
pub fn kill_all_shell_children() {
	let pids: Vec<u32> = SHELL_CHILDREN
		.lock()
		.unwrap()
		.as_mut()
		.map(|set| set.drain().collect())
		.unwrap_or_default();

	for pid in pids {
		let pgid = pid as libc::pid_t;
		// SAFETY: kill is always safe with valid arguments.
		// Negative pgid targets the entire process group.
		unsafe {
			libc::kill(-pgid, libc::SIGKILL);
		}
	}
}

#[cfg(not(unix))]
pub fn kill_all_shell_children() {
	// On non-unix, clear the set; kill_on_drop handles the direct child.
	if let Some(set) = SHELL_CHILDREN.lock().unwrap().as_mut() {
		set.clear();
	}
}
// Define the shell function for the MCP protocol with enhanced description
pub fn get_shell_function() -> McpFunction {
	McpFunction {
		name: "shell".to_string(),
		description: "Execute a command in the shell. Returns stdout+stderr combined, with success/failure indication.

Each command runs in its own process — state (cd, exports) does not persist. Chain with `&&`: `cd foo && cargo build`.

Background: set `background: true` to get a PID immediately; kill with `kill <pid>`.

Examples:
- `{\"command\": \"cargo test\"}`
- `{\"command\": \"python -m http.server 8000\", \"background\": true}`
- `{\"command\": \"kill 12345\"}`
".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"command": {
					"type": "string",
					"description": "The shell command to execute (runs from current working directory)"
				},
				"background": {
					"type": "boolean",
					"default": false,
					"description": "Run command in background and return PID instead of waiting for completion (no need to append '&')"
				}
			},
			"required": ["command"]
		}),
	}
}

// Each entry: (triggering programs, required tool name, hint message).
// The hint is only shown when the recommended tool is actually enabled.
static SHELL_MISUSE_HINTS: &[(&[&str], &str, &str)] = &[
	(
		&["cat", "head", "tail", "less", "more"],
		"view",
		"⚠️ Prefer `view` for reading files (line-numbered, supports ranges). Use shell only when piping output.",
	),
	(
		&["grep", "egrep", "fgrep", "rg"],
		"ast_grep",
		"⚠️ Prefer `ast_grep` for code search or `view` with content= for text search (.gitignore-aware). Use shell grep only for unsupported raw flags.",
	),
	(
		&["find", "ls"],
		"view",
		"⚠️ Prefer `view` for directory listing (.gitignore-aware, pattern/content filtering). Use shell only for system paths outside the project.",
	),
	(
		&["sed", "awk"],
		"text_editor",
		"⚠️ Prefer `text_editor` str_replace/line_replace for file edits (atomic, tracked). Use sed/awk only for stream transforms in pipelines.",
	),
];

// Detect shell commands that should use a dedicated MCP tool instead.
// Returns a hint only when the recommended tool is actually enabled in the current session.
fn detect_shell_misuse(command: &str) -> Option<&'static str> {
	let cmd = command.trim();

	// Check if cmd is exactly `prog` or starts with `prog ` / `prog\t`
	let is_prog = |prog: &str| -> bool {
		cmd == prog || cmd.starts_with(&format!("{prog} ")) || cmd.starts_with(&format!("{prog}\t"))
	};

	for (progs, _tool, hint) in SHELL_MISUSE_HINTS {
		if progs.iter().any(|p| is_prog(p)) {
			// Always emit hint — no tool_map in octofs
			return Some(hint);
		}
	}

	None
}

// Execute a shell command
pub async fn execute_shell_command(call: &McpToolCall) -> Result<McpToolResult> {
	use tokio::process::Command as TokioCommand;

	// Extract command parameter
	let command = match call.parameters.get("command") {
		Some(Value::String(cmd)) => {
			if cmd.trim().is_empty() {
				return Ok(McpToolResult::error(
					call.tool_name.clone(),
					call.tool_id.clone(),
					"Command parameter cannot be empty".to_string(),
				));
			}
			cmd.clone()
		}
		Some(_) => {
			return Ok(McpToolResult::error(
				call.tool_name.clone(),
				call.tool_id.clone(),
				"Command parameter must be a string".to_string(),
			));
		}
		None => {
			return Ok(McpToolResult::error(
				call.tool_name.clone(),
				call.tool_id.clone(),
				"Missing required 'command' parameter".to_string(),
			));
		}
	};

	// Extract background parameter
	let background = call
		.parameters
		.get("background")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// NOTE: We do NOT add MCP tool commands to shell history
	// NOTE: We do NOT add MCP tool commands to shell history
	// Only direct user commands via `octomind shell` CLI should persist to history
	// (see src/commands/shell.rs for user-initiated shell history)

	// Get the working directory from thread-local storage
	let working_dir = get_thread_working_directory();

	// Use tokio::process::Command for better cancellation support
	let mut cmd = if cfg!(target_os = "windows") {
		let mut cmd = TokioCommand::new("cmd");
		cmd.args(["/C", &command]);
		cmd.current_dir(&working_dir);
		cmd
	} else {
		let mut cmd = TokioCommand::new("sh");
		cmd.args(["-c", &command]);
		cmd.current_dir(&working_dir);
		cmd
	};

	// Force non-interactive: put the child in its own process group so it
	// cannot access the controlling terminal (/dev/tty opens fail with ENXIO
	// when combined with stdin=null set below). We use process_group(0)
	// instead of setsid() — setsid() creates a new *session* which makes the
	// child unreachable by the parent's process-group signals (e.g. when the
	// MCP client kills our process group on Ctrl+C). process_group(0) gives
	// us the /dev/tty isolation we need while keeping the child killable.
	#[cfg(unix)]
	{
		cmd.process_group(0);
	}

	// Configure the command based on execution mode
	if background {
		// Background execution: detach process and return PID immediately
		cmd.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.stdin(std::process::Stdio::null())
			.kill_on_drop(false); // Don't kill when dropped - let it run independently
	} else {
		// Foreground execution: capture output and wait for completion
		cmd.stdout(std::process::Stdio::piped())
			.stderr(std::process::Stdio::piped())
			.stdin(std::process::Stdio::null())
			.kill_on_drop(true); // CRITICAL: Kill process when dropped
	}

	// Spawn the process
	let child = cmd
		.spawn()
		.map_err(|e| anyhow!("Failed to spawn command: {}", e))?;

	// Handle background vs foreground execution
	if background {
		// Background execution: return PID immediately
		let pid = child
			.id()
			.ok_or_else(|| anyhow!("Failed to get process ID"))?;

		// Detach the child process so it continues running independently
		// We do this by forgetting the child handle, which prevents kill_on_drop
		std::mem::forget(child);

		return Ok(McpToolResult {
			tool_name: "shell".to_string(),
			tool_id: call.tool_id.clone(),
			result: json!({
				"success": true,
				"background": true,
				"pid": pid,
				"command": command,
				"message": format!("Command started in background with PID {pid}"),
				"note": format!("Use 'kill {pid}' to terminate this background process if needed")
			}),
		});
	}

	// Track the child's PID so kill_all_shell_children() can nuke its
	// entire process group (including grandchildren) on shutdown.
	let child_pid = child.id();
	if let Some(pid) = child_pid {
		register_child(pid);
	}

	// Foreground execution: wait for completion and return output
	let result = child.wait_with_output().await;

	// Child finished — remove from tracker before processing result.
	if let Some(pid) = child_pid {
		unregister_child(pid);
	}

	match result.map_err(|e| anyhow!("Command execution failed: {}", e)) {
		Ok(output) => {
			let stdout = String::from_utf8_lossy(&output.stdout).to_string();
			let stderr = String::from_utf8_lossy(&output.stderr).to_string();

			// Format the output more clearly with error handling
			let combined = if stderr.is_empty() {
				stdout
			} else if stdout.is_empty() {
				stderr
			} else {
				format!("{stdout}\n\nError: {stderr}")
			};

			// Apply global truncation (handled by global MCP response truncation)
			let final_output = combined;

			// Add detailed execution results including status code
			let status_code = output.status.code().unwrap_or(-1);
			let success = output.status.success();

			// Push misuse hint into accumulator — injected as a user message after all tools finish
			if let Some(hint) = detect_shell_misuse(&command) {
				crate::mcp::hint_accumulator::push_hint(hint);
			}

			// MCP Protocol Compliance: Use error() for failed commands, success() for successful ones
			if success {
				// Command succeeded - use success format
				Ok(McpToolResult::success(
					"shell".to_string(),
					call.tool_id.clone(),
					final_output,
				))
			} else {
				// Command failed - use error format per MCP protocol
				Ok(McpToolResult::error(
					"shell".to_string(),
					call.tool_id.clone(),
					format!(
						"Command failed with exit code {status_code}\nCommand: {command}\n\nOutput:\n{final_output}"
					),
				))
			}
		}
		Err(e) => Ok(McpToolResult::error(
			"shell".to_string(),
			call.tool_id.clone(),
			format!("Error: {e}"),
		)),
	}
}
