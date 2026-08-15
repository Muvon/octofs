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
/// current content around the target with fresh ids, where the expected content
/// moved to (matching hashes), and suggests a ranged `view` — so the model can
/// retarget from the error alone instead of re-reading the whole file.
pub fn verify_line_id(line: usize, expected_hash: &str, lines: &[&str]) -> Result<usize, String> {
	let total = lines.len();
	if line == 0 || line > total {
		let mut msg =
			format!("Stale line id \"{line}:{expected_hash}\": the file has {total} lines now.");
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
		"Stale line id \"{line}:{expected_hash}\" — the file changed since you viewed it. Current content around line {line}:\n"
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

/// Append "content with this hash is now at ..." candidates (nearest to
/// `expected_line` first, up to 3) so a moved-but-unchanged line is a one-step fix.
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
		"Content matching hash {expected_hash} is now at: {} (your target may have moved).",
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
mod tests {
	use super::*;

	#[test]
	fn test_line_hash_deterministic_and_content_only() {
		assert_eq!(line_hash("hello world"), line_hash("hello world"));
		assert_ne!(line_hash("line one"), line_hash("line two"));
		// Content-only: same content always hashes the same regardless of position.
		assert_eq!(
			line_id(1, "same").split(':').nth(1),
			line_id(9, "same").split(':').nth(1)
		);
	}

	#[test]
	fn test_line_hash_format() {
		let h = line_hash("test");
		assert_eq!(h.len(), 2);
		assert!(h
			.chars()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
	}

	#[test]
	fn test_line_id_shape() {
		let lines = vec!["fn main() {", "    println!(\"hello\");", "}"];
		assert_eq!(line_id_at(&lines, 1), format!("1:{}", line_hash(lines[0])));
		assert_eq!(line_id_at(&lines, 3), format!("3:{}", line_hash(lines[2])));
	}

	#[test]
	fn test_verify_line_id_ok() {
		let lines = vec!["first", "second", "third"];
		let hash = line_hash("second");
		assert_eq!(verify_line_id(2, &hash, &lines).unwrap(), 2);
	}

	#[test]
	fn test_verify_line_id_stale_shows_fresh_context() {
		let lines = vec!["first", "CHANGED", "third"];
		let old_hash = line_hash("second");
		let err = verify_line_id(2, &old_hash, &lines).unwrap_err();
		assert!(err.contains("Stale line id"), "got: {err}");
		assert!(err.contains("CHANGED"), "fresh content shown: {err}");
		assert!(
			err.contains(&line_id_at(&lines, 2)),
			"fresh id shown: {err}"
		);
		assert!(err.contains("view"), "suggests ranged view: {err}");
	}

	#[test]
	fn test_verify_line_id_reports_moved_content() {
		// "target" moved from line 2 to line 4.
		let lines = vec!["first", "inserted", "also inserted", "target"];
		let hash = line_hash("target");
		let err = verify_line_id(2, &hash, &lines).unwrap_err();
		assert!(
			err.contains(&format!("4:{hash}")),
			"relocation candidate shown: {err}"
		);
	}

	#[test]
	fn test_verify_line_id_beyond_eof() {
		let lines = vec!["only line"];
		let err = verify_line_id(9, "ab", &lines).unwrap_err();
		assert!(err.contains("1 lines"), "got: {err}");
	}

	#[test]
	fn test_parse_endpoint_numbers() {
		use serde_json::json;
		assert_eq!(parse_endpoint(&json!(42)).unwrap(), Endpoint::Number(42));
		assert_eq!(parse_endpoint(&json!(0)).unwrap(), Endpoint::Number(0));
		assert_eq!(parse_endpoint(&json!(-1)).unwrap(), Endpoint::Number(-1));
		// Numeric strings are tolerated as line numbers.
		assert_eq!(parse_endpoint(&json!("10")).unwrap(), Endpoint::Number(10));
		assert_eq!(parse_endpoint(&json!("-3")).unwrap(), Endpoint::Number(-3));
	}

	#[test]
	fn test_parse_endpoint_ids() {
		use serde_json::json;
		assert_eq!(
			parse_endpoint(&json!("12:a3")).unwrap(),
			Endpoint::Id {
				line: 12,
				hash: "a3".to_string()
			}
		);
		// Uppercase hex is normalized.
		assert_eq!(
			parse_endpoint(&json!("7:FF")).unwrap(),
			Endpoint::Id {
				line: 7,
				hash: "ff".to_string()
			}
		);
	}

	#[test]
	fn test_parse_endpoint_rejects_garbage() {
		use serde_json::json;
		assert!(parse_endpoint(&json!("")).is_err());
		assert!(parse_endpoint(&json!("abc")).is_err());
		assert!(parse_endpoint(&json!("0:a3")).is_err());
		assert!(parse_endpoint(&json!("12:xyz")).is_err());
		assert!(parse_endpoint(&json!("12:a")).is_err());
		let arr_err = parse_endpoint(&json!([1, 2])).unwrap_err();
		assert!(arr_err.contains("got [1,2]"), "got: {arr_err}");
		let null_err = parse_endpoint(&json!(null)).unwrap_err();
		assert!(null_err.contains("got null"), "got: {null_err}");
	}

	#[test]
	fn test_resolve_line_index_basic_and_negative() {
		assert_eq!(resolve_line_index(1, 5).unwrap(), 1);
		assert_eq!(resolve_line_index(5, 5).unwrap(), 5);
		assert_eq!(resolve_line_index(-1, 5).unwrap(), 5);
		assert_eq!(resolve_line_index(-5, 5).unwrap(), 1);
		assert!(resolve_line_index(0, 5).is_err());
		assert!(resolve_line_index(6, 5).is_err());
		assert!(resolve_line_index(-6, 5).is_err());
	}

	#[test]
	fn test_resolve_line_index_i64_min_does_not_panic() {
		// i64::MIN negation overflows; checked_neg must turn it into a clean out-of-range error
		// (and the clamped variant into a clamp) in every build profile, never a panic.
		assert!(resolve_line_index(i64::MIN, 5).is_err());
		assert_eq!(resolve_line_index_clamped(i64::MIN, 5).unwrap(), (1, true));
	}

	#[test]
	fn test_resolve_line_index_clamped() {
		assert_eq!(resolve_line_index_clamped(3, 5).unwrap(), (3, false));
		assert_eq!(resolve_line_index_clamped(99, 5).unwrap(), (5, true));
		assert_eq!(resolve_line_index_clamped(-1, 5).unwrap(), (5, false));
		assert_eq!(resolve_line_index_clamped(-99, 5).unwrap(), (1, true));
		assert!(resolve_line_index_clamped(0, 5).is_err());
	}

	#[test]
	fn test_modified_line_changes_hash() {
		assert_ne!(line_hash("beta"), line_hash("BETA_MODIFIED"));
		assert_eq!(line_hash("alpha"), line_hash("alpha"));
	}
}
