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

//! Verified, conservative selections of complete test/build output.

use std::path::Path;

// Below 4 KiB the savings rarely justify a spill file and another tool call.
const MIN_OUTPUT_BYTES: usize = 4 * 1024;
// Save at least 30%, including the stream label and spill marker.
const MAX_DIGEST_PERCENT: usize = 70;

struct Selection {
	stdout: Vec<usize>,
	stderr: Vec<usize>,
}

// Recognition covers both complete streams; an unknown or incomplete stream returns None.
type Parser = fn(&[&str], &[&str]) -> Option<Selection>;

// A parser's module and registry entry stay together so adding one needs one line.
macro_rules! parsers {
	($($name:ident),* $(,)?) => {
		$(mod $name;)*
		const PARSERS: &[Parser] = &[$($name::select),*];
	};
}

parsers!(
	cargo, pytest, go_test, vitest, jest, phpunit, rspec, pip, pnpm, go_mod, make, vite, webpack,
	docker, tsc,
);

pub(super) fn join_streams(stdout: &str, stderr: &str) -> String {
	if stderr.is_empty() {
		stdout.to_owned()
	} else if stdout.is_empty() {
		stderr.to_owned()
	} else {
		format!("{stdout}\n\nstderr:\n{stderr}")
	}
}

/// None means the caller must return the original output without a spill marker.
pub(super) fn digest(stdout: &str, stderr: &str, spill: &Path) -> Option<String> {
	digest_with(stdout, stderr, spill, PARSERS)
}

fn digest_with(stdout: &str, stderr: &str, spill: &Path, parsers: &[Parser]) -> Option<String> {
	let spill = spill.to_str()?;
	if spill.contains(['\n', '\r']) {
		return None;
	}
	let original = join_streams(stdout, stderr);
	if original.len() <= MIN_OUTPUT_BYTES {
		return None;
	}
	let stdout = lines(stdout);
	let stderr = lines(stderr);
	for parser in parsers {
		let Some(selection) = parser(&stdout, &stderr) else {
			continue;
		};
		// Indices make generation impossible. Strict ordering also rules out
		// duplication and movement, including for repeated important lines.
		if !valid_selection(&stdout, &selection.stdout)
			|| !valid_selection(&stderr, &selection.stderr)
		{
			return None;
		}
		let omitted = stdout.len() + stderr.len() - selection.stdout.len() - selection.stderr.len();
		let output = join_streams(
			&render(&stdout, &selection.stdout),
			&render(&stderr, &selection.stderr),
		);
		if output.is_empty() {
			return None;
		}
		let output = format!("{output}\n[digest: {omitted} lines omitted — full output: {spill}]");
		if output.len() * 100 <= original.len() * MAX_DIGEST_PERCENT {
			return Some(output);
		}
		return None;
	}
	None
}

fn lines(input: &str) -> Vec<&str> {
	if input.is_empty() {
		Vec::new()
	} else {
		input.split('\n').collect()
	}
}

fn render(lines: &[&str], indices: &[usize]) -> String {
	indices
		.iter()
		.map(|&i| lines[i])
		.collect::<Vec<_>>()
		.join("\n")
}

fn important(line: &str) -> bool {
	let lower = line.to_ascii_lowercase();
	[
		"error",
		"warn",
		"panic",
		"failed",
		"failure",
		"test result:",
		"-->",
		"finished",
		"stack backtrace",
		"note:",
		"help:",
		"caused by:",
	]
	.iter()
	.any(|pattern| lower.contains(pattern))
}

fn diagnostic_start(line: &str) -> bool {
	let line = line.trim_start().to_ascii_lowercase();
	[
		"error:",
		"error[",
		"warning:",
		"warning[",
		"fatal error:",
		"note:",
		"help:",
	]
	.iter()
	.any(|prefix| line.starts_with(prefix))
}

fn valid_selection(lines: &[&str], indices: &[usize]) -> bool {
	let mut kept = vec![false; lines.len()];
	let mut previous = None;
	for &index in indices {
		if index >= lines.len() || previous.is_some_and(|prev| index <= prev) {
			return false;
		}
		kept[index] = true;
		previous = Some(index);
	}
	let mut required = vec![false; lines.len()];
	for (i, &line) in lines.iter().enumerate() {
		if important(line) {
			required[i] = true;
		}
		if diagnostic_start(line) {
			let end = lines[i + 1..]
				.iter()
				.position(|line| line.is_empty())
				.map_or(lines.len(), |offset| i + offset + 2);
			required[i..end].fill(true);
		}
		if line == "Caused by:" {
			let mut end = i + 1;
			while end < lines.len() {
				if lines[end].is_empty() {
					end += 1;
					break;
				}
				// Cargo cause bodies use two spaces; progress uses three.
				if !lines[end].starts_with("  ") || lines[end].starts_with("   ") {
					break;
				}
				end += 1;
			}
			required[i..end].fill(true);
		}
		if line.trim() == "failures:"
			|| (line.starts_with("---- ") && line.ends_with(" stdout ----"))
		{
			let end = lines[i + 1..]
				.iter()
				.position(|line| line.starts_with("test result:"))
				.map_or(lines.len(), |offset| i + offset + 2);
			required[i..end].fill(true);
		}
	}
	required
		.iter()
		.zip(kept)
		.all(|(&required, kept)| !required || kept)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
pub(super) mod mod_tests;

#[cfg(test)]
#[path = "build_capture_tests.rs"]
mod build_capture_tests;
