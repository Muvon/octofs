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
	store.push("a".to_string(), Some("x".repeat(100)), 250);
	store.push("b".to_string(), Some("y".repeat(100)), 250);
	store.push("c".to_string(), Some("z".repeat(100)), 250);
	assert!(store.pop("a").is_none(), "oldest snapshot must be evicted");
	assert!(store.pop("b").is_some());
	assert!(store.pop("c").is_some());
}

#[test]
fn test_history_store_never_evicts_the_snapshot_just_pushed() {
	let mut store = HistoryStore::default();
	store.push("big".to_string(), Some("x".repeat(1000)), 250);
	assert!(
		store.pop("big").is_some(),
		"the newest snapshot survives even when it alone exceeds the cap"
	);
}

#[test]
fn test_history_store_per_file_cap() {
	let mut store = HistoryStore::default();
	for i in 0..12 {
		store.push("f".to_string(), Some(format!("v{i}")), usize::MAX);
	}
	let mut n = 0;
	while store.pop("f").is_some() {
		n += 1;
	}
	assert_eq!(n, 10);
}

const ALREADY_NUMBERED: &str = "464:3c|alpha\n465:aa|beta\n466:bb|needle\n467:cc|delta\n";

fn line_span(first: i64, last: i64) -> (Endpoint, Endpoint) {
	(Endpoint::Number(first), Endpoint::Number(last))
}

#[tokio::test]
async fn view_ranges_shows_a_spill_file_as_stored_and_numbers_a_txt_file() {
	let dir = tempfile::tempdir().unwrap();
	let spans = [line_span(1, 1), line_span(3, 3)];

	let spill = dir.path().join("out.spill");
	std::fs::write(&spill, ALREADY_NUMBERED).unwrap();
	let out = view_file_ranges(&PathSource::Local(spill), &spans).await.unwrap();
	assert_eq!(
		out,
		"464:3c|alpha\n[... 1 lines omitted]\n466:bb|needle\n[... 1 more lines, 4 total]"
	);

	let text = dir.path().join("out.txt");
	std::fs::write(&text, ALREADY_NUMBERED).unwrap();
	let out = view_file_ranges(&PathSource::Local(text), &spans).await.unwrap();
	assert!(
		out.starts_with("1:") && out.contains("|464:3c|alpha\n"),
		"out was: {out}"
	);
}
