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

// Each entry: (triggering programs, error message with usage example).
// Checked before execution — misuse is always a hard error: the dedicated tools
// are strictly better for the model (line ids, gitignore-awareness, remote hosts),
// so shell access to these programs is intentionally blocked.
static SHELL_MISUSE_HINTS: &[(&[&str], &str)] = &[
	(
		&["cat", "head", "tail", "less", "more"],
		"Reading files with this command is forbidden — use `view` instead (line-numbered, supports ranges, works on remote hosts).\n\n  Example:\n    view path=\"src/main.rs\"                # read whole file\n    view path=\"src/main.rs\" start=10 end=50  # read lines 10–50\n    view path=\"ssh://user@host/path/file\"   # remote file — no `ssh cat` needed\n\n  Shell is allowed only for pipelines or system paths outside the project.",
	),
	(
		&["grep", "egrep", "fgrep", "rg"],
		"Searching file text with this command is forbidden — use `view` with content= instead (gitignore-aware, context lines, line numbers, works on remote hosts).\n\n  Example:\n    view path=\"src/main.rs\" content=\"fulfill_input_requests\"\n    view path=\"src/\" content=\"TODO\" regex=true\n    view path=\"ssh://user@host/dir\" content=\"TODO\"  # remote search — no `ssh grep` needed\n\n  Shell is allowed only for pipelines or system paths outside the project.",
	),
	(
		&["find", "ls"],
		"Directory listing with this command is forbidden — use `view` instead (.gitignore-aware, pattern/content filtering, works on remote hosts).\n\n  Example:\n    view path=\"src/\"                # list directory\n    view path=\"src/\" pattern=\"*.rs\"  # ripgrep-style glob filter (`*`, `**`, `?`, `[abc]`, `{a,b}`, leading `!`)\n    view path=\"ssh://user@host/dir\"  # remote listing — no `ssh ls` needed\n\n  A pattern without `/` matches filenames at any depth; with `/`, it matches the returned relative path. Use `|` for ordered globs.\n  Shell is allowed only for system paths outside the project.",
	),
	(
		&["sed", "awk"],
		"Editing files with this command is forbidden — use `text_editor` str_replace or `batch_edit` instead (atomic, tracked, works on remote hosts via ssh:// paths).\n\n  Example:\n    text_editor command=\"str_replace\" path=\"src/main.rs\" old_text=\"foo\" new_text=\"bar\"\n\n  Shell is allowed only for stream transforms in pipelines.",
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
// Returns the misuse guidance message the caller rejects the command with.
fn detect_shell_misuse(command: &str) -> Option<&'static str> {
	// Split into individual commands on shell separators, respecting
	// quoting so that `&&`/`||`/`;` inside quoted strings (e.g. SSH remote
	// commands: `ssh host 'cd /path && ls'`) are not treated as local
	// separators. Pipelines (`|`) are intentionally NOT split: stream
	// transforms such as `cargo build 2>&1 | grep error` remain allowed.
	for segment in split_shell_segments(command) {
		let segment = segment.trim();
		// Skip leading env assignments (FOO=bar cmd ...) to reach the program.
		let prog = segment
			.split_whitespace()
			.find(|tok| !tok.contains('='))
			.unwrap_or("");
		// Strip any path prefix: /bin/grep -> grep
		let prog = prog.rsplit('/').next().unwrap_or(prog);

		for (progs, hint) in SHELL_MISUSE_HINTS {
			if progs.contains(&prog) {
				return Some(hint);
			}
		}
	}

	None
}

/// Split a shell command string into segments on `;`, `&&`, `||`, `\n`,
/// `$(`, and backticks — but only when those separators appear *outside*
/// single or double quotes. This prevents false positives where a quoted
/// remote command (e.g. `ssh host 'cd /x && ls'`) contains `&&` that is
/// part of the remote command, not a local shell separator.
fn split_shell_segments(command: &str) -> Vec<&str> {
	let bytes = command.as_bytes();
	let mut segments = Vec::new();
	let mut start = 0;
	let mut i = 0;
	let mut in_single = false;
	let mut in_double = false;

	while i < bytes.len() {
		let b = bytes[i];
		if in_single {
			if b == b'\'' {
				in_single = false;
			}
		} else if in_double {
			if b == b'\\' && i + 1 < bytes.len() {
				i += 1; // skip escaped char inside double quotes
			} else if b == b'"' {
				in_double = false;
			}
		} else {
			match b {
				b'\'' => in_single = true,
				b'"' => in_double = true,
				b'\\' if i + 1 < bytes.len() => i += 1, // skip escaped char
				b';' | b'\n' => {
					segments.push(&command[start..i]);
					start = i + 1;
				}
				b'&' if i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
					segments.push(&command[start..i]);
					i += 1;
					start = i + 1;
				}
				b'|' if i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
					segments.push(&command[start..i]);
					i += 1;
					start = i + 1;
				}
				b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'(' => {
					segments.push(&command[start..i]);
					i += 1;
					start = i + 1;
				}
				b'`' => {
					segments.push(&command[start..i]);
					start = i + 1;
				}
				_ => {}
			}
		}
		i += 1;
	}
	segments.push(&command[start..]);
	segments
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

	// Reject commands that can be done with dedicated MCP tools.
	if let Some(msg) = detect_shell_misuse(&command) {
		bail!("{msg}");
	}

	// Extract background parameter
	let background = call
		.parameters
		.get("background")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	// Get the working directory from the call context
	let working_dir = call.workdir.clone();

	// Shell commands always execute on the LOCAL machine. With a remote (ssh://)
	// workdir there is nowhere sane to run them — fail with guidance instead of a
	// cryptic spawn failure on the URL-shaped cwd.
	if crate::mcp::fs::parse_path_source(&working_dir.to_string_lossy()).is_remote() {
		bail!(
			"The shell tool runs commands on the local machine only, but the working directory is remote ({}). Use view/text_editor/batch_edit/extract_lines for remote file access.",
			working_dir.display()
		);
	}

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

		#[cfg(unix)]
		let kill_hint = format!("kill -- -{pid} (negative PID kills its whole process group)");
		#[cfg(not(unix))]
		let kill_hint = format!("taskkill /PID {pid} /T /F (/T kills the whole process tree)");

		return Ok(format!(
			"Command started in background with PID {pid}\nTo terminate it later run: {kill_hint}"
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

	#[tokio::test]
	async fn test_shell_misuse_always_rejected() {
		// No modes: misuse is a hard error, nothing executes.
		let call = crate::mcp::McpToolCall::test_call(
			"shell",
			serde_json::json!({ "command": "cat src/main.rs" }),
		);
		let err = execute_shell_command(&call)
			.await
			.expect_err("misuse must be rejected");
		assert!(err.to_string().contains("view"), "err: {err}");
	}

	#[tokio::test]
	async fn test_rejects_remote_workdir() {
		let mut call = crate::mcp::McpToolCall::test_call(
			"shell",
			serde_json::json!({ "command": "echo hi" }),
		);
		call.workdir = std::path::PathBuf::from("ssh://user@host:22/tmp");
		let err = execute_shell_command(&call)
			.await
			.expect_err("remote workdir must be rejected");
		assert!(err.to_string().contains("local machine"), "err: {err}");
	}

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

	#[test]
	fn test_detect_shell_misuse() {
		// Bare commands are caught
		assert!(detect_shell_misuse("grep -rn foo src/").is_some());
		assert!(detect_shell_misuse("cat src/main.rs").is_some());
		assert!(detect_shell_misuse("ls -la").is_some());
		assert!(detect_shell_misuse("find . -name '*.rs'").is_some());

		// Compound commands: forbidden tool after a separator is caught
		assert!(detect_shell_misuse("cd /path && grep -rn foo").is_some());
		assert!(detect_shell_misuse("cd /path; cat file.rs").is_some());
		assert!(detect_shell_misuse("true || ls -la").is_some());
		assert!(detect_shell_misuse("echo $(grep foo bar)").is_some());
		assert!(detect_shell_misuse("echo `cat file`").is_some());
		assert!(detect_shell_misuse("cd /path\ngrep -rn foo").is_some());

		// Path-qualified and env-prefixed invocations are caught
		assert!(detect_shell_misuse("/bin/grep foo bar").is_some());
		assert!(detect_shell_misuse("FOO=bar grep x y").is_some());

		// Pipelines stay allowed (stream transforms)
		assert!(detect_shell_misuse("cargo build 2>&1 | grep error").is_none());
		// Legitimate commands pass
		assert!(detect_shell_misuse("cargo test").is_none());
		assert!(detect_shell_misuse("git status && git diff").is_none());
		assert!(detect_shell_misuse("echo grep").is_none());

		// Quoted separators are not treated as local command separators
		// (e.g. SSH remote commands with && inside quotes)
		assert!(detect_shell_misuse("ssh host 'cd /path && ls'").is_none());
		assert!(detect_shell_misuse("ssh host 'cat file'").is_none());
		assert!(detect_shell_misuse("ssh host \"cd /path && grep foo\"").is_none());
		assert!(detect_shell_misuse("echo \"hello && ls\"").is_none());
		assert!(detect_shell_misuse("bash -lc 'cd /x && git log && ls'").is_none());
		assert!(
			detect_shell_misuse("ssh box@host 'bash -lc \"cd ~/work && git log && ls\"'").is_none()
		);
		// Unquoted separators after a quoted block are still caught
		assert!(detect_shell_misuse("ssh host 'ls' && cat file").is_some());
	}
}
