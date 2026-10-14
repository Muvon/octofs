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

//! Complete Vite production builds with asset tables and chunk warnings.

use super::Selection;
use regex::Regex;
use std::sync::OnceLock;

#[derive(PartialEq)]
enum Stage {
	Banner,
	Transforming,
	Modules,
	Rendering,
	Gzip,
	Assets,
}

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	// Only successful builds have droppable asset tables. Failure captures are
	// already small, and their plugin diagnostics may contain arbitrary text.
	let lines: Vec<_> = stdout.iter().chain(stderr).copied().collect();
	let mut kept = Vec::new();
	let mut stage = Stage::Banner;
	let mut assets = 0;
	let mut finished = false;
	let mut warning = false;
	let mut i = 0;
	while i < lines.len() {
		let line = lines[i];
		if line.is_empty() {
			i += 1;
			continue;
		}
		if line.starts_with("(!) Some chunks are larger than ") {
			if warning || stage != Stage::Assets {
				return None;
			}
			let limit = line
				.strip_prefix("(!) Some chunks are larger than ")?
				.strip_suffix(" kB after minification. Consider:")?
				.parse::<usize>()
				.ok()?;
			if limit == 0
				|| lines.get(i + 1)
					!= Some(&"- Using dynamic import() to code-split the application")
				|| !lines.get(i + 2)?.starts_with(
					"- Use build.rollupOptions.output.manualChunks to improve chunking: https://",
				) || lines.get(i + 3)
				!= Some(
					&"- Adjust chunk size limit for this warning via build.chunkSizeWarningLimit.",
				) {
				return None;
			}
			kept.extend(i..i + 4);
			warning = true;
			i += 4;
			continue;
		}
		if finished {
			return None;
		}
		match stage {
			Stage::Banner if banner().is_match(line) => stage = Stage::Transforming,
			Stage::Transforming if line == "transforming..." => stage = Stage::Modules,
			Stage::Modules if transformed().is_match(line) => stage = Stage::Rendering,
			Stage::Rendering if line == "rendering chunks..." => stage = Stage::Gzip,
			Stage::Gzip if line == "computing gzip size..." => stage = Stage::Assets,
			Stage::Assets if asset().is_match(line) && !warning => assets += 1,
			Stage::Assets if assets > 0 && built().is_match(line) => {
				kept.push(i);
				finished = true;
			}
			_ => return None,
		}
		i += 1;
	}
	if !finished {
		return None;
	}
	Some(Selection {
		stdout: kept.iter().copied().filter(|&i| i < stdout.len()).collect(),
		stderr: kept
			.into_iter()
			.filter(|&i| i >= stdout.len())
			.map(|i| i - stdout.len())
			.collect(),
	})
}

fn banner() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^vite v\d+\.\d+\.\d+ building for production\.\.\.$").unwrap())
}

fn transformed() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^✓ \d+ modules transformed\.$").unwrap())
}

fn asset() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^\S+\s+\d+\.\d+ kB │ gzip:\s+\d+\.\d+ kB$").unwrap())
}

fn built() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^✓ built in \d+(?:\.\d+)?(?:ms|s)$").unwrap())
}

#[cfg(test)]
#[path = "vite_tests.rs"]
mod vite_tests;
