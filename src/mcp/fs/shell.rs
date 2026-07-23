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

// Shell execution functionality for the Filesystem MCP provider

use super::super::McpToolCall;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
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

// Each entry: (triggering programs, hint message).
static SHELL_MISUSE_HINTS: &[(&[&str], &str)] = &[
	(
		&["cat", "head", "tail", "less", "more"],
		"⚠️ Prefer `view` for reading files (line-numbered, supports ranges). Use shell only when piping output.",
	),
	(
		&["grep", "egrep", "fgrep", "rg"],
		"⚠️ Use `view` with content= to search for text in files or directories (gitignore-aware, context lines, line numbers).",
	),
	(
		&["find", "ls"],
		"⚠️ Prefer `view` for directory listing (.gitignore-aware, pattern/content filtering). Use shell only for system paths outside the project.",
	),
	(
		&["sed", "awk"],
		"⚠️ Prefer `text_editor` str_replace or `batch_edit` for file edits (atomic, tracked). Use sed/awk only for stream transforms in pipelines.",
	),
];

// Force well-behaved interactive tools to fail fast instead of prompting.
// stdin=null + process_group(0) already makes input physically impossible;
// these env vars make cooperative tools surface a clean error instead of
// printing a prompt and hitting EOF mid-read (or invoking a pager that
// misbehaves without a TTY).
static NONINTERACTIVE_ENVS: &[(&str, &str)] = &[
	("GIT_TERMINAL_PROMPT", "0"), // git: fail instead of prompting for credentials
	("DEBIAN_FRONTEND", "noninteractive"), // apt/dpkg: never prompt
	("PAGER", "cat"),             // don't invoke less (hangs / garbled without TTY)
	("GIT_PAGER", "cat"),         // same for git log/diff/show
	("NO_COLOR", "1"),            // suppress ANSI colors at the source (no-color.org)
];

// Matches terminal escape sequences: CSI (colors, cursor movement), OSC (titles,
// hyperlinks; BEL- or ST-terminated), and any other two-char ESC sequence.
// ponytail: regex covers real-world output; swap in the `strip-ansi-escapes` crate
// (vte parser) if exotic partial sequences ever survive.
static ANSI_ESCAPES: OnceLock<regex::Regex> = OnceLock::new();

fn ansi_re() -> &'static regex::Regex {
	ANSI_ESCAPES.get_or_init(|| {
		regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|.)").unwrap()
	})
}

// Compact captured output to exactly what a human would see on the terminal after
// the command finished — nothing a tool printed as a real line is dropped:
//   - ANSI escape sequences (colors, cursor movement, OSC links) render as nothing
//   - \r-overwritten progress frames: only the final frame is visible
//     (curl/pip/wget redraw the same line hundreds of times into captured stderr)
//   - backspaces erase the character before them (spinner animations)
//   - remaining control chars (BEL, NUL, …) render nothing
//   - trailing whitespace is invisible (progress frames pad with spaces to erase
//     longer previous frames); leading/trailing blank lines carry no information
//   - runs of identical consecutive lines (repeated warnings, wget dots, retry
//     loops) collapse to the line plus a repeat count — the count preserves the
//     information, the repeats were pure token cost
// Leading spaces on the first content line are kept — they may be table alignment.
fn clean_terminal_noise(raw: &str) -> String {
	let stripped = ansi_re().replace_all(raw, "");
	let mut lines: Vec<String> = Vec::new();
	let mut repeats = 0usize;
	for line in stripped.split('\n') {
		// CRLF: the trailing \r is a line ending, not a progress redraw.
		let line = line.strip_suffix('\r').unwrap_or(line);
		// A mid-line \r overwrites everything before it — keep the final frame.
		let line = line.rsplit('\r').next().unwrap_or(line);
		let mut out = String::with_capacity(line.len());
		for ch in line.chars() {
			match ch {
				'\u{8}' => {
					out.pop();
				}
				c if c.is_control() && c != '\t' => {}
				c => out.push(c),
			}
		}
		out.truncate(out.trim_end().len());

		// Blank lines are exempt from run-collapsing — a marker after a blank
		// line reads as nonsense and blank runs are cheap anyway.
		if !out.is_empty() && lines.last() == Some(&out) {
			repeats += 1;
			continue;
		}
		flush_repeats(&mut lines, &mut repeats);
		lines.push(out);
	}
	flush_repeats(&mut lines, &mut repeats);
	lines
		.join("\n")
		.trim_start_matches('\n')
		.trim_end()
		.to_string()
}

// Emit the pending duplicate-run count from clean_terminal_noise.
fn flush_repeats(lines: &mut Vec<String>, repeats: &mut usize) {
	match *repeats {
		0 => {}
		// A single duplicate is kept verbatim — the marker would cost more.
		1 => {
			if let Some(last) = lines.last().cloned() {
				lines.push(last);
			}
		}
		n => lines.push(format!("[... last line repeated {n} more times]")),
	}
	*repeats = 0;
}

// Detect shell commands that should use a dedicated MCP tool instead.
fn detect_shell_misuse(command: &str) -> Option<&'static str> {
	let cmd = command.trim();

	// Check if cmd is exactly `prog` or starts with `prog ` / `prog\t`
	let is_prog = |prog: &str| -> bool {
		cmd == prog || cmd.starts_with(&format!("{prog} ")) || cmd.starts_with(&format!("{prog}\t"))
	};

	for (progs, hint) in SHELL_MISUSE_HINTS {
		if progs.iter().any(|p| is_prog(p)) {
			return Some(hint);
		}
	}

	None
}

// Execute a shell command
pub async fn execute_shell_command(call: &McpToolCall) -> Result<String> {
	use tokio::process::Command as TokioCommand;

	// Extract command parameter
	let command = match call.parameters.get("command") {
		Some(Value::String(cmd)) => {
			if cmd.trim().is_empty() {
				bail!("Command parameter cannot be empty");
			}
			cmd.clone()
		}
		Some(_) => {
			bail!("Command parameter must be a string");
		}
		None => {
			bail!("Missing required 'command' parameter");
		}
	};

	// Extract background parameter
	let background = call
		.parameters
		.get("background")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// Get the working directory from the call context
	let working_dir = call.workdir.clone();
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

	// Inject environment variables that force non-interactive behavior.
	// This prevents prompts for credentials (git), passwords (sudo),
	// editor launches, and confirmation dialogs.
	for (key, value) in NONINTERACTIVE_ENVS {
		cmd.env(key, value);
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

		// Drop (don't forget) the handle: kill_on_drop is false so the child keeps
		// running, and tokio's background reaper collects its exit status — forgetting
		// the handle would leave a zombie behind once the child exits.
		drop(child);

		return Ok(format!(
			"Command started in background with PID {pid}\nTo terminate it later run: kill -- -{pid} (negative PID kills its whole process group)"
		));
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
			let stdout = clean_terminal_noise(&String::from_utf8_lossy(&output.stdout));
			let stderr = clean_terminal_noise(&String::from_utf8_lossy(&output.stderr));

			// Format the output more clearly with error handling
			let final_output = if stderr.is_empty() {
				stdout
			} else if stdout.is_empty() {
				stderr
			} else {
				format!("{stdout}\n\nError: {stderr}")
			};

			// Add detailed execution results including status code
			let status_code = output.status.code().unwrap_or(-1);
			let success = output.status.success();

			// Push misuse hint into accumulator — injected as a user message after all tools finish
			if let Some(hint) = detect_shell_misuse(&command) {
				crate::mcp::hint_accumulator::push_hint(hint);
			}

			// MCP Protocol Compliance: Use error() for failed commands, success() for successful ones
			if success {
				Ok(final_output)
			} else {
				// No command echo: the model just wrote the command and results are
				// matched by tool id — repeating it back is pure token cost.
				bail!("Command failed with exit code {status_code}\n\n{final_output}")
			}
		}
		Err(e) => bail!("Error: {e}"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_clean_terminal_noise() {
		// ANSI colors and cursor codes stripped
		assert_eq!(clean_terminal_noise("\x1b[1;32mok\x1b[0m"), "ok");
		assert_eq!(clean_terminal_noise("\x1b[2K\x1b[1Adone"), "done");
		// OSC hyperlink wrapper stripped, visible text kept
		assert_eq!(
			clean_terminal_noise("\x1b]8;;http://x\x07link\x1b]8;;\x07"),
			"link"
		);
		// \r progress frames collapse to the final visible frame
		assert_eq!(
			clean_terminal_noise("Downloading 10%\rDownloading 55%\rDone.\n"),
			"Done."
		);
		// CRLF line endings are line endings, not progress redraws
		assert_eq!(clean_terminal_noise("a\r\nb\r\n"), "a\nb");
		// Backspaces erase like on a real terminal; stray BEL renders nothing
		assert_eq!(clean_terminal_noise("abcd\x08\x08X"), "abX");
		assert_eq!(clean_terminal_noise("ding\x07!"), "ding!");
		// Invisible trailing padding and blank lines around output are dropped;
		// leading spaces on the first content line survive (table alignment)
		assert_eq!(clean_terminal_noise("Done.   \t\n\n\n"), "Done.");
		assert_eq!(
			clean_terminal_noise("\n\n  % Total\nbody"),
			"  % Total\nbody"
		);
		// Runs of identical lines collapse to line + count; info is preserved
		assert_eq!(
			clean_terminal_noise("same\nsame\nsame\nsame\nnext"),
			"same\n[... last line repeated 3 more times]\nnext"
		);
		// A line appearing just twice stays verbatim (marker would cost more)
		assert_eq!(clean_terminal_noise("dup\ndup\nend"), "dup\ndup\nend");
		// Plain output passes through untouched
		assert_eq!(clean_terminal_noise("hello\nworld"), "hello\nworld");
	}
}
