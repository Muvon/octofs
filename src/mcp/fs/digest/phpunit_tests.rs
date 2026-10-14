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

//! Real phpunit captures, golden selections, and conservative rejection.

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
			stdout: include_str!(concat!("fixtures/phpunit/", $name, $suffix)),
			stderr: $stderr,
			golden: include_str!(concat!("fixtures/phpunit/", $name, $golden)),
			exit_code: $exit,
		}
	};
}

const GOLDENS: &[Fixture] = &[
	fixture!(
		"fail-testdox",
		".stdout",
		include_str!("fixtures/phpunit/fail-testdox.stderr"),
		".separate.digest",
		2
	),
	fixture!("fail-testdox", ".merged", "", ".merged.digest", 2),
];

const PASSTHROUGH: &[(&str, &str, &str, &str)] = &[
	(
		"fail-default",
		include_str!("fixtures/phpunit/fail-default.stdout"),
		include_str!("fixtures/phpunit/fail-default.stderr"),
		include_str!("fixtures/phpunit/fail-default.merged"),
	),
	(
		"fail-display-all",
		include_str!("fixtures/phpunit/fail-display-all.stdout"),
		include_str!("fixtures/phpunit/fail-display-all.stderr"),
		include_str!("fixtures/phpunit/fail-display-all.merged"),
	),
	(
		"pass-default",
		include_str!("fixtures/phpunit/pass-default.stdout"),
		include_str!("fixtures/phpunit/pass-default.stderr"),
		include_str!("fixtures/phpunit/pass-default.merged"),
	),
	(
		"pass-testdox",
		include_str!("fixtures/phpunit/pass-testdox.stdout"),
		include_str!("fixtures/phpunit/pass-testdox.stderr"),
		include_str!("fixtures/phpunit/pass-testdox.merged"),
	),
	(
		"syntax-error",
		include_str!("fixtures/phpunit/syntax-error.stdout"),
		include_str!("fixtures/phpunit/syntax-error.stderr"),
		include_str!("fixtures/phpunit/syntax-error.merged"),
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
	let stdout = stdout.replace("Tests: 173", "Tests: 172");
	let stderr = stderr.replace("Tests: 173", "Tests: 172");
	assert!(select(&lines(&stdout), &lines(&stderr)).is_none());
	assert!(digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).is_none());
}

#[test]
fn issue_counts_and_numeric_echo_output_are_preserved() {
	let (stdout, stderr) = GOLDENS[0].cleaned();
	let invalid = stdout.replace("Warnings: 1", "Warnings: 2");
	assert!(select(&lines(&invalid), &lines(&stderr)).is_none());
	let stdout = stdout.replace("starting checkout flow", "123 checkout records");
	let rows = lines(&stdout);
	let selection = select(&rows, &lines(&stderr)).unwrap();
	assert!(render(&rows, &selection.stdout).contains("FFEEDW123 checkout records"));
	assert_eq!(render(&lines(&stderr), &selection.stderr), stderr);
}

#[test]
fn cleaned_repetitions_follow_noise_and_validate_counts() {
	let raw = r#"PHPUnit 12.5.39 by Sebastian Bergmann and contributors.
.
.
.
Time: 00:00.001, Memory: 18.05 MB
Group
 ✔ Repeated
 ✔ Repeated
 ✔ Repeated
OK (3 tests, 3 assertions)"#;
	let cleaned = clean_terminal_noise(raw);
	assert!(cleaned.contains("[... last line repeated 2 more times]"));
	let rows = lines(&cleaned);
	let selection = select(&rows, &[]).unwrap();
	assert_eq!(
		render(&rows, &selection.stdout),
		r#"Time: 00:00.001, Memory: 18.05 MB
OK (3 tests, 3 assertions)"#
	);
	let inconsistent = cleaned.replace("3 tests, 3 assertions", "2 tests, 3 assertions");
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
		for target in [
			"   │  body line one",
			r#"PHP Warning:  Module "yaml" is already loaded in Unknown on line 0"#,
		] {
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
