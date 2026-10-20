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

//! Shared checks for independently registered build parsers and real captures.

use super::{digest_with, join_streams, lines, render, Parser};
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

pub(super) const SPILL: &str = "/tmp/octofs-digest/full-output.log";

pub(super) struct Capture {
	pub name: &'static str,
	pub stdout: &'static str,
	pub stderr: &'static str,
	pub merged: &'static str,
	pub split_golden: Option<&'static str>,
	pub merged_golden: Option<&'static str>,
	pub split_digest: bool,
	pub merged_digest: bool,
}

pub(super) fn check(captures: &[Capture], parser: Parser) {
	for capture in captures {
		for (mode, stdout, stderr, golden, digested) in [
			(
				"split",
				capture.stdout,
				capture.stderr,
				capture.split_golden,
				capture.split_digest,
			),
			(
				"merged",
				capture.merged,
				"",
				capture.merged_golden,
				capture.merged_digest,
			),
		] {
			let stdout = clean_terminal_noise(stdout);
			let stderr = clean_terminal_noise(stderr);
			let stdout_lines = lines(&stdout);
			let stderr_lines = lines(&stderr);
			let selection = parser(&stdout_lines, &stderr_lines);
			assert_eq!(
				selection.is_some(),
				golden.is_some(),
				"{} {mode} recognition",
				capture.name
			);
			if let Some(golden) = golden {
				let selection = selection.unwrap();
				let body = join_streams(
					&render(&stdout_lines, &selection.stdout),
					&render(&stderr_lines, &selection.stderr),
				);
				assert_eq!(body, golden, "{} {mode} selection", capture.name);
			}
			let first = digest_with(&stdout, &stderr, Path::new(SPILL), &[parser]);
			assert_eq!(first.is_some(), digested, "{} {mode} framework", capture.name);
			if let Some(output) = &first {
				let selection = parser(&stdout_lines, &stderr_lines).unwrap();
				let omitted = stdout_lines.len() + stderr_lines.len() - selection.stdout.len() - selection.stderr.len();
				assert_eq!(
					output,
					&format!(
						"{}\n[digest: {omitted} lines omitted — full output: {SPILL}]",
						golden.unwrap()
					)
				);
			}
			println!(
				"{} {mode}: {} -> {} bytes{}",
				capture.name,
				join_streams(&stdout, &stderr).len(),
				first
					.as_ref()
					.map_or_else(|| join_streams(&stdout, &stderr).len(), String::len),
				if first.is_none() { " (pass-through)" } else { "" }
			);
			for _ in 0..3 {
				assert_eq!(first, digest_with(&stdout, &stderr, Path::new(SPILL), &[parser]));
			}
			if golden.is_some() {
				let mut foreign = stdout.clone();
				foreign.push_str("\nunrelated application output");
				assert!(
					digest_with(&foreign, &stderr, Path::new(SPILL), &[parser]).is_none(),
					"{} {mode} foreign stdout",
					capture.name
				);
				let foreign = format!("{stderr}\nunrelated application output");
				assert!(
					digest_with(&stdout, &foreign, Path::new(SPILL), &[parser]).is_none(),
					"{} {mode} foreign stderr",
					capture.name
				);
			}
		}
	}
}
