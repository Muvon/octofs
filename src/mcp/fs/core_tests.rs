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
fn test_history_store_byte_cap_evicts_globally_oldest() {
	let mut store = HistoryStore::default();
	store.push("a".to_string(), "x".repeat(100), 250);
	store.push("b".to_string(), "y".repeat(100), 250);
	store.push("c".to_string(), "z".repeat(100), 250);
	assert!(store.pop("a").is_none(), "oldest snapshot must be evicted");
	assert!(store.pop("b").is_some());
	assert!(store.pop("c").is_some());
}

#[test]
fn test_history_store_never_evicts_the_snapshot_just_pushed() {
	let mut store = HistoryStore::default();
	store.push("big".to_string(), "x".repeat(1000), 250);
	assert!(
		store.pop("big").is_some(),
		"the newest snapshot survives even when it alone exceeds the cap"
	);
}

#[test]
fn test_history_store_per_file_cap() {
	let mut store = HistoryStore::default();
	for i in 0..12 {
		store.push("f".to_string(), format!("v{i}"), usize::MAX);
	}
	let mut n = 0;
	while store.pop("f").is_some() {
		n += 1;
	}
	assert_eq!(n, 10);
}
