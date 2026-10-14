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

//! Jest verbose suites with console code frames and complete failure reports.

use super::{important, Selection};

#[derive(Default)]
struct Run {
	files: [usize; 4],
	items: [usize; 4],
	summary: bool,
}

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let mut run = Run::default();
	let stdout = stream(stdout, &mut run)?;
	let stderr = stream(stderr, &mut run)?;
	if !run.summary {
		return None;
	}
	Some(Selection { stdout, stderr })
}

fn stream(lines: &[&str], run: &mut Run) -> Option<Vec<usize>> {
	let mut kept = Vec::new();
	let mut i = 0;
	while i < lines.len() {
		let (line, repetitions) = repeated_line(lines, i)?;
		if line.is_empty() {
			i += 1;
			continue;
		}
		if let Some(rest) = line.strip_prefix("Test Suites: ") {
			if run.summary || counts(rest)? != run.files {
				return None;
			}
			let tests = lines.get(i + 1)?.strip_prefix("Tests:")?.trim_start();
			if counts(tests)? != run.items {
				return None;
			}
			if counts(lines.get(i + 2)?.strip_prefix("Snapshots:")?.trim_start())? != [0; 4] {
				return None;
			}
			let time = lines.get(i + 3)?.strip_prefix("Time:")?.trim_start();
			let seconds = time.split_whitespace().next()?.parse::<f64>().ok()?;
			if !seconds.is_finite() || seconds.is_sign_negative() {
				return None;
			}
			if !lines.get(i + 4)?.starts_with("Ran all test suites") {
				return None;
			}
			run.summary = true;
			kept.extend(i..i + 5);
			i += 5;
			continue;
		} else if line == "Jest did not exit one second after the test run has completed." {
			if !run.summary {
				return None;
			}
			kept.push(i);
			i += 1;
			while lines.get(i).is_some_and(|line| line.is_empty()) {
				kept.push(i);
				i += 1;
			}
			if !lines
				.get(i)?
				.starts_with("'This usually means that there are asynchronous operations")
			{
				return None;
			}
			kept.push(i);
		} else if console(line) {
			let start = i;
			i += 1;
			while i < lines.len() {
				let (line, _) = repeated_line(lines, i)?;
				if (!line.is_empty() && !line.starts_with("    ")) || console(line) {
					break;
				}
				i += 1;
			}
			if i == start + 1 {
				return None;
			}
			kept.extend(start..i);
			continue;
		} else {
			let (failed, file) = if let Some(file) = line.strip_prefix("PASS ") {
				(false, file)
			} else {
				(true, line.strip_prefix("FAIL ")?)
			};
			if run.summary || (!file.contains(".test.") && !file.contains(".spec.")) {
				return None;
			}
			run.files[usize::from(failed)] =
				run.files[usize::from(failed)].checked_add(repetitions)?;
			if failed || important(line) {
				kept.push(i);
			}
			i += 1;
			let mut diagnostic = false;
			let mut failures = 0usize;
			let mut diagnostics = 0usize;
			while i < lines.len() {
				let (line, repetitions) = repeated_line(lines, i)?;
				if boundary(line) {
					break;
				}
				if line.starts_with("  ● ") {
					diagnostic = true;
					if line != "  ● Console" {
						diagnostics = diagnostics.checked_add(repetitions)?;
					}
				}
				if diagnostic && !line.is_empty() && !line.starts_with("  ") {
					return None;
				}
				if !diagnostic {
					if let Some(slot) = item(line) {
						run.items[slot] = run.items[slot].checked_add(repetitions)?;
						if slot == 1 {
							failures = failures.checked_add(repetitions)?;
						}
					} else if !line.is_empty()
						&& (!line.starts_with("  ") || line.starts_with("    "))
						&& !console(line)
					{
						return None;
					}
				}
				if failed || diagnostic || important(line) {
					kept.push(i);
				}
				i += 1;
			}
			if failed && failures != diagnostics {
				return None;
			}
			continue;
		}
		i += 1;
	}
	Some(kept)
}

fn repeated_line<'a>(lines: &[&'a str], i: usize) -> Option<(&'a str, usize)> {
	let line = lines[i];
	let Some(count) = line.strip_prefix("[... last line repeated ") else {
		return Some((line, 1));
	};
	let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
	let previous = *lines.get(i.checked_sub(1)?)?;
	if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ") {
		return None;
	}
	Some((previous, count))
}

fn console(line: &str) -> bool {
	matches!(
		line.trim_start(),
		"console.log" | "console.warn" | "console.error" | "console.info" | "console.debug"
	) && line.starts_with("  ")
}

fn item(line: &str) -> Option<usize> {
	if !line.starts_with("    ") {
		return None;
	}
	let text = line.trim_start();
	if text.starts_with("✓ ") {
		Some(0)
	} else if text.starts_with("✕ ") {
		Some(1)
	} else if text.starts_with("○ skipped ") {
		Some(2)
	} else if text.starts_with("✎ todo ") {
		Some(3)
	} else {
		None
	}
}

fn boundary(line: &str) -> bool {
	line.starts_with("PASS ")
		|| line.starts_with("FAIL ")
		|| line.starts_with("Test Suites: ")
		|| console(line)
}

fn counts(text: &str) -> Option<[usize; 4]> {
	let mut result = [0usize; 4];
	let mut seen = [false; 4];
	let mut total = None;
	for part in text.split(", ") {
		let (number, label) = part.split_once(' ')?;
		let number = number.parse::<usize>().ok()?;
		if label == "total" {
			if total.replace(number).is_some() {
				return None;
			}
		} else {
			let slot = match label {
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
			result[slot] = number;
		}
	}
	(result
		.iter()
		.try_fold(0usize, |sum, n| sum.checked_add(*n))?
		== total?)
		.then_some(result)
}
#[cfg(test)]
#[path = "jest_tests.rs"]
mod jest_tests;
