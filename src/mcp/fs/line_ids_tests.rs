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
use crate::utils::line_hash::{line_id, verify_line_id};

const NOTES: &str = "alpha\n\n  beta\ngamma: 1\n- delta\n";
const NOTES_LINES: [&str; 5] = ["alpha", "", "  beta", "gamma: 1", "- delta"];

/// A workdir holding `notes.txt` and `sub/other.txt`.
fn workdir() -> tempfile::TempDir {
	let dir = tempfile::tempdir().expect("temp workdir");
	std::fs::write(dir.path().join("notes.txt"), NOTES).expect("write notes");
	std::fs::create_dir(dir.path().join("sub")).expect("create sub");
	std::fs::write(dir.path().join("sub/other.txt"), "one\ntwo\n").expect("write other");
	dir
}

/// Line `number` of `notes.txt` the way `view` prints it.
fn note(number: usize) -> String {
	let line = NOTES_LINES[number - 1];
	format!("{} {line}", line_id(number, line))
}

#[test]
fn a_search_of_one_file_gets_ids() {
	let dir = workdir();
	let output = "3:  beta\n4-gamma: 1\n5:- delta";
	assert_eq!(
		stamp(output, "grep -n -A1 beta notes.txt", dir.path()),
		[note(3), note(4), note(5)].join("\n")
	);
}

#[test]
fn a_numbered_listing_gets_ids_and_loses_its_padding() {
	let dir = workdir();
	// `nl -ba` and `cat -n` pad the number and follow it with a tab; the tab of
	// a blank line is gone after terminal cleaning.
	let output = "     1\talpha\n     2\n     3\t  beta";
	assert_eq!(
		stamp(output, "nl -ba notes.txt | sed -n '1,3p'", dir.path()),
		[note(1), note(2), note(3)].join("\n")
	);
}

#[test]
fn any_short_separator_after_the_number_is_read() {
	let dir = workdir();
	for (separator, command) in [
		(": ", "awk '{print NR\": \"$0}' notes.txt"),
		(" | ", "awk '{printf \"%d | %s\\n\", NR, $0}' notes.txt"),
		("  ", "awk '{print NR\"  \"$0}' notes.txt"),
	] {
		let output = format!("1{separator}alpha\n3{separator}  beta\n5{separator}- delta");
		assert_eq!(
			stamp(&output, command, dir.path()),
			[note(1), note(3), note(5)].join("\n"),
			"{separator:?}"
		);
	}
}

#[test]
fn a_search_of_several_files_keeps_the_path_and_gets_ids() {
	let dir = workdir();
	let output = "notes.txt:1:alpha\nnotes.txt-2-\nsub/other.txt:2:two\n--";
	let expected = format!(
		"notes.txt:{}\nnotes.txt-{}\nsub/other.txt:{} two\n--",
		note(1),
		note(2),
		line_id(2, "two")
	);
	assert_eq!(stamp(output, "rg -n -A1 'alpha|two'", dir.path()), expected);
}

#[test]
fn lines_under_a_path_heading_get_ids() {
	let dir = workdir();
	let output = "notes.txt\n1:alpha\n\nsub/other.txt\n1:one";
	let expected = format!("notes.txt\n{}\n\nsub/other.txt\n{} one", note(1), line_id(1, "one"));
	assert_eq!(stamp(output, "rg -n --heading 'alpha|one'", dir.path()), expected);
}

#[test]
fn a_file_is_found_from_the_directory_the_command_moved_to() {
	let dir = workdir();
	let expected = format!("{} two", line_id(2, "two"));
	assert_eq!(stamp("2:two", "cd sub && grep -n two other.txt", dir.path()), expected);
}

#[test]
fn output_that_is_no_file_line_stays_as_printed() {
	let dir = workdir();
	for (output, command) in [
		// A clock, a line count and a compiler position start like numbered lines.
		("12:30:01 up 3 days", "uptime; grep -c a notes.txt"),
		("       5 notes.txt", "wc -l notes.txt"),
		("notes.txt:3:5: error: unexpected indent", "lint notes.txt"),
		// Line 2 is blank, but a lone count is not a listing.
		("2", "grep -c a notes.txt"),
		// The number is right and the text is not the file's.
		("3:  BETA", "grep -n beta notes.txt | tr a-z A-Z"),
		("9:alpha", "grep -n alpha notes.txt"),
		// Already carries ids.
		("1:86 alpha", "grep -n alpha notes.txt"),
	] {
		assert_eq!(stamp(output, command, dir.path()), output, "{command}");
	}
}

#[test]
fn git_show_lines_get_ids_when_the_worktree_lines_match() {
	let dir = workdir();
	std::fs::create_dir(dir.path().join(".git")).expect("create repository marker");
	let output = "1:alpha\n3:  beta\n4:older content";
	let expected = [note(1), note(3), "4:older content".to_string()].join("\n");
	for (command, workdir) in [
		("git show HEAD:notes.txt | grep -n .", dir.path().to_path_buf()),
		("git show 'main~1:notes.txt' | grep -n .", dir.path().join("sub")),
		("git show HEAD:../notes.txt | grep -n .", dir.path().join("sub")),
	] {
		assert_eq!(stamp(output, command, &workdir), expected, "{command}");
	}
}

#[test]
fn a_file_the_command_does_not_name_gives_no_ids() {
	let dir = workdir();
	std::fs::create_dir(dir.path().join(".git")).expect("create repository marker");
	assert_eq!(
		stamp("1:alpha", "git show HEAD:notes | grep -n alpha", dir.path()),
		"1:alpha"
	);
}

#[test]
fn the_ids_are_the_ones_the_edit_tools_verify() {
	let dir = workdir();
	let stamped = stamp("3:  beta", "grep -n beta notes.txt", dir.path());
	let (id, text) = stamped.split_once(' ').expect("id and text");
	let (number, hash) = id.split_once(':').expect("number and hash");
	assert_eq!(text, "  beta");
	assert_eq!(
		verify_line_id(number.parse().expect("line number"), hash, &NOTES_LINES),
		Ok(3)
	);
}

#[test]
fn every_way_a_command_names_a_file_is_followed() {
	let dir = workdir();
	let absolute = dir.path().join("notes.txt");
	for command in [
		"grep -n alpha ./notes.txt".to_string(),
		format!("grep -n alpha {}", absolute.display()),
		format!("grep -n alpha \"{}\"", absolute.display()),
		"grep -n alpha 'notes.txt'".to_string(),
		"grep -n alpha --file=notes.txt".to_string(),
		"grep -n alpha<notes.txt".to_string(),
		"(nl -ba notes.txt)|head -1".to_string(),
		"cd sub && cd .. && grep -n alpha notes.txt".to_string(),
		format!("cd {} && grep -n alpha notes.txt", dir.path().display()),
	] {
		assert_eq!(stamp("1:alpha", &command, dir.path()), note(1), "{command}");
	}
}

#[test]
fn each_file_of_a_chain_gives_its_own_lines() {
	let dir = workdir();
	let output = "     1\talpha\n     1\tone\n     2\ttwo";
	let expected = format!("{}\n{} one\n{} two", note(1), line_id(1, "one"), line_id(2, "two"));
	assert_eq!(
		stamp(output, "nl -ba notes.txt | sed -n 1p; nl -ba sub/other.txt", dir.path()),
		expected
	);
}

#[test]
fn a_context_line_of_a_file_with_dashes_and_digits_in_its_name_is_read() {
	let dir = workdir();
	std::fs::write(dir.path().join("my-notes-2.txt"), "one\ntwo\n").expect("write file");
	let output = "my-notes-2.txt-1-one\nmy-notes-2.txt:2:two";
	let expected = format!(
		"my-notes-2.txt-{} one\nmy-notes-2.txt:{} two",
		line_id(1, "one"),
		line_id(2, "two")
	);
	assert_eq!(stamp(output, "grep -n -B1 two *.txt", dir.path()), expected);
}

#[test]
fn the_shortest_separator_decides_between_two_files() {
	let dir = workdir();
	std::fs::write(dir.path().join("flat.txt"), "x\n").expect("write flat");
	std::fs::write(dir.path().join("indented.txt"), " x\n").expect("write indented");
	// `1: x` is ` x` after `:` and `x` after `: `. Reading the space as file text
	// keeps the line as the program printed it.
	let expected = format!("{}  x", line_id(1, " x"));
	assert_eq!(stamp("1: x", "grep -n x flat.txt indented.txt", dir.path()), expected);
	assert_eq!(stamp("1: x", "grep -n x indented.txt flat.txt", dir.path()), expected);
}

#[test]
fn blank_lines_of_a_listing_get_ids_and_a_lone_number_does_not() {
	let dir = workdir();
	std::fs::write(dir.path().join("gaps.txt"), "\n\nz\n\n").expect("write gaps");
	let blank = |number| format!("{} ", line_id(number, ""));
	// A listing that starts with blank lines: each is numbered around its neighbour.
	assert_eq!(
		stamp("     1\n     2\n     3\tz\n     4", "nl -ba gaps.txt", dir.path()),
		format!("{}\n{}\n{} z\n{}", blank(1), blank(2), line_id(3, "z"), blank(4))
	);
	// `grep -n` prints the separator of a blank match, which is enough on its own.
	assert_eq!(stamp("4:", "grep -n '^$' gaps.txt | tail -1", dir.path()), blank(4));
	// A count between other text is not a listing, even on a blank line's number.
	assert_eq!(
		stamp(
			"matches\n2\ndone",
			"echo matches; grep -c z gaps.txt; echo done",
			dir.path()
		),
		"matches\n2\ndone"
	);
}

#[test]
fn trailing_whitespace_and_line_endings_of_the_file_are_kept_in_the_id() {
	let dir = workdir();
	std::fs::write(dir.path().join("crlf.txt"), "first  \r\nsecond\r\n").expect("write crlf");
	// Terminal cleaning drops the trailing spaces from the output; the id is of the file's line.
	assert_eq!(
		stamp("1:first\n2:second", "grep -n . crlf.txt", dir.path()),
		format!("{} first  \n{} second", line_id(1, "first  "), line_id(2, "second"))
	);
	assert_eq!(
		stamp("crlf.txt:1:first", "grep -rn first .", dir.path()),
		format!("crlf.txt:{} first  ", line_id(1, "first  "))
	);
}

#[test]
fn a_heading_holds_until_a_blank_line() {
	let dir = workdir();
	let output = "notes.txt\n1:alpha\n\n1:alpha";
	assert_eq!(
		stamp(output, "rg -n --heading alpha", dir.path()),
		format!("notes.txt\n{}\n\n1:alpha", note(1))
	);
}

#[test]
fn files_that_cannot_be_viewed_give_no_ids() {
	let dir = workdir();
	std::fs::write(dir.path().join("binary.dat"), [b'a', b'\n', 0xff, 0xfe, b'\n']).expect("write binary");
	assert_eq!(stamp("1:a", "grep -na a binary.dat", dir.path()), "1:a");

	let big = "x\n".repeat(MAX_VIEW_FILE_BYTES as usize / 2 + 1);
	std::fs::write(dir.path().join("big.txt"), big).expect("write big");
	assert_eq!(stamp("1:x", "grep -n -m1 x big.txt", dir.path()), "1:x");

	// A directory named like the output's path, and line numbers past the end or zero.
	assert_eq!(stamp("sub:1:one", "grep -rn one sub", dir.path()), "sub:1:one");
	assert_eq!(
		stamp("0:alpha\n6:alpha", "grep -n alpha notes.txt", dir.path()),
		"0:alpha\n6:alpha"
	);
	for number in ["99999999999999999999999".to_string(), usize::MAX.to_string()] {
		let output = format!("{number}:alpha\n{number}");
		assert_eq!(stamp(&output, "grep -n alpha notes.txt", dir.path()), output);
	}
}

#[test]
fn output_without_numbered_file_lines_is_returned_unchanged() {
	let dir = workdir();
	for output in [
		"",
		"alpha\n\n  beta\ngamma: 1\n- delta",
		"   Compiling octofs v0.25.2\nerror[E0425]: cannot find value `x`\n  --> src/main.rs:12:5\n   |\n12 |     x\n   |     ^",
		"total 8\n-rw-r--r--  1 box  box  33 Nov 18 10:00 notes.txt\ndrwxr-xr-x  2 box  box  64 Nov 18 10:00 sub",
		"notes.txt\nsub/other.txt",
		"3 files changed, 10 insertions(+), 2 deletions(-)",
		"2026-11-18 10:00:01 started\n2026-11-18 10:00:02 done",
	] {
		assert_eq!(stamp(output, "cat notes.txt; cargo build; ls -la; git diff --stat", dir.path()), output);
	}
}

#[tokio::test]
async fn only_stdout_of_a_command_gets_ids() {
	use super::super::background::{discard, prepare, process_output, PreparedJob};

	let dir = workdir();
	let PreparedJob { job, .. } = prepare("grep -n beta notes.txt", dir.path())
		.await
		.expect("prepare job");
	let (output, stored) = process_output(b"\x1b[32m3:  beta\x1b[0m\n", b"3:  beta\n", &job).await;
	assert_eq!(output, format!("{}\n\nstderr:\n3:  beta", note(3)));
	assert!(stored.is_none(), "a short output is not spilled");
	discard(&job).await;
}
