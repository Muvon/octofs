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
	assert!(err.contains("does not match"), "got: {err}");
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
