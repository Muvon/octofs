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

// Composite line identifiers: every line is addressed as "N:hh" — its 1-indexed
// position plus a 2-char hex hash of its content. View output renders lines as
// "N:hh|content"; edit tools take the same ids back and verify the hash against
// the file before touching anything, so a stale id fails loudly (with the fresh
// content in the error) instead of silently editing the wrong line. This is the
// single line-id format — there is no mode switch.

/// FNV-1a hash of line content folded to 8 bits (2 hex chars).
/// Content-only — position lives in the id's number part, so a line keeps its
/// hash when it moves and a stale-id error can report where the content went.
/// 8 bits means a changed line keeps its hash with probability 1/256 — accepted
/// trade-off for id brevity (the position check catches gross drift anyway).
fn fnv1a_8(content: &str) -> u8 {
	const FNV_OFFSET: u32 = 2166136261;
	const FNV_PRIME: u32 = 16777619;

	let mut hash = FNV_OFFSET;
	for byte in content.bytes() {
		hash ^= byte as u32;
		hash = hash.wrapping_mul(FNV_PRIME);
	}

	// Fold 32 -> 16 -> 8 via XOR
	let h16 = ((hash >> 16) ^ (hash & 0xFFFF)) as u16;
	((h16 >> 8) ^ (h16 & 0xFF)) as u8
}

/// 2-char hex content hash of a single line.
pub fn line_hash(content: &str) -> String {
	format!("{:02x}", fnv1a_8(content))
}

/// Composite id "N:hh" for a 1-indexed line with the given content.
pub fn line_id(line_1idx: usize, content: &str) -> String {
	format!("{}:{}", line_1idx, line_hash(content))
}

/// Composite id for a 1-indexed line looked up in `lines`.
pub fn line_id_at(lines: &[&str], line_1idx: usize) -> String {
	line_id(line_1idx, lines[line_1idx - 1])
}

/// Verify a composite id against the current file lines. Returns the 1-indexed
/// line on success. On mismatch the error is self-contained: it shows the
/// current content around the target with fresh ids, where the expected hash
/// actually is, and suggests a ranged `view` — so the model can retarget from
/// the error alone instead of re-reading the whole file. It does not claim WHY
/// the id is off: a neighbour's hash pasted next to the wrong line number looks
/// exactly like a one-line shift, and the fix is the same either way.
pub fn verify_line_id(line: usize, expected_hash: &str, lines: &[&str]) -> Result<usize, String> {
	let total = lines.len();
	if line == 0 || line > total {
		let mut msg = format!(
			"Line id \"{line}:{expected_hash}\" is out of range: the file has {total} lines now."
		);
		push_relocation(&mut msg, expected_hash, line, lines);
		msg.push_str("\nRun `view` on this file to get fresh ids, then retry.");
		return Err(msg);
	}

	if line_hash(lines[line - 1]) == expected_hash {
		return Ok(line);
	}

	const CONTEXT: usize = 2;
	let win_start = line.saturating_sub(CONTEXT).max(1);
	let win_end = (line + CONTEXT).min(total);
	let mut msg = format!(
		"Line id \"{line}:{expected_hash}\" does not match: line {line} is currently \"{}\". Content around it:\n",
		line_id_at(lines, line)
	);
	for i in win_start..=win_end {
		msg.push_str(&format!("{}|{}\n", line_id_at(lines, i), lines[i - 1]));
	}
	push_relocation(&mut msg, expected_hash, line, lines);
	msg.push_str(&format!(
		"\nRetry with the fresh ids above, or run `view` with start: {win_start}, end: {win_end} (or a wider range) to confirm before editing."
	));
	Err(msg)
}

/// Append where content with this hash actually is (nearest to `expected_line`
/// first, up to 3) so a moved line or a mis-paired hash is a one-step fix.
fn push_relocation(msg: &mut String, expected_hash: &str, expected_line: usize, lines: &[&str]) {
	let mut candidates: Vec<usize> = lines
		.iter()
		.enumerate()
		.filter(|(_, l)| line_hash(l) == expected_hash)
		.map(|(i, _)| i + 1)
		.collect();
	if candidates.is_empty() {
		return;
	}
	candidates.sort_by_key(|&n| n.abs_diff(expected_line));
	let shown: Vec<String> = candidates
		.iter()
		.take(3)
		.map(|&n| line_id_at(lines, n))
		.collect();
	msg.push_str(&format!(
		"Hash {expected_hash} currently matches: {} — you may have paired line {expected_line} with a neighbour's hash, or the content moved.",
		shown.join(", ")
	));
}

// ── Line number resolution ─────────────────────────────────────────────────────────
//
// Shared by every tool that targets lines by number. Negative indices count from the
// end (-1 = last line). `checked_neg` guards i64::MIN, whose negation overflows.

/// Resolve a possibly-negative line index to a 1-indexed line number (strict).
/// Line 0 and out-of-range indices are errors.
pub(crate) fn resolve_line_index(index: i64, total_lines: usize) -> Result<usize, String> {
	if index == 0 {
		return Err("Line numbers are 1-indexed, use 1 for first line".to_string());
	}
	if index > 0 {
		let pos_index = index as usize;
		if pos_index > total_lines {
			return Err(format!(
				"Line {index} exceeds file length ({total_lines} lines)"
			));
		}
		Ok(pos_index)
	} else {
		let from_end = index
			.checked_neg()
			.map(|v| v as usize)
			.unwrap_or(usize::MAX);
		if from_end > total_lines {
			return Err(format!(
				"Negative index {index} exceeds file length ({total_lines} lines)"
			));
		}
		Ok(total_lines - from_end + 1)
	}
}

/// View-only variant: clamp out-of-bounds indices to the nearest valid line instead of
/// erroring. Returns `(resolved_index, was_clamped)`. Line 0 is still rejected — it's a
/// spec violation, not out-of-bounds.
pub(crate) fn resolve_line_index_clamped(
	index: i64,
	total_lines: usize,
) -> Result<(usize, bool), String> {
	if index == 0 {
		return Err("Line numbers are 1-indexed, use 1 for first line".to_string());
	}
	if index > 0 {
		let pos = index as usize;
		if pos > total_lines {
			Ok((total_lines, true))
		} else {
			Ok((pos, false))
		}
	} else {
		let from_end = index
			.checked_neg()
			.map(|v| v as usize)
			.unwrap_or(usize::MAX);
		if from_end > total_lines {
			// Negative index past the beginning — clamp to first line.
			Ok((1, true))
		} else {
			Ok((total_lines - from_end + 1, false))
		}
	}
}

// ── Line endpoint parsing ────────────────────────────────────────────────────────
//
// Tools take line targets as scalar params (`start`/`end`, `append_line`, op
// endpoints). An endpoint is either a composite id string "N:hh" (verified against
// the file before edits) or a plain integer line number where positions are allowed
// (view ranges, insert anchors 0/-1). Numeric strings are tolerated as integers for
// clients that stringify numbers.

/// A single line endpoint parsed from a JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
	/// Plain line number (1-indexed; 0 and negatives carry position-specific meaning per tool).
	Number(i64),
	/// Composite id: 1-indexed line plus its expected 2-char content hash.
	Id { line: usize, hash: String },
}

/// Parse a JSON value into a line [`Endpoint`].
///
/// - JSON integer → [`Endpoint::Number`].
/// - String "N:hh" (line + 2 hex chars, as rendered in view output) → [`Endpoint::Id`].
/// - Numeric string → [`Endpoint::Number`] (tolerates clients that stringify integers).
pub fn parse_endpoint(value: &serde_json::Value) -> Result<Endpoint, String> {
	match value {
		serde_json::Value::Number(n) => n
			.as_i64()
			.map(Endpoint::Number)
			.ok_or_else(|| "line number must be an integer".to_string()),
		serde_json::Value::String(s) => {
			let s = s.trim();
			if s.is_empty() {
				return Err("line value is empty".to_string());
			}
			if let Ok(n) = s.parse::<i64>() {
				return Ok(Endpoint::Number(n));
			}
			if let Some((num, hash)) = s.split_once(':') {
				let line: usize = num.parse().map_err(|_| {
					format!("invalid line id '{s}': expected \"N:hh\" as shown in view output (e.g. \"12:a3\")")
				})?;
				if line == 0 {
					return Err(format!("invalid line id '{s}': lines are 1-indexed"));
				}
				let hash = hash.to_ascii_lowercase();
				if hash.len() != 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
					return Err(format!(
						"invalid line id '{s}': the hash part must be 2 hex chars (e.g. \"12:a3\")"
					));
				}
				return Ok(Endpoint::Id { line, hash });
			}
			Err(format!(
				"invalid line value '{s}': pass a line id like \"12:a3\" (from view output) or an integer line number"
			))
		}
		_ => Err(format!(
			"line value must be a line id like \"12:a3\" or an integer line number, got {value}",
		)),
	}
}

#[cfg(test)]
#[path = "line_hash_tests.rs"]
mod line_hash_tests;
