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

//! PHPUnit testdox results, embedded echo output, and startup warnings.

use super::{important, Selection};

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let stdout = session(stdout)?;
	let stderr = startup(stderr)?;
	Some(Selection { stdout, stderr })
}

fn session(lines: &[&str]) -> Option<Vec<usize>> {
	let end = lines.iter().rposition(|line| !line.is_empty())?;
	let (expected, issues) = if lines[end].starts_with("Tests: ") {
		summary(lines[end])?
	} else {
		let rest = lines[end]
			.strip_prefix("OK (")?
			.strip_suffix(" assertions)")?;
		let (tests, assertions) = rest.split_once(" tests, ")?;
		assertions.parse::<usize>().ok()?;
		(tests.parse::<usize>().ok()?, [0usize; 7])
	};
	if lines[end].starts_with("Tests: ") {
		let status = if issues[0] > 0 {
			"ERRORS!"
		} else if issues[1] > 0 {
			"FAILURES!"
		} else {
			"OK, but there were issues!"
		};
		if lines[..end].iter().rev().find(|line| !line.is_empty()) != Some(&status) {
			return None;
		}
	}
	let start = lines.iter().position(|line| line.starts_with("PHPUnit "))?;
	let mut kept = startup(&lines[..start])?;
	let mut banner = false;
	let mut time = false;
	let mut tests = 0usize;
	let mut progress = 0usize;
	let mut progress_issues = [0usize; 7];
	let mut doc_issues = [0usize; 4];
	let mut report = false;
	let mut details = false;
	for (i, &line) in lines[..=end].iter().enumerate().skip(start) {
		let (line, repetitions) = if let Some(count) = line.strip_prefix("[... last line repeated ")
		{
			let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
			let previous = *lines.get(i.checked_sub(1)?)?;
			if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ")
			{
				return None;
			}
			(previous, count)
		} else {
			(line, 1)
		};
		if !banner {
			let rest = line.strip_prefix("PHPUnit ")?;
			let (version, _) = rest.split_once(" by ")?;
			if version.split('.').any(|n| n.parse::<usize>().is_err()) {
				return None;
			}
			banner = true;
		} else if line.is_empty() {
			if details || report {
				kept.push(i);
			}
		} else if i == end {
			kept.push(i);
		} else if line.starts_with("Time: ") && line.contains(", Memory: ") {
			if time {
				return None;
			}
			time = true;
			kept.push(i);
		} else if !time && line.starts_with("Runtime: ") {
			if important(line) {
				kept.push(i);
			}
		} else if !time {
			let marks = line.bytes().take_while(|b| b".FEWDSIR".contains(b)).count();
			progress = progress.checked_add(marks.checked_mul(repetitions)?)?;
			for mark in line.bytes().take(marks) {
				if let Some(slot) = b"EFWDSIR".iter().position(|&c| c == mark) {
					progress_issues[slot] = progress_issues[slot].checked_add(repetitions)?;
				}
			}
			let rest = &line[marks..];
			if marks == 0 {
				return None;
			}
			let noise = if rest.trim().is_empty() {
				true
			} else if let Some((current, total, percent)) = progress_count(rest) {
				if current != progress || total != expected || percent > 100 {
					return None;
				}
				true
			} else {
				false
			};
			// PHPUnit splices echo output into its progress line. Such lines
			// are indivisible selections and must survive in their entirety.
			if !noise || important(line) {
				kept.push(i);
			}
		} else if line.starts_with(" ✔ ") {
			tests = tests.checked_add(repetitions)?;
			details = false;
			if important(line) {
				kept.push(i);
			}
		} else if [" ✘ ", " ⚠ ", " ↩ ", " ∅ "]
			.iter()
			.any(|p| line.starts_with(p))
		{
			tests = tests.checked_add(repetitions)?;
			let slot = if line.starts_with(" ✘ ") {
				0
			} else if line.starts_with(" ⚠ ") {
				1
			} else if line.starts_with(" ↩ ") {
				2
			} else {
				3
			};
			doc_issues[slot] = doc_issues[slot].checked_add(repetitions)?;
			details = true;
			kept.push(i);
		} else if line.starts_with("   │") && details {
			kept.push(i);
		} else if line.starts_with("There was ") || line.starts_with("There were ") {
			report = true;
			kept.push(i);
		} else if report || line == "ERRORS!" || line == "FAILURES!" || line.starts_with("OK, but ")
		{
			kept.push(i);
		} else if !line.starts_with(' ') && line.chars().all(|c| c.is_alphanumeric() || c == ' ') {
			details = false;
			if important(line) {
				kept.push(i);
			}
		} else {
			return None;
		}
	}
	if !banner
		|| !time
		|| tests != expected
		|| progress != expected
		|| progress_issues != issues
		|| doc_issues
			!= [
				issues[0] + issues[1],
				issues[2] + issues[3] + issues[6],
				issues[4],
				issues[5],
			] {
		return None;
	}
	Some(kept)
}

fn summary(line: &str) -> Option<(usize, [usize; 7])> {
	let mut fields = [None; 9];
	for part in line.strip_suffix('.')?.split(", ") {
		let (label, count) = part.split_once(": ")?;
		let slot = match label {
			"Tests" => 0,
			"Assertions" => 1,
			"Errors" => 2,
			"Failures" => 3,
			"Warnings" => 4,
			"Deprecations" => 5,
			"Skipped" => 6,
			"Incomplete" => 7,
			"Risky" => 8,
			_ => return None,
		};
		if fields[slot].replace(count.parse::<usize>().ok()?).is_some() {
			return None;
		}
	}
	let tests = fields[0]?;
	fields[1]?;
	// PHPUnit omits issue categories whose counts are zero.
	let mut issues = [0usize; 7];
	for (slot, count) in fields[2..].iter().enumerate() {
		if let Some(count) = count {
			issues[slot] = *count;
		}
	}
	Some((tests, issues))
}

fn progress_count(text: &str) -> Option<(usize, usize, usize)> {
	let (current, rest) = text.trim().split_once(" / ")?;
	let (total, percent) = rest.split_once(" (")?;
	Some((
		current.parse().ok()?,
		total.parse().ok()?,
		percent.strip_suffix("%)")?.trim().parse().ok()?,
	))
}

fn startup(lines: &[&str]) -> Option<Vec<usize>> {
	let mut warning = false;
	for (i, &line) in lines.iter().enumerate() {
		let line = if let Some(count) = line.strip_prefix("[... last line repeated ") {
			let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
			let previous = *lines.get(i.checked_sub(1)?)?;
			if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ")
			{
				return None;
			}
			previous
		} else {
			line
		};
		if line.is_empty() {
			continue;
		}
		if line.starts_with("PHP Warning: ") {
			warning = true;
		} else if !warning
			|| !(line.starts_with("Module compiled with module API=")
				|| line.starts_with("PHP    compiled with module API=")
				|| line == "These options need to match"
				|| line == " in Unknown on line 0")
		{
			return None;
		}
	}
	Some((0..lines.len()).collect())
}
#[cfg(test)]
#[path = "phpunit_tests.rs"]
mod phpunit_tests;
