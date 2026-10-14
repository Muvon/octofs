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

//! Real go_test captures, golden selections, and conservative rejection.

use super::super::{
	digest_with, join_streams, lines,
	mod_tests::{Fixture, SPILL_PATH},
	render,
};
use super::select;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

macro_rules! fixture {
	($name:literal, $suffix:literal, $stderr:expr, $golden:literal, $exit:literal) => {
		Fixture {
			name: concat!($name, $suffix),
			stdout: include_str!(concat!("fixtures/go_test/", $name, $suffix)),
			stderr: $stderr,
			golden: include_str!(concat!("fixtures/go_test/", $name, $golden)),
			exit_code: $exit,
		}
	};
}

const GOLDENS: &[Fixture] = &[
	fixture!(
		"pass-verbose",
		".stdout",
		include_str!("fixtures/go_test/pass-verbose.stderr"),
		".separate.digest",
		0
	),
	fixture!("pass-verbose", ".merged", "", ".merged.digest", 0),
	fixture!(
		"fail-verbose",
		".stdout",
		include_str!("fixtures/go_test/fail-verbose.stderr"),
		".separate.digest",
		1
	),
	fixture!("fail-verbose", ".merged", "", ".merged.digest", 1),
];

const PASSTHROUGH: &[(&str, &str, &str, &str)] = &[
	(
		"build-fail-default",
		include_str!("fixtures/go_test/build-fail-default.stdout"),
		include_str!("fixtures/go_test/build-fail-default.stderr"),
		include_str!("fixtures/go_test/build-fail-default.merged"),
	),
	(
		"build-fail-verbose",
		include_str!("fixtures/go_test/build-fail-verbose.stdout"),
		include_str!("fixtures/go_test/build-fail-verbose.stderr"),
		include_str!("fixtures/go_test/build-fail-verbose.merged"),
	),
	(
		"fail-default",
		include_str!("fixtures/go_test/fail-default.stdout"),
		include_str!("fixtures/go_test/fail-default.stderr"),
		include_str!("fixtures/go_test/fail-default.merged"),
	),
	(
		"fail-single-pkg",
		include_str!("fixtures/go_test/fail-single-pkg.stdout"),
		include_str!("fixtures/go_test/fail-single-pkg.stderr"),
		include_str!("fixtures/go_test/fail-single-pkg.merged"),
	),
	(
		"panic-default",
		include_str!("fixtures/go_test/panic-default.stdout"),
		include_str!("fixtures/go_test/panic-default.stderr"),
		include_str!("fixtures/go_test/panic-default.merged"),
	),
	(
		"panic-verbose",
		include_str!("fixtures/go_test/panic-verbose.stdout"),
		include_str!("fixtures/go_test/panic-verbose.stderr"),
		include_str!("fixtures/go_test/panic-verbose.merged"),
	),
	(
		"pass-cached",
		include_str!("fixtures/go_test/pass-cached.stdout"),
		include_str!("fixtures/go_test/pass-cached.stderr"),
		include_str!("fixtures/go_test/pass-cached.merged"),
	),
	(
		"pass-default",
		include_str!("fixtures/go_test/pass-default.stdout"),
		include_str!("fixtures/go_test/pass-default.stderr"),
		include_str!("fixtures/go_test/pass-default.merged"),
	),
	(
		"race-pass",
		include_str!("fixtures/go_test/race-pass.stdout"),
		include_str!("fixtures/go_test/race-pass.stderr"),
		include_str!("fixtures/go_test/race-pass.merged"),
	),
	(
		"timeout-default",
		include_str!("fixtures/go_test/timeout-default.stdout"),
		include_str!("fixtures/go_test/timeout-default.stderr"),
		include_str!("fixtures/go_test/timeout-default.merged"),
	),
];

#[test]
fn real_captures_match_goldens_through_the_framework() {
	for fixture in GOLDENS {
		let (stdout, stderr) = fixture.cleaned();
		let stdout_lines = lines(&stdout);
		let stderr_lines = lines(&stderr);
		let selection = select(&stdout_lines, &stderr_lines)
			.unwrap_or_else(|| panic!("{} not recognized", fixture.name));
		let body = join_streams(
			&render(&stdout_lines, &selection.stdout),
			&render(&stderr_lines, &selection.stderr),
		);
		assert_eq!(body, fixture.golden, "{}", fixture.name);
		let omitted = stdout_lines.len() + stderr_lines.len()
			- selection.stdout.len()
			- selection.stderr.len();
		let expected =
			format!("{body}\n[digest: {omitted} lines omitted — full output: {SPILL_PATH}]");
		let actual = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select])
			.unwrap_or_else(|| panic!("{} did not digest", fixture.name));
		assert_eq!(actual, expected, "{}", fixture.name);
		for _ in 0..3 {
			assert_eq!(
				Some(&actual),
				digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).as_ref()
			);
		}
		println!(
			"{}: {} -> {} bytes",
			fixture.name,
			join_streams(&stdout, &stderr).len(),
			actual.len()
		);
	}
}

#[test]
fn small_and_unprofitable_captures_pass_through() {
	for &(name, stdout, stderr, merged) in PASSTHROUGH {
		for (stdout, stderr) in [(stdout, stderr), (merged, "")] {
			let stdout = clean_terminal_noise(stdout);
			let stderr = clean_terminal_noise(stderr);
			assert!(
				digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).is_none(),
				"{name}"
			);
		}
	}
}

#[test]
fn foreign_and_truncated_output_is_rejected() {
	for fixture in GOLDENS {
		let (stdout, stderr) = fixture.cleaned();
		for (stdout, stderr) in [
			(
				format!("{stdout}\napplication output unrelated to the runner"),
				stderr.clone(),
			),
			(
				stdout.clone(),
				format!("{stderr}\napplication output unrelated to the runner"),
			),
		] {
			assert!(
				select(&lines(&stdout), &lines(&stderr)).is_none(),
				"{}",
				fixture.name
			);
			assert!(digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).is_none());
		}
		let (primary, other, on_stderr) = if stderr.contains("Test Suites:") {
			(&stderr, &stdout, true)
		} else {
			(&stdout, &stderr, false)
		};
		// Removing the last nonblank line removes a terminal control line,
		// or one required entry from a terminal failure list.
		let last = primary.lines().rfind(|line| !line.is_empty()).unwrap();
		let truncated = &primary[..primary.rfind(last).unwrap()];
		let (stdout, stderr) = if on_stderr {
			(other.as_str(), truncated)
		} else {
			(truncated, other.as_str())
		};
		assert!(
			select(&lines(stdout), &lines(stderr)).is_none(),
			"{}",
			fixture.name
		);
		assert!(digest_with(stdout, stderr, Path::new(SPILL_PATH), &[select]).is_none());
	}
	let foreign = "ordinary program output\n".repeat(500);
	assert!(select(&lines(&foreign), &[]).is_none());
	assert!(digest_with(&foreign, "", Path::new(SPILL_PATH), &[select]).is_none());
}

#[test]
fn inconsistent_counts_are_rejected() {
	let (stdout, stderr) = GOLDENS[0].cleaned();
	let mut rows = lines(&stdout);
	let index = rows
		.iter()
		.position(|line| line.starts_with("--- PASS: "))
		.unwrap();
	rows.remove(index);
	assert!(select(&rows, &lines(&stderr)).is_none());
}

#[test]
fn panic_blocks_keep_tab_indented_messages_and_goroutine_dumps() {
	for raw in [
		include_str!("fixtures/go_test/panic-verbose.stdout"),
		include_str!("fixtures/go_test/panic-verbose.merged"),
	] {
		let cleaned = clean_terminal_noise(raw);
		let rows = lines(&cleaned);
		let selection = select(&rows, &[]).unwrap();
		let mut panic = false;
		for (i, line) in rows.iter().enumerate() {
			if line.starts_with("panic: ") {
				panic = true;
			}
			if panic {
				assert!(selection.stdout.contains(&i), "{line}");
			}
			if line.starts_with("FAIL\t") {
				panic = false;
			}
		}
	}
}

#[test]
fn parallel_test_control_lines_require_matching_runs() {
	let stdout = "=== RUN   TestParallel\n=== PAUSE TestParallel\n=== CONT  TestParallel\n--- PASS: TestParallel (0.01s)\nPASS\nok  \texample.com/parallel\t0.01s\n";
	let rows = lines(stdout);
	let selection = select(&rows, &[]).unwrap();
	assert_eq!(
		render(&rows, &selection.stdout),
		"PASS\nok  \texample.com/parallel\t0.01s"
	);
	for invalid in [
		stdout.replace("=== PAUSE TestParallel", "=== PAUSE TestMissing"),
		stdout.replace("--- PASS: TestParallel", "--- PASS: TestMissing"),
		stdout.replace("ok  \texample.com/parallel", "FAIL\texample.com/parallel"),
	] {
		assert!(select(&lines(&invalid), &[]).is_none());
	}
}

#[test]
fn cleaned_repetitions_follow_noise_and_validate_counts() {
	let raw = r#"=== RUN   TestParallel
=== PAUSE TestParallel
=== PAUSE TestParallel
=== PAUSE TestParallel
=== CONT  TestParallel
--- PASS: TestParallel (0.01s)
PASS
ok  	example.com/parallel	0.01s"#;
	let cleaned = clean_terminal_noise(raw);
	assert!(cleaned.contains("[... last line repeated 2 more times]"));
	let rows = lines(&cleaned);
	let selection = select(&rows, &[]).unwrap();
	assert_eq!(
		render(&rows, &selection.stdout),
		r#"PASS
ok  	example.com/parallel	0.01s"#
	);
	let inconsistent = cleaned.replace("--- PASS: TestParallel", "--- PASS: TestMissing");
	assert!(select(&lines(&inconsistent), &[]).is_none());
	for invalid in [
		cleaned.replace("repeated 2 more", "repeated 1 more"),
		cleaned.replace("repeated 2 more", "repeated many more"),
		format!("[... last line repeated 2 more times]\n{cleaned}"),
	] {
		assert!(select(&lines(&invalid), &[]).is_none());
	}
}

#[test]
fn cleaned_repetitions_keep_whole_diagnostics_in_both_stream_layouts() {
	for fixture in GOLDENS.iter().filter(|fixture| fixture.exit_code != 0) {
		for target in ["        second log line", "        --- want"] {
			let (stdout, stderr) = fixture.cleaned();
			let repeated = format!("{target}\n{target}\n{target}");
			let stdout = clean_terminal_noise(&stdout.replacen(target, &repeated, 1));
			let stderr = clean_terminal_noise(&stderr.replacen(target, &repeated, 1));
			let stdout_lines = lines(&stdout);
			let stderr_lines = lines(&stderr);
			let selection = select(&stdout_lines, &stderr_lines).unwrap();
			let expected = fixture.golden.replacen(
				target,
				&format!("{target}\n[... last line repeated 2 more times]"),
				1,
			);
			assert_ne!(expected, fixture.golden, "{}: {target}", fixture.name);
			let body = join_streams(
				&render(&stdout_lines, &selection.stdout),
				&render(&stderr_lines, &selection.stderr),
			);
			assert_eq!(body, expected, "{}: {target}", fixture.name);
			let actual = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).unwrap();
			assert!(actual.starts_with(&format!("{expected}\n[digest: ")));
		}
	}
}

#[test]
fn cleaned_compiler_and_terminal_repetitions_are_preserved() {
	let stdout = clean_terminal_noise("FAIL\texample.com/broken\t[build failed]\nFAIL\nFAIL\nFAIL");
	let stderr = clean_terminal_noise(
		"# example.com/broken\nbroken_test.go:3: undefined: missing\nbroken_test.go:3: undefined: missing\nbroken_test.go:3: undefined: missing",
	);
	let rows = lines(&stdout);
	let errors = lines(&stderr);
	let selection = select(&rows, &errors).unwrap();
	assert_eq!(render(&rows, &selection.stdout), stdout);
	assert_eq!(render(&errors, &selection.stderr), stderr);
	let merged = format!("{stderr}\n{stdout}");
	let rows = lines(&merged);
	let selection = select(&rows, &[]).unwrap();
	assert_eq!(render(&rows, &selection.stdout), merged);
}
