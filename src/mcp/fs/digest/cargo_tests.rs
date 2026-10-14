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

//! Cargo recognition and golden selections from real tool runs.

use super::*;
use crate::mcp::fs::digest::{fixtures_tests::captures, join_streams, lines, render};

#[tokio::test]
async fn real_cargo_captures_match_goldens() {
	let captures = captures().await;
	for capture in &captures.logs {
		let stdout = lines(&capture.stdout);
		let stderr = lines(&capture.stderr);
		let selection = select(&stdout, &stderr).unwrap_or_else(|| {
			panic!(
				"{} not recognized:\n{}\n{}",
				capture.name, capture.stdout, capture.stderr
			)
		});
		assert_eq!(
			render(&stdout, &selection.stdout),
			capture.expected_stdout,
			"{}",
			capture.name
		);
		assert_eq!(
			render(&stderr, &selection.stderr),
			capture.expected_stderr,
			"{}",
			capture.name
		);
		let spill = captures.root.path().join("full-output.log");
		let digest = crate::mcp::fs::digest::digest(&capture.stdout, &capture.stderr, &spill)
			.unwrap_or_else(|| panic!("{} did not digest", capture.name));
		let (body, marker) = digest.rsplit_once("\n[digest: ").unwrap();
		assert_eq!(
			body,
			join_streams(&capture.expected_stdout, &capture.expected_stderr),
			"{}",
			capture.name
		);
		let omitted = stdout.len() + stderr.len() - selection.stdout.len() - selection.stderr.len();
		assert_eq!(
			marker,
			format!(
				"{omitted} lines omitted — full output: {}]",
				spill.display()
			)
		);
		let original = join_streams(&capture.stdout, &capture.stderr);
		println!(
			"{}: {} -> {} bytes",
			capture.name,
			original.len(),
			digest.len()
		);
		if capture.name == "pass" {
			assert!(
				digest.len() * 10 < original.len(),
				"all-pass noise should drop by over 90%"
			);
		}
	}
}

#[tokio::test]
async fn truncated_and_mixed_real_captures_are_rejected() {
	let captures = captures().await;
	let pass = &captures.logs[0];
	let fail = &captures.logs[1];
	let spill = captures.root.path().join("full-output.log");
	for (stdout, stderr) in [
		(
			pass.stdout[..pass.stdout.rfind("test result:").unwrap()].to_string(),
			pass.stderr.clone(),
		),
		(
			fail.stdout[..fail.stdout.rfind("test result:").unwrap()].to_string(),
			fail.stderr.clone(),
		),
		(
			pass.stdout.replace("240 passed", "239 passed"),
			pass.stderr.clone(),
		),
		(
			format!("{}\napplication output unrelated to Cargo", pass.stdout),
			pass.stderr.clone(),
		),
		(
			pass.stdout.clone(),
			format!("{}\n   Compiling unfinished v0.1.0", pass.stderr),
		),
		(
			pass.stdout.clone(),
			format!(
				"{}\nwarning: truncated diagnostic\n  incomplete details",
				pass.stderr
			),
		),
		(
			fail.stdout
				.replace("---- assert_failure stdout ----", "missing failure section"),
			fail.stderr.clone(),
		),
	] {
		assert!(crate::mcp::fs::digest::digest(&stdout, &stderr, &spill).is_none());
	}
}

#[tokio::test]
async fn digest_is_deterministic_on_real_captures() {
	let captures = captures().await;
	let spill = captures.root.path().join("full-output.log");
	for capture in &captures.logs {
		let first = crate::mcp::fs::digest::digest(&capture.stdout, &capture.stderr, &spill);
		for _ in 0..10 {
			assert_eq!(
				first,
				crate::mcp::fs::digest::digest(&capture.stdout, &capture.stderr, &spill)
			);
		}
	}
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
