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
	let mut call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
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
		let mut call =
			crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
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
	let mut call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": "echo hi" }));
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
	let mut call = crate::mcp::McpToolCall::test_call(
		"shell",
		serde_json::json!({ "command": "seq 1 1500000" }),
	);
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

const WORKDIR: &str = "/work";

/// The rejection message, when the gate refuses the command run from `WORKDIR`.
fn rejection(command: &str) -> Option<String> {
	match detect_shell_misuse(command, Some(Path::new(WORKDIR))) {
		Some(Misuse::Reject(msg)) => Some(msg),
		_ => None,
	}
}

/// The hint, when the gate lets the command run but points at a dedicated tool.
fn hint(command: &str) -> Option<String> {
	match detect_shell_misuse(command, Some(Path::new(WORKDIR))) {
		Some(Misuse::Hint(hint)) => Some(hint),
		_ => None,
	}
}

fn passes(command: &str) -> bool {
	detect_shell_misuse(command, Some(Path::new(WORKDIR))).is_none()
}

#[test]
fn test_detect_shell_misuse() {
	// A lone read is exactly one `view` call, so it is rejected
	assert!(rejection("grep -rn foo src/").is_some());
	assert!(rejection("cat src/main.rs").is_some());
	assert!(rejection("ls -la").is_some());
	assert!(rejection("find . -name '*.rs'").is_some());

	// Path-qualified and env-prefixed invocations are caught
	assert!(rejection("/bin/grep foo bar").is_some());
	assert!(rejection("FOO=bar grep x y").is_some());

	// Subshell/group openers don't hide a read
	assert!(rejection("(cat file)").is_some());
	assert!(rejection("{ grep foo bar; }").is_some());

	// A read in a chain, a substitution or a pipeline head is rejected too: a hint
	// arrives only after the shell read already ran
	for cmd in [
		"cd /path && grep -rn foo",
		"cd /path; cat file.rs",
		"true || ls -la",
		"echo $(grep foo bar)",
		"echo `cat file`",
		"cd /path\ngrep -rn foo",
		"grep -rn foo src | head -30",
		"ls test | grep route",
		"npm test > /tmp/unit.log 2>&1; grep -E '^not ok' /tmp/unit.log",
	] {
		let msg = rejection(cmd).unwrap_or_else(|| panic!("{cmd} must be rejected"));
		assert!(msg.contains("`view`"), "{msg}");
	}
	// Piping a file or heredoc into a program is told to redirect stdin instead
	let msg = rejection("cat dump.sql | psql").expect("cat as a pipeline head is blocked");
	assert!(msg.contains("psql < dump.sql"), "{msg}");
	assert!(rejection("cat <<'EOF' | kubectl apply -f -\nkind: Pod\nEOF").is_some());

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
	assert!(rejection("ssh host 'cat file'").is_some());
	assert!(rejection("ssh dev grep -rn foo /path").is_some());
	assert!(rejection("ssh -p 2222 user@host 'grep foo /x'").is_some());
	assert!(rejection("ssh -o StrictHostKeyChecking=no host 'ls /x'").is_some());
	assert!(rejection("ssh a 'ssh b \"grep x /y\"'").is_some());
	assert!(rejection("ssh host 'ls' && cat file").is_some());
	assert!(rejection("ssh host 'cd /path && ls'").is_some());
	assert!(rejection("ssh host \"cd /path && grep foo\"").is_some());
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
	assert!(passes(
		"ssh box@host 'bash -lc \"cd ~/work && git log && ls\"'"
	));

	// Bare / chained sleep is blocked in every common shape
	assert!(rejection("sleep 40").is_some());
	assert!(rejection("sleep 40; echo done").is_some());
	assert!(rejection("sleep 5 && cargo test").is_some());
	assert!(rejection("cargo build && sleep 5").is_some());
	assert!(rejection("sleep 30 || true").is_some());
	assert!(rejection("sleep $((5*60))").is_some());
	assert!(rejection("(sleep 5 && echo hi) &").is_some());
	// Sleep inside a do...done loop body is legitimate polling
	assert!(passes("until test -f /tmp/x; do sleep 2; done"));
	assert!(passes("while ! nc -z localhost 8080; do sleep 1; done"));
	assert!(passes("while true; do echo waiting; sleep 5; done"));
	// Loop depth resets after `done` — a trailing sleep is still caught
	assert!(rejection("until ok; do sleep 1; done; sleep 40").is_some());

	// Writing file content into the workdir via shell redirects is blocked
	assert!(rejection("echo 'fn main() {}' > src/main.rs").is_some());
	assert!(rejection("printf '%s\\n' hi >> notes.txt").is_some());
	assert!(rejection("echo hi > /work/notes.txt").is_some());
	assert!(rejection("tee out.txt").is_some());
	assert!(rejection("cd /x && echo data > f").is_some());
	// A redirect that also writes into the workdir is still a tracked-file write
	assert!(rejection("echo hi > /tmp/a > src/b").is_some());
	assert!(rejection("echo hi >/tmp/a>src/b").is_some());
	// cat with a redirect gets the write guidance, not the read guidance
	let msg = rejection("cat > f.txt").unwrap();
	assert!(msg.contains("text_editor"), "msg: {msg}");
	// Redirecting other programs' output stays allowed
	assert!(passes("cargo test > out.log 2>&1"));
	assert!(passes("make 2>&1 | tee build.log"));
	// Fd duplication and quoted `>` are not file writes
	assert!(passes("echo error >&2"));
	assert!(passes("echo \"a > b\""));

	// Never-terminating programs are blocked
	assert!(rejection("watch -n1 date").is_some());
	assert!(rejection("top").is_some());
}

// `/tmp/...` is an absolute path only on Unix.
#[cfg(unix)]
#[test]
fn scratch_writes_outside_the_workdir_run_with_a_hint() {
	// Scratch files outside the workdir are no tracked edit: they run with a hint
	let scratch = hint("cat > /tmp/repro.js <<'EOF'\nconsole.log(1)\nEOF\nnode /tmp/repro.js")
		.expect("a /tmp heredoc runs");
	assert!(scratch.contains("text_editor"), "{scratch}");
	assert!(hint("echo '{}' > \"/tmp/x.json\"").is_some());
}

#[test]
fn remote_writes_count_as_tracked() {
	// Without a local workdir (an ssh remote command) no write target is known to be
	// scratch, so content writes stay rejected wherever they point.
	assert!(matches!(
		detect_shell_misuse("echo hi > /tmp/x", None),
		Some(Misuse::Reject(_))
	));
	assert!(rejection("ssh host 'echo hi > /tmp/x'").is_some());
}

#[test]
fn a_blocked_compound_names_the_program_and_says_nothing_ran() {
	// Observed: an agent sent `php -v && git status && git log && find …` three
	// times, permuting joiners, because the rejection named neither the offending
	// program nor the fact that the legitimate parts never ran.
	let msg = rejection("php -v && git log --oneline -3 && sleep 5").expect("sleep is blocked");
	assert!(msg.contains("`sleep`"), "names the program: {msg}");
	assert!(
		msg.contains("no part of it ran"),
		"states the whole command was rejected: {msg}"
	);
}

#[test]
fn a_read_in_a_compound_is_rejected_by_name() {
	let msg =
		rejection("php -v && git log --oneline -3 && find . -name x").expect("find is blocked");
	assert!(msg.contains("`find` is blocked"), "{msg}");
}

#[test]
fn a_read_only_awk_check_is_not_an_edit() {
	// Observed six times across one benchmark arm: `awk 'length > 88 …' files`
	// (a line-length check that writes nothing) rejected as "editing files",
	// and the agent retrying variants of it.
	assert!(passes(
		"git diff; awk 'length > 88 {print FILENAME\": \"FNR}' src/a.py"
	));
	assert!(passes("sed -n '1,5p' file.txt | wc -l"));
}

#[test]
fn an_in_place_sed_is_blocked_as_an_edit() {
	for cmd in [
		"cd /workspace && sed -i 's/a/b/' src/x.c",
		"sed -i.bak -e 's/a/b/' src/x.c",
		"sed --in-place 's/a/b/' src/x.c",
		"sed -ni 's/a/b/p' src/x.c",
	] {
		let msg = rejection(cmd).unwrap_or_else(|| panic!("{cmd} must be blocked"));
		assert!(msg.contains("`sed` is blocked"), "{msg}");
		assert!(msg.contains("in place"), "{msg}");
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
	let call =
		crate::mcp::McpToolCall::test_call("shell", serde_json::json!({ "command": command }));
	let out = execute_shell_command(&call, None).await.expect("runs");
	assert!(out.text.contains("hello"), "{}", out.text);
}
