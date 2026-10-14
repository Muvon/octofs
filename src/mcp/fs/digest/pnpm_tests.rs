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

//! pnpm counts, lifecycle boundaries, and selections from real captures.

use super::*;
use crate::mcp::fs::digest::{
	digest_with, join_streams, lines,
	mod_tests::{Fixture, SPILL_PATH},
	render, valid_selection,
};
use std::path::Path;

macro_rules! capture {
	($name:literal) => {
		(
			Fixture {
				name: $name,
				stdout: include_str!(concat!("fixtures/pnpm/", $name, ".stdout")),
				stderr: include_str!(concat!("fixtures/pnpm/", $name, ".stderr")),
				golden: include_str!(concat!("fixtures/pnpm/", $name, ".digest")),
				exit_code: 0,
			},
			Fixture {
				name: concat!($name, "-merged"),
				stdout: include_str!(concat!("fixtures/pnpm/", $name, ".merged")),
				stderr: "",
				golden: include_str!(concat!("fixtures/pnpm/", $name, ".merged.digest")),
				exit_code: 0,
			},
		)
	};
}

const CAPTURES: &[(Fixture, Fixture)] = &[capture!("fresh-install"), capture!("cached-reinstall")];

#[test]
fn real_installs_match_reviewed_goldens() {
	for fixture in CAPTURES.iter().flat_map(|(split, merged)| [split, merged]) {
		let (stdout, stderr) = fixture.cleaned();
		let out = lines(&stdout);
		let err = lines(&stderr);
		let selection = select(&out, &err).unwrap_or_else(|| panic!("{}", fixture.name));
		assert!(valid_selection(&out, &selection.stdout));
		assert!(valid_selection(&err, &selection.stderr));
		let body = join_streams(
			&render(&out, &selection.stdout),
			&render(&err, &selection.stderr),
		);
		assert_eq!(body, fixture.golden, "{}", fixture.name);
		let actual = digest_with(&stdout, &stderr, Path::new(SPILL_PATH), &[select]);
		if fixture.name == "fresh-install" {
			let omitted = out.len() - selection.stdout.len();
			assert_eq!(
				actual,
				Some(format!(
					"{body}\n[digest: {omitted} lines omitted — full output: {SPILL_PATH}]"
				))
			);
		} else {
			// Without the one-off banner, the preserved logs leave less than 30% savings.
			assert!(actual.is_none());
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
fn missing_completion_mismatched_counts_and_foreign_output_are_rejected() {
	let (stdout, _) = CAPTURES[0].0.cleaned();
	for corrupted in [
		stdout[..stdout.rfind("Done in ").unwrap()].to_owned(),
		stdout.replace("Packages: +705", "Packages: +704"),
		stdout.replace("added 705, done", "added 704, done"),
		stdout.replace("resolved 706", "resolved 704"),
		stdout.replace("install: Done", "install: still running"),
		stdout.replace("+ axios 1.20.0", "+ unrecognized table row"),
		stdout.replace(
			"Follow @pnpmjs for updates: https://twitter.com/pnpmjs",
			"foreign banner output",
		),
		format!("{stdout}\nforeign application output"),
		stdout.replace("Done in 4.7s", "Done in NaNs"),
	] {
		assert!(select(&lines(&corrupted), &[]).is_none());
		assert!(digest_with(&corrupted, "", Path::new(SPILL_PATH), &[select]).is_none());
	}
	assert!(select(&lines(&stdout), &["foreign stderr"]).is_none());
}

#[test]
fn lifecycle_logs_remain_whole_even_when_they_contain_diagnostics() {
	let (stdout, _) = CAPTURES[0].0.cleaned();
	let stdout = stdout.replace(
		"install: node-pre-gyp info ok",
		"install: warning: backend diagnostic\n\n.../sqlite3@5.1.6/node_modules/sqlite3 install: source excerpt",
	);
	let out = lines(&stdout);
	let selected = select(&out, &[]).unwrap();
	let body = render(&out, &selected.stdout);
	let first = stdout
		.find(".../sqlite3@5.1.6/node_modules/sqlite3 install$")
		.unwrap();
	let last = stdout
		.find(".../sqlite3@5.1.6/node_modules/sqlite3 install: Done")
		.unwrap()
		+ ".../sqlite3@5.1.6/node_modules/sqlite3 install: Done".len();
	assert!(body.contains(&stdout[first..last]));
}

#[test]
fn small_errors_and_large_unrecognized_failures_pass_through() {
	for input in [
		include_str!("fixtures/pnpm/fail-e404.stdout"),
		include_str!("fixtures/pnpm/fail-e404.merged"),
		include_str!("fixtures/pnpm/fail-badversion.stdout"),
		include_str!("fixtures/pnpm/fail-badversion.merged"),
	] {
		assert!(select(&lines(input), &[]).is_none());
		assert!(digest_with(input, "", Path::new(SPILL_PATH), &[select]).is_none());
		let large = format!(
			"{}{}",
			"Progress: resolved 1, reused 0, downloaded 0, added 0\n".repeat(100),
			input
		);
		assert!(digest_with(&large, "", Path::new(SPILL_PATH), &[select]).is_none());
	}
}
