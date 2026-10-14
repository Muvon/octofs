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

//! Complete pip installs, with download noise separated from diagnostics.

use super::{important, Selection};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let mut install = Install::default();
	let stdout = install.stream(stdout)?;
	let stderr = install.stream(stderr)?;
	let complete = install.finished
		|| install.failed
		|| (install.satisfied > 0
			&& install.collected == 0
			&& install.packages.is_none()
			&& install.notice);
	if install.conflict != install.conflict_error
		|| (install.finished && install.failed)
		|| !complete
		|| (install.collected == 0 && install.satisfied == 0)
	{
		return None;
	}
	Some(Selection { stdout, stderr })
}

#[derive(Default)]
struct Install {
	collected: usize,
	satisfied: usize,
	packages: Option<BTreeSet<String>>,
	finished: bool,
	failed: bool,
	conflict: bool,
	conflict_error: bool,
	notice: bool,
}

impl Install {
	fn stream(&mut self, lines: &[&str]) -> Option<Vec<usize>> {
		let mut kept = Vec::new();
		let mut build = None;
		let mut i = 0;
		while i < lines.len() {
			let line = lines[i];
			if line.is_empty() {
				kept.push(i);
			} else if let Some(versions) = line.strip_prefix("[notice] A new release of pip is available: ") {
				let (current, available) = versions.split_once(" -> ")?;
				for version in [current, available] {
					if version.split('.').any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit())) {
						return None;
					}
				}
				if lines.get(i + 1)?
					!= &"[notice] To update, run: python3 -m pip install --upgrade pip"
				{
					return None;
				}
				self.notice = true;
				i += 2;
				continue;
			} else if let Some(packages) = line.strip_prefix("Installing collected packages: ") {
				if self.packages.is_some() || self.finished || self.failed {
					return None;
				}
				self.packages = Some(package_list(packages)?);
			} else if let Some(packages) = line.strip_prefix("Successfully installed ") {
				if self.finished || self.failed {
					return None;
				}
				let expected = self.packages.as_ref()?;
				let mut installed = BTreeSet::new();
				for package in packages.split_whitespace() {
					let (name, version) = package.rsplit_once('-')?;
					if !version.starts_with(|c: char| c.is_ascii_digit())
						|| !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '!' | '_'))
						|| !installed.insert(normalize(name)?)
					{
						return None;
					}
				}
				if &installed != expected {
					return None;
				}
				self.finished = true;
				kept.push(i);
			} else if line.starts_with("ERROR: Cannot install ")
				&& line.ends_with("because these package versions have conflicting dependencies.")
			{
				self.conflict_error = true;
				kept.push(i);
			} else if line == "ERROR: ResolutionImpossible: for help visit https://pip.pypa.io/en/latest/topics/dependency-resolution/#dealing-with-dependency-conflicts" {
				if !self.conflict {
					return None;
				}
				self.failed = true;
				kept.push(i);
			} else if let Some(requirement) = line.strip_prefix("ERROR: No matching distribution found for ") {
				if requirement.is_empty() || requirement.chars().any(char::is_whitespace) {
					return None;
				}
				self.failed = true;
				kept.push(i);
			} else if line
				.starts_with("ERROR: Could not find a version that satisfies the requirement ")
				&& line.contains(" (from versions: ")
				&& line.ends_with(')')
			{
				kept.push(i);
			} else if line == "The conflict is caused by:" {
				let end = conflict_end(lines, i)?;
				self.conflict = true;
				kept.extend(i..end);
				i = end;
				continue;
			} else if line.trim_start() == "error: subprocess-exited-with-error" {
				let end = subprocess_end(lines, i)?;
				self.failed = true;
				kept.extend(i..end);
				i = end;
				continue;
			} else if line.starts_with("WARNING: ") && line.len() > "WARNING: ".len() {
				kept.push(i);
			} else if let Some((phase, status)) = build_status(line) {
				match status {
					"started" if build.is_none() => build = Some((phase, i)),
					"finished with status 'done'" | "finished with status 'error'" => {
						let (started, start) = build.take()?;
						if started != phase {
							return None;
						}
						if status == "finished with status 'error'" {
							kept.push(start);
						}
					}
					_ => return None,
				}
				if important(line) {
					kept.push(i);
				}
			} else if noise().is_match(line) {
				if self.finished || self.failed || self.packages.is_some() {
					return None;
				}
				if line.starts_with("Collecting ") {
					self.collected += 1;
				} else if line.starts_with("Requirement already satisfied: ") {
					self.satisfied += 1;
				}
				if important(line) {
					kept.push(i);
				}
			} else {
				return None;
			}
			i += 1;
		}
		if build.is_some() {
			return None;
		}
		kept.sort_unstable();
		Some(kept)
	}
}

fn build_status(line: &str) -> Option<(&str, &str)> {
	let (phase, status) = line.strip_prefix("  ")?.split_once(": ")?;
	matches!(
		phase,
		"Installing build dependencies"
			| "Getting requirements to build wheel"
			| "Preparing metadata (pyproject.toml)"
	)
	.then_some((phase, status))
}

fn package_list(text: &str) -> Option<BTreeSet<String>> {
	let mut names = BTreeSet::new();
	for name in text.split(", ") {
		if !names.insert(normalize(name)?) {
			return None;
		}
	}
	Some(names)
}

fn normalize(name: &str) -> Option<String> {
	if name.is_empty()
		|| !name
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
	{
		return None;
	}
	Some(name.to_ascii_lowercase().replace(['_', '.'], "-"))
}

fn noise() -> &'static Regex {
	static NOISE: OnceLock<Regex> = OnceLock::new();
	NOISE.get_or_init(|| {
		Regex::new(concat!(
			r"^(?:Collecting [^\s]+(?: \(from .+\))?",
			r"|Requirement already satisfied: [^\s]+ in [^\s]+(?: \(from .+\))? \([^()]+\)",
			r"| *(?:Downloading|Using cached) [^\s]+ \([0-9.]+ [kMGT]?B\)",
			r"| +[━╸]+ +[0-9.]+/[0-9.]+ [kMGT]?B +[0-9.]+ [kMGT]?B/s eta [0-9]+:[0-9]{2}:[0-9]{2}",
			r"|INFO: pip is looking at multiple versions of [^\s]+ to determine which version is compatible with other requirements\. This could take a while\.)$",
		)).expect("valid pip progress grammar")
	})
}

fn conflict_end(lines: &[&str], start: usize) -> Option<usize> {
	let mut end = start + 1;
	while lines.get(end)?.starts_with("    ") && !lines[end].trim().is_empty() {
		end += 1;
	}
	if end == start + 1 || lines.get(end)? != &"" {
		return None;
	}
	let suggestions = [
		"To fix this you could try to:",
		"1. loosen the range of package versions you've specified",
		"2. remove package versions to allow pip attempt to solve the dependency conflict",
	];
	end += 1;
	if lines.get(end..end + suggestions.len())? != suggestions.as_slice() {
		return None;
	}
	Some(end + suggestions.len())
}

fn subprocess_end(lines: &[&str], start: usize) -> Option<usize> {
	let indent = lines[start].strip_suffix("error: subprocess-exited-with-error")?;
	let mut i = start + 1;
	if !lines.get(i)?.trim().is_empty() {
		return None;
	}
	i += 1;
	if !lines.get(i)?.trim_start().starts_with("× ")
		|| !lines[i].ends_with(" did not run successfully.")
	{
		return None;
	}
	i += 1;
	let code = lines
		.get(i)?
		.trim_start()
		.strip_prefix("│ exit code: ")?
		.parse::<usize>()
		.ok()?;
	if code == 0 {
		return None;
	}
	i += 1;
	let output = lines.get(i)?.trim_start().strip_prefix("╰─> ")?;
	i += 1;
	if output != "See above for output." {
		let count = output
			.strip_prefix('[')?
			.strip_suffix(" lines of output]")?
			.parse::<usize>()
			.ok()?;
		let end = i.checked_add(count)?;
		let body_indent = format!("{indent}    ");
		if !lines
			.get(i..end)?
			.iter()
			.all(|line| line.is_empty() || line.starts_with(&body_indent))
			|| lines.get(end)?.trim() != "[end of output]"
		{
			return None;
		}
		i = end + 1;
	}
	if !lines.get(i)?.trim().is_empty() {
		return None;
	}
	i += 1;
	if lines.get(i)?.trim_start()
		!= "note: This error originates from a subprocess, and is likely not a problem with pip."
	{
		return None;
	}
	Some(i + 1)
}

#[cfg(test)]
#[path = "pip_tests.rs"]
mod pip_tests;
