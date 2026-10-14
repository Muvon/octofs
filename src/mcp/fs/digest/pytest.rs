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

//! Complete pytest sessions, progress counts, and intact diagnostic sections.

use super::{important, Selection};
use std::collections::BTreeSet;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if stderr.iter().any(|line| !line.is_empty()) {
		return None;
	}
	let end = stdout.iter().rposition(|line| !line.is_empty())?;
	let expected = summary(stdout[end])?;
	let mut observed = [0usize; 7];
	let mut kept = Vec::new();
	let mut nodes = BTreeSet::new();
	let mut collected = None;
	let mut section = "";
	let mut started = false;
	let mut failures = 0usize;
	let mut errors = 0usize;
	for (i, &line) in stdout[..end].iter().enumerate() {
		let (line, repetitions) = if let Some(count) = line.strip_prefix("[... last line repeated ")
		{
			let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
			let previous = *stdout.get(i.checked_sub(1)?)?;
			if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ")
			{
				return None;
			}
			(previous, count)
		} else {
			(line, 1)
		};
		if line.is_empty() {
			if !section.is_empty() {
				kept.push(i);
			}
			continue;
		}
		if let Some(title) = heading(line) {
			if title == "test session starts" {
				if started || i != 0 {
					return None;
				}
				started = true;
				continue;
			}
			if !matches!(
				title,
				"ERRORS"
					| "FAILURES" | "warnings summary"
					| "PASSES" | "XPASSES"
					| "short test summary info"
			) && !(title.starts_with("slowest ") && title.ends_with(" durations"))
			{
				return None;
			}
			section = title;
			kept.push(i);
			continue;
		}
		if section == "short test summary info" {
			let (verdict, rest) = line.split_once(' ')?;
			if rest.is_empty()
				|| !matches!(
					verdict,
					"PASSED" | "FAILED" | "ERROR" | "SKIPPED" | "XFAIL" | "XPASS"
				) {
				return None;
			}
			if verdict != "PASSED" || important(line) {
				kept.push(i);
			}
		} else if !section.is_empty() {
			if line.starts_with("___") && line.ends_with("___") {
				if section == "FAILURES" {
					failures = failures.checked_add(repetitions)?;
				}
				if section == "ERRORS" {
					errors = errors.checked_add(repetitions)?;
				}
			}
			// Captured output and tracebacks can contain arbitrary text and blank
			// lines; only pytest's section headings delimit these blocks.
			kept.push(i);
		} else if let Some(rest) = line
			.strip_prefix("collected ")
			.or_else(|| line.strip_prefix("collecting ... collected "))
		{
			let count = rest.strip_suffix(" items")?.parse::<usize>().ok()?;
			if collected.replace(count).is_some() {
				return None;
			}
			kept.push(i);
		} else if started && collected.is_none() && header_line(line) {
			if important(line) {
				kept.push(i);
			}
		} else {
			let body = percentage(line)?;
			if let Some((node, result)) = body.split_once(' ') {
				if node.contains("::") && node.contains(".py") {
					let verdict = result.split_whitespace().next()?;
					let slot = match verdict {
						"PASSED" => 0,
						"FAILED" => 1,
						"SKIPPED" => 2,
						"XFAIL" => 3,
						"XPASS" => 4,
						"ERROR" => 6,
						_ => return None,
					};
					observed[slot] = observed[slot].checked_add(repetitions)?;
					nodes.insert(node);
					if slot != 0 || important(line) {
						kept.push(i);
					}
					continue;
				}
			}
			let progress = if let Some((path, marks)) = body.split_once(' ') {
				if !path.ends_with(".py") {
					return None;
				}
				marks.trim()
			} else {
				body
			};
			if progress.is_empty() {
				return None;
			}
			for mark in progress.chars() {
				let slot = match mark {
					'.' => 0,
					'F' => 1,
					's' => 2,
					'x' => 3,
					'X' => 4,
					'E' => 6,
					_ => return None,
				};
				observed[slot] = observed[slot].checked_add(repetitions)?;
			}
			if important(line) {
				kept.push(i);
			}
		}
	}
	for slot in [0, 1, 2, 3, 4, 6] {
		if observed[slot] != expected[slot] {
			return None;
		}
	}
	if !nodes.is_empty() && collected != Some(nodes.len()) {
		return None;
	}
	if failures != expected[1] || errors != expected[6] {
		return None;
	}
	if expected[1] > 0 && !stdout.iter().any(|line| heading(line) == Some("FAILURES")) {
		return None;
	}
	if expected[6] > 0 && !stdout.iter().any(|line| heading(line) == Some("ERRORS")) {
		return None;
	}
	if expected[5] > 0
		&& !stdout
			.iter()
			.any(|line| heading(line) == Some("warnings summary"))
	{
		return None;
	}
	kept.push(end);
	Some(Selection {
		stdout: kept,
		stderr: Vec::new(),
	})
}

fn heading(line: &str) -> Option<&str> {
	if !line.starts_with("===") || !line.ends_with("===") {
		return None;
	}
	Some(line.trim_matches('=').trim())
}

fn summary(line: &str) -> Option<[usize; 7]> {
	let text = if line.starts_with('=') {
		line.strip_prefix('=')?
			.strip_suffix('=')?
			.trim_matches('=')
			.trim()
	} else {
		line
	};
	let (counts, elapsed) = text.rsplit_once(" in ")?;
	let seconds = elapsed.strip_suffix('s')?.parse::<f64>().ok()?;
	if !seconds.is_finite() || seconds.is_sign_negative() {
		return None;
	}
	let mut result = [0; 7];
	let mut seen = BTreeSet::new();
	for part in counts.split(", ") {
		let (count, label) = part.split_once(' ')?;
		let slot = match label {
			"passed" => 0,
			"failed" => 1,
			"skipped" => 2,
			"xfailed" => 3,
			"xpassed" => 4,
			"warnings" | "warning" => 5,
			"errors" | "error" => 6,
			_ => return None,
		};
		if !seen.insert(slot) {
			return None;
		}
		result[slot] = count.parse().ok()?;
	}
	Some(result)
}

fn percentage(line: &str) -> Option<&str> {
	let (body, percentage) = line.rsplit_once('[')?;
	let value = percentage
		.strip_suffix("%]")?
		.trim()
		.parse::<usize>()
		.ok()?;
	if value > 100 {
		return None;
	}
	Some(body.trim_end())
}

fn header_line(line: &str) -> bool {
	[
		"platform ",
		"cachedir: ",
		"rootdir: ",
		"configfile: ",
		"testpaths: ",
		"plugins: ",
		"asyncio: ",
	]
	.iter()
	.any(|prefix| line.starts_with(prefix))
}
#[cfg(test)]
#[path = "pytest_tests.rs"]
mod pytest_tests;
