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

//! Go test control lines, package completion, logs, and panic dumps.

use super::{important, Selection};
use std::collections::BTreeSet;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let mut kept = Vec::new();
	let mut running = BTreeSet::new();
	let mut failed = false;
	let mut packages = 0;
	let mut terminal = false;
	let mut panic = false;
	let mut diagnostic = false;
	let mut verbose = false;
	let mut package_failed = false;
	let mut outcome = None;
	let mut last = None;
	for (i, &line) in stdout.iter().enumerate() {
		let line = if let Some(count) = line.strip_prefix("[... last line repeated ") {
			let count = count.strip_suffix(" more times]")?.parse::<usize>().ok()?;
			let previous = *stdout.get(i.checked_sub(1)?)?;
			if count < 2 || previous.is_empty() || previous.starts_with("[... last line repeated ")
			{
				return None;
			}
			previous
		} else {
			line
		};
		let trimmed = line.trim_start();
		if line.is_empty() {
			if panic || diagnostic || !running.is_empty() {
				kept.push(i);
			}
			continue;
		}
		last = Some(line);
		if let Some((status, _)) = package(line) {
			if !running.is_empty() && !panic {
				return None;
			}
			if status != "FAIL" && (package_failed || panic) {
				return None;
			}
			if outcome.is_some_and(|failed| failed != (status == "FAIL")) {
				return None;
			}
			running.clear();
			if status == "FAIL" {
				failed = true;
			}
			packages += 1;
			terminal = true;
			panic = false;
			diagnostic = false;
			verbose = false;
			package_failed = false;
			outcome = None;
			kept.push(i);
		} else if line == "FAIL" || line == "PASS" {
			if !running.is_empty() {
				return None;
			}
			if line == "FAIL" {
				failed = true;
			}
			if !terminal {
				outcome = Some(line == "FAIL");
			} else if line == "PASS" {
				return None;
			}
			kept.push(i);
		} else if let Some(name) = line.strip_prefix("=== RUN   ") {
			if name.is_empty() || !running.insert(name) {
				return None;
			}
			terminal = false;
			verbose = true;
			if important(line) {
				kept.push(i);
			}
		} else if let Some(name) = line
			.strip_prefix("=== PAUSE ")
			.or_else(|| line.strip_prefix("=== CONT  "))
		{
			if !running.contains(name) {
				return None;
			}
			if important(line) {
				kept.push(i);
			}
		} else if let Some((verdict, rest)) = trimmed
			.strip_prefix("--- ")
			.and_then(|rest| rest.split_once(": "))
			.filter(|(verdict, _)| matches!(*verdict, "PASS" | "FAIL" | "SKIP"))
		{
			let (name, elapsed) = rest.rsplit_once(" (")?;
			let seconds = elapsed.strip_suffix("s)")?.parse::<f64>().ok()?;
			if name.is_empty() || !seconds.is_finite() || seconds.is_sign_negative() {
				return None;
			}
			if verbose && !running.remove(name) {
				return None;
			}
			package_failed |= verdict == "FAIL";
			terminal = false;
			if verdict != "PASS" || important(line) {
				kept.push(i);
			}
		} else if line.starts_with("panic: ") {
			panic = true;
			terminal = false;
			kept.push(i);
		} else if line.starts_with("# ") {
			diagnostic = true;
			terminal = false;
			kept.push(i);
		} else if panic
			|| (diagnostic && compiler_line(line))
			|| line.starts_with("    ")
			|| !running.is_empty()
		{
			// t.Log precedes --- FAIL in verbose mode, so retain logs before
			// the verdict too. Panic messages and stacks span blank lines.
			terminal = false;
			kept.push(i);
		} else {
			return None;
		}
	}
	if packages == 0 || !terminal || !running.is_empty() {
		return None;
	}
	if failed && last != Some("FAIL") {
		return None;
	}
	let stderr = compiler(stderr)?;
	Some(Selection {
		stdout: kept,
		stderr,
	})
}

fn package(line: &str) -> Option<(&str, &str)> {
	let mut fields = line.split_whitespace();
	let status = fields.next()?;
	if !matches!(status, "ok" | "FAIL" | "?") {
		return None;
	}
	let name = fields.next()?;
	let rest = fields.collect::<Vec<_>>().join(" ");
	let valid = match status {
		"ok" => rest == "(cached)" || duration(&rest),
		"FAIL" => rest == "[build failed]" || rest == "[setup failed]" || duration(&rest),
		"?" => rest == "[no test files]",
		_ => false,
	};
	valid.then_some((status, name))
}

fn duration(text: &str) -> bool {
	text.strip_suffix('s')
		.and_then(|n| n.parse::<f64>().ok())
		.is_some_and(|n| n.is_finite() && !n.is_sign_negative())
}

fn compiler_line(line: &str) -> bool {
	let Some((path, rest)) = line.split_once(':') else {
		return false;
	};
	let Some((number, message)) = rest.split_once(':') else {
		return false;
	};
	path.ends_with(".go") && number.parse::<usize>().is_ok() && !message.is_empty()
}

fn compiler(lines: &[&str]) -> Option<Vec<usize>> {
	let mut header = false;
	let mut body = false;
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
		if line.starts_with("# ") {
			if header && !body && !line.starts_with("# [") {
				return None;
			}
			header = true;
			body = false;
		} else if header && (compiler_line(line) || line.starts_with('\t')) {
			body = true;
		} else {
			return None;
		}
	}
	if header && !body {
		return None;
	}
	Some((0..lines.len()).collect())
}
#[cfg(test)]
#[path = "go_test_tests.rs"]
mod go_test_tests;
