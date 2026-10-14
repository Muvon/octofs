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

//! Verified pairs of Go module download requests and successful responses.

use super::{important, Selection};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if stderr.is_empty() {
		Some(Selection {
			stdout: module_output(stdout)?,
			stderr: Vec::new(),
		})
	} else if stdout.is_empty() {
		Some(Selection {
			stdout: Vec::new(),
			stderr: module_output(stderr)?,
		})
	} else {
		None
	}
}

fn module_output(lines: &[&str]) -> Option<Vec<usize>> {
	if !lines
		.iter()
		.find(|line| !line.is_empty())?
		.starts_with("# get ")
	{
		return None;
	}
	let mut pending = BTreeSet::new();
	let mut kept = Vec::new();
	for (i, &line) in lines.iter().enumerate() {
		if line.is_empty() {
			continue;
		}
		let fetch = fetch_grammar().captures(line)?;
		let url = fetch.get(1)?.as_str();
		if fetch.get(2).is_some() {
			if !pending.remove(url) {
				return None;
			}
		} else if !pending.insert(url) {
			return None;
		}
		if important(line) {
			kept.push(i);
		}
	}
	pending.is_empty().then_some(kept)
}

fn fetch_grammar() -> &'static Regex {
	static FETCH: OnceLock<Regex> = OnceLock::new();
	FETCH.get_or_init(|| {
		Regex::new(r"^# get (https?://[^\s]+?)(: 200 OK \([0-9]+(?:\.[0-9]+)?s\))?$")
			.expect("valid Go fetch grammar")
	})
}

#[cfg(test)]
#[path = "go_mod_tests.rs"]
mod go_mod_tests;
