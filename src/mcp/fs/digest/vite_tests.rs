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

//! Real vite captures, golden selections, and incomplete/foreign output rejection.

use super::super::{digest_with, lines};
use super::*;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

use super::super::build_capture_tests as captures;
use captures::{Capture, SPILL};

const CAPTURES: &[Capture] = &[
	Capture {
		name: "build-import-error",
		stdout: include_str!("fixtures/vite/build-import-error.stdout"),
		stderr: include_str!("fixtures/vite/build-import-error.stderr"),
		merged: include_str!("fixtures/vite/build-import-error.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "build-ok",
		stdout: include_str!("fixtures/vite/build-ok.stdout"),
		stderr: include_str!("fixtures/vite/build-ok.stderr"),
		merged: include_str!("fixtures/vite/build-ok.merged"),
		split_golden: Some(include_str!("fixtures/vite/build-ok.digest")),
		merged_golden: Some(include_str!("fixtures/vite/build-ok.merged.digest")),
		split_digest: true,
		merged_digest: true,
	},
	Capture {
		name: "build-syntax-error",
		stdout: include_str!("fixtures/vite/build-syntax-error.stdout"),
		stderr: include_str!("fixtures/vite/build-syntax-error.stderr"),
		merged: include_str!("fixtures/vite/build-syntax-error.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
];

#[test]
fn real_captures_match_goldens_and_framework_limits() {
	captures::check(CAPTURES, select);
}

#[test]
fn unfinished_assets_and_unknown_warning_text_are_rejected() {
	let complete = clean_terminal_noise(include_str!("fixtures/vite/build-ok.merged"));
	let truncated = &complete[..complete.rfind("✓ built in ").unwrap()];
	assert!(select(&lines(truncated), &[]).is_none());
	assert!(digest_with(truncated, "", Path::new(SPILL), &[select]).is_none());
	for corrupted in [
		complete.replace("✓ 97 modules transformed.", "✓ modules transformed."),
		complete.replace(
			"- Using dynamic import() to code-split the application",
			"unrelated text",
		),
		complete.replace("gzip:", "not-an-asset:"),
		format!("{complete}\n✓ built in 12ms"),
	] {
		assert!(select(&lines(&corrupted), &[]).is_none());
	}
}
