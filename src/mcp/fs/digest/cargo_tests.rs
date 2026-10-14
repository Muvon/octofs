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

//! Cargo recognition and verified selections from static real tool captures.

use super::*;
use crate::mcp::fs::digest::{
	digest, join_streams, lines,
	mod_tests::{FIXTURES, SPILL_PATH},
	render,
};
use std::path::Path;

#[test]
fn real_cargo_captures_match_goldens() {
	for fixture in FIXTURES {
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
		let digest = digest(&stdout, &stderr, Path::new(SPILL_PATH))
			.unwrap_or_else(|| panic!("{} did not digest", fixture.name));
		let omitted = stdout_lines.len() + stderr_lines.len()
			- selection.stdout.len()
			- selection.stderr.len();
		assert_eq!(
			digest,
			format!("{body}\n[digest: {omitted} lines omitted — full output: {SPILL_PATH}]")
		);
		let original = join_streams(&stdout, &stderr);
		println!(
			"{}: {} -> {} bytes",
			fixture.name,
			original.len(),
			digest.len()
		);
		if fixture.name == "pass" {
			assert!(
				digest.len() * 10 < original.len(),
				"all-pass noise should drop by over 90%"
			);
		}
	}
}

#[test]
fn truncated_and_mixed_real_captures_are_rejected() {
	let (pass_stdout, pass_stderr) = FIXTURES[0].cleaned();
	let (fail_stdout, fail_stderr) = FIXTURES[1].cleaned();
	for (stdout, stderr) in [
		(
			pass_stdout[..pass_stdout.rfind("test result:").unwrap()].to_string(),
			pass_stderr.clone(),
		),
		(
			fail_stdout[..fail_stdout.rfind("test result:").unwrap()].to_string(),
			fail_stderr.clone(),
		),
		(
			pass_stdout.replace("240 passed", "239 passed"),
			pass_stderr.clone(),
		),
		(
			format!("{pass_stdout}\napplication output unrelated to Cargo"),
			pass_stderr.clone(),
		),
		(
			pass_stdout.clone(),
			format!("{pass_stderr}\n   Compiling unfinished v0.1.0"),
		),
		(
			pass_stdout.clone(),
			pass_stderr
				.lines()
				.map(|line| {
					if line.trim_start().starts_with("Running ") {
						"Running an unrelated application"
					} else {
						line
					}
				})
				.collect::<Vec<_>>()
				.join("\n"),
		),
		(
			pass_stdout.clone(),
			format!("{pass_stderr}\nwarning: truncated diagnostic\n  incomplete details"),
		),
		(
			fail_stdout.replace("---- assert_failure stdout ----", "missing failure section"),
			fail_stderr.clone(),
		),
	] {
		assert!(digest(&stdout, &stderr, Path::new(SPILL_PATH)).is_none());
	}
	let (merged, _) = FIXTURES[8].cleaned();
	for corrupted in [
		merged[..merged.rfind("test result:").unwrap()].to_string(),
		merged.replace("358 passed", "357 passed"),
		format!("{merged}\napplication output unrelated to Cargo"),
		format!("{merged}\n   Compiling unfinished v0.1.0"),
	] {
		assert!(digest(&corrupted, "", Path::new(SPILL_PATH)).is_none());
	}
}

#[test]
fn digest_is_deterministic_on_real_captures() {
	for fixture in FIXTURES {
		let (stdout, stderr) = fixture.cleaned();
		let first = digest(&stdout, &stderr, Path::new(SPILL_PATH)).unwrap();
		for _ in 0..10 {
			assert_eq!(
				Some(&first),
				digest(&stdout, &stderr, Path::new(SPILL_PATH)).as_ref()
			);
		}
	}
}

#[test]
fn small_real_test_suite_is_untouched() {
	let (stdout, _) = FIXTURES[0].cleaned();
	let summary = stdout
		.lines()
		.find(|line| line.starts_with("test result:"))
		.unwrap();
	assert!(digest(summary, "", Path::new(SPILL_PATH)).is_none());
}

#[test]
fn invalid_suite_counts_and_unfinished_builds_are_rejected() {
	for stdout in [
		"running 1 test\ntest sample ... ok",
		"running 1 test\ntest sample ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
		"running 1 test\ntest sample ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
		"running 1 test\ntest sample ... mystery",
	] {
		assert!(select(&lines(stdout), &[]).is_none(), "{stdout}");
	}
	assert!(select(&[], &lines("   Compiling unfinished v0.1.0")).is_none());
	assert!(select(&[], &lines("ordinary non-cargo text")).is_none());
}

#[test]
fn summaries_reject_nonfinite_and_partial_counts() {
	for line in [
		"test result: ok. 1 passed; 0 failed",
		"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in NaNs",
		"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in infs",
		"test result: ok. 1 passed; 0 failed; 0 ignored; 1 measured; 0 filtered out; finished in 0.00s",
	] {
		assert!(summary(line).is_none(), "{line}");
	}
}

macro_rules! install_capture {
	($name:literal) => {
		(
			$name,
			include_str!(concat!("fixtures/cargo/install-", $name, ".stdout")),
			include_str!(concat!("fixtures/cargo/install-", $name, ".stderr")),
			include_str!(concat!("fixtures/cargo/install-", $name, ".merged")),
		)
	};
}

#[test]
fn fresh_install_builds_digest_but_fetches_keep_the_terminal_requirement() {
	use crate::mcp::fs::digest::digest_with;
	use crate::mcp::fs::shell::clean_terminal_noise;

	for (name, stdout, stderr, merged, golden, merged_golden) in [
		{
			let (name, stdout, stderr, merged) = install_capture!("build-fresh");
			(
				name,
				stdout,
				stderr,
				merged,
				include_str!("fixtures/cargo/install-build-fresh.digest"),
				include_str!("fixtures/cargo/install-build-fresh.merged.digest"),
			)
		},
		{
			let (name, stdout, stderr, merged) = install_capture!("compile-error-build-fresh");
			(
				name,
				stdout,
				stderr,
				merged,
				include_str!("fixtures/cargo/install-compile-error-build-fresh.digest"),
				include_str!("fixtures/cargo/install-compile-error-build-fresh.merged.digest"),
			)
		},
	] {
		for (stdout, stderr, golden) in [(stdout, stderr, golden), (merged, "", merged_golden)] {
			let stdout = clean_terminal_noise(stdout);
			let stderr = clean_terminal_noise(stderr);
			let out = lines(&stdout);
			let err = lines(&stderr);
			let selection = select(&out, &err).unwrap();
			let body = join_streams(
				&render(&out, &selection.stdout),
				&render(&err, &selection.stderr),
			);
			assert_eq!(body, golden, "{name}");
			let omitted = out.len() + err.len() - selection.stdout.len() - selection.stderr.len();
			let output = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).unwrap();
			assert_eq!(
				output,
				format!("{body}\n[digest: {omitted} lines omitted — full output: {SPILL_PATH}]")
			);
			println!(
				"install {name}: {} -> {} bytes",
				join_streams(&stdout, &stderr).len(),
				output.len()
			);
			for _ in 0..10 {
				assert_eq!(
					Some(&output),
					digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).as_ref()
				);
			}
			let corrupted = format!("{stderr}\n      Adding unfinished v1.0.0");
			assert!(digest_with(&stdout, &corrupted, Path::new(SPILL_PATH), &[select]).is_none());
		}
	}
	for (name, stdout, stderr, merged) in [
		install_capture!("fetch-fresh"),
		install_capture!("build-cached"),
		install_capture!("compile-error-build-cached"),
		install_capture!("build-badversion"),
	] {
		for (stdout, stderr) in [(stdout, stderr), (merged, "")] {
			let stdout = clean_terminal_noise(stdout);
			let stderr = clean_terminal_noise(stderr);
			assert!(
				digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).is_none(),
				"{name}"
			);
			if name == "fetch-fresh" {
				assert!(select(&lines(&stdout), &lines(&stderr)).is_none());
			}
		}
	}
	assert!(select(&[], &lines("      Adding unfinished v1.0.0")).is_none());
}
