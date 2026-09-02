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
use std::time::Duration;

// Every command starts in the foreground for quick, interactive-latency work.
// Anything still running at this deadline is automatically promoted to a
// background job without killing or restarting it.
#[cfg(not(test))]
const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const FOREGROUND_TIMEOUT: Duration = Duration::from_millis(200);
// Track PIDs of in-flight foreground shell children.
// Each child is spawned with process_group(0) so PGID == child PID.
// On shutdown we kill(-pid, SIGKILL) to terminate the entire process group,
// including any grandchildren the command may have spawned.
static SHELL_CHILDREN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

pub(super) fn register_child(pid: u32) {
	SHELL_CHILDREN
		.lock()
		.unwrap()
		.get_or_insert_with(HashSet::new)
		.insert(pid);
}

pub(super) fn unregister_child(pid: u32) {
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
		"Reading files with the shell is blocked — use `view` for any path, local or remote: view path=\"src/main.rs\" start=10 end=50, view path=\"ssh://host/~/file\". Only a later pipeline stage (`cargo test 2>&1 | tail -20`) stays allowed.",
	),
	(
		&["grep", "egrep", "fgrep", "rg"],
		"Searching with the shell is blocked — use `view` with content= for any path, local or remote: view path=\"src/\" content=\"TODO\" regex=true. Only a later pipeline stage (`cargo build 2>&1 | grep error`) stays allowed.",
	),
	(
		&["find", "ls"],
		"Listing with the shell is blocked — use `view` for any path, local or remote: view path=\"src/\" pattern=\"*.rs\" (ripgrep glob), view path=\"ssh://host/~/dir\".",
	),
	(
		&["sed", "awk"],
		"Editing files with the shell is blocked — use `text_editor` str_replace or `batch_edit` (atomic, tracked, remote-capable). Only a later pipeline stage (`... | sed 's/a/b/'`) stays allowed.",
	),
	(
		&["sleep"],
		"Bare `sleep` is blocked — it wastes the call. Poll a condition instead: until <check>; do sleep 2; done. Commands you start move to the background automatically and notify you on exit, so never sleep or chain short sleeps to wait for them.",
	),
	(
		&["watch", "top", "htop"],
		"This program never exits, so it would never complete or notify you. Run the underlying command once; long runs move to the background automatically.",
	),
];

// Writing file content from the shell (`echo ... > file`, heredocs into cat/tee)
// breaks on quoting/escaping and bypasses tracked edits. Redirecting other
// programs' OUTPUT to a file stays allowed — only content-authoring is blocked.
static REDIRECT_WRITE_HINT: &str = "Writing file content with echo/printf/cat/tee redirects is blocked — quoting corrupts content and the write is untracked. Use text_editor command=\"create\" or str_replace. Redirecting a program's output (e.g. `cargo test > out.log`) stays allowed.";

// Force well-behaved interactive tools to fail fast instead of prompting.
// stdin=null + process_group(0) already makes input physically impossible;
// these env vars make cooperative tools surface a clean error instead of
// printing a prompt and hitting EOF mid-read (or invoking a pager that
// misbehaves without a TTY).
pub(super) static NONINTERACTIVE_ENVS: &[(&str, &str)] = &[
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
	// Depth of `do ... done` loop bodies: `sleep` there is legitimate polling
	// (`until <check>; do sleep 2; done`); everywhere else it's dead waiting.
	let mut loop_depth = 0usize;
	// Split into individual commands on shell separators, respecting
	// quoting so that `&&`/`||`/`;` inside quoted strings (e.g. SSH remote
	// commands: `ssh host 'cd /path && ls'`) are not treated as local
	// separators. Pipelines (`|`) are intentionally NOT split: stream
	// transforms such as `cargo build 2>&1 | grep error` remain allowed.
	for segment in split_shell_segments(command) {
		let segment = segment.trim();
		// Skip leading env assignments (FOO=bar cmd ...) and group openers
		// (`{`) to reach the program; strip subshell parens glued to tokens
		// (`(cat file)`) so they can't hide a forbidden program.
		let prog = segment
			.split_whitespace()
			.map(|tok| tok.trim_start_matches('(').trim_end_matches(')'))
			.find(|tok| !tok.is_empty() && !tok.contains('=') && *tok != "{")
			.unwrap_or("");
		// Strip any path prefix: /bin/grep -> grep
		let prog = prog.rsplit('/').next().unwrap_or(prog);

		match prog {
			// The command after `do` shares its segment, so `do sleep 2`
			// already passes today — keep that leniency unchanged.
			"do" => {
				loop_depth += 1;
				continue;
			}
			"done" => {
				loop_depth = loop_depth.saturating_sub(1);
				continue;
			}
			"sleep" if loop_depth > 0 => continue,
			_ => {}
		}

		// A remote command obeys the same rules as a local one — `ssh host
		// 'grep …'` must not dodge the gate that view/text_editor already
		// cover via ssh:// paths.
		if prog == "ssh" {
			if let Some(remote) = ssh_remote_command(segment) {
				if let Some(hint) = detect_shell_misuse(remote) {
					return Some(hint);
				}
			}
			continue;
		}

		// Content-authoring programs writing a file via redirect, or standalone
		// tee (writes stdin to a file). Checked before the hint table so
		// `cat > file` gets the write guidance, not the read guidance.
		if prog == "tee"
			|| (matches!(prog, "echo" | "printf" | "cat") && has_file_redirect(segment))
		{
			return Some(REDIRECT_WRITE_HINT);
		}

		for (progs, hint) in SHELL_MISUSE_HINTS {
			if progs.contains(&prog) {
				return Some(hint);
			}
		}
	}

	None
}

/// True if the segment contains an unquoted `>` or `>>` file redirect.
/// Fd duplications (`>&2`, `2>&1`) are not file writes and don't count.
fn has_file_redirect(segment: &str) -> bool {
	let bytes = segment.as_bytes();
	let mut in_single = false;
	let mut in_double = false;
	let mut i = 0;
	while i < bytes.len() {
		match bytes[i] {
			b'\'' if !in_double => in_single = !in_single,
			b'"' if !in_single => in_double = !in_double,
			b'\\' if !in_single => i += 1, // skip escaped char
			b'>' if !in_single && !in_double => {
				let mut j = i + 1;
				if j < bytes.len() && bytes[j] == b'>' {
					j += 1;
				}
				while j < bytes.len() && bytes[j] == b' ' {
					j += 1;
				}
				if j < bytes.len() && bytes[j] != b'&' {
					return true;
				}
				i = j;
				continue;
			}
			_ => {}
		}
		i += 1;
	}
	false
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

/// The remote-command part of an `ssh …` segment, cut at any local pipe and
/// with one whole-string quote layer stripped, so `detect_shell_misuse` can
/// check it by the same rules as a local command. None when ssh runs no
/// remote command (interactive session, -N port forwarding).
fn ssh_remote_command(segment: &str) -> Option<&str> {
	// Single-letter ssh options that take the next token as their argument
	// when not glued to it (`-p 22` vs `-p22`).
	const ARG_OPTS: &[u8] = b"bBcDEeFIiJLlmOopQRSWw";

	let spans = shell_token_spans(segment);
	// Land on the `ssh` token itself, skipping env prefixes and group openers
	// the same way prog extraction does.
	let mut idx = spans.iter().position(|&(s, e)| {
		let tok = segment[s..e].trim_start_matches('(');
		!tok.is_empty() && !tok.contains('=') && tok != "{"
	})?;
	idx += 1; // step past `ssh`

	// Skip options; the first non-option token is the destination.
	while idx < spans.len() {
		let (s, e) = spans[idx];
		let tok = &segment[s..e];
		idx += 1;
		let Some(opt) = tok.strip_prefix('-') else {
			// Destination reached; the remote command is everything after it,
			// up to a local pipe — `ssh host 'dmesg' | grep x` pipes locally,
			// keeping the same downstream-transform leniency as pipelines.
			let rest = segment[e..].trim();
			let rest = rest[..unquoted_pipe(rest).unwrap_or(rest.len())].trim();
			if rest.is_empty() {
				return None;
			}
			// Strip one whole-string quote layer so the remote command parses
			// like a local one (`"cd /x && grep y"` → `cd /x && grep y`).
			let b = rest.as_bytes();
			if rest.len() >= 2
				&& (b[0] == b'\'' || b[0] == b'"')
				&& b[rest.len() - 1] == b[0]
				&& shell_token_spans(rest).len() == 1
			{
				return Some(&rest[1..rest.len() - 1]);
			}
			return Some(rest);
		};
		// A cluster of plain option letters ending in an arg-taker consumes
		// the next token; glued args (`-p22`, `-oKey=val`) do not.
		if opt.bytes().all(|c| c.is_ascii_alphabetic())
			&& opt.bytes().last().is_some_and(|c| ARG_OPTS.contains(&c))
		{
			idx += 1;
		}
	}
	None
}

/// Byte offset of the first `|` outside quotes. `||` cannot appear here —
/// segment splitting already consumed it — so any unquoted `|` is a pipe.
fn unquoted_pipe(s: &str) -> Option<usize> {
	let bytes = s.as_bytes();
	let mut in_single = false;
	let mut in_double = false;
	let mut i = 0;
	while i < bytes.len() {
		match bytes[i] {
			b'\'' if !in_double => in_single = !in_single,
			b'"' if !in_single => in_double = !in_double,
			b'\\' if !in_single && i + 1 < bytes.len() => i += 1,
			b'|' if !in_single && !in_double => return Some(i),
			_ => {}
		}
		i += 1;
	}
	None
}

/// Whitespace-delimited token spans with shell quoting respected, so a quoted
/// argument containing spaces stays one token.
fn shell_token_spans(s: &str) -> Vec<(usize, usize)> {
	let bytes = s.as_bytes();
	let mut spans = Vec::new();
	let mut i = 0;
	while i < bytes.len() {
		while i < bytes.len() && bytes[i].is_ascii_whitespace() {
			i += 1;
		}
		if i >= bytes.len() {
			break;
		}
		let start = i;
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
					i += 1;
				} else if b == b'"' {
					in_double = false;
				}
			} else {
				match b {
					b'\'' => in_single = true,
					b'"' => in_double = true,
					b'\\' if i + 1 < bytes.len() => i += 1,
					_ if b.is_ascii_whitespace() => break,
					_ => {}
				}
			}
			i += 1;
		}
		spans.push((start, i));
	}
	spans
}

/// Delivers a finished background job's resource URI so the server layer can
/// emit `resources/updated`. Boxed so `shell.rs` stays free of MCP/peer types —
/// the server constructs one that captures the rmcp peer.
pub type BackgroundNotify = Box<dyn FnOnce(String) + Send>;

/// The outcome of a shell call. `resource_uri` is set only when the command was
/// automatically promoted to the background: the server advertises it as an
/// MCP resource link so the client can follow it generically. Commands that
/// finish inside the foreground window carry their output inline.
#[derive(Debug)]
pub struct ShellOutcome {
	pub text: String,
	pub resource_uri: Option<String>,
}

impl ShellOutcome {
	fn text(text: String) -> Self {
		Self {
			text,
			resource_uri: None,
		}
	}
}

// Execute a shell command
pub async fn execute_shell_command(
	call: &McpToolCall,
	on_background: Option<BackgroundNotify>,
) -> Result<ShellOutcome> {
	execute_with_timeout(call, FOREGROUND_TIMEOUT, on_background).await
}

async fn execute_with_timeout(
	call: &McpToolCall,
	timeout: Duration,
	on_background: Option<BackgroundNotify>,
) -> Result<ShellOutcome> {
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

	// Get the working directory from the call context
	let working_dir = call.workdir.clone();

	// Shell commands always execute on the LOCAL machine. With a remote (ssh://)
	// workdir there is nowhere sane to run them — fail with guidance instead of a
	// cryptic spawn failure on the URL-shaped cwd.
	if crate::mcp::fs::parse_path_source(&working_dir.to_string_lossy()).is_remote() {
		bail!(
			"The shell runs on the local machine, but the working directory is remote ({}). Use view/text_editor/batch_edit for remote files.",
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

	// Capture output in durable files from byte one. A command that crosses the
	// foreground deadline can then keep running as the exact same child while the
	// background resource continues exposing all output produced so far.
	let prepared = super::background::prepare(&command, &working_dir).await?;
	let super::background::PreparedJob {
		mut job,
		stdout_file,
		stderr_file,
	} = prepared;
	cmd.stdout(std::process::Stdio::from(stdout_file))
		.stderr(std::process::Stdio::from(stderr_file))
		.stdin(std::process::Stdio::null())
		// Cancellation before promotion still cleans up the direct child. The
		// timeout path moves `child`, so it does not trigger this guard.
		.kill_on_drop(true);

	// Spawn the process
	let mut child = match cmd.spawn() {
		Ok(child) => child,
		Err(error) => {
			drop(cmd);
			super::background::discard(&job).await;
			return Err(anyhow!("Failed to spawn command: {}", error));
		}
	};

	// Track the child's PID so kill_all_shell_children() can nuke its
	// entire process group (including grandchildren) on shutdown.
	let Some(child_pid) = child.id() else {
		drop(child);
		super::background::discard(&job).await;
		bail!("Failed to get shell process ID");
	};
	job.pid = child_pid;
	register_child(child_pid);

	// Wait normally until the foreground deadline. If it expires, transfer the
	// still-running child into the background registry; dropping the timed wait
	// does not drop, kill, or restart the child.
	let result = match tokio::time::timeout(timeout, child.wait()).await {
		Ok(result) => result,
		Err(_) => {
			let job_id = job.id.clone();
			let job_pid = job.pid;
			let uri = super::background::resource_uri(&job_id);
			let notify = on_background.unwrap_or_else(|| Box::new(|_: String| {}));
			super::background::promote(job, child, notify);
			return Ok(ShellOutcome {
				text: format!(
					"Still running after the foreground limit — moved to background job `{}` \
					 (PID {}). Output keeps streaming to the linked resource; you will be \
					 notified on exit with the exit code and output tail. Stop early: kill -- -{}",
					job_id, job_pid, job_pid
				),
				resource_uri: Some(uri),
			});
		}
	};

	// Child finished — remove from tracker before processing result.
	drop(child);
	unregister_child(child_pid);

	let stdout = tokio::fs::read(&job.stdout_path).await;
	let stderr = tokio::fs::read(&job.stderr_path).await;
	super::background::discard(&job).await;

	match result.map_err(|e| anyhow!("Command execution failed: {}", e)) {
		Ok(status) => {
			let stdout = stdout.map_err(|e| anyhow!("Failed to read command stdout: {}", e))?;
			let stderr = stderr.map_err(|e| anyhow!("Failed to read command stderr: {}", e))?;
			let stdout = clean_terminal_noise(&String::from_utf8_lossy(&stdout));
			let stderr = clean_terminal_noise(&String::from_utf8_lossy(&stderr));

			// Label stderr by stream, not verdict: warnings and progress land there
			// on success too, and the exit code below is the only failure signal.
			let final_output = if stderr.is_empty() {
				stdout
			} else if stdout.is_empty() {
				stderr
			} else {
				format!("{stdout}\n\nstderr:\n{stderr}")
			};

			// Add detailed execution results including status code
			let status_code = status.code().unwrap_or(-1);
			let success = status.success();

			// MCP Protocol Compliance: Use error() for failed commands, success() for successful ones
			if success {
				Ok(ShellOutcome::text(final_output))
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
#[path = "shell_tests.rs"]
mod shell_tests;
