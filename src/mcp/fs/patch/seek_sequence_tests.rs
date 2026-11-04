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

//! Upstream sequence matching regression tests.

use super::seek_sequence;
use std::string::ToString;

fn to_vec(strings: &[&str]) -> Vec<String> {
	strings.iter().map(ToString::to_string).collect()
}

#[test]
fn test_exact_match_finds_sequence() {
	let lines = to_vec(&["foo", "bar", "baz"]);
	let pattern = to_vec(&["bar", "baz"]);
	assert_eq!(seek_sequence(&lines, &pattern, /*start*/ 0, /*eof*/ false,), Some(1));
}

#[test]
fn test_rstrip_match_ignores_trailing_whitespace() {
	let lines = to_vec(&["foo   ", "bar\t\t"]);
	// Pattern omits trailing whitespace.
	let pattern = to_vec(&["foo", "bar"]);
	assert_eq!(seek_sequence(&lines, &pattern, /*start*/ 0, /*eof*/ false,), Some(0));
}

#[test]
fn test_trim_match_ignores_leading_and_trailing_whitespace() {
	let lines = to_vec(&["    foo   ", "   bar\t"]);
	// Pattern omits any additional whitespace.
	let pattern = to_vec(&["foo", "bar"]);
	assert_eq!(seek_sequence(&lines, &pattern, /*start*/ 0, /*eof*/ false,), Some(0));
}

#[test]
fn test_pattern_longer_than_input_returns_none() {
	let lines = to_vec(&["just one line"]);
	let pattern = to_vec(&["too", "many", "lines"]);
	// Should not panic – must return None when pattern cannot possibly fit.
	assert_eq!(seek_sequence(&lines, &pattern, /*start*/ 0, /*eof*/ false,), None);
}
