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

use super::*;

#[tokio::test]
async fn test_shell_misuse_always_rejected() {
	// No modes: misuse is a hard error, nothing executes.
	let call = crate::mcp::McpToolCall::test_call(
		"shell",
		serde_json::json!({ "command": "cat src/main.rs" }),
	);
	let err = execute_shell_command(&call, None)
		.await
		.expect_err("misuse must be rejected");
	assert!(err.to_string().contains("view"), "err: {err}");
}

#[tokio::test]
async fn test_quick_command_keeps_foreground_response() {
	let command = if cfg!(target_os = "windows") {
		"echo stdout & echo stderr 1>&2"
	} else {
		"printf stdout; printf stderr 1>&2"
	};
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_secs(5), None)
		.await
		.expect("quick command succeeds");
	assert!(outcome.resource_uri.is_none(), "quick command stays inline");
	assert_eq!(outcome.text, "stdout\n\nError: stderr");
}

#[tokio::test]
async fn test_foreground_timeout_promotes_same_command() {
	let command = if cfg!(target_os = "windows") {
		"echo started & ping -n 2 127.0.0.1 & echo finished"
	} else {
		"for i in 1 2; do echo tick-$i; sleep 1; done"
	};
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_millis(100), None)
		.await
		.expect("an overrun must be promoted, not killed");
	assert!(
		outcome.text.contains("automatically moved") && outcome.text.contains("same process"),
		"outcome: {}",
		outcome.text
	);
	let uri = outcome.resource_uri.expect("promoted job resource");
	let id = super::super::background::job_id_from_uri(&uri).expect("job id");
	let mut finished = None;
	for _ in 0..250 {
		let view = super::super::background::read(id).expect("promoted job is registered");
		if matches!(view.status, super::super::background::JobStatus::Exited(0)) {
			finished = Some(view);
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	let finished = finished.expect("the original child must finish");
	assert!(
		finished.output.contains("started") || finished.output.contains("tick-1"),
		"output from before promotion is preserved: {:?}",
		finished.output
	);
	assert!(
		finished.output.contains("finished") || finished.output.contains("tick-2"),
		"output after promotion is preserved: {:?}",
		finished.output
	);
}

#[tokio::test]
async fn test_rejects_remote_workdir() {
	let mut call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "echo hi" }));
	call.workdir = std::path::PathBuf::from("ssh://user@host:22/tmp");
	let err = execute_shell_command(&call, None)
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
	assert!(detect_shell_misuse("echo \"hello && ls\"").is_none());
	assert!(detect_shell_misuse("bash -lc 'cd /x && git log && ls'").is_none());

	// ssh remote commands obey the same rules as local ones
	assert!(detect_shell_misuse("ssh host 'cat file'").is_some());
	assert!(detect_shell_misuse("ssh host 'cd /path && ls'").is_some());
	assert!(detect_shell_misuse("ssh host \"cd /path && grep foo\"").is_some());
	assert!(detect_shell_misuse("ssh dev grep -rn foo /path").is_some());
	assert!(detect_shell_misuse("ssh -p 2222 user@host 'grep foo /x'").is_some());
	assert!(detect_shell_misuse("ssh -o StrictHostKeyChecking=no host 'ls /x'").is_some());
	assert!(detect_shell_misuse("ssh a 'ssh b \"grep x /y\"'").is_some());
	// Unquoted separators after a quoted block are still caught
	assert!(detect_shell_misuse("ssh host 'ls' && cat file").is_some());
	// Legitimate remote commands stay allowed
	assert!(detect_shell_misuse("ssh host uptime").is_none());
	assert!(detect_shell_misuse("ssh host 'systemctl status nginx'").is_none());
	assert!(detect_shell_misuse("ssh host 'cd /x && git log'").is_none());
	assert!(detect_shell_misuse("ssh host").is_none());
	assert!(detect_shell_misuse("ssh -N -L 8080:localhost:80 host").is_none());
	// Remote pipelines keep the local stream-transform leniency
	assert!(detect_shell_misuse("ssh host 'journalctl -u app | grep error'").is_none());
	// A pipe after ssh is a local downstream transform, not the remote command
	assert!(detect_shell_misuse("ssh host 'dmesg' | grep oops").is_none());
	// One quote layer strips; a nested interpreter stays opaque, same as locally
	assert!(
		detect_shell_misuse("ssh box@host 'bash -lc \"cd ~/work && git log && ls\"'").is_none()
	);

	// Bare / chained sleep is blocked in every common shape
	assert!(detect_shell_misuse("sleep 40").is_some());
	assert!(detect_shell_misuse("sleep 40; echo done").is_some());
	assert!(detect_shell_misuse("sleep 5 && cargo test").is_some());
	assert!(detect_shell_misuse("cargo build && sleep 5").is_some());
	assert!(detect_shell_misuse("sleep 30 || true").is_some());
	assert!(detect_shell_misuse("sleep $((5*60))").is_some());
	assert!(detect_shell_misuse("(sleep 5 && echo hi) &").is_some());
	// Sleep inside a do...done loop body is legitimate polling
	assert!(detect_shell_misuse("until test -f /tmp/x; do sleep 2; done").is_none());
	assert!(detect_shell_misuse("while ! nc -z localhost 8080; do sleep 1; done").is_none());
	assert!(detect_shell_misuse("while true; do echo waiting; sleep 5; done").is_none());
	// Loop depth resets after `done` — a trailing sleep is still caught
	assert!(detect_shell_misuse("until ok; do sleep 1; done; sleep 40").is_some());

	// Subshell/group openers no longer hide a forbidden program
	assert!(detect_shell_misuse("(cat file)").is_some());
	assert!(detect_shell_misuse("{ grep foo bar; }").is_some());

	// Writing file content via shell redirects is blocked
	assert!(detect_shell_misuse("echo 'fn main() {}' > src/main.rs").is_some());
	assert!(detect_shell_misuse("printf '%s\\n' hi >> notes.txt").is_some());
	assert!(detect_shell_misuse("tee out.txt").is_some());
	assert!(detect_shell_misuse("cd /x && echo data > f").is_some());
	// cat with a redirect gets the write guidance, not the read guidance
	let msg = detect_shell_misuse("cat > f.txt").unwrap();
	assert!(msg.contains("text_editor"), "msg: {msg}");
	// Redirecting other programs' output stays allowed
	assert!(detect_shell_misuse("cargo test > out.log 2>&1").is_none());
	assert!(detect_shell_misuse("make 2>&1 | tee build.log").is_none());
	// Fd duplication and quoted `>` are not file writes
	assert!(detect_shell_misuse("echo error >&2").is_none());
	assert!(detect_shell_misuse("echo \"a > b\"").is_none());

	// Never-terminating programs are blocked
	assert!(detect_shell_misuse("watch -n1 date").is_some());
	assert!(detect_shell_misuse("top").is_some());
}
