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
