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

//! Line ids on shell output.
//!
//! A program that prints a file's lines with their numbers (`grep -n`, `rg -n`,
//! `nl -ba`, `cat -n`, `awk '{print NR": "$0}'`) gives the model everything an
//! edit needs except the hash half of the line id. This module adds it: such a
//! line comes back as `N:hh content`, the form `view` prints, so the edit tools
//! take it without a second read.
//!
//! A number at the start of a line is not proof of a file line (`12:30:01 up`,
//! `120 src/main.rs`, `src/a.rs:12:5: error`). So a line gets an id only when a
//! file the command or the output names holds exactly that text on that line.
//! The id is then right by construction, and every other line is left as the
//! program printed it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::utils::line_hash::format_line;

use super::file_ops::MAX_VIEW_FILE_BYTES;

// What programs put between the number and the line: `:` (grep match), `-`
// (grep context), a tab (`nl`, `cat -n`), `: ` or ` | ` (awk, printf).
const SEPARATOR_CHARS: &[char] = &[':', '-', '|', '\t', ' '];
const MAX_SEPARATOR_CHARS: usize = 3;

// Characters that end a word of a command for the purpose of finding the files
// it names: `--file=src/a.rs`, `"src/a.rs"`, `nl -ba src/a.rs|sed -n 1,9p`.
const WORD_BREAKS: &[char] = &[
	';', '|', '&', '(', ')', '<', '>', '\'', '"', '`', '=', '{', '}', '$', ',',
];

/// `output` of `command`, run in `working_dir`, with a line id on every line
/// that is a numbered line of a file.
pub(super) fn stamp(output: &str, command: &str, working_dir: &Path) -> String {
	let mut files = Files::of_command(command, working_dir);
	let lines: Vec<&str> = output.split('\n').collect();
	let mut stamped: Vec<String> = Vec::with_capacity(lines.len());
	// The file a `rg --heading` group belongs to: its path stands alone on the
	// line above the numbered lines, and a blank line ends the group.
	let mut heading: Option<PathBuf> = None;
	// The file line the previous output line was, when it got an id.
	let mut previous: Option<usize> = None;

	for (index, line) in lines.iter().enumerate() {
		let following = lines
			.get(index + 1)
			.and_then(|next| leading_number(next))
			.map(|(n, _)| n);
		let numbered = leading_number(line).and_then(|(number, rest)| {
			// A line that is only a number is a blank file line (`nl -ba` pads it
			// with a tab that terminal cleaning drops) or a count (`grep -c`). It is
			// a file line when its neighbours are numbered around it.
			let in_sequence = previous.is_some_and(|p| p + 1 == number)
				|| number.checked_add(1).is_some_and(|next| following == Some(next));
			files
				.numbered_line(heading.as_deref(), number, rest, in_sequence)
				.map(|text| (number, text))
		});
		if let Some((number, text)) = numbered {
			previous = Some(number);
			stamped.push(text);
			continue;
		}
		previous = None;
		if let Some(text) = files.path_numbered_line(line) {
			stamped.push(text);
			continue;
		}
		if line.is_empty() {
			heading = None;
		} else if !line.contains(char::is_whitespace) {
			if let Some(path) = files.find(line) {
				heading = Some(path);
			}
		}
		stamped.push((*line).to_string());
	}
	stamped.join("\n")
}

/// The number a line starts with, after any padding, and what follows it.
fn leading_number(line: &str) -> Option<(usize, &str)> {
	let text = line.trim_start();
	let digits = text.bytes().take_while(u8::is_ascii_digit).count();
	let number = text[..digits].parse().ok()?;
	Some((number, &text[digits..]))
}

fn is_separator(text: &str) -> bool {
	text.chars().count() <= MAX_SEPARATOR_CHARS && text.chars().all(|c| SEPARATOR_CHARS.contains(&c))
}

/// Text files the command could have printed lines of.
struct Files {
	/// The working directory and every directory the command moved to with `cd`.
	bases: Vec<PathBuf>,
	/// Lines by path; `None` for a path that is no text file small enough to view.
	loaded: HashMap<PathBuf, Option<Vec<String>>>,
	/// Files named by a word of the command, in the order named.
	named: Vec<PathBuf>,
}

impl Files {
	fn of_command(command: &str, working_dir: &Path) -> Self {
		let words: Vec<&str> = command
			.split(|c: char| c.is_whitespace() || WORD_BREAKS.contains(&c))
			.filter(|word| !word.is_empty())
			.collect();
		let mut bases = vec![working_dir.to_path_buf()];
		for pair in words.windows(2) {
			if pair[0] == "cd" {
				let dir = bases[bases.len() - 1].join(pair[1]);
				if dir.is_dir() {
					bases.push(dir);
				}
			}
		}
		let mut files = Self {
			bases,
			loaded: HashMap::new(),
			named: Vec::new(),
		};
		for word in words {
			if let Some(path) = files.find(word) {
				if !files.named.contains(&path) {
					files.named.push(path);
				}
			}
		}
		files
	}

	/// The text file `name` is from where the command ran, latest `cd` first.
	fn find(&mut self, name: &str) -> Option<PathBuf> {
		for base in self.bases.iter().rev() {
			let path = base.join(name);
			let lines = self.loaded.entry(path.clone()).or_insert_with(|| read_lines(&path));
			if lines.is_some() {
				return Some(path);
			}
		}
		None
	}

	fn line(&self, path: &Path, number: usize) -> Option<&str> {
		let lines = self.loaded.get(path)?.as_ref()?;
		lines.get(number.checked_sub(1)?).map(String::as_str)
	}

	/// `rest` is what follows `number` on an output line. When it is a separator
	/// and then line `number` of the heading file or of a file the command names,
	/// the line with its id.
	fn numbered_line(&self, heading: Option<&Path>, number: usize, rest: &str, in_sequence: bool) -> Option<String> {
		heading
			.into_iter()
			.chain(self.named.iter().map(PathBuf::as_path))
			.filter_map(|path| {
				let line = self.line(path, number)?;
				let text = line.trim_end();
				let rest = rest.trim_end();
				let separator = if text.is_empty() {
					rest
				} else {
					rest.strip_suffix(text)?
				};
				let fits = if separator.is_empty() {
					text.is_empty() && in_sequence
				} else {
					is_separator(separator)
				};
				fits.then_some((separator.len(), line))
			})
			// The shortest separator reads the most of the output as file text:
			// `12: x` is line ` x` after `:` before it is line `x` after `: `.
			.min_by_key(|(separator, _)| *separator)
			.map(|(_, line)| format_line(number, line))
	}

	/// `path:12:text` (a match) or `path-12-text` (a context line) of a search
	/// over several files, with the id in place of the number.
	fn path_numbered_line(&mut self, line: &str) -> Option<String> {
		let bytes = line.as_bytes();
		for (start, &mark) in bytes.iter().enumerate() {
			if start == 0 || (mark != b':' && mark != b'-') {
				continue;
			}
			let digits = bytes[start + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
			let end = start + 1 + digits;
			if digits == 0 || bytes.get(end) != Some(&mark) {
				continue;
			}
			let Ok(number) = line[start + 1..end].parse::<usize>() else {
				continue;
			};
			let Some(path) = self.find(&line[..start]) else {
				continue;
			};
			let Some(text) = self.line(&path, number) else {
				continue;
			};
			if text.trim_end() == line[end + 1..].trim_end() {
				return Some(format!("{}{}", &line[..=start], format_line(number, text)));
			}
		}
		None
	}
}

/// The lines of a text file the model could view, split the way `view` splits them.
fn read_lines(path: &Path) -> Option<Vec<String>> {
	let size = path.metadata().ok().filter(|meta| meta.is_file())?.len();
	if size > MAX_VIEW_FILE_BYTES {
		return None;
	}
	// A file that is not UTF-8 text has no line ids to give.
	let text = std::fs::read_to_string(path).ok()?;
	Some(text.lines().map(str::to_owned).collect())
}

#[cfg(test)]
#[path = "line_ids_tests.rs"]
mod tests;
