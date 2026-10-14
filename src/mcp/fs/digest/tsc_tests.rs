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

//! TypeScript emission noise and unchanged diagnostic-only captures.

use super::super::{digest_with, lines};
use super::*;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

use super::super::build_capture_tests as captures;
use captures::{Capture, SPILL};

const CAPTURES: &[Capture] = &[
	Capture {
		name: "clean-build-listemitted",
		stdout: include_str!("fixtures/tsc/clean-build-listemitted.stdout"),
		stderr: include_str!("fixtures/tsc/clean-build-listemitted.stderr"),
		merged: include_str!("fixtures/tsc/clean-build-listemitted.merged"),
		split_golden: Some(include_str!("fixtures/tsc/clean-build-listemitted.digest")),
		merged_golden: Some(include_str!(
			"fixtures/tsc/clean-build-listemitted.merged.digest"
		)),
		split_digest: true,
		merged_digest: true,
	},
	Capture {
		name: "clean-build",
		stdout: include_str!("fixtures/tsc/clean-build.stdout"),
		stderr: include_str!("fixtures/tsc/clean-build.stderr"),
		merged: include_str!("fixtures/tsc/clean-build.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "errors-multifile-pretty",
		stdout: include_str!("fixtures/tsc/errors-multifile-pretty.stdout"),
		stderr: include_str!("fixtures/tsc/errors-multifile-pretty.stderr"),
		merged: include_str!("fixtures/tsc/errors-multifile-pretty.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "errors-multifile",
		stdout: include_str!("fixtures/tsc/errors-multifile.stdout"),
		stderr: include_str!("fixtures/tsc/errors-multifile.stderr"),
		merged: include_str!("fixtures/tsc/errors-multifile.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "noemit-many-errors-pretty",
		stdout: include_str!("fixtures/tsc/noemit-many-errors-pretty.stdout"),
		stderr: include_str!("fixtures/tsc/noemit-many-errors-pretty.stderr"),
		merged: include_str!("fixtures/tsc/noemit-many-errors-pretty.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "noemit-many-errors",
		stdout: include_str!("fixtures/tsc/noemit-many-errors.stdout"),
		stderr: include_str!("fixtures/tsc/noemit-many-errors.stderr"),
		merged: include_str!("fixtures/tsc/noemit-many-errors.merged"),
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
fn incomplete_and_invalid_diagnostic_tables_are_rejected() {
	let complete =
		clean_terminal_noise(include_str!("fixtures/tsc/clean-build-listemitted.stdout"));
	let truncated = &complete[..complete.rfind("Total time:").unwrap()];
	assert!(select(&lines(truncated), &[]).is_none());
	assert!(digest_with(truncated, "", Path::new(SPILL), &[select]).is_none());
	let numeric = complete.replace("0.89s", "NaNs");
	let negative = complete.replace("0.89s", "-0.01s");
	let foreign = format!("project.ts(1,1): error TS2322: invalid value\n{complete}");
	for corrupted in [numeric, negative, foreign] {
		assert!(select(&lines(&corrupted), &[]).is_none());
	}
}
