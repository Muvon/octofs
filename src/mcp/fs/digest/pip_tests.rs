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

//! Real pip captures, cross-stream failures, and conservative rejection.

use super::*;
use crate::mcp::fs::digest::{
	digest_with, join_streams, lines,
	mod_tests::{Fixture, SPILL_PATH},
	render, valid_selection,
};
use std::path::Path;

macro_rules! capture {
	($name:literal, $exit:literal) => {
		(
			Fixture {
				name: $name,
				stdout: include_str!(concat!("fixtures/pip/", $name, ".stdout")),
				stderr: include_str!(concat!("fixtures/pip/", $name, ".stderr")),
				golden: include_str!(concat!("fixtures/pip/", $name, ".digest")),
				exit_code: $exit,
			},
			Fixture {
				name: concat!($name, "-merged"),
				stdout: include_str!(concat!("fixtures/pip/", $name, ".merged")),
				stderr: "",
				golden: include_str!(concat!("fixtures/pip/", $name, ".merged.digest")),
				exit_code: $exit,
			},
		)
	};
}

const CAPTURES: &[(Fixture, Fixture)] = &[
	capture!("fresh-install", 0),
	capture!("already-satisfied", 0),
	capture!("resolve-conflict", 1),
	capture!("build-fail", 1),
	capture!("no-matching-version", 1),
];

#[test]
fn captures_match_reviewed_goldens_and_framework() {
	for fixture in CAPTURES.iter().flat_map(|(split, merged)| [split, merged]) {
		let (stdout, stderr) = fixture.cleaned();
		let out = lines(&stdout);
		let err = lines(&stderr);
		let selection = select(&out, &err).unwrap_or_else(|| panic!("{}", fixture.name));
		assert!(
			valid_selection(&out, &selection.stdout),
			"{} stdout",
			fixture.name
		);
		assert!(
			valid_selection(&err, &selection.stderr),
			"{} stderr",
			fixture.name
		);
		let body = join_streams(
			&render(&out, &selection.stdout),
			&render(&err, &selection.stderr),
		);
		assert_eq!(body, fixture.golden, "{}", fixture.name);
		let actual = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]);
		if fixture.name.starts_with("fresh-install") {
			let omitted = out.len() + err.len() - selection.stdout.len() - selection.stderr.len();
			assert_eq!(
				actual,
				Some(format!(
					"{body}\n[digest: {omitted} lines omitted — full output: {SPILL_PATH}]"
				))
			);
		} else {
			// All-satisfied output has no retained body; errors are below 4 KiB.
			assert!(actual.is_none(), "{}", fixture.name);
		}
		let original = join_streams(&stdout, &stderr);
		println!(
			"{}: {} -> {} bytes",
			fixture.name,
			original.len(),
			actual.as_ref().map_or(original.len(), String::len)
		);
		for _ in 0..10 {
			assert_eq!(
				actual,
				digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select])
			);
		}
	}
}

#[test]
fn large_failures_keep_explanations_and_captured_build_output() {
	let (fresh, _) = CAPTURES[0].0.cleaned();
	let progress = &fresh[..fresh.find("Installing collected packages:").unwrap()];
	for (split, merged) in &CAPTURES[2..] {
		for fixture in [split, merged] {
			let (stdout, stderr) = fixture.cleaned();
			let stdout = format!("{progress}{stdout}");
			let output = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).unwrap();
			assert!(output.starts_with(fixture.golden), "{}", fixture.name);
			let original = join_streams(&stdout, &stderr);
			println!(
				"padded {}: {} -> {} bytes",
				fixture.name,
				original.len(),
				output.len()
			);
		}
	}
}

#[test]
fn cached_downloads_preserve_the_same_install_summary() {
	let (stdout, stderr) = CAPTURES[0].0.cleaned();
	let stdout = stdout.replace("Downloading ", "Using cached ");
	let output = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]).unwrap();
	assert!(output.starts_with(CAPTURES[0].0.golden));
}

#[test]
fn incomplete_mixed_and_inconsistent_installs_pass_through() {
	let (stdout, stderr) = CAPTURES[0].0.cleaned();
	for corrupted in [
		stdout[..stdout.find("Successfully installed ").unwrap()].to_owned(),
		stdout.replace("wcwidth-0.9.2 ", ""),
		stdout.replace(
			"Installing collected packages:",
			"Installing unknown packages:",
		),
		format!("{stdout}\nforeign application output"),
		format!("{stdout}\nCollecting unfinished==1.0"),
	] {
		assert!(select(&lines(&corrupted), &lines(&stderr)).is_none());
		assert!(digest_with(&corrupted, &stderr, Path::new(SPILL_PATH), &[select]).is_none());
	}
	assert!(digest_with(
		&stdout,
		&format!("{stderr}\nforeign stderr"),
		Path::new(SPILL_PATH),
		&[select]
	)
	.is_none());
	let (stdout, stderr) = CAPTURES[3].0.cleaned();
	assert!(select(
		&lines(&stdout),
		&lines(&stderr.replace("[34 lines of output]", "[33 lines of output]"))
	)
	.is_none());
	assert!(select(
		&lines(&stdout),
		&lines(&stderr.replace("[end of output]", "unfinished backend"))
	)
	.is_none());
	let (stdout, stderr) = CAPTURES[2].0.cleaned();
	assert!(select(
		&lines(&stdout.replace("The conflict is caused by:", "unfinished conflict")),
		&lines(&stderr)
	)
	.is_none());
}

#[test]
fn uv_and_unknown_output_are_untouched() {
	for (stdout, stderr, merged) in [
		(
			include_str!("fixtures/pip/uv/fresh-install.stdout"),
			include_str!("fixtures/pip/uv/fresh-install.stderr"),
			include_str!("fixtures/pip/uv/fresh-install.merged"),
		),
		(
			include_str!("fixtures/pip/uv/build-fail.stdout"),
			include_str!("fixtures/pip/uv/build-fail.stderr"),
			include_str!("fixtures/pip/uv/build-fail.merged"),
		),
		(
			include_str!("fixtures/pip/uv/resolve-conflict.stdout"),
			include_str!("fixtures/pip/uv/resolve-conflict.stderr"),
			include_str!("fixtures/pip/uv/resolve-conflict.merged"),
		),
		(
			include_str!("fixtures/pip/uv/already-satisfied.stdout"),
			include_str!("fixtures/pip/uv/already-satisfied.stderr"),
			include_str!("fixtures/pip/uv/already-satisfied.merged"),
		),
		(
			include_str!("fixtures/pip/uv/no-matching-version.stdout"),
			include_str!("fixtures/pip/uv/no-matching-version.stderr"),
			include_str!("fixtures/pip/uv/no-matching-version.merged"),
		),
	] {
		for (stdout, stderr) in [(stdout, stderr), (merged, "")] {
			assert!(select(&lines(stdout), &lines(stderr)).is_none());
			assert!(digest_with(stdout, stderr, Path::new(SPILL_PATH), &[select]).is_none());
		}
	}
	let unknown = "foreign output\n".repeat(1000);
	assert!(digest_with(&unknown, "", Path::new(SPILL_PATH), &[select]).is_none());
}
