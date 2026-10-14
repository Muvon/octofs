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

//! Real make captures, golden selections, and incomplete/foreign output rejection.

use super::super::{digest_with, lines, render};
use super::*;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

use super::super::build_capture_tests as captures;
use captures::{Capture, SPILL};

const CAPTURES: &[Capture] = &[
	Capture {
		name: "cmake-build-err-keepgoing",
		stdout: include_str!("fixtures/make/cmake-build-err-keepgoing.stdout"),
		stderr: include_str!("fixtures/make/cmake-build-err-keepgoing.stderr"),
		merged: include_str!("fixtures/make/cmake-build-err-keepgoing.merged"),
		split_golden: Some(include_str!(
			"fixtures/make/cmake-build-err-keepgoing.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/make/cmake-build-err-keepgoing.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "cmake-build-err",
		stdout: include_str!("fixtures/make/cmake-build-err.stdout"),
		stderr: include_str!("fixtures/make/cmake-build-err.stderr"),
		merged: include_str!("fixtures/make/cmake-build-err.merged"),
		split_golden: Some(include_str!("fixtures/make/cmake-build-err.digest")),
		merged_golden: Some(include_str!("fixtures/make/cmake-build-err.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "cmake-build-link-error",
		stdout: include_str!("fixtures/make/cmake-build-link-error.stdout"),
		stderr: include_str!("fixtures/make/cmake-build-link-error.stderr"),
		merged: include_str!("fixtures/make/cmake-build-link-error.merged"),
		split_golden: Some(include_str!("fixtures/make/cmake-build-link-error.digest")),
		merged_golden: Some(include_str!(
			"fixtures/make/cmake-build-link-error.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "cmake-build-ok-large",
		stdout: include_str!("fixtures/make/cmake-build-ok-large.stdout"),
		stderr: include_str!("fixtures/make/cmake-build-ok-large.stderr"),
		merged: include_str!("fixtures/make/cmake-build-ok-large.merged"),
		split_golden: Some(include_str!("fixtures/make/cmake-build-ok-large.digest")),
		merged_golden: Some(include_str!(
			"fixtures/make/cmake-build-ok-large.merged.digest"
		)),
		split_digest: true,
		merged_digest: true,
	},
	Capture {
		name: "cmake-build-ok",
		stdout: include_str!("fixtures/make/cmake-build-ok.stdout"),
		stderr: include_str!("fixtures/make/cmake-build-ok.stderr"),
		merged: include_str!("fixtures/make/cmake-build-ok.merged"),
		split_golden: Some(include_str!("fixtures/make/cmake-build-ok.digest")),
		merged_golden: Some(include_str!("fixtures/make/cmake-build-ok.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "cmake-configure-fresh",
		stdout: include_str!("fixtures/make/cmake-configure-fresh.stdout"),
		stderr: include_str!("fixtures/make/cmake-configure-fresh.stderr"),
		merged: include_str!("fixtures/make/cmake-configure-fresh.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "err-keepgoing-j8",
		stdout: include_str!("fixtures/make/err-keepgoing-j8.stdout"),
		stderr: include_str!("fixtures/make/err-keepgoing-j8.stderr"),
		merged: include_str!("fixtures/make/err-keepgoing-j8.merged"),
		split_golden: Some(include_str!("fixtures/make/err-keepgoing-j8.digest")),
		merged_golden: Some(include_str!("fixtures/make/err-keepgoing-j8.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "err-keepgoing-serial",
		stdout: include_str!("fixtures/make/err-keepgoing-serial.stdout"),
		stderr: include_str!("fixtures/make/err-keepgoing-serial.stderr"),
		merged: include_str!("fixtures/make/err-keepgoing-serial.merged"),
		split_golden: Some(include_str!("fixtures/make/err-keepgoing-serial.digest")),
		merged_golden: Some(include_str!(
			"fixtures/make/err-keepgoing-serial.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "err-stop-first",
		stdout: include_str!("fixtures/make/err-stop-first.stdout"),
		stderr: include_str!("fixtures/make/err-stop-first.stderr"),
		merged: include_str!("fixtures/make/err-stop-first.merged"),
		split_golden: Some(include_str!("fixtures/make/err-stop-first.digest")),
		merged_golden: Some(include_str!("fixtures/make/err-stop-first.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "link-error",
		stdout: include_str!("fixtures/make/link-error.stdout"),
		stderr: include_str!("fixtures/make/link-error.stderr"),
		merged: include_str!("fixtures/make/link-error.merged"),
		split_golden: Some(include_str!("fixtures/make/link-error.digest")),
		merged_golden: Some(include_str!("fixtures/make/link-error.merged.digest")),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "ok-j8",
		stdout: include_str!("fixtures/make/ok-j8.stdout"),
		stderr: include_str!("fixtures/make/ok-j8.stderr"),
		merged: include_str!("fixtures/make/ok-j8.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "ok-large-j8",
		stdout: include_str!("fixtures/make/ok-large-j8.stdout"),
		stderr: include_str!("fixtures/make/ok-large-j8.stderr"),
		merged: include_str!("fixtures/make/ok-large-j8.merged"),
		split_golden: None,
		merged_golden: None,
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "ok-serial",
		stdout: include_str!("fixtures/make/ok-serial.stdout"),
		stderr: include_str!("fixtures/make/ok-serial.stderr"),
		merged: include_str!("fixtures/make/ok-serial.merged"),
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
fn unfinished_builds_and_foreign_diagnostics_are_rejected() {
	let complete = clean_terminal_noise(include_str!("fixtures/make/cmake-build-ok-large.merged"));
	let truncated = &complete[..complete.rfind("[100%] Built target").unwrap()];
	assert!(select(&lines(truncated), &[]).is_none());
	assert!(digest_with(truncated, "", Path::new(SPILL), &[select]).is_none());
	for output in [
		"cc -c source.c -o source.o",
		"[ 30%] Building C object file.o\napplication output\n[100%] Built target app",
		"[101%] Built target app",
		"make: *** [Makefile:4: app] Error 1\nwarning: unrelated warning",
		"src/a.c:2:1: error: broken\n  2 | a\n    | ^",
		"cc -c a.c -o a.o && echo foreign\n[100%] Built target app",
		"mkdir -p build; echo foreign\n[100%] Built target app",
		"[ 20%] Building C object a.o\ngmake[2]: *** [build.make:4: a.o] Error 1",
	] {
		assert!(select(&lines(output), &[]).is_none(), "{output}");
	}
}

#[test]
fn unordered_cmake_percentages_and_gcc_unicode_quotes_keep_diagnostics() {
	let stdout = "[ 80%] Building C object late.o\n[ 20%] Building C object early.o";
	let stderr = "src/a.c: In function ‘f’:\nsrc/a.c:2:1: error: ‘x’ undeclared\n  2 | x\n    | ^\nmake: *** [Makefile:4: app] Error 1";
	let selection = select(&lines(stdout), &lines(stderr)).unwrap();
	assert!(selection.stdout.is_empty());
	assert_eq!(render(&lines(stderr), &selection.stderr), stderr);
	let complete = format!("make[1]: Entering directory '/project'\n{stdout}\n[100%] Built target app\nmake[1]: Leaving directory '/project'");
	assert_eq!(
		render(
			&lines(&complete),
			&select(&lines(&complete), &[]).unwrap().stdout
		),
		"[100%] Built target app"
	);
}
