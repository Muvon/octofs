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

//! Real webpack captures, golden selections, and incomplete/foreign output rejection.

use super::super::{digest_with, lines, render};
use super::*;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

use super::super::build_capture_tests as captures;
use captures::{Capture, SPILL};

const CAPTURES: &[Capture] = &[
	Capture {
		name: "build-import-errors",
		stdout: include_str!("fixtures/webpack/build-import-errors.stdout"),
		stderr: include_str!("fixtures/webpack/build-import-errors.stderr"),
		merged: include_str!("fixtures/webpack/build-import-errors.merged"),
		split_golden: Some(include_str!("fixtures/webpack/build-import-errors.digest")),
		merged_golden: Some(include_str!(
			"fixtures/webpack/build-import-errors.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "build-ok",
		stdout: include_str!("fixtures/webpack/build-ok.stdout"),
		stderr: include_str!("fixtures/webpack/build-ok.stderr"),
		merged: include_str!("fixtures/webpack/build-ok.merged"),
		split_golden: Some(include_str!("fixtures/webpack/build-ok.digest")),
		merged_golden: Some(include_str!("fixtures/webpack/build-ok.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "build-syntax-error",
		stdout: include_str!("fixtures/webpack/build-syntax-error.stdout"),
		stderr: include_str!("fixtures/webpack/build-syntax-error.stderr"),
		merged: include_str!("fixtures/webpack/build-syntax-error.merged"),
		split_golden: Some(include_str!("fixtures/webpack/build-syntax-error.digest")),
		merged_golden: Some(include_str!(
			"fixtures/webpack/build-syntax-error.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
];

#[test]
fn real_captures_match_goldens_and_framework_limits() {
	captures::check(CAPTURES, select);
}

#[test]
fn resolver_traces_and_source_excerpts_are_preserved() {
	for name in ["build-import-errors", "build-syntax-error"] {
		let capture = CAPTURES
			.iter()
			.find(|capture| capture.name == name)
			.unwrap();
		let input = clean_terminal_noise(capture.stdout);
		let selection = select(&lines(&input), &[]).unwrap();
		let output = render(&lines(&input), &selection.stdout);
		assert!(output.contains(&input[input.find("ERROR in ").unwrap()..]));
	}
}

#[test]
fn truncated_diagnostics_and_incorrect_counts_are_rejected() {
	let complete =
		clean_terminal_noise(include_str!("fixtures/webpack/build-import-errors.stdout"));
	let truncated = &complete[..complete.rfind("webpack ").unwrap()];
	assert!(select(&lines(truncated), &[]).is_none());
	assert!(digest_with(truncated, "", Path::new(SPILL), &[select]).is_none());
	for corrupted in [
		complete.replace("compiled with 2 errors", "compiled with 1 error"),
		complete.replace(" @ ./app-err/main.js", "missing importer"),
		complete.replace(
			"assets by status 852 KiB [cached] 61 assets",
			"assets by status foreign output",
		),
		format!("{complete}\nwebpack 5.111.1 compiled successfully in 10 ms"),
	] {
		assert!(select(&lines(&corrupted), &[]).is_none());
	}
	assert_eq!(
		summary("webpack 5.1.0 compiled with 2 errors and 3 warnings in 12 ms"),
		Some((2, 3))
	);
	assert_eq!(
		summary("webpack 5.1.0 compiled successfully in 12 ms"),
		Some((0, 0))
	);
	for line in [
		"webpack 5.1.0 compiled with 2 error in 12 ms",
		"webpack 5.1.0 compiled with 0 warnings in 12 ms",
		"webpack 5.1.0 compiled with 2 errors and 3 errors in 12 ms",
	] {
		assert!(summary(line).is_none());
	}
}
