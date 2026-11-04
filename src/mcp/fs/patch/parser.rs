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

//! This module is responsible for parsing & validating a patch into a list of "hunks".
//! (It does not attempt to actually check that the patch can be applied to the filesystem.)
//!
//! The official Lark grammar for the apply-patch format is:
//!
//! start: begin_patch hunk+ end_patch
//! begin_patch: "*** Begin Patch" LF
//! end_patch: "*** End Patch" LF?
//!
//! hunk: add_hunk | delete_hunk | update_hunk
//! add_hunk: "*** Add File: " filename LF add_line+
//! delete_hunk: "*** Delete File: " filename LF
//! update_hunk: "*** Update File: " filename LF change_move? change?
//! filename: /(.+)/
//! add_line: "+" /(.+)/ LF -> line
//!
//! change_move: "*** Move to: " filename LF
//! change: (change_context | change_line)+ eof_line?
//! change_context: ("@@" | "@@ " /(.+)/) LF
//! change_line: ("+" | "-" | " ") /(.+)/ LF
//! eof_line: "*** End of File" LF
//!
//! The parser below is a little more lenient than the explicit spec and allows for
//! leading/trailing whitespace around patch markers.

pub(crate) const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
pub(crate) const END_PATCH_MARKER: &str = "*** End Patch";
pub(crate) const ADD_FILE_MARKER: &str = "*** Add File: ";
pub(crate) const DELETE_FILE_MARKER: &str = "*** Delete File: ";
pub(crate) const UPDATE_FILE_MARKER: &str = "*** Update File: ";
pub(crate) const MOVE_TO_MARKER: &str = "*** Move to: ";
pub(crate) const EOF_MARKER: &str = "*** End of File";
pub(crate) const CHANGE_CONTEXT_MARKER: &str = "@@ ";
pub(crate) const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

#[derive(Debug, PartialEq, Clone)]
pub enum ParseError {
	InvalidPatchError(String),
	InvalidHunkError { message: String, line_number: usize },
}
use ParseError::*;

#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Hunk {
	AddFile {
		path: String,
		contents: String,
	},
	DeleteFile {
		path: String,
	},
	UpdateFile {
		path: String,
		move_path: Option<String>,

		/// Chunks should be in order, i.e. the `change_context` of one chunk
		/// should occur later in the file than the previous chunk.
		chunks: Vec<UpdateFileChunk>,
	},
}

use Hunk::*;

#[derive(Debug, PartialEq, Clone)]
pub struct UpdateFileChunk {
	/// A single line of context used to narrow down the position of the chunk
	/// (this is usually a class, method, or function definition.)
	pub change_context: Option<String>,

	/// A contiguous block of lines that should be replaced with `new_lines`.
	/// `old_lines` must occur strictly after `change_context`.
	pub old_lines: Vec<String>,
	pub new_lines: Vec<String>,
	pub context_line_indices: Vec<(usize, usize)>,

	/// If set to true, `old_lines` must occur at the end of the source file.
	/// (Tolerance around trailing newlines should be encouraged.)
	pub is_end_of_file: bool,
}

pub fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ParseError> {
	let lines: Vec<&str> = patch.trim().lines().collect();
	let (_, mut remaining_lines) = check_patch_boundaries_strict(&lines)?;
	let mut line_number = 2;
	let mut hunks = Vec::new();
	while !remaining_lines.is_empty() {
		let (hunk, consumed) = parse_one_hunk(remaining_lines, line_number)?;
		hunks.push(hunk);
		line_number += consumed;
		remaining_lines = &remaining_lines[consumed..];
	}
	Ok(hunks)
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			InvalidPatchError(message) => write!(f, "invalid patch: {message}"),
			InvalidHunkError { message, line_number } => write!(f, "invalid hunk at line {line_number}: {message}"),
		}
	}
}
impl std::error::Error for ParseError {}

/// Checks the start and end lines of the patch text for `apply_patch`,
/// returning an error if they do not match the expected markers.
fn check_patch_boundaries_strict<'a>(lines: &'a [&'a str]) -> Result<(&'a [&'a str], &'a [&'a str]), ParseError> {
	let (first_line, last_line) = match lines {
		[] => (None, None),
		[first] => (Some(first), Some(first)),
		[first, .., last] => (Some(first), Some(last)),
	};
	check_start_and_end_lines_strict(first_line, last_line)?;
	Ok((lines, &lines[1..lines.len() - 1]))
}

fn check_start_and_end_lines_strict(first_line: Option<&&str>, last_line: Option<&&str>) -> Result<(), ParseError> {
	let first_line = first_line.map(|line| line.trim());
	let last_line = last_line.map(|line| line.trim());

	match (first_line, last_line) {
		(Some(first), Some(last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => Ok(()),
		(Some(first), _) if first != BEGIN_PATCH_MARKER => Err(InvalidPatchError(String::from(
			"The first line of the patch must be '*** Begin Patch'",
		))),
		_ => Err(InvalidPatchError(String::from(
			"The last line of the patch must be '*** End Patch'",
		))),
	}
}

/// Attempts to parse a single hunk from the start of lines.
/// Returns the parsed hunk and the number of lines parsed (or a ParseError).
fn parse_one_hunk(lines: &[&str], line_number: usize) -> Result<(Hunk, usize), ParseError> {
	let first_line = lines[0].trim();
	if let Some(path) = first_line.strip_prefix(ADD_FILE_MARKER) {
		let mut contents = String::new();
		let mut parsed_lines = 1;
		for add_line in &lines[1..] {
			if let Some(line_to_add) = add_line.strip_prefix('+') {
				contents.push_str(line_to_add);
				contents.push('\n');
				parsed_lines += 1;
			} else {
				break;
			}
		}
		return Ok((
			AddFile {
				path: String::from(path),
				contents,
			},
			parsed_lines,
		));
	} else if let Some(path) = first_line.strip_prefix(DELETE_FILE_MARKER) {
		return Ok((
			DeleteFile {
				path: String::from(path),
			},
			1,
		));
	} else if let Some(path) = first_line.strip_prefix(UPDATE_FILE_MARKER) {
		let mut remaining_lines = &lines[1..];
		let mut parsed_lines = 1;
		let move_path = remaining_lines.first().and_then(|x| x.strip_prefix(MOVE_TO_MARKER));

		if move_path.is_some() {
			remaining_lines = &remaining_lines[1..];
			parsed_lines += 1;
		}

		let mut chunks = Vec::new();
		while !remaining_lines.is_empty() {
			if remaining_lines[0].trim().is_empty() {
				parsed_lines += 1;
				remaining_lines = &remaining_lines[1..];
				continue;
			}

			if remaining_lines[0].starts_with('*') {
				break;
			}

			let (chunk, chunk_lines) =
				parse_update_file_chunk(remaining_lines, line_number + parsed_lines, chunks.is_empty())?;
			chunks.push(chunk);
			parsed_lines += chunk_lines;
			remaining_lines = &remaining_lines[chunk_lines..]
		}

		if chunks.is_empty() {
			return Err(InvalidHunkError {
				message: format!("Update file hunk for path '{}' is empty", path),
				line_number,
			});
		}

		return Ok((
			UpdateFile {
				path: String::from(path),
				move_path: move_path.map(String::from),
				chunks,
			},
			parsed_lines,
		));
	}

	Err(InvalidHunkError {
        message: format!(
            "'{first_line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        ),
        line_number,
    })
}

fn parse_update_file_chunk(
	lines: &[&str],
	line_number: usize,
	allow_missing_context: bool,
) -> Result<(UpdateFileChunk, usize), ParseError> {
	if lines.is_empty() {
		return Err(InvalidHunkError {
			message: "Update hunk does not contain any lines".to_string(),
			line_number,
		});
	}
	let (change_context, start_index) = if lines[0] == EMPTY_CHANGE_CONTEXT_MARKER {
		(None, 1)
	} else if let Some(context) = lines[0].strip_prefix(CHANGE_CONTEXT_MARKER) {
		(Some(context.to_string()), 1)
	} else {
		if !allow_missing_context {
			return Err(InvalidHunkError {
				message: format!(
					"Expected update hunk to start with a @@ context marker, got: '{}'",
					lines[0]
				),
				line_number,
			});
		}
		(None, 0)
	};
	if start_index >= lines.len() {
		return Err(InvalidHunkError {
			message: "Update hunk does not contain any lines".to_string(),
			line_number: line_number + 1,
		});
	}
	let mut chunk = UpdateFileChunk {
		change_context,
		old_lines: Vec::new(),
		new_lines: Vec::new(),
		context_line_indices: Vec::new(),
		is_end_of_file: false,
	};
	let mut parsed_lines = 0;
	for line in &lines[start_index..] {
		match *line {
			EOF_MARKER => {
				if parsed_lines == 0 {
					return Err(InvalidHunkError {
						message: "Update hunk does not contain any lines".to_string(),
						line_number: line_number + 1,
					});
				}
				chunk.is_end_of_file = true;
				parsed_lines += 1;
				break;
			}
			line_contents => {
				match line_contents.chars().next() {
					None => {
						// Preserve context bytes when matching tolerates whitespace.
						chunk
							.context_line_indices
							.push((chunk.old_lines.len(), chunk.new_lines.len()));
						chunk.old_lines.push(String::new());
						chunk.new_lines.push(String::new());
					}
					Some(' ') => {
						chunk
							.context_line_indices
							.push((chunk.old_lines.len(), chunk.new_lines.len()));
						chunk.old_lines.push(line_contents[1..].to_string());
						chunk.new_lines.push(line_contents[1..].to_string());
					}
					Some('+') => {
						chunk.new_lines.push(line_contents[1..].to_string());
					}
					Some('-') => {
						chunk.old_lines.push(line_contents[1..].to_string());
					}
					_ => {
						if parsed_lines == 0 {
							return Err(InvalidHunkError {
                                message: format!(
                                    "Unexpected line found in update hunk: '{line_contents}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                                ),
                                line_number: line_number + 1,
                            });
						}
						// Assume this is the start of the next hunk.
						break;
					}
				}
				parsed_lines += 1;
			}
		}
	}

	Ok((chunk, parsed_lines + start_index))
}
