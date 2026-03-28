// Copyright 2025 Muvon Un Limited
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

// Content-based line hashing for stable line identifiers.
// Each line gets a 4-char hex hash derived from its content.
// Collisions are resolved by incrementing until a free slot is found.

use std::collections::{HashMap, HashSet};
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
/// Produces a deterministic hash from line content.
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

/// Compute 4-char hex hashes for all lines with collision resolution.
/// Lines are processed top-to-bottom; on collision, hash is incremented until free.
pub fn compute_line_hashes(lines: &[&str]) -> Vec<String> {
	let mut used = HashSet::with_capacity(lines.len());
	let mut hashes = Vec::with_capacity(lines.len());

	for line in lines {
		let mut h = fnv1a_16(line);
		while used.contains(&h) {
			h = h.wrapping_add(1);
		}
		used.insert(h);
		hashes.push(format!("{:04x}", h));
	}

	hashes
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
		let h1 = fnv1a_16("hello world");
		let h2 = fnv1a_16("hello world");
		assert_eq!(h1, h2);
	}

	#[test]
	fn test_different_content_different_hash() {
		let h1 = fnv1a_16("line one");
		let h2 = fnv1a_16("line two");
		assert_ne!(h1, h2);
	}

	#[test]
	fn test_compute_unique_lines() {
		let lines = vec!["fn main() {", "    println!(\"hello\");", "}"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		// All unique content → all unique hashes
		let unique: HashSet<&String> = hashes.iter().collect();
		assert_eq!(unique.len(), 3);
		// Each hash is 4 hex chars
		for h in &hashes {
			assert_eq!(h.len(), 4);
			assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
		}
	}

	#[test]
	fn test_collision_resolution_duplicates() {
		let lines = vec!["same", "same", "same"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		// All must be unique despite identical content
		let unique: HashSet<&String> = hashes.iter().collect();
		assert_eq!(unique.len(), 3);
		// First keeps base hash, others are incremented
		let base = fnv1a_16("same");
		assert_eq!(hashes[0], format!("{:04x}", base));
		assert_eq!(hashes[1], format!("{:04x}", base.wrapping_add(1)));
		assert_eq!(hashes[2], format!("{:04x}", base.wrapping_add(2)));
	}

	#[test]
	fn test_stability_unique_lines() {
		// Unique lines keep their hash regardless of surrounding content
		let lines_before = vec!["alpha", "beta", "gamma"];
		let hashes_before = compute_line_hashes(&lines_before);

		// Insert a line in the middle
		let lines_after = vec!["alpha", "NEW LINE", "beta", "gamma"];
		let hashes_after = compute_line_hashes(&lines_after);

		// alpha, beta, gamma should keep the same hashes
		assert_eq!(hashes_before[0], hashes_after[0]); // alpha
		assert_eq!(hashes_before[1], hashes_after[2]); // beta
		assert_eq!(hashes_before[2], hashes_after[3]); // gamma
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
		let lines = vec!["", "", "content"];
		let hashes = compute_line_hashes(&lines);
		assert_eq!(hashes.len(), 3);
		let unique: HashSet<&String> = hashes.iter().collect();
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
	fn test_modified_line_changes_hash() {
		let lines_before = vec!["alpha", "beta", "gamma"];
		let hashes_before = compute_line_hashes(&lines_before);

		let lines_after = vec!["alpha", "BETA_MODIFIED", "gamma"];
		let hashes_after = compute_line_hashes(&lines_after);

		assert_eq!(hashes_before[0], hashes_after[0]); // alpha unchanged
		assert_ne!(hashes_before[1], hashes_after[1]); // beta changed
		assert_eq!(hashes_before[2], hashes_after[2]); // gamma unchanged
	}
}
