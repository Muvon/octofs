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

//! Paired Go fetches and conservative pass-through for summary-less runs and errors.

use super::*;
use crate::mcp::fs::digest::{digest_with, join_streams, lines, render};
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

const SPILL: &str = "/tmp/octofs-digest/full-output.log";

macro_rules! capture {
	($name:literal) => {
		(
			$name,
			include_str!(concat!("fixtures/go_mod/", $name, ".stdout")),
			include_str!(concat!("fixtures/go_mod/", $name, ".stderr")),
			include_str!(concat!("fixtures/go_mod/", $name, ".merged")),
		)
	};
}

const CAPTURES: &[(&str, &str, &str, &str)] = &[
	capture!("mod-tidy-fresh"),
	capture!("mod-download-fresh"),
	capture!("build-fresh"),
	capture!("build-cached"),
	capture!("build-badversion"),
	capture!("mod-tidy-badversion"),
	capture!("mod-download-unknownrev"),
	capture!("build-unknownrev"),
	capture!("mod-tidy-unknownrev"),
];

#[test]
fn successful_progress_has_no_summary_and_errors_remain_untouched() {
	for &(name, stdout, stderr, merged) in CAPTURES {
		for (stdout, stderr) in [(stdout, stderr), (merged, "")] {
			let stdout = clean_terminal_noise(stdout);
			let stderr = clean_terminal_noise(stderr);
			if name == "mod-download-fresh" {
				let out = lines(&stdout);
				let err = lines(&stderr);
				let selection = select(&out, &err).unwrap_or_else(|| panic!("{name}"));
				let body = join_streams(
					&render(&out, &selection.stdout),
					&render(&err, &selection.stderr),
				);
				assert_eq!(
					body,
					include_str!("fixtures/go_mod/mod-download-fresh.digest")
				);
			} else {
				assert!(select(&lines(&stdout), &lines(&stderr)).is_none(), "{name}");
			}
			// The framework rejects an empty body rather than emitting a marker alone.
			for _ in 0..10 {
				assert!(
					digest_with(&stdout, &stderr, Path::new(SPILL), &[select]).is_none(),
					"{name}"
				);
			}
			let size = join_streams(&stdout, &stderr).len();
			println!("{name}: {size} -> {size} bytes (pass-through)");
		}
	}
}

#[test]
fn fetches_require_every_request_to_have_a_successful_response() {
	let stderr = clean_terminal_noise(CAPTURES[1].2);
	let complete = lines(&stderr);
	assert!(select(&[], &complete).is_some());
	assert!(select(&[], &complete[..complete.len() - 1]).is_none());
	let request = complete
		.iter()
		.find(|line| !line.contains(": 200 OK ("))
		.unwrap();
	let response = complete
		.iter()
		.find(|line| line.contains(": 200 OK ("))
		.unwrap();
	for corrupted in [
		format!("{stderr}\n{request}"),
		format!("{stderr}\n{response}"),
		stderr.replace(": 200 OK (", ": 404 Not Found ("),
		format!("{stderr}\nforeign output"),
		format!("{stderr}\n# get https://example.com/unfinished"),
	] {
		assert!(select(&[], &lines(&corrupted)).is_none());
	}
	assert!(select(&["foreign stdout"], &complete).is_none());
}

#[test]
fn unresolved_module_queries_and_mixed_tool_output_are_rejected() {
	let stderr = clean_terminal_noise(CAPTURES[0].2);
	assert!(select(&[], &lines(&stderr)).is_none());
	for corrupted in [
		stderr.replace(
			"go: found github.com/google/uuid in github.com/google/uuid v1.6.0",
			"",
		),
		stderr.replace(
			"go: found github.com/google/uuid",
			"go: found github.com/unknown/module",
		),
		format!("{stderr}\nforeign application output"),
		format!("{stderr}\ngo: downloading unfinished"),
	] {
		assert!(select(&[], &lines(&corrupted)).is_none());
	}
}
