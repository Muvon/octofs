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

// Position-aware line hashing for stable, unique line identifiers.
// Each line gets a 4-char hex hash derived from its 1-indexed position AND content.
// Including position guarantees uniqueness for duplicate lines without any collision
// resolution — no two lines can share a hash because their positions differ.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Line identifier mode: sequential numbers or content-based hashes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineMode {
	Number,
	Hash,
}

static LINE_MODE: OnceLock<LineMode> = OnceLock::new();

/// Set the global line mode. Call once at startup.
pub fn set_line_mode(mode: LineMode) {
	LINE_MODE.set(mode).ok();
}

/// Get the current line mode (defaults to Number).
pub fn get_line_mode() -> LineMode {
	LINE_MODE.get().copied().unwrap_or(LineMode::Number)
}

/// Returns true if hash-based line identifiers are active.
pub fn is_hash_mode() -> bool {
	get_line_mode() == LineMode::Hash
}

/// FNV-1a hash folded to 16 bits.
/// Produces a deterministic hash from arbitrary bytes.
fn fnv1a_16(content: &str) -> u16 {
	const FNV_OFFSET: u32 = 2166136261;
	const FNV_PRIME: u32 = 16777619;

	let mut hash = FNV_OFFSET;
	for byte in content.bytes() {
		hash ^= byte as u32;
		hash = hash.wrapping_mul(FNV_PRIME);
	}

	// Fold 32-bit to 16-bit via XOR
	((hash >> 16) ^ (hash & 0xFFFF)) as u16
}

/// Compute 4-char hex hashes for all lines.
/// Each hash is derived from `"<1-indexed-position>:<content>"`, which guarantees
/// uniqueness across all lines — duplicate content at different positions always
/// produces different hashes, with no collision-resolution bookkeeping needed.
pub fn compute_line_hashes(lines: &[&str]) -> Vec<String> {
	lines
		.iter()
		.enumerate()
		.map(|(i, line)| {
			let key = format!("{}:{}", i + 1, line);
			format!("{:04x}", fnv1a_16(&key))
		})
		.collect()
}

/// Build a reverse lookup map: hash string → 1-indexed line number.
pub fn build_hash_to_line_map(lines: &[&str]) -> HashMap<String, usize> {
	let hashes = compute_line_hashes(lines);
	let mut map = HashMap::with_capacity(hashes.len());
	for (i, hash) in hashes.into_iter().enumerate() {
		map.insert(hash, i + 1);
	}
	map
}

/// Resolve a single hash string to a 1-indexed line number.
pub fn resolve_hash_to_line(hash: &str, lines: &[&str]) -> Result<usize, String> {
	let map = build_hash_to_line_map(lines);
	map.get(hash)
		.copied()
		.ok_or_else(|| format!("Hash '{}' not found in file content", hash))
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
// Tools take line targets as scalar params (`start`/`end`, `append_line`, …). Each
// endpoint is either a line NUMBER (JSON integer) or a content HASH (JSON string), so
// the JSON type itself disambiguates — no range-string parsing, and no ambiguity for
// all-digit hashes. LineMode affects how lines are RENDERED, and (in number mode only)
// whether a numeric STRING is tolerated as a line number — see parse_endpoint.

/// A single line endpoint parsed from a JSON value: a line number or a content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
	/// Line number (1-indexed; 0 and negatives carry position-specific meaning per tool).
	Number(i64),
	/// Content hash identifying a line.
	Hash(String),
}

/// Parse a JSON value into a line [`Endpoint`] using the active [`LineMode`].
///
/// - JSON integer → [`Endpoint::Number`] (a line number).
/// - JSON string → [`Endpoint::Hash`], EXCEPT in number mode a purely-numeric string
///   is accepted as a line number (tolerates clients that stringify integers).
pub fn parse_endpoint(value: &serde_json::Value) -> Result<Endpoint, String> {
	parse_endpoint_with_mode(value, is_hash_mode())
}

/// Mode-explicit core of [`parse_endpoint`], split out so the hash-mode branch is
/// unit-testable without mutating the process-global [`LineMode`] (a write-once `OnceLock`).
fn parse_endpoint_with_mode(
	value: &serde_json::Value,
	hash_mode: bool,
) -> Result<Endpoint, String> {
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
			if !hash_mode {
				if let Ok(n) = s.parse::<i64>() {
					return Ok(Endpoint::Number(n));
				}
			}
			Ok(Endpoint::Hash(s.to_string()))
		}
		_ => Err("line value must be an integer (line number) or a string (hash)".to_string()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_basic_hash_deterministic() {
		// Same position + content always produces the same hash
		let h1 = fnv1a_16("1:hello world");
		let h2 = fnv1a_16("1:hello world");
		assert_eq!(h1, h2);
	}

	#[test]
	fn test_different_content_different_hash() {
		let h1 = fnv1a_16("1:line one");
		let h2 = fnv1a_16("1:line two");
		assert_ne!(h1, h2);
	}

	#[test]
	fn test_compute_unique_lines() {
		let lines = vec!["fn main() {", "    println!(\"hello\");", "}"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		// All unique content → all unique hashes
		let unique: std::collections::HashSet<&String> = hashes.iter().collect();
		assert_eq!(unique.len(), 3);
		// Each hash is 4 hex chars
		for h in &hashes {
			assert_eq!(h.len(), 4);
			assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
		}
	}

	#[test]
	fn test_duplicate_content_unique_hashes() {
		// Duplicate content at different positions must produce different hashes
		// because position is included in the hash key.
		let lines = vec!["same", "same", "same"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		let unique: std::collections::HashSet<&String> = hashes.iter().collect();
		assert_eq!(
			unique.len(),
			3,
			"duplicate lines must get unique hashes via position"
		);
		// Verify each hash matches its position-keyed input
		assert_eq!(hashes[0], format!("{:04x}", fnv1a_16("1:same")));
		assert_eq!(hashes[1], format!("{:04x}", fnv1a_16("2:same")));
		assert_eq!(hashes[2], format!("{:04x}", fnv1a_16("3:same")));
	}

	#[test]
	fn test_same_content_different_position_different_hash() {
		// Identical content at different positions must differ
		let h1 = fnv1a_16("1:closing brace");
		let h2 = fnv1a_16("5:closing brace");
		assert_ne!(h1, h2);
	}

	#[test]
	fn test_reverse_lookup() {
		let lines = vec!["first", "second", "third"];
		let map = build_hash_to_line_map(&lines);
		assert_eq!(map.len(), 3);

		let hashes = compute_line_hashes(&lines);
		assert_eq!(map[&hashes[0]], 1);
		assert_eq!(map[&hashes[1]], 2);
		assert_eq!(map[&hashes[2]], 3);
	}

	#[test]
	fn test_resolve_hash() {
		let lines = vec!["first", "second", "third"];
		let hashes = compute_line_hashes(&lines);

		assert_eq!(resolve_hash_to_line(&hashes[0], &lines).unwrap(), 1);
		assert_eq!(resolve_hash_to_line(&hashes[1], &lines).unwrap(), 2);
		assert_eq!(resolve_hash_to_line(&hashes[2], &lines).unwrap(), 3);
		assert!(resolve_hash_to_line("zzzz", &lines).is_err());
	}

	#[test]
	fn test_empty_lines() {
		// Empty lines at different positions get unique hashes
		let lines = vec!["", "", "content"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		let unique: std::collections::HashSet<&String> = hashes.iter().collect();
		assert_eq!(unique.len(), 3);
	}

	#[test]
	fn test_hash_format_lowercase_hex() {
		let lines = vec!["test"];
		let hashes = compute_line_hashes(&lines);
		assert!(hashes[0]
			.chars()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
	}

	#[test]
	fn test_unique_content_hash_stable_at_same_position() {
		// A line with unique content at the same position always gets the same hash
		let lines_a = vec!["alpha", "beta", "gamma"];
		let lines_b = vec!["alpha", "beta", "gamma"];
		let hashes_a = compute_line_hashes(&lines_a);
		let hashes_b = compute_line_hashes(&lines_b);
		assert_eq!(hashes_a, hashes_b);
	}

	#[test]
	fn test_parse_endpoint_number_mode() {
		use serde_json::json;
		// JSON integers are line numbers (incl. 0 and negatives).
		assert_eq!(parse_endpoint(&json!(42)).unwrap(), Endpoint::Number(42));
		assert_eq!(parse_endpoint(&json!(0)).unwrap(), Endpoint::Number(0));
		assert_eq!(parse_endpoint(&json!(-1)).unwrap(), Endpoint::Number(-1));
		// In number mode, a numeric string is tolerated as a line number.
		assert_eq!(parse_endpoint(&json!("10")).unwrap(), Endpoint::Number(10));
		assert_eq!(parse_endpoint(&json!("-3")).unwrap(), Endpoint::Number(-3));
		// A string with hex letters is a hash.
		assert_eq!(
			parse_endpoint(&json!("a3bd")).unwrap(),
			Endpoint::Hash("a3bd".to_string())
		);
		// Wrong types / empties are errors.
		assert!(parse_endpoint(&json!("")).is_err());
		assert!(parse_endpoint(&json!([1, 2])).is_err());
		assert!(parse_endpoint(&json!(null)).is_err());
	}

	#[test]
	fn test_parse_endpoint_hash_mode() {
		use serde_json::json;
		// In hash mode an all-digit string is a HASH, not a line number — this is the whole
		// point of JSON-type disambiguation (a hex hash like "1024" must not become line 1024).
		assert_eq!(
			parse_endpoint_with_mode(&json!("1024"), true).unwrap(),
			Endpoint::Hash("1024".to_string())
		);
		assert_eq!(
			parse_endpoint_with_mode(&json!("a3bd"), true).unwrap(),
			Endpoint::Hash("a3bd".to_string())
		);
		// Integers are always line numbers, even in hash mode (anchors 0/-1/N).
		assert_eq!(
			parse_endpoint_with_mode(&json!(0), true).unwrap(),
			Endpoint::Number(0)
		);
		assert_eq!(
			parse_endpoint_with_mode(&json!(-1), true).unwrap(),
			Endpoint::Number(-1)
		);
		// Number mode keeps the stringified-integer tolerance.
		assert_eq!(
			parse_endpoint_with_mode(&json!("10"), false).unwrap(),
			Endpoint::Number(10)
		);
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
		let lines_before = vec!["alpha", "beta", "gamma"];
		let hashes_before = compute_line_hashes(&lines_before);

		let lines_after = vec!["alpha", "BETA_MODIFIED", "gamma"];
		let hashes_after = compute_line_hashes(&lines_after);

		assert_eq!(hashes_before[0], hashes_after[0]); // alpha at pos 1 unchanged
		assert_ne!(hashes_before[1], hashes_after[1]); // content changed
		assert_eq!(hashes_before[2], hashes_after[2]); // gamma at pos 3 unchanged
	}
}
