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

//! Completed pnpm installs, preserving warnings and lifecycle-script output.

use super::{important, Selection};
use regex::Regex;
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if !stderr.is_empty() {
		return None;
	}
	let mut kept = Vec::new();
	let mut packages = None;
	let mut added = None;
	let mut progress_done = false;
	let mut finished = false;
	let mut script = None;
	let mut section = None;
	let mut entries = 0;
	let mut i = 0;
	while i < stdout.len() {
		let line = stdout[i];
		if line.is_empty() {
			if script.is_some() {
				kept.push(i);
			}
			i += 1;
			continue;
		}
		if finished {
			return None;
		}
		if let Some(active) = script {
			let output = line.strip_prefix(active)?.strip_prefix(": ")?;
			kept.push(i);
			if output == "Done" {
				script = None;
			}
		} else if let Some(captures) = progress().captures(line) {
			if progress_done || section.is_some() {
				return None;
			}
			let resolved = captures.get(1)?.as_str().parse::<usize>().ok()?;
			let reused = captures.get(2)?.as_str().parse::<usize>().ok()?;
			let downloaded = captures.get(3)?.as_str().parse::<usize>().ok()?;
			let current = captures.get(4)?.as_str().parse::<usize>().ok()?;
			if reused.checked_add(downloaded)? > resolved
				|| current > reused.checked_add(downloaded)?
			{
				return None;
			}
			added = Some(current);
			progress_done = captures.get(5).is_some();
			if progress_done && Some(current) != packages {
				return None;
			}
		} else if let Some(count) = line.strip_prefix("Packages: +") {
			if packages.is_some() || section.is_some() || progress_done {
				return None;
			}
			packages = Some(count.parse::<usize>().ok()?);
			kept.push(i);
		} else if line.chars().all(|c| c == '+') {
			if packages.is_none() || progress_done || section.is_some() {
				return None;
			}
		} else if line.starts_with(" WARN  ") && line.len() > " WARN  ".len() {
			kept.push(i);
		} else if line == "Lockfile is up to date, resolution step is skipped" {
			if i != 0 {
				return None;
			}
		} else if line.starts_with("   ╭") {
			let end = banner_end(stdout, i)?;
			i = end;
			continue;
		} else if line.starts_with(".../") {
			if !progress_done || section.is_some() {
				return None;
			}
			let (prefix, command) = line.split_once("$ ")?;
			let (path, phase) = prefix.rsplit_once(' ')?;
			if !path.contains("/node_modules/")
				|| !matches!(phase, "install" | "postinstall" | "preinstall")
				|| command.is_empty()
			{
				return None;
			}
			script = Some(prefix);
			kept.push(i);
		} else if matches!(
			line,
			"dependencies:" | "devDependencies:" | "optionalDependencies:"
		) {
			if !progress_done || section == Some(line) {
				return None;
			}
			section = Some(line);
			kept.push(i);
		} else if line.starts_with("+ ") {
			if section.is_none() || !dependency().is_match(line) {
				return None;
			}
			entries += 1;
			if line.ends_with(" deprecated") || important(line) {
				kept.push(i);
			}
		} else {
			let elapsed = line.strip_prefix("Done in ")?;
			let elapsed = if let Some(seconds) = elapsed.strip_suffix("ms") {
				seconds
			} else {
				elapsed.strip_suffix('s')?
			};
			let elapsed = elapsed.parse::<f64>().ok()?;
			if !elapsed.is_finite()
				|| elapsed.is_sign_negative()
				|| !progress_done
				|| added != packages
				|| entries == 0
			{
				return None;
			}
			finished = true;
			kept.push(i);
		}
		i += 1;
	}
	finished.then_some(Selection {
		stdout: kept,
		stderr: Vec::new(),
	})
}

fn progress() -> &'static Regex {
	static PROGRESS: OnceLock<Regex> = OnceLock::new();
	PROGRESS.get_or_init(|| {
		Regex::new(
			r"^Progress: resolved ([0-9]+), reused ([0-9]+), downloaded ([0-9]+), added ([0-9]+)(, done)?$",
		)
		.expect("valid pnpm progress grammar")
	})
}

fn dependency() -> &'static Regex {
	static DEPENDENCY: OnceLock<Regex> = OnceLock::new();
	DEPENDENCY.get_or_init(|| {
		Regex::new(r"^\+ (?:@[A-Za-z0-9_.-]+/)?[A-Za-z0-9_.-]+ [0-9][A-Za-z0-9.+-]*(?: \([0-9][A-Za-z0-9.+-]* is available\))?(?: deprecated)?$")
			.expect("valid pnpm dependency grammar")
	})
}

fn banner_end(lines: &[&str], start: usize) -> Option<usize> {
	let top = lines[start].strip_prefix("   ╭")?.strip_suffix('╮')?;
	if top.is_empty() || !top.chars().all(|c| c == '─') {
		return None;
	}
	let mut update = false;
	let mut i = start + 1;
	while i < lines.len() {
		if lines[i] == format!("   ╰{top}╯") {
			return update.then_some(i + 1);
		}
		let text = lines[i].strip_prefix("   │")?.strip_suffix('│')?.trim();
		if text.starts_with("Update available! ") && text.ends_with('.') && text.contains('→') {
			update = true;
		} else if !text.is_empty()
			&& !text.starts_with("Changelog: https://github.com/pnpm/pnpm/releases/tag/")
			&& text != "Run \"pnpm add -g pnpm\" to update."
			&& text != "Follow @pnpmjs for updates: https://twitter.com/pnpmjs"
		{
			return None;
		}
		i += 1;
	}
	None
}

#[cfg(test)]
#[path = "pnpm_tests.rs"]
mod pnpm_tests;
