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

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::mcp::fs::{execute_text_editor, execute_view};
use crate::mcp::request_ctx::with_request_context;
use crate::mcp::McpToolCall;
use crate::utils::line_hash::line_id;

/// Replay `ops` and check they walk both inputs in order and rebuild `new`.
fn check_ops(old: &[&str], new: &[&str], ops: &[DiffOp]) -> usize {
	let (mut i, mut j, mut edits) = (0, 0, 0);
	let mut rebuilt = Vec::new();
	for op in ops {
		match *op {
			DiffOp::Equal(a, b) => {
				assert_eq!((a, b), (i, j));
				assert_eq!(old[a], new[b]);
				rebuilt.push(new[b]);
				i += 1;
				j += 1;
			}
			DiffOp::Delete(a) => {
				assert_eq!(a, i);
				i += 1;
				edits += 1;
			}
			DiffOp::Insert(b) => {
				assert_eq!(b, j);
				rebuilt.push(new[b]);
				j += 1;
				edits += 1;
			}
		}
	}
	assert_eq!((i, j), (old.len(), new.len()));
	assert_eq!(rebuilt, new);
	edits
}

#[test]
fn myers_classic_example_has_minimal_edit_distance() {
	let old = ["A", "B", "C", "A", "B", "B", "A"];
	let new = ["C", "B", "A", "B", "A", "C"];
	let ops = diff_lines(&old, &new, 100).unwrap();
	assert_eq!(check_ops(&old, &new, &ops), 5);
}

#[test]
fn myers_edge_cases() {
	let same = ["x", "y", "z"];
	let ops = diff_lines(&same, &same, 10).unwrap();
	assert_eq!(check_ops(&same, &same, &ops), 0);

	let ops = diff_lines(&[], &same, 10).unwrap();
	assert_eq!(check_ops(&[], &same, &ops), 3);

	let ops = diff_lines(&same, &[], 10).unwrap();
	assert_eq!(check_ops(&same, &[], &ops), 3);

	assert!(diff_lines(&same, &[], 0).is_none());

	let old = ["a", "b", "c", "d", "e"];
	let new = ["a", "X", "c", "e", "f"];
	let ops = diff_lines(&old, &new, 100).unwrap();
	assert_eq!(check_ops(&old, &new, &ops), 4);
	assert!(diff_lines(&old, &new, 3).is_none());
}

#[test]
fn delta_renders_hunks_with_fresh_ids_and_collapsed_removals() {
	let old: Vec<String> = (1..=20).map(|i| format!("l{i}")).collect();
	let old: Vec<&str> = old.iter().map(String::as_str).collect();
	// Line 5 edited, lines 12-14 removed.
	let mut new = old.clone();
	new[4] = "L5";
	new.drain(11..14);
	let out = render_delta(&old, &new).unwrap();
	let lines: Vec<&str> = out.lines().collect();
	assert_eq!(
		lines[0],
		"[changed since your last view: 2 hunk(s), 17 lines now]"
	);
	assert_eq!(lines[1], "...");
	assert_eq!(lines[2], format!("{}|l3", line_id(3, "l3")));
	assert_eq!(lines[3], format!("{}|l4", line_id(4, "l4")));
	assert_eq!(lines[4], format!("-{} (1 line)", line_id(5, "l5")));
	assert_eq!(lines[5], format!("+{}|L5", line_id(5, "L5")));
	assert_eq!(lines[6], format!("{}|l6", line_id(6, "l6")));
	assert_eq!(lines[7], format!("{}|l7", line_id(7, "l7")));
	assert_eq!(lines[8], "...");
	assert_eq!(lines[9], format!("{}|l10", line_id(10, "l10")));
	assert_eq!(lines[10], format!("{}|l11", line_id(11, "l11")));
	assert_eq!(
		lines[11],
		format!("-{}..{} (3 lines)", line_id(12, "l12"), line_id(14, "l14"))
	);
	// Kept lines after the removal carry their NEW positions.
	assert_eq!(lines[12], format!("{}|l15", line_id(12, "l15")));
	assert_eq!(lines[13], format!("{}|l16", line_id(13, "l16")));
	assert_eq!(lines[14], "...");
	assert_eq!(lines.len(), 15);
}

#[test]
fn delta_without_trailing_context_has_no_closing_marker() {
	let old = ["a", "b", "c"];
	let new = ["a", "b", "c", "d"];
	let out = render_delta(&old, &new).unwrap();
	assert_eq!(
		out,
		format!(
			"[changed since your last view: 1 hunk(s), 4 lines now]\n...\n{}|b\n{}|c\n+{}|d",
			line_id(2, "b"),
			line_id(3, "c"),
			line_id(4, "d")
		)
	);
}

fn view_call(path: &std::path::Path, extra: serde_json::Value) -> McpToolCall {
	let mut params = json!({ "path": path.to_string_lossy() });
	for (k, v) in extra.as_object().into_iter().flatten() {
		params[k.as_str()] = v.clone();
	}
	McpToolCall::test_call("view", params)
}

#[tokio::test]
async fn whole_file_reviews_return_unchanged_then_delta() {
	let dir = tempfile::tempdir().unwrap();
	let file = dir.path().join("d.txt");
	let body: String = (1..=20).map(|i| format!("l{i}\n")).collect();
	std::fs::write(&file, &body).unwrap();

	with_request_context(Arc::new(ViewCache::default()), async {
		let first = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert!(
			first.starts_with(&format!("{}|l1\n", line_id(1, "l1"))),
			"{first}"
		);
		assert_eq!(first.lines().count(), 20);

		let second = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert_eq!(
			second,
			"[unchanged since you last viewed or edited it: 20 lines. Pass full: true to re-read.]"
		);

		// Ranged views neither use nor touch the cache.
		let ranged = execute_view(&view_call(&file, json!({ "start": 3, "end": 4 })))
			.await
			.unwrap();
		assert!(
			ranged.starts_with(&format!("{}|l3\n", line_id(3, "l3"))),
			"{ranged}"
		);

		// External change: the next whole view is a delta.
		std::fs::write(&file, body.replace("l10\n", "L10\n")).unwrap();
		let third = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert!(
			third.starts_with("[changed since your last view: 1 hunk(s), 20 lines now]"),
			"{third}"
		);
		assert!(
			third.contains(&format!("-{} (1 line)", line_id(10, "l10"))),
			"{third}"
		);
		assert!(
			third.contains(&format!("+{}|L10", line_id(10, "L10"))),
			"{third}"
		);
		assert_eq!(third.lines().count(), 9);

		// full: true forces the complete file and refreshes the cache.
		let full = execute_view(&view_call(&file, json!({ "full": true })))
			.await
			.unwrap();
		assert_eq!(full.lines().count(), 20);
		assert!(
			full.contains(&format!("{}|L10", line_id(10, "L10"))),
			"{full}"
		);
	})
	.await;
}

#[tokio::test]
async fn own_edits_keep_the_cache_current_but_unseen_changes_invalidate_it() {
	let dir = tempfile::tempdir().unwrap();
	let file = dir.path().join("e.txt");
	let body: String = (1..=10).map(|i| format!("l{i}\n")).collect();
	std::fs::write(&file, &body).unwrap();
	let replace = |old: &str, new: &str| {
		McpToolCall::test_call(
			"text_editor",
			json!({
				"command": "str_replace",
				"path": file.to_string_lossy(),
				"old_text": old,
				"new_text": new
			}),
		)
	};

	with_request_context(Arc::new(ViewCache::default()), async {
		execute_view(&view_call(&file, json!({}))).await.unwrap();
		execute_text_editor(&replace("l3", "L3")).await.unwrap();
		let after_edit = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert!(after_edit.starts_with("[unchanged since"), "{after_edit}");

		// A change the model never saw, followed by an edit that still applies:
		// the cache must not claim the file is known.
		let current = std::fs::read_to_string(&file).unwrap();
		std::fs::write(&file, current.replace("l8\n", "L8\n")).unwrap();
		execute_text_editor(&replace("l5", "L5")).await.unwrap();
		let after = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert!(
			after.starts_with(&format!("{}|l1\n", line_id(1, "l1"))),
			"{after}"
		);
		assert_eq!(after.lines().count(), 10);
	})
	.await;
}

#[tokio::test]
async fn outside_a_request_scope_views_are_always_full() {
	let dir = tempfile::tempdir().unwrap();
	let file = dir.path().join("n.txt");
	std::fs::write(&file, "a\nb\n").unwrap();
	for _ in 0..2 {
		let out = execute_view(&view_call(&file, json!({}))).await.unwrap();
		assert_eq!(out, format!("{}|a\n{}|b", line_id(1, "a"), line_id(2, "b")));
	}
}
