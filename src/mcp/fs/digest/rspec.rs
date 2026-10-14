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

//! RSpec documentation output with intact pending and failure sections.

use super::{important, Selection};

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let stdout = documentation(stdout)?;
	let stderr = warnings(stderr)?;
	Some(Selection { stdout, stderr })
}

fn documentation(lines: &[&str]) -> Option<Vec<usize>> {
	let finish = lines
		.iter()
		.position(|line| line.starts_with("Finished in "))?;
	let timing = lines[finish].strip_prefix("Finished in ")?;
	let (seconds, load) = timing.split_once(" seconds (files took ")?;
	let load = load.strip_suffix(" seconds to load)")?;
	for text in [seconds, load] {
		let seconds = text.parse::<f64>().ok()?;
		if !seconds.is_finite() || seconds.is_sign_negative() {
			return None;
		}
	}
	let (examples, failures, pending) = summary(lines.get(finish + 1)?)?;
	let mut kept = Vec::new();
	let mut observed = 0usize;
	let mut failed = 0usize;
	let mut skipped = 0usize;
	let mut section = "";
	let mut entries = 0;
	let mut pending_entries = 0;
	let mut group = false;
	for i in 0..finish {
		let (line, repetitions) = repeated_line(lines, i)?;
		if line.starts_with("Pending: (") {
			if !section.is_empty() {
				return None;
			}
			section = "pending";
			kept.push(i);
		} else if line == "Failures:" {
			section = "failures";
			kept.push(i);
		} else if !section.is_empty() {
			if let Some(text) = line.strip_prefix("  ") {
				if !text.starts_with(' ') {
					let (number, _) = text.split_once(") ")?;
					let number = number.parse::<usize>().ok()?;
					let count = if section == "pending" {
						&mut pending_entries
					} else {
						&mut entries
					};
					*count += 1;
					if number != *count {
						return None;
					}
				}
			}
			kept.push(i);
		} else if let Some(text) = line.strip_prefix("  ") {
			if !group || text.is_empty() || text.starts_with(' ') {
				return None;
			}
			observed = observed.checked_add(repetitions)?;
			if text.contains(" (FAILED - ") {
				failed = failed.checked_add(repetitions)?;
				kept.push(i);
			} else if text.contains(" (PENDING: ") {
				skipped = skipped.checked_add(repetitions)?;
				kept.push(i);
			} else if important(line) {
				kept.push(i);
			}
		} else if !line.is_empty() {
			if line.starts_with('.') || line.starts_with('F') || line.starts_with('*') {
				return None;
			}
			if !group && (line.starts_with("DEPRECATION: ") || line.starts_with("Ignoring ")) {
				kept.push(i);
				continue;
			}
			// RSpec's documentation formatter leaves puts/warn output unadorned.
			// Keep these lines, including group labels, rather than guessing.
			group = true;
			kept.push(i);
		}
	}
	if observed != examples
		|| failed != failures
		|| skipped != pending
		|| entries != failures
		|| pending_entries != pending
	{
		return None;
	}
	kept.extend(finish..=finish + 1);
	let mut reruns = 0usize;
	let mut rerun_header = false;
	for i in finish + 2..lines.len() {
		let (line, repetitions) = repeated_line(lines, i)?;
		if line.is_empty() {
			kept.push(i);
		} else if line == "Failed examples:" && failures > 0 && !rerun_header {
			rerun_header = true;
			kept.push(i);
		} else if rerun_header && line.starts_with("rspec ./") && line.contains(" # ") {
			reruns = reruns.checked_add(repetitions)?;
			kept.push(i);
		} else {
			return None;
		}
	}
	if reruns != failures {
		return None;
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

fn summary(line: &str) -> Option<(usize, usize, usize)> {
	let parts: Vec<_> = line.split(", ").collect();
	let [examples, failures] = parts.as_slice() else {
		let [examples, failures, pending] = parts.as_slice() else {
			return None;
		};
		return Some((
			count(examples, " examples")?,
			count(failures, " failures")?,
			count(pending, " pending")?,
		));
	};
	Some((
		count(examples, " examples")?,
		count(failures, " failures")?,
		0,
	))
}

fn count(text: &str, suffix: &str) -> Option<usize> {
	text.strip_suffix(suffix)?.parse().ok()
}

fn warnings(lines: &[&str]) -> Option<Vec<usize>> {
	if let Some(i) = lines.iter().rposition(|line| !line.is_empty()) {
		let (last, _) = repeated_line(lines, i)?;
		if !last.starts_with("DEPRECATION: ")
			&& !(last.starts_with("Ignoring ") && last.contains("extensions are not built"))
		{
			return None;
		}
	}
	// Ruby warn writes plain text; RSpec's terminal deprecation warning
	// positively identifies this otherwise unstructured warning stream.
	Some((0..lines.len()).collect())
}
#[cfg(test)]
#[path = "rspec_tests.rs"]
mod rspec_tests;
