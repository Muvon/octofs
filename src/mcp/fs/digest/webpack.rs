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

//! Webpack statistics with complete diagnostic blocks and verified counts.

use super::Selection;
use regex::Regex;
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if !stderr.is_empty() {
		return None;
	}
	let mut kept = Vec::new();
	let mut statistics = Vec::new();
	let mut errors = 0;
	let mut warnings = 0;
	let mut completed = false;
	let mut i = 0;
	while i < stdout.len() {
		let line = stdout[i];
		if line.is_empty() {
			i += 1;
			continue;
		}
		if completed {
			return None;
		}
		if let Some(counts) = summary(line) {
			if counts != (errors, warnings) || statistics.is_empty() {
				return None;
			}
			// Failed compilation statistics can describe the affected graph.
			if errors > 0 {
				kept.extend_from_slice(&statistics);
			}
			kept.push(i);
			completed = true;
			i += 1;
			continue;
		}
		let error = line.starts_with("ERROR in ");
		if error || line.starts_with("WARNING in ") {
			let start = i;
			i += 1;
			while i < stdout.len() && !stdout[i].is_empty() {
				if stdout[i].starts_with("webpack ")
					|| stdout[i].starts_with("ERROR in ")
					|| stdout[i].starts_with("WARNING in ")
				{
					return None;
				}
				i += 1;
			}
			if i == stdout.len() || i == start + 1 {
				return None;
			}
			if error {
				let body = &stdout[start + 1..i];
				if !(body[0].starts_with("Module not found: Error: Can't resolve ")
					|| body[0].starts_with("Module parse failed: "))
					|| !body.last()?.starts_with(" @ ")
				{
					return None;
				}
				errors += 1;
			} else {
				warnings += 1;
			}
			kept.extend(start..i + 1);
			continue;
		}
		if errors != 0 || warnings != 0 || !statistic().is_match(line) {
			return None;
		}
		statistics.push(i);
		i += 1;
	}
	if !completed {
		return None;
	}
	kept.sort_unstable();
	Some(Selection {
		stdout: kept,
		stderr: Vec::new(),
	})
}

fn summary(line: &str) -> Option<(usize, usize)> {
	static RE: OnceLock<Regex> = OnceLock::new();
	let captures = RE
		.get_or_init(|| {
			Regex::new(r"^webpack \d+\.\d+\.\d+ compiled (successfully|with (.+)) in \d+ ms$")
				.unwrap()
		})
		.captures(line)?;
	if &captures[1] == "successfully" {
		return Some((0, 0));
	}
	let mut errors = None;
	let mut warnings = None;
	for part in captures.get(2)?.as_str().split(" and ") {
		let (count, unit) = part.split_once(' ')?;
		let count = count.parse::<usize>().ok()?;
		if count == 0 {
			return None;
		}
		match unit {
			"error" if count == 1 && errors.is_none() => errors = Some(count),
			"errors" if count > 1 && errors.is_none() => errors = Some(count),
			"warning" if count == 1 && warnings.is_none() => warnings = Some(count),
			"warnings" if count > 1 && warnings.is_none() => warnings = Some(count),
			_ => return None,
		}
	}
	Some((errors.unwrap_or(0), warnings.unwrap_or(0)))
}

fn statistic() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		let size = r"\d+(?:\.\d+)? (?:bytes|[KMG]iB)";
		Regex::new(&format!(
			r"^(?:assets by path \S+ {size}|assets by status {size} (?:\[[^\]]+\] )?\d+ assets|orphan modules {size} \[orphan\] \d+ modules|runtime modules {size} \d+ modules|(?:\s+)?modules by path \S+ {size}(?: \(javascript\) {size} \(asset\))?|\s+(?:asset \S+ {size}(?: \[.*\]| \(.*\))*|javascript modules {size} \d+ modules|asset modules {size} \(asset\) {size} \(javascript\)|\+ \d+ (?:assets|modules)|\./\S+ {size}(?: \(asset\) {size} \(javascript\))?(?: \[.*\])*))$"
		))
		.unwrap()
	})
}

#[cfg(test)]
#[path = "webpack_tests.rs"]
mod webpack_tests;
