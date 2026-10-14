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

//! GNU make, GCC diagnostics, and CMake's Makefiles build progress.

use super::Selection;
use regex::Regex;
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let lines: Vec<_> = stdout.iter().chain(stderr).copied().collect();
	let mut kept = Vec::new();
	let mut terminal = false;
	let mut recognized = false;
	let mut diagnostic = false;
	for (i, &line) in lines.iter().enumerate() {
		if line.is_empty() {
			continue;
		}
		if directory().is_match(line) {
			continue;
		}
		if let Some(progress) = cmake().captures(line) {
			let percent = progress[1].parse::<usize>().ok()?;
			if percent > 100 {
				return None;
			}
			recognized = true;
			diagnostic = false;
			terminal = percent == 100 && progress[2].starts_with("Built target ");
			if progress[2].starts_with("Built target ") {
				kept.push(i);
			}
			continue;
		}
		if command().is_match(line) || preparation().is_match(line) {
			terminal = false;
			diagnostic = false;
			continue;
		}
		if make_status().is_match(line) {
			recognized = true;
			terminal = (line.starts_with("make: ") || line.starts_with("gmake: "))
				&& !line.ends_with("Waiting for unfinished jobs....");
			diagnostic = false;
			kept.push(i);
			continue;
		}
		if diagnostic_header().is_match(line) || linker().is_match(line) {
			diagnostic = true;
			terminal = false;
			kept.push(i);
			continue;
		}
		if diagnostic
			&& (excerpt().is_match(line)
				|| include().is_match(line)
				|| line == "compilation terminated.")
		{
			kept.push(i);
			continue;
		}
		return None;
	}
	if !recognized || !terminal {
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

fn cmake() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^\[\s*(\d+)%\] ((?:Building (?:C|CXX) object|Linking (?:C|CXX) (?:static library|shared library|executable)|Built target) .+)$").unwrap()
	})
}

fn directory() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^(?:g?make)(?:\[\d+\])?: (?:Entering|Leaving) directory ['‘].+['’]$").unwrap()
	})
}

fn command() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r#"^(?:[^\s;&|<>`$'"]*/)?(?:cc|gcc|g\+\+|c\+\+) (?:[^\s;&|<>`$'"]+ )*-o [^\s;&|<>`$'"]+(?: [^\s;&|<>`$'"]+)*$"#).unwrap()
	})
}

fn preparation() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r#"^mkdir -p [^\s;&|<>`$'"]+(?: [^\s;&|<>`$'"]+)*$"#).unwrap())
}

fn make_status() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^g?make(?:\[\d+\])?: (?:\*\*\* (?:\[.+\] Error \d+|Waiting for unfinished jobs\.\.\.\.)|Target ['‘].+['’] not remade because of errors\.)$").unwrap()
	})
}

fn diagnostic_header() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^\S+:(?:(?:\d+:)+ (?:fatal error|error|warning|note): .+| (?:In function|In member function|In instantiation of|At global scope).+:)$").unwrap()
	})
}

fn excerpt() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^\s*(?:\d+ )?\|.*$").unwrap())
}

fn include() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^(?:In file included from |\s+from )\S+:\d+(?::\d+)?[,:]$").unwrap()
	})
}

fn linker() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^(?:(?:\S*/)?ld: .+|collect2: error: ld returned \d+ exit status|\S+: (?:undefined reference to .+|multiple definition of .+|first defined here))$").unwrap()
	})
}

#[cfg(test)]
#[path = "make_tests.rs"]
mod make_tests;
