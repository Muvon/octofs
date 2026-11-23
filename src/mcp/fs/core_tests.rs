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
use crate::mcp::fs::delta::ViewCache;
use crate::mcp::request_ctx::with_request_context;
use serde_json::json;
use std::sync::Arc;

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

const ALREADY_NUMBERED: &str = "464:3c alpha\n465:aa beta\n466:bb needle\n467:cc delta\n";

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
		"464:3c alpha\n[... 1 lines omitted]\n466:bb needle\n[... 1 more lines, 4 total]"
	);

	let text = dir.path().join("out.txt");
	std::fs::write(&text, ALREADY_NUMBERED).unwrap();
	let out = view_file_ranges(&PathSource::Local(text), &spans).await.unwrap();
	assert!(
		out.starts_with("1:") && out.contains(" 464:3c alpha\n"),
		"out was: {out}"
	);
}

#[test]
fn split_path_list_trims_entries_and_rejects_unsafe_lists() {
	assert_eq!(
		split_path_list("src/a.rs | src/b.rs").unwrap(),
		["src/a.rs", "src/b.rs"]
	);

	let too_many = (0..=MAX_PATH_LIST_ENTRIES)
		.map(|index| format!("p{index}"))
		.collect::<Vec<_>>()
		.join("|");
	for (list, expected) in [
		("a||b", "non-empty"),
		("a|a", "duplicate path"),
		("a|\nb", "control characters"),
		(too_many.as_str(), "32-path limit"),
	] {
		let error = split_path_list(list).unwrap_err().to_string();
		assert!(error.contains(expected), "{list:?}: expected {expected:?} in {error}");
	}
}

#[test]
fn ranges_by_file_groups_spans_by_file_in_first_seen_order() {
	let call = McpToolCall::test_call(
		"view",
		json!({ "path": "a.rs", "full": true, "ranges": [
			{ "path": " b.rs ", "start": 1, "end": 2 },
			{ "start": 5, "end": 6 },
			{ "path": "b.rs", "start": 9 },
		] }),
	);
	let reads = ranges_by_file(&call, &["a.rs"]).unwrap().expect("an item names a file");
	let paths: Vec<&str> = reads.iter().map(|(path, _)| path.as_str()).collect();
	assert_eq!(paths, ["b.rs", "a.rs"]);
	assert_eq!(reads[0].1["path"], "b.rs");
	assert_eq!(reads[0].1["ranges"].as_array().map(Vec::len), Some(2));
	assert_eq!(reads[1].1["ranges"], json!([{ "start": 5, "end": 6 }]));
	assert_eq!(reads[0].1["full"], true, "the call's other parameters reach every read");

	// A span without `path` reads every path of the call's `|` list.
	let reads = ranges_by_file(&call, &["a.rs", "c.rs"])
		.unwrap()
		.expect("an item names a file");
	let paths: Vec<&str> = reads.iter().map(|(path, _)| path.as_str()).collect();
	assert_eq!(paths, ["b.rs", "a.rs", "c.rs"]);
	assert_eq!(reads[2].1["ranges"], json!([{ "start": 5, "end": 6 }]));

	// No item names a file: the ordinary read stays as it is.
	let single = McpToolCall::test_call("view", json!({ "path": "a.rs", "ranges": [{ "start": 1, "end": 2 }] }));
	assert!(ranges_by_file(&single, &["a.rs"]).unwrap().is_none());

	let unnamed = McpToolCall::test_call("view", json!({ "path": "a.rs", "ranges": [{ "path": " " }] }));
	let error = ranges_by_file(&unnamed, &["a.rs"]).unwrap_err().to_string();
	assert!(error.contains("`ranges[0].path`"), "{error}");
}

/// Each read fits one tool result alone, but together they can pass the size a client's
/// connection survives. A read that no longer fits is named instead of returned, and its
/// next whole view is still the content, not "unchanged": the model never saw it.
#[tokio::test]
async fn view_sections_names_the_reads_that_pass_the_result_limit() {
	const FILES: usize = 12;
	let dir = tempfile::tempdir().unwrap();
	// Each file stays under the view cache's size bound, so a returned whole view is
	// remembered; together the twelve pass the result limit.
	let content = format!("{}\n", "x".repeat(1023)).repeat(900);
	let paths: Vec<String> = (0..FILES)
		.map(|index| {
			let path = dir.path().join(format!("f{index}.txt"));
			std::fs::write(&path, &content).unwrap();
			path.to_string_lossy().to_string()
		})
		.collect();
	let first = &paths[0];
	let last = &paths[FILES - 1];
	let call = McpToolCall::test_call("view", json!({ "path": paths.join("|") }));

	let (result, again) = with_request_context(Some(Arc::new(ViewCache::default())), async {
		let result = execute_view(&call).await.unwrap();
		let alone = McpToolCall::test_call("view", json!({ "path": last }));
		(result, execute_view(&alone).await.unwrap())
	})
	.await;

	assert!(result.len() <= MAX_RESULT_BYTES, "result is {} bytes", result.len());
	let returned = FILES - result.matches(READ_NOT_RETURNED).count();
	assert!((1..FILES).contains(&returned), "returned {returned} of {FILES} reads");
	assert!(
		result.starts_with(&format!("{first}:\n1:")),
		"the first read is returned"
	);
	assert!(
		result.ends_with(&format!("{last}:\n{READ_NOT_RETURNED}")),
		"the last read is named, not returned"
	);
	assert!(
		again.starts_with("1:"),
		"a read that was not returned is not remembered as seen"
	);
}
