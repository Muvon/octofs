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

//! Verbose Vitest results with split or merged console and failure blocks.

use super::{important, Selection};
use std::collections::BTreeMap;

#[derive(Default)]
struct Run<'a> {
	banner: bool,
	files: BTreeMap<&'a str, bool>,
	items: [usize; 4],
	file_counts: Option<[usize; 4]>,
	test_counts: Option<[usize; 4]>,
	diagnostics: usize,
	diagnostic_total: Option<usize>,
}

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let mut run = Run::default();
	let stdout = stream(stdout, &mut run)?;
	let stderr = stream(stderr, &mut run)?;
	let tests = run.test_counts?;
	let files = run.file_counts?;
	let failed_files = run.files.values().filter(|&&failed| failed).count();
	if !run.banner
		|| run.items != tests
		|| run.files.len() != files.iter().sum::<usize>()
		|| failed_files != files[1]
		|| run.diagnostics != tests[1]
		|| (tests[1] > 0 && run.diagnostic_total != Some(tests[1]))
	{
		return None;
	}
	Some(Selection { stdout, stderr })
}

fn stream<'a>(lines: &[&'a str], run: &mut Run<'a>) -> Option<Vec<usize>> {
	let mut kept = Vec::new();
	let mut i = 0;
	while i < lines.len() {
		let (line, repetitions) = if let Some(count) =
			lines[i].strip_prefix("[... last line repeated ")
		{
			let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
			let previous = *lines.get(i.checked_sub(1)?)?;
			if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ")
			{
				return None;
			}
			(previous, count)
		} else {
			(lines[i], 1)
		};
		if line.is_empty() {
			i += 1;
			continue;
		}
		if let Some(rest) = line.strip_prefix(" RUN  v") {
			let (version, path) = rest.split_once(' ')?;
			if run.banner
				|| version.split('.').any(|n| n.parse::<usize>().is_err())
				|| path.is_empty()
			{
				return None;
			}
			run.banner = true;
		} else if let Some(rest) = line.strip_prefix(" Test Files  ") {
			if run.file_counts.replace(counts(rest)?).is_some() {
				return None;
			}
			let tests = lines.get(i + 1)?.trim_start().strip_prefix("Tests  ")?;
			if run.test_counts.replace(counts(tests)?).is_some() {
				return None;
			}
			if !lines.get(i + 2)?.starts_with("   Start at  ")
				|| !lines.get(i + 3)?.starts_with("   Duration  ")
			{
				return None;
			}
			kept.extend(i..i + 4);
			i += 4;
			continue;
		} else if line.starts_with("stdout | ") || line.starts_with("stderr | ") {
			if !line.contains(" > ") {
				return None;
			}
			let start = i;
			i += 1;
			while i < lines.len() && !lines[i].is_empty() {
				i += 1;
			}
			if i == lines.len() || i == start + 1 {
				return None;
			}
			kept.extend(start..=i);
			i += 1;
			continue;
		} else if line.starts_with('⎯') && line.contains(" Failed Tests ") {
			let text = line
				.trim_matches('⎯')
				.trim()
				.strip_prefix("Failed Tests ")?;
			if run.diagnostic_total.replace(text.parse().ok()?).is_some() {
				return None;
			}
			kept.push(i);
		} else if line.starts_with(" FAIL  ") {
			if !line.contains(" > ") {
				return None;
			}
			let start = i;
			i += 1;
			while i < lines.len() && !lines[i].starts_with('⎯') {
				i += 1;
			}
			let footer = lines.get(i)?.trim_matches('⎯');
			let (number, total) = footer
				.strip_prefix('[')?
				.strip_suffix(']')?
				.split_once('/')?;
			run.diagnostics += 1;
			if number.parse::<usize>().ok()? != run.diagnostics
				|| total.parse::<usize>().ok()? != run.diagnostic_total?
				|| i == start + 1
			{
				return None;
			}
			kept.extend(start..=i);
			i += 1;
			continue;
		} else {
			let (slot, rest) = if let Some(rest) = line.strip_prefix(" ✓ ") {
				(0, rest)
			} else if let Some(rest) = line.strip_prefix(" × ") {
				(1, rest)
			} else if let Some(rest) = line.strip_prefix(" ↓ ") {
				(2, rest)
			} else {
				(3, line.strip_prefix(" □ ")?)
			};
			let (file, test) = rest.split_once(" > ")?;
			if !file.contains(".test.") && !file.contains(".spec.") {
				return None;
			}
			if !test.contains(" > ") {
				return None;
			}
			if slot < 2 {
				let (_, elapsed) = test.rsplit_once(' ')?;
				elapsed.strip_suffix("ms")?.parse::<usize>().ok()?;
			}
			run.items[slot] = run.items[slot].checked_add(repetitions)?;
			*run.files.entry(file).or_insert(false) |= slot == 1;
			if slot != 0 || important(line) {
				kept.push(i);
			}
			if slot == 1 {
				let start = i + 1;
				if !lines.get(start)?.starts_with("   → ") {
					return None;
				}
				i = start;
				while i < lines.len() && !event(lines[i]) {
					i += 1;
				}
				kept.extend(start..i);
				continue;
			}
		}
		i += 1;
	}
	Some(kept)
}

fn event(line: &str) -> bool {
	[
		" ✓ ",
		" × ",
		" ↓ ",
		" □ ",
		"stdout | ",
		"stderr | ",
		" Test Files  ",
		" FAIL  ",
		"⎯",
	]
	.iter()
	.any(|prefix| line.starts_with(prefix))
}

fn counts(text: &str) -> Option<[usize; 4]> {
	let (parts, total) = text.rsplit_once(" (")?;
	let total = total.strip_suffix(')')?.parse::<usize>().ok()?;
	let mut counts = [0usize; 4];
	let mut seen = [false; 4];
	for part in parts.split(" | ") {
		let (count, verdict) = part.split_once(' ')?;
		let slot = match verdict {
			"passed" => 0,
			"failed" => 1,
			"skipped" => 2,
			"todo" => 3,
			_ => return None,
		};
		if seen[slot] {
			return None;
		}
		seen[slot] = true;
		counts[slot] = count.parse().ok()?;
	}
	(counts
		.iter()
		.try_fold(0usize, |sum, n| sum.checked_add(*n))?
		== total)
		.then_some(counts)
}
#[cfg(test)]
#[path = "vitest_tests.rs"]
mod vitest_tests;
