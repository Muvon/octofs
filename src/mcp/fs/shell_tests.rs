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

//! Shell execution, background promotion, and misuse-policy tests.

use super::*;

#[tokio::test]
async fn test_shell_misuse_is_rejected() {
	let call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "sleep 5" }));
	let err = execute_shell_command(&call, None)
		.await
		.expect_err("hard mode rejects misuse");
	assert!(err.to_string().contains("`sleep` is blocked"), "err: {err}");
}

#[tokio::test]
async fn test_quick_command_keeps_foreground_response() {
	let command = if cfg!(target_os = "windows") {
		"echo stdout & echo stderr 1>&2"
	} else {
		"printf stdout; printf stderr 1>&2"
	};
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_secs(5), None)
		.await
		.expect("quick command succeeds");
	assert!(outcome.resource_uri.is_none(), "quick command stays inline");
	assert_eq!(outcome.text, "stdout\n\nstderr:\nstderr");
}

#[cfg(unix)]
#[tokio::test]
async fn children_run_without_a_controlling_terminal() {
	// An inherited terminal lets an interactive shell (`zsh -ic`) take its foreground
	// and leave the MCP client stopped. `ps` prints `?` (Linux) or `??` (macOS) for
	// no controlling tty; PGID == PID keeps kill(-pid) cleanup reaching the group.
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call = crate::mcp::McpToolCall::test_call(
		"shell",
		serde_json::json!({ "command": "echo $$; ps -o pgid= -o tty= -p $$" }),
	);
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_secs(5), None)
		.await
		.expect("ps runs");
	let fields: Vec<&str> = outcome.text.split_whitespace().collect();
	let [pid, pgid, tty] = fields[..] else {
		panic!("unexpected ps output: {:?}", outcome.text);
	};
	assert_eq!(pgid, pid, "the child leads its own process group");
	assert!(
		tty.chars().all(|c| c == '?'),
		"the child must have no controlling terminal, got {tty:?}"
	);
}

#[tokio::test]
async fn test_foreground_timeout_promotes_same_command() {
	let command = if cfg!(target_os = "windows") {
		"echo started & ping -n 2 127.0.0.1 & echo finished"
	} else {
		"for i in 1 2; do echo tick-$i; sleep 1; done"
	};
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_millis(100), None)
		.await
		.expect("an overrun must be promoted, not killed");
	assert!(
		outcome.text.contains("moved to background job") && outcome.text.contains("Stop early"),
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

#[test]
fn stashing_commands_are_recognised() {
	for cmd in [
		"git stash",
		"git stash -q && npm test; git stash pop",
		"git stash push lib/a.js -q && node --test",
		"cd /workspace; git -C /workspace stash save wip",
		"/usr/bin/git --no-pager stash --keep-index",
	] {
		assert!(stashes_changes(cmd), "{cmd}");
	}
	for cmd in [
		"git stash list",
		"git stash pop",
		"git stash show -p",
		"git status && git diff",
		"echo git stash",
	] {
		assert!(!stashes_changes(cmd), "{cmd}");
	}
}

#[cfg(unix)]
#[tokio::test]
async fn a_promoted_stashing_command_warns_that_changes_are_off_disk() {
	// The tree lacks the caller's changes until the command pops them, so the
	// promotion message must say so; other promotions carry no such note.
	let temp = tempfile::tempdir().unwrap();
	for (command, warns) in [
		("false && git stash; for i in 1 2; do sleep 1; done", true),
		("for i in 1 2; do sleep 1; done", false),
	] {
		let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
		call.workdir = temp.path().to_path_buf();
		let outcome = execute_with_timeout(&call, Duration::from_millis(100), None)
			.await
			.expect("promoted");
		assert!(outcome.resource_uri.is_some(), "{}", outcome.text);
		assert_eq!(
			outcome.text.contains("stashed working-tree changes"),
			warns,
			"{command}: {}",
			outcome.text
		);
	}
}

#[tokio::test]
async fn test_rejects_remote_workdir() {
	let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "echo hi" }));
	call.workdir = std::path::PathBuf::from("ssh://user@host:22/tmp");
	let err = execute_shell_command(&call, None)
		.await
		.expect_err("remote workdir must be rejected");
	assert!(err.to_string().contains("local machine"), "err: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn output_too_large_to_deliver_fails_with_the_exit_code() {
	// ~11 MB of distinct lines, so terminal-noise collapsing can't shrink it.
	let temp = tempfile::tempdir().expect("temp workdir");
	let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "seq 1 1500000" }));
	call.workdir = temp.path().to_path_buf();
	let err = execute_with_timeout(&call, Duration::from_secs(30), None)
		.await
		.expect_err("oversized output must not be returned");
	let err = err.to_string();
	assert!(
		err.starts_with("Command exited with code 0, but its output is"),
		"err: {err}"
	);
	assert!(
		err.contains("too large to return in one tool result (limit 8.0 MB)"),
		"err: {err}"
	);
}

#[test]
fn test_clean_terminal_noise() {
	// ANSI colors and cursor codes stripped
	assert_eq!(clean_terminal_noise("\x1b[1;32mok\x1b[0m"), "ok");
	assert_eq!(clean_terminal_noise("\x1b[2K\x1b[1Adone"), "done");
	// OSC hyperlink wrapper stripped, visible text kept
	assert_eq!(clean_terminal_noise("\x1b]8;;http://x\x07link\x1b]8;;\x07"), "link");
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
	assert_eq!(clean_terminal_noise("\n\n  % Total\nbody"), "  % Total\nbody");
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

fn passes(command: &str) -> bool {
	detect_shell_misuse(command).is_none()
}

#[test]
fn test_detect_shell_misuse() {
	// Bare sleep is rejected wherever it sits: chain, substitution or newline
	for cmd in [
		"cd /path && sleep 5",
		"cd /path; sleep 5",
		"true || sleep 5",
		"echo $(sleep 5)",
		"echo `sleep 5`",
		"cd /path\nsleep 5",
		"(sleep 5)",
	] {
		let msg = detect_shell_misuse(cmd).unwrap_or_else(|| panic!("{cmd} must be rejected"));
		assert!(msg.contains("`sleep` is blocked"), "{msg}");
	}
	// A path-qualified or env-prefixed invocation is still caught
	assert!(detect_shell_misuse("/bin/sleep 5").is_some());
	assert!(detect_shell_misuse("FOO=bar sleep 5").is_some());

	// Pipelines stay allowed (stream transforms)
	assert!(passes("cargo build 2>&1 | grep error"));
	// Legitimate commands pass
	assert!(passes("cargo test"));
	assert!(passes("git status && git diff"));
	assert!(passes("echo grep"));

	// Quoted separators are not treated as local command separators
	assert!(passes("echo \"hello && ls\""));
	assert!(passes("bash -lc 'cd /x && git log && ls'"));

	// ssh remote commands obey the same rules, checked as their own command
	assert!(detect_shell_misuse("ssh host 'sleep 5'").is_some());
	assert!(detect_shell_misuse("ssh dev sleep 5").is_some());
	assert!(detect_shell_misuse("ssh -p 2222 user@host 'sleep 5'").is_some());
	assert!(detect_shell_misuse("ssh -o StrictHostKeyChecking=no host 'top'").is_some());
	assert!(detect_shell_misuse("ssh a 'ssh b \"sleep 5\"'").is_some());
	assert!(detect_shell_misuse("ssh host 'ls' && sleep 5").is_some());
	assert!(detect_shell_misuse("ssh host 'cd /path && sleep 5'").is_some());
	assert!(detect_shell_misuse("ssh host \"cd /path && sleep 5\"").is_some());
	// Legitimate remote commands stay allowed
	assert!(passes("ssh host uptime"));
	assert!(passes("ssh host 'systemctl status nginx'"));
	assert!(passes("ssh host 'cd /x && git log'"));
	assert!(passes("ssh host"));
	assert!(passes("ssh -N -L 8080:localhost:80 host"));
	// Remote pipelines keep the local stream-transform leniency
	assert!(passes("ssh host 'journalctl -u app | grep error'"));
	// A pipe after ssh is a local downstream transform, not the remote command
	assert!(passes("ssh host 'dmesg' | grep oops"));
	// One quote layer strips; a nested interpreter stays opaque, same as locally
	assert!(passes("ssh box@host 'bash -lc \"cd ~/work && git log && ls\"'"));

	// Bare / chained sleep is blocked in every common shape
	assert!(detect_shell_misuse("sleep 40").is_some());
	assert!(detect_shell_misuse("sleep 40; echo done").is_some());
	assert!(detect_shell_misuse("sleep 5 && cargo test").is_some());
	assert!(detect_shell_misuse("cargo build && sleep 5").is_some());
	assert!(detect_shell_misuse("sleep 30 || true").is_some());
	assert!(detect_shell_misuse("sleep $((5*60))").is_some());
	assert!(detect_shell_misuse("(sleep 5 && echo hi) &").is_some());
	// Sleep inside a do...done loop body is legitimate polling
	assert!(passes("until test -f /tmp/x; do sleep 2; done"));
	assert!(passes("while ! nc -z localhost 8080; do sleep 1; done"));
	assert!(passes("while true; do echo waiting; sleep 5; done"));
	// Loop depth resets after `done` — a trailing sleep is still caught
	assert!(detect_shell_misuse("until ok; do sleep 1; done; sleep 40").is_some());

	// Redirecting other programs' output stays allowed
	assert!(passes("cargo test > out.log 2>&1"));
	assert!(passes("make 2>&1 | tee build.log"));
	// Fd duplication and quoted `>` are not file writes
	assert!(passes("echo error >&2"));
	assert!(passes("echo \"a > b\""));

	// Never-terminating programs are blocked
	assert!(detect_shell_misuse("watch -n1 date").is_some());
	assert!(detect_shell_misuse("top").is_some());
}

#[test]
fn writing_files_with_the_shell_is_not_rejected() {
	for cmd in [
		"echo hi > notes.txt",
		"printf 'a' >> /tmp/x",
		"cat a.rs > b.rs",
		"ssh host 'echo hi > /tmp/x'",
		"echo hi >&2",
		"sed -i 's/a/b/' f",
		"tee out.txt",
		"cargo test > out.log",
	] {
		assert!(passes(cmd), "{cmd} must not be rejected");
	}
}

#[test]
fn a_blocked_compound_names_the_program_and_says_nothing_ran() {
	// Observed: an agent sent `php -v && git status && git log && find …` three
	// times, permuting joiners, because the rejection named neither the offending
	// program nor the fact that the legitimate parts never ran.
	assert!(detect_shell_misuse("php -v && git log --oneline -3 && sleep 5").is_some());
	let msg = blocked_message(
		"sleep",
		"Bare `sleep` in the shell is blocked — job status is available through its resource.",
	);
	assert!(msg.contains("`sleep`"), "names the program: {msg}");
	assert!(
		msg.contains("no part of it ran"),
		"states the whole command was rejected: {msg}"
	);
}

#[test]
fn reading_searching_and_listing_with_the_shell_is_not_rejected() {
	for cmd in [
		"cat src/main.rs",
		"head -20 src/main.rs",
		"tail -n 5 log.txt",
		"grep -rn foo src/ | head -30",
		"rg -n TODO",
		"find . -name '*.rs'",
		"ls -la src",
		"cd /path && rg TODO",
		"git status; grep -rn TODO .",
		"echo $(cat VERSION)",
		"(cat file)",
		"/bin/grep -n foo a.rs",
		"FOO=bar grep -n foo a.rs",
		"ssh host 'ls -la /var/log'",
		"ssh host 'grep -rn foo /etc'",
	] {
		assert!(detect_shell_misuse(cmd).is_none(), "{cmd} must not be rejected");
	}
}

#[test]
fn a_rejected_program_after_a_read_is_still_rejected_by_name() {
	let msg = detect_shell_misuse("grep foo a.rs; sleep 5").expect("sleep is blocked");
	assert!(msg.contains("`sleep` is blocked"), "{msg}");
}

#[test]
fn a_read_only_awk_check_is_not_an_edit() {
	// Observed six times across one benchmark arm: `awk 'length > 88 …' files`
	// (a line-length check that writes nothing) rejected as "editing files",
	// and the agent retrying variants of it.
	assert!(passes("git diff; awk 'length > 88 {print FILENAME\": \"FNR}' src/a.py"));
	assert!(passes("sed -n '1,5p' file.txt | wc -l"));
}

#[test]
fn writing_files_with_the_shell_is_not_rejected() {
	for cmd in [
		"sed -i 's/a/b/' src/main.rs",
		"sed -i.bak -e 's/a/b/' f",
		"sed --in-place 's/a/b/' f",
		"sed -ni '1,3p' f",
		"ls src && sed -i 's/a/b/' src/main.rs",
		"grep -rl foo . | xargs sed -i 's/foo/bar/g'",
		"tee out.txt",
		"make 2>&1 | tee build.log",
		"echo hi > notes.txt",
		"printf 'a\n' >> notes.txt",
		"cat src/a.rs > src/b.rs",
		"cat <<'EOF' > notes.txt\nhello\nEOF",
		"cd /tmp/x && printf 'x' > f",
		"ssh host 'echo hi > /tmp/x'",
		"ssh host \"sed -i 's/a/b/' /etc/x\"",
	] {
		assert!(detect_shell_misuse(cmd).is_none(), "{cmd} must not be rejected");
	}
}

#[cfg(unix)]
#[tokio::test]
async fn a_scratch_heredoc_and_a_piped_read_run() {
	// End to end: what the gate lets through actually executes.
	let scratch = tempfile::tempdir().unwrap();
	let file = scratch.path().join("repro.txt");
	let command = format!(
		"cat > {f} <<'EOF'\nhello\nEOF\nsort {f} | grep hello",
		f = file.display()
	);
	let call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	let out = execute_shell_command(&call, None).await.expect("runs");
	assert!(out.text.contains("hello"), "{}", out.text);
}

#[test]
fn ssh_remote_command_extracts_and_unquotes_the_body() {
	// Options with separate args are skipped; the body is unquoted.
	assert_eq!(
		ssh_remote_command("ssh -p 22 host 'cat /etc/passwd'"),
		Some("cat /etc/passwd")
	);
	assert_eq!(
		ssh_remote_command("ssh host \"grep x /var/log\""),
		Some("grep x /var/log")
	);
	// A local pipe cuts the remote command at the pipe.
	assert_eq!(ssh_remote_command("ssh host 'dmesg' | tail -1"), Some("dmesg"));
	// No remote command (interactive session / port forward).
	assert_eq!(ssh_remote_command("ssh -N -L 8080:h:80 host"), None);
	assert_eq!(ssh_remote_command("ssh host"), None);
	// Glued option args (-p22) do not swallow the destination.
	assert_eq!(ssh_remote_command("ssh -p22 host 'ls /'"), Some("ls /"));
}

#[test]
fn a_rejected_program_after_a_write_is_still_rejected_by_name() {
	let msg = detect_shell_misuse("echo hi > f && sleep 5").expect("sleep is blocked");
	assert!(msg.contains("`sleep` is blocked"), "{msg}");
	let msg = detect_shell_misuse("sed -i 's/a/b/' f; top").expect("top is blocked");
	assert!(msg.contains("`top` is blocked"), "{msg}");
	assert!(detect_shell_misuse("htop").is_some());
	assert!(detect_shell_misuse("top -b -n1").is_some());
}

#[test]
fn stashes_changes_ignores_read_only_stash_subcommands() {
	for cmd in [
		"git stash list",
		"git stash show -p",
		"git stash pop",
		"git stash apply stash@{0}",
		"git stash drop stash@{0}",
		"git stash branch wip",
		"git -C /repo stash clear",
	] {
		assert!(!stashes_changes(cmd), "{cmd} must not count as stashing");
	}
	for cmd in ["git stash push -m wip", "git stash save", "git stash --keep-index"] {
		assert!(stashes_changes(cmd), "{cmd} must count as stashing");
	}
}

#[test]
fn clean_terminal_noise_keeps_tabs_and_collapses_blank_runs() {
	// Tabs are real content (alignment) and survive; other control chars drop.
	assert_eq!(clean_terminal_noise("a\tb"), "a\tb");
	assert_eq!(clean_terminal_noise("a\u{1}b"), "ab");
	// A blank line between repeats flushes the run: no bogus marker.
	assert_eq!(clean_terminal_noise("x\n\nx\nx"), "x\n\nx\nx");
	// Empty input stays empty.
	assert_eq!(clean_terminal_noise(""), "");
}

// `grep`, `nl` and `cat -n` are Unix programs; `line_ids_tests.rs` covers the stamping itself on every OS.
#[cfg(unix)]
#[tokio::test]
async fn numbered_file_lines_come_back_with_line_ids() {
	let temp = tempfile::tempdir().expect("temp workdir");
	std::fs::write(temp.path().join("notes.txt"), "alpha\n\n  beta\n").expect("write notes");
	let expected = format!("{}|  beta", crate::utils::line_hash::line_id(3, "  beta"));
	for command in [
		"grep -n beta notes.txt",
		"nl -ba notes.txt | sed -n '3p'",
		"cat -n notes.txt | grep beta",
		"awk '{print NR\": \"$0}' notes.txt | tail -1",
	] {
		let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
		call.workdir = temp.path().to_path_buf();
		let outcome = execute_with_timeout(&call, Duration::from_secs(5), None)
			.await
			.expect("the command runs");
		assert_eq!(outcome.text, expected, "{command}");
	}
	// A line printed without its number has no id.
	let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "cat notes.txt" }));
	call.workdir = temp.path().to_path_buf();
	let outcome = execute_with_timeout(&call, Duration::from_secs(5), None)
		.await
		.expect("cat runs");
	assert_eq!(outcome.text, "alpha\n\n  beta");
}

// `sed`, `tee` and `grep` are Unix programs; the gate itself is covered above on every OS.
#[cfg(unix)]
#[tokio::test]
async fn files_written_with_the_shell_are_written_and_read_back_with_ids() {
	let temp = tempfile::tempdir().expect("temp workdir");
	std::fs::write(temp.path().join("notes.txt"), "alpha\nbeta\n").expect("write notes");
	let run = |command: &'static str| {
		let mut call = crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
		call.workdir = temp.path().to_path_buf();
		async move {
			execute_with_timeout(&call, Duration::from_secs(5), None)
				.await
				.expect("the command runs")
				.text
		}
	};
	// An in-place edit and the search that shows the result, in one command.
	// `-i.bak` is the one spelling both GNU and BSD `sed` take.
	let edited = format!("{}|gamma", crate::utils::line_hash::line_id(2, "gamma"));
	assert_eq!(
		run("sed -i.bak 's/beta/gamma/' notes.txt && grep -n gamma notes.txt").await,
		edited
	);
	assert_eq!(
		std::fs::read_to_string(temp.path().join("notes.txt")).expect("read notes"),
		"alpha\ngamma\n"
	);
	// `tee` and a redirect write too.
	run("echo delta | tee -a notes.txt > /dev/null; echo epsilon >> notes.txt").await;
	assert_eq!(
		std::fs::read_to_string(temp.path().join("notes.txt")).expect("read notes"),
		"alpha\ngamma\ndelta\nepsilon\n"
	);
	let listed = run("grep -n epsilon notes.txt").await;
	assert_eq!(
		listed,
		format!("{}|epsilon", crate::utils::line_hash::line_id(4, "epsilon"))
	);
}
