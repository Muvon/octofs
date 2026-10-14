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

//! BuildKit step lifecycles and the legacy Docker builder.

use super::Selection;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub(super) fn select(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	if stdout
		.first()
		.is_some_and(|line| line.starts_with("#0 building with "))
		&& stderr.is_empty()
	{
		return Some(Selection {
			stdout: buildkit(stdout)?,
			stderr: Vec::new(),
		});
	}
	if stderr
		.first()
		.is_some_and(|line| line.starts_with("#0 building with "))
		&& stdout.is_empty()
	{
		return Some(Selection {
			stdout: Vec::new(),
			stderr: buildkit(stderr)?,
		});
	}
	legacy(stdout, stderr)
}

#[derive(PartialEq)]
enum State {
	Active,
	Done,
	Cached,
	Failed,
	Canceled,
}

struct Step<'a> {
	header: &'a str,
	indices: Vec<usize>,
	state: State,
	diagnostic: bool,
}

enum Header {
	Internal,
	Build { position: usize, total: usize },
}

fn buildkit(lines: &[&str]) -> Option<Vec<usize>> {
	let banner = lines.first()?.strip_prefix("#0 building with ")?;
	if !banner.starts_with('"') || !banner.ends_with("\" instance using docker driver") {
		return None;
	}
	let mut steps: BTreeMap<usize, Step<'_>> = BTreeMap::new();
	let mut positions = BTreeSet::new();
	let mut total = None;
	let mut kept = Vec::new();
	let mut naming = None;
	let mut final_error = false;
	let mut recap = false;
	let mut i = 1;
	while i < lines.len() {
		let line = lines[i];
		if line.is_empty() {
			i += 1;
			continue;
		}
		if final_error {
			return None;
		}
		if line == "------" {
			if recap || !steps.values().any(|step| step.state == State::Failed) {
				return None;
			}
			let end = failure_recap(lines, i, &steps)?;
			kept.extend(i..end);
			i = end;
			recap = true;
			continue;
		}
		if line.starts_with("ERROR: failed to build: failed to solve: ") {
			if !recap {
				return None;
			}
			kept.push(i);
			final_error = true;
			i += 1;
			continue;
		}
		if recap {
			return None;
		}
		if redrawn_apt().is_match(line) {
			// CR redraws can erase BuildKit's prefix. A sole active RUN is
			// the only unambiguous owner of the surviving dpkg progress frame.
			let mut running = steps.values_mut().filter(|step| {
				step.state == State::Active
					&& step
						.header
						.split_once("] ")
						.is_some_and(|(_, command)| command.starts_with("RUN "))
			});
			let step = running.next()?;
			if running.next().is_some() {
				return None;
			}
			step.indices.push(i);
			i += 1;
			continue;
		}
		let (id, body) = step_line(line)?;
		if id == 0 {
			return None;
		}
		let header = step_header(body);
		if header.is_some() || body == "exporting to image" {
			if let Some(Header::Build {
				position,
				total: maximum,
			}) = header
			{
				if total.is_some_and(|total| total != maximum) {
					return None;
				}
				total = Some(maximum);
				positions.insert(position);
			}
			if let Some(step) = steps.get_mut(&id) {
				if step.header != body || matches!(step.state, State::Failed | State::Canceled) {
					return None;
				}
				// BuildKit repeats headers when a suspended solve resumes, even
				// after an earlier DONE (e.g. another build-context transfer).
				step.state = State::Active;
				step.indices.push(i);
			} else {
				steps.insert(
					id,
					Step {
						header: body,
						indices: vec![i],
						state: State::Active,
						diagnostic: false,
					},
				);
			}
			i += 1;
			continue;
		}
		let step = steps.get_mut(&id)?;
		if step.state != State::Active {
			return None;
		}
		step.indices.push(i);
		if done().is_match(body) {
			step.state = State::Done;
		} else if body == "CACHED" {
			step.state = State::Cached;
		} else if body == "CANCELED" {
			step.state = State::Canceled;
		} else if body.starts_with("ERROR: ") {
			step.state = State::Failed;
		} else if let Some(log) = timed_log(body) {
			step.diagnostic |= diagnostic(log);
		} else if body.starts_with("naming to ") && step.header == "exporting to image" {
			if !body.ends_with(" done") {
				return None;
			}
			naming = Some(id);
			kept.push(i);
		} else if body != "..." && !metadata().is_match(body) {
			return None;
		}
		i += 1;
	}
	if steps.is_empty() || steps.values().any(|step| step.state == State::Active) {
		return None;
	}
	if final_error {
		if naming.is_some() {
			return None;
		}
	} else {
		let export = steps.get(&naming?)?;
		if export.state != State::Done
			|| positions.len() != total?
			|| steps
				.values()
				.any(|step| matches!(step.state, State::Failed | State::Canceled))
		{
			return None;
		}
	}
	for step in steps.values() {
		if matches!(step.state, State::Failed | State::Canceled) || step.diagnostic {
			// RUN logs are arbitrary programs' output. Keeping the entire step
			// preserves multiline warnings and diagnostics without guessing ends.
			kept.extend_from_slice(&step.indices);
		}
	}
	kept.sort_unstable();
	kept.dedup();
	Some(kept)
}

fn failure_recap(lines: &[&str], start: usize, steps: &BTreeMap<usize, Step<'_>>) -> Option<usize> {
	let header = lines
		.get(start + 1)?
		.strip_prefix(" > ")?
		.strip_suffix(':')?;
	if !steps
		.values()
		.any(|step| step.state == State::Failed && step.header == header)
	{
		return None;
	}
	let mut i = start + 2;
	while *lines.get(i)? != "------" {
		if !lines[i].is_empty() && timed_log(lines[i]).is_none() {
			return None;
		}
		i += 1;
	}
	i += 1;
	if !dockerfile().is_match(lines.get(i)?) || *lines.get(i + 1)? != "--------------------" {
		return None;
	}
	i += 2;
	let mut marked = false;
	while *lines.get(i)? != "--------------------" {
		if !source_line().is_match(lines[i]) {
			return None;
		}
		marked |= lines[i].contains("| >>> ");
		i += 1;
	}
	if !marked {
		return None;
	}
	Some(i + 1)
}

fn step_line(line: &str) -> Option<(usize, &str)> {
	let (id, body) = line.strip_prefix('#')?.split_once(' ')?;
	Some((id.parse().ok()?, body))
}

fn step_header(body: &str) -> Option<Header> {
	let (label, rest) = body.strip_prefix('[')?.split_once("] ")?;
	if rest.is_empty() {
		return None;
	}
	if label == "internal" {
		return Some(Header::Internal);
	}
	let (position, total) = label.split_once('/')?;
	let position = position.parse::<usize>().ok()?;
	let total = total.parse::<usize>().ok()?;
	if position == 0 || position > total {
		return None;
	}
	Some(Header::Build { position, total })
}

fn timed_log(body: &str) -> Option<&str> {
	static RE: OnceLock<Regex> = OnceLock::new();
	let captures = RE
		.get_or_init(|| Regex::new(r"^\d+\.\d+(?: (.*))?$").unwrap())
		.captures(body)?;
	// A stripped timestamp-only line is a blank RUN log frame.
	Some(captures.get(1).map_or("", |log| log.as_str()))
}

fn diagnostic(log: &str) -> bool {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(
			r"(?i)(?:^|:\s)(?:fatal error|error|warning|note|help|panic):|\bdeprecat(?:ed|ion)\b",
		)
		.unwrap()
	})
	.is_match(log.trim_start())
}

fn done() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^DONE \d+(?:\.\d+)?s$").unwrap())
}

fn metadata() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^(?:transferring (?:dockerfile|context):.*|exporting layers(?: .+)?|writing image sha256:[a-f0-9]+(?: .+)?)$").unwrap()
	})
}

fn redrawn_apt() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| {
		Regex::new(r"^\(Reading database \.\.\. \d+ files and directories currently installed\.\)$")
			.unwrap()
	})
}

fn dockerfile() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^Dockerfile(?:[.\w-]*):\d+$").unwrap())
}

fn source_line() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"^\s*\d+ \|.*$").unwrap())
}

fn legacy(stdout: &[&str], stderr: &[&str]) -> Option<Selection> {
	let lines: Vec<_> = stdout.iter().chain(stderr).copied().collect();
	let mut kept = Vec::new();
	let mut active: Option<Step<'_>> = None;
	let mut expected = 1;
	let mut total = None;
	let mut terminal = None;
	let mut sending = false;
	let mut deprecated = false;
	let mut i = 0;
	while i < lines.len() {
		let line = lines[i];
		if line.starts_with("DEPRECATED: The legacy builder is deprecated ") {
			if deprecated
				|| line != "DEPRECATED: The legacy builder is deprecated and will be removed in a future release."
				|| lines.get(i + 1) != Some(&"            BuildKit is currently disabled; enable it by removing the DOCKER_BUILDKIT=0")
				|| lines.get(i + 2) != Some(&"            environment-variable.")
			{
				return None;
			}
			kept.extend(i..i + 3);
			deprecated = true;
			i += 3;
			continue;
		}
		if line.is_empty() {
			if let Some(step) = &mut active {
				step.indices.push(i);
			}
			i += 1;
			continue;
		}
		if terminal.is_some() {
			if terminal == Some(State::Done) && line.starts_with("Successfully tagged ") {
				kept.push(i);
				i += 1;
				continue;
			}
			return None;
		}
		if line.starts_with("Sending build context to Docker daemon ") {
			if sending || active.is_some() {
				return None;
			}
			sending = true;
			i += 1;
			continue;
		}
		if let Some(rest) = line.strip_prefix("Step ") {
			let (counts, command) = rest.split_once(" : ")?;
			let (number, maximum) = counts.split_once('/')?;
			let number = number.parse::<usize>().ok()?;
			let maximum = maximum.parse::<usize>().ok()?;
			if number != expected
				|| number > maximum
				|| total.is_some_and(|total| total != maximum)
				|| command.is_empty()
			{
				return None;
			}
			if let Some(step) = active.take() {
				if step.state != State::Done {
					return None;
				}
				if step.diagnostic {
					kept.extend(step.indices);
				}
			}
			total = Some(maximum);
			expected += 1;
			active = Some(Step {
				header: command,
				indices: vec![i],
				state: State::Active,
				diagnostic: false,
			});
		} else if let Some(failure) = line.strip_prefix("The command '") {
			let (command, code) = failure.rsplit_once("' returned a non-zero code: ")?;
			let step = active.take()?;
			if code.parse::<usize>().ok()? == 0
				|| step.state != State::Active
				|| step.header.strip_prefix("RUN ")? != command.strip_prefix("/bin/sh -c ")?
			{
				return None;
			}
			kept.extend(step.indices);
			kept.push(i);
			terminal = Some(State::Failed);
		} else if let Some(hash) = line.strip_prefix("Successfully built ") {
			let step = active.take()?;
			if !image_hash(hash) || step.state != State::Done || Some(expected - 1) != total {
				return None;
			}
			if step.diagnostic {
				kept.extend(step.indices);
			}
			kept.push(i);
			terminal = Some(State::Done);
		} else {
			let step = active.as_mut()?;
			step.indices.push(i);
			if let Some(hash) = line.strip_prefix(" ---> ") {
				if image_hash(hash) {
					step.state = State::Done;
				} else if !(hash.strip_prefix("Running in ").is_some_and(image_hash)
					|| hash
						.strip_prefix("Removed intermediate container ")
						.is_some_and(image_hash)
					|| hash == "Using cache")
				{
					return None;
				}
			} else if step.state == State::Active && step.header.starts_with("RUN ") {
				step.diagnostic |= diagnostic(line);
			} else {
				return None;
			}
		}
		i += 1;
	}
	if terminal.is_none() || active.is_some() {
		return None;
	}
	kept.sort_unstable();
	Some(Selection {
		stdout: kept.iter().copied().filter(|&i| i < stdout.len()).collect(),
		stderr: kept
			.into_iter()
			.filter(|&i| i >= stdout.len())
			.map(|i| i - stdout.len())
			.collect(),
	})
}

fn image_hash(text: &str) -> bool {
	text.len() == 12 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod docker_tests;
