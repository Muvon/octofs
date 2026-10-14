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

//! Cargo progress, rustc diagnostics, and complete libtest suites.

use super::{diagnostic_start, important, Selection};

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if stderr.is_empty() {
		return merged(stdout);
	}
	select_streams(stdout, stderr)
}

fn select_streams(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let tests = libtest(stdout)?;
	let stderr = cargo(stderr, &tests.suites)?;
	if tests.suites.is_empty() && !stderr.recognized {
		return None;
	}
	Some(Selection {
		stdout: tests.kept,
		stderr: stderr.kept,
	})
}

fn merged(lines: &[&str]) -> Option<Selection> {
	let mut test_lines = Vec::new();
	let mut test_indices = Vec::new();
	let mut cargo_lines = Vec::new();
	let mut cargo_indices = Vec::new();
	let mut in_failures = false;
	let mut i = 0;
	while i < lines.len() {
		let line = lines[i];
		if in_failures
			|| line == "failures:"
			|| running(line).is_some()
			|| line.starts_with("test ")
			|| line.starts_with("test result:")
		{
			if line == "failures:" {
				in_failures = true;
			} else if line.starts_with("test result:") {
				in_failures = false;
			}
			test_lines.push(line);
			test_indices.push(i);
			i += 1;
		} else {
			let end = match classify(line)? {
				CargoLine::Diagnostic => diagnostic_end(lines, i),
				_ => i + 1,
			};
			cargo_lines.extend_from_slice(&lines[i..end]);
			cargo_indices.extend(i..end);
			i = end;
		}
	}
	let selection = select_streams(&test_lines, &cargo_lines)?;
	let mut kept: Vec<_> = selection
		.stdout
		.iter()
		.map(|&i| test_indices[i])
		.chain(selection.stderr.iter().map(|&i| cargo_indices[i]))
		.collect();
	kept.sort_unstable();
	Some(Selection {
		stdout: kept,
		stderr: Vec::new(),
	})
}

struct Tests {
	kept: Vec<usize>,
	suites: Vec<bool>,
}

struct Suite<'a> {
	start: usize,
	expected: usize,
	passed: usize,
	ignored: usize,
	failed: Vec<&'a str>,
	failure_start: Option<usize>,
}

fn libtest(lines: &[&str]) -> Option<Tests> {
	let mut kept = vec![false; lines.len()];
	let mut active: Option<Suite<'_>> = None;
	let mut suites = Vec::new();
	for (i, &line) in lines.iter().enumerate() {
		if line.starts_with("test result:") {
			let suite = active.take()?;
			let (passed, failures, ignored) = summary(line)?;
			if (passed, failures, ignored) != (suite.passed, suite.failed.len(), suite.ignored)
				|| passed.checked_add(failures)?.checked_add(ignored)? != suite.expected
			{
				return None;
			}
			if failures > 0 {
				let start = suite.failure_start?;
				let block = &lines[start..i];
				let list = block.iter().rposition(|line| *line == "failures:")?;
				if list == 0
					|| suite.failed.iter().any(|name| {
						!block.contains(&format!("---- {name} stdout ----").as_str())
							|| !block[list + 1..].iter().any(|line| line.trim() == *name)
					}) {
					return None;
				}
				kept[suite.start] = true;
			} else if suite.failure_start.is_some() {
				return None;
			}
			kept[i] = true;
			suites.push(failures > 0);
			continue;
		}
		if active
			.as_ref()
			.is_some_and(|suite| suite.failure_start.is_some())
		{
			// Failure sections may contain arbitrary captured stdout and blank
			// lines. Only the validated suite summary terminates the section.
			kept[i] = true;
			continue;
		}
		if line.is_empty() {
			continue;
		}
		if let Some(count) = running(line) {
			if active.is_some() {
				return None;
			}
			active = Some(Suite {
				start: i,
				expected: count,
				passed: 0,
				ignored: 0,
				failed: Vec::new(),
				failure_start: None,
			});
			continue;
		}
		let suite = active.as_mut()?;
		if line == "failures:" && !suite.failed.is_empty() {
			suite.failure_start = Some(i);
			kept[i] = true;
			continue;
		}
		let (name, verdict) = line.strip_prefix("test ")?.rsplit_once(" ... ")?;
		if name.is_empty() {
			return None;
		}
		match verdict {
			"ok" => suite.passed += 1,
			"FAILED" => suite.failed.push(name),
			"ignored" => suite.ignored += 1,
			verdict if verdict.starts_with("ignored, ") => suite.ignored += 1,
			_ => return None,
		}
		kept[i] = important(line);
	}
	if active.is_some() {
		return None;
	}
	Some(Tests {
		kept: indices(&kept),
		suites,
	})
}

fn running(line: &str) -> Option<usize> {
	let rest = line.strip_prefix("running ")?;
	let (count, unit) = rest.split_once(' ')?;
	if unit != "tests" && unit != "test" {
		return None;
	}
	count.parse().ok()
}

fn summary(line: &str) -> Option<(usize, usize, usize)> {
	let (verdict, rest) = line.strip_prefix("test result: ")?.split_once(". ")?;
	let parts: Vec<&str> = rest.split("; ").collect();
	let [passed, failed, ignored, measured, filtered, elapsed] = parts.as_slice() else {
		return None;
	};
	let passed = count(passed, " passed")?;
	let failed = count(failed, " failed")?;
	let ignored = count(ignored, " ignored")?;
	let elapsed = elapsed
		.strip_prefix("finished in ")?
		.strip_suffix('s')?
		.parse::<f64>()
		.ok()?;
	if count(measured, " measured")? != 0
		|| count(filtered, " filtered out").is_none()
		|| !elapsed.is_finite()
		|| elapsed.is_sign_negative()
		|| (verdict != "ok" && verdict != "FAILED")
		|| (verdict == "FAILED") != (failed > 0)
	{
		return None;
	}
	Some((passed, failed, ignored))
}

fn count(text: &str, suffix: &str) -> Option<usize> {
	text.strip_suffix(suffix)?.parse().ok()
}

struct Cargo {
	kept: Vec<usize>,
	recognized: bool,
}

fn cargo(lines: &[&str], suites: &[bool]) -> Option<Cargo> {
	let mut kept = vec![false; lines.len()];
	let mut terminal = false;
	let mut progress_pending = false;
	let mut headers = 0;
	let mut i = 0;
	while i < lines.len() {
		match classify(lines[i])? {
			CargoLine::Blank => {}
			CargoLine::Diagnostic => {
				let end = diagnostic_end(lines, i);
				for line in &lines[i..end] {
					let line = line.trim_start();
					if progress(line) {
						progress_pending = true;
					}
					if finished(line) || compile_failed(line) {
						terminal = true;
						progress_pending = false;
					}
					if test_header(line) {
						headers += 1;
					}
					if test_failed(line) && !suites.contains(&true) {
						return None;
					}
				}
				kept[i..end].fill(true);
				i = end;
				continue;
			}
			CargoLine::Finished => {
				terminal = true;
				progress_pending = false;
				kept[i] = true;
			}
			CargoLine::Progress => {
				progress_pending = true;
				kept[i] = important(lines[i]);
			}
			CargoLine::TestHeader => {
				let failed = *suites.get(headers)?;
				headers += 1;
				kept[i] = failed || important(lines[i]);
			}
			CargoLine::Explanation => kept[i] = true,
		}
		i += 1;
	}
	if progress_pending
		|| (headers != 0 && headers != suites.len())
		|| (!lines.is_empty() && !terminal && suites.is_empty())
	{
		return None;
	}
	if let Some(last) = lines.iter().rev().find(|line| !line.is_empty()) {
		let last = last.trim_start();
		if !finished(last) && !compile_failed(last) && !test_failed(last) && !test_header(last) {
			return None;
		}
	}
	Some(Cargo {
		kept: indices(&kept),
		recognized: terminal,
	})
}

enum CargoLine {
	Blank,
	Diagnostic,
	Finished,
	Progress,
	TestHeader,
	Explanation,
}

fn classify(line: &str) -> Option<CargoLine> {
	let line = line.trim_start();
	if line.is_empty() {
		Some(CargoLine::Blank)
	} else if diagnostic_start(line) {
		Some(CargoLine::Diagnostic)
	} else if finished(line) {
		Some(CargoLine::Finished)
	} else if progress(line) {
		Some(CargoLine::Progress)
	} else if test_header(line) {
		Some(CargoLine::TestHeader)
	} else if line.starts_with("For more information about this error, try ")
		|| line.starts_with("Some errors have detailed explanations:")
	{
		Some(CargoLine::Explanation)
	} else {
		None
	}
}

fn diagnostic_end(lines: &[&str], start: usize) -> usize {
	let mut end = start + 1;
	while end < lines.len() && !lines[end].is_empty() {
		end += 1;
	}
	if end < lines.len() {
		end += 1;
	}
	// Cargo's anyhow cause chains can span paragraphs. Their context and
	// indented causes belong to the diagnostic, while Cargo control lines do not.
	loop {
		let mut next = end;
		while next < lines.len() && lines[next].is_empty() {
			next += 1;
		}
		if next == lines.len()
			|| !(lines[next].starts_with("Caused by:") || lines[next].starts_with("failed to "))
		{
			return end;
		}
		end = next + 1;
		while end < lines.len() && !lines[end].is_empty() {
			if matches!(
				classify(lines[end]),
				Some(CargoLine::Finished | CargoLine::Progress | CargoLine::TestHeader)
			) {
				return end;
			}
			end += 1;
		}
		if end < lines.len() {
			end += 1;
		}
	}
}

fn finished(line: &str) -> bool {
	line.starts_with("Finished ")
		&& line.contains(" profile ")
		&& line.contains(" target(s) in ")
		&& line.ends_with('s')
}

fn compile_failed(line: &str) -> bool {
	line.starts_with("error: could not compile ")
		&& line.contains(" due to ")
		&& (line.ends_with("previous error")
			|| line.ends_with("previous errors")
			|| line.ends_with("warning emitted")
			|| line.ends_with("warnings emitted"))
}

fn test_failed(line: &str) -> bool {
	(line.starts_with("error: test failed, to rerun pass ")
		|| line.starts_with("error: doctest failed, to rerun pass "))
		&& line.ends_with('`')
}

fn test_header(line: &str) -> bool {
	if let Some(name) = line.strip_prefix("Doc-tests ") {
		return !name.is_empty() && !name.contains(char::is_whitespace);
	}
	line.strip_prefix("Running ")
		.is_some_and(|target| target.contains(" (") && target.ends_with(')'))
}

fn progress(line: &str) -> bool {
	[
		"Compiling ",
		"Checking ",
		"Fresh ",
		"Downloading ",
		"Downloaded ",
		"Updating ",
		"Locking ",
		"Adding ",
		"Blocking waiting ",
	]
	.iter()
	.any(|prefix| {
		line.strip_prefix(prefix)
			.is_some_and(|rest| !rest.is_empty())
	})
}

fn indices(kept: &[bool]) -> Vec<usize> {
	kept.iter()
		.enumerate()
		.filter_map(|(i, &keep)| keep.then_some(i))
		.collect()
}

#[cfg(test)]
#[path = "cargo_tests.rs"]
mod cargo_tests;
