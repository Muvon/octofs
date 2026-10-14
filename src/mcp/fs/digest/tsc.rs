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

//! TypeScript emitted-file echoes followed by a complete extended diagnostics table.

use super::Selection;

const METRICS: &[&str] = &[
	"Files",
	"Lines of Library",
	"Lines of Definitions",
	"Lines of TypeScript",
	"Lines of JavaScript",
	"Lines of JSON",
	"Lines of Other",
	"Identifiers",
	"Symbols",
	"Types",
	"Instantiations",
	"Memory used",
	"Assignability cache size",
	"Identity cache size",
	"Subtype cache size",
	"Strict subtype cache size",
	"I/O Read time",
	"Parse time",
	"ResolveModule time",
	"ResolveLibrary time",
	"Program time",
	"Bind time",
	"Check time",
	"transformTime time",
	"Source Map time",
	"commentTime time",
	"I/O Write time",
	"printTime time",
	"Emit time",
	"Total time",
];

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if !stderr.is_empty() {
		return None;
	}
	let emitted = stdout
		.iter()
		.take_while(|line| {
			line.strip_prefix("TSFILE: ")
				.is_some_and(|path| !path.is_empty())
		})
		.count();
	if emitted == 0 || stdout.len() != emitted + METRICS.len() {
		return None;
	}
	for (line, expected) in stdout[emitted..].iter().zip(METRICS) {
		let (metric, value) = line.split_once(':')?;
		if metric != *expected {
			return None;
		}
		let value = value.trim();
		if metric.ends_with(" time") {
			let seconds = value.strip_suffix('s')?.parse::<f64>().ok()?;
			if !seconds.is_finite() || seconds.is_sign_negative() {
				return None;
			}
		} else {
			let value = if metric == "Memory used" {
				value.strip_suffix('K')?
			} else {
				value
			};
			value.parse::<usize>().ok()?;
		}
	}
	Some(Selection {
		stdout: (emitted..stdout.len()).collect(),
		stderr: Vec::new(),
	})
}

#[cfg(test)]
#[path = "tsc_tests.rs"]
mod tsc_tests;
