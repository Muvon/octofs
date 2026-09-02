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

//! Delta views — a whole-file `view` of a file this session already served
//! returns only what changed since, in the same diff style as edit results.
//!
//! The cache holds, per session and per file, the last content the model saw
//! in full. Whole-file views set it; this server's own writes keep it in step
//! (see `note_write`); ranged views and searches leave it alone because they
//! show the model only part of the file.

use std::collections::HashMap;
use std::sync::Mutex;

use super::text_editing::lock_key_for_source;
use super::PathSource;
use crate::mcp::request_ctx::with_view_cache;
use crate::utils::line_hash::line_id_at;
use crate::utils::truncation::format_content_with_line_numbers;

/// Files above this size are served in full every time: holding multi-MB
/// artifacts per session is not worth the delta.
const MAX_CACHED_BYTES: usize = 1024 * 1024;
/// Context lines around each hunk, matching edit diffs.
const CONTEXT: usize = 2;
/// Edit-distance ceiling for a delta. Beyond it the full file is about as
/// short, and the Myers trace (O(D²) memory) would start to cost.
const MAX_EDIT_DISTANCE: usize = 500;

/// Last full content served per file, keyed like the file locks.
// ponytail: unbounded per session (1MB cap per entry); add LRU eviction if a
// session ever views thousands of large files.
#[derive(Debug, Default)]
pub struct ViewCache {
	entries: Mutex<HashMap<String, String>>,
}

impl ViewCache {
	fn get(&self, key: &str) -> Option<String> {
		self.entries.lock().ok()?.get(key).cloned()
	}

	fn set(&self, key: String, content: String) {
		let Ok(mut entries) = self.entries.lock() else {
			return;
		};
		if content.len() > MAX_CACHED_BYTES {
			entries.remove(&key);
		} else {
			entries.insert(key, content);
		}
	}

	fn forget(&self, key: &str) {
		if let Ok(mut entries) = self.entries.lock() {
			entries.remove(key);
		}
	}
}

/// Line-ending agnostic form: views and edits both work on `lines()`.
fn normalize(content: &str) -> String {
	content.replace("\r\n", "\n")
}

/// Record a write this server performed. The entry stays valid only if the
/// model's picture was current before the write (`before` matches the cache);
/// otherwise it is dropped so the next whole-file view is served in full.
pub fn note_write(source: &PathSource, before: &str, after: &str) {
	let key = lock_key_for_source(source);
	let before = normalize(before);
	with_view_cache(|cache| {
		if cache.get(&key).as_deref() == Some(before.as_str()) {
			cache.set(key.clone(), normalize(after));
		} else {
			cache.forget(&key);
		}
	});
}

/// Record a file the model created: it authored every line.
pub fn note_create(source: &PathSource, content: &str) {
	let key = lock_key_for_source(source);
	with_view_cache(|cache| cache.set(key.clone(), normalize(content)));
}

/// Drop the entry for a file whose content the model can no longer predict.
pub fn forget(source: &PathSource) {
	let key = lock_key_for_source(source);
	with_view_cache(|cache| cache.forget(&key));
}

/// Render a whole-file view: the full file the first time (or when `full`),
/// afterwards only the hunks changed since the model last saw it.
pub fn render_whole_file(source: &PathSource, content: &str, full: bool) -> String {
	let key = lock_key_for_source(source);
	let current = normalize(content);
	let previous = if full {
		None
	} else {
		with_view_cache(|cache| cache.get(&key)).flatten()
	};
	with_view_cache(|cache| cache.set(key.clone(), current.clone()));

	let new_lines: Vec<&str> = current.lines().collect();
	match previous {
		Some(prev) if prev == current => format!(
			"[unchanged since you last viewed or edited it: {} lines. Pass full=true to re-read.]",
			new_lines.len()
		),
		Some(prev) => {
			let old_lines: Vec<&str> = prev.lines().collect();
			match render_delta(&old_lines, &new_lines) {
				Some(delta) => delta,
				None => format!(
					"[changed since your last view: too many changes for a delta, full content follows]\n{}",
					format_content_with_line_numbers(&new_lines, 1, None)
				),
			}
		}
		None => format_content_with_line_numbers(&new_lines, 1, None),
	}
}

/// Hunks with fresh ids for kept/added lines; removed lines collapse to their
/// old id range since the model already saw them. `None` when the change is
/// too large for a delta to pay off.
pub fn render_delta(old: &[&str], new: &[&str]) -> Option<String> {
	let ops = diff_lines(old, new, MAX_EDIT_DISTANCE)?;

	// Hunks are op-index ranges around each change, merged when their context touches.
	let mut hunks: Vec<(usize, usize)> = Vec::new();
	for (i, op) in ops.iter().enumerate() {
		if matches!(op, DiffOp::Equal(..)) {
			continue;
		}
		let start = i.saturating_sub(CONTEXT);
		let end = (i + CONTEXT).min(ops.len() - 1);
		match hunks.last_mut() {
			Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
			_ => hunks.push((start, end)),
		}
	}

	let mut out = vec![format!(
		"[changed since your last view: {} hunk(s), {} lines now]",
		hunks.len(),
		new.len()
	)];
	for &(start, end) in &hunks {
		if start > 0 {
			out.push("...".to_string());
		}
		let mut i = start;
		while i <= end {
			match ops[i] {
				DiffOp::Equal(_, j) => out.push(format!("{}|{}", line_id_at(new, j + 1), new[j])),
				DiffOp::Insert(j) => out.push(format!("+{}|{}", line_id_at(new, j + 1), new[j])),
				DiffOp::Delete(first) => {
					let mut last = first;
					while i < end {
						let DiffOp::Delete(k) = ops[i + 1] else { break };
						last = k;
						i += 1;
					}
					out.push(removed_marker(old, first, last));
				}
			}
			i += 1;
		}
	}
	if hunks.last().is_some_and(|&(_, end)| end + 1 < ops.len()) {
		out.push("...".to_string());
	}
	Some(out.join("\n"))
}

/// Removed lines as an old-id range (0-based inclusive indices into `old`).
pub fn removed_marker(old: &[&str], first: usize, last: usize) -> String {
	if first == last {
		format!("-{} (1 line)", line_id_at(old, first + 1))
	} else {
		format!(
			"-{}..{} ({} lines)",
			line_id_at(old, first + 1),
			line_id_at(old, last + 1),
			last - first + 1
		)
	}
}

/// One step of a line diff; indices are 0-based into `old` / `new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOp {
	Equal(usize, usize),
	Delete(usize),
	Insert(usize),
}

/// Myers line diff, O((N+M)·D). `None` once the edit distance exceeds `max_d`.
pub fn diff_lines(old: &[&str], new: &[&str], max_d: usize) -> Option<Vec<DiffOp>> {
	let n = old.len();
	let m = new.len();
	let max_d = max_d.min(n + m);
	// Diagonal k = x - y lives at v[k + offset]; the spare slots keep k±1 in range.
	let offset = max_d as i64 + 1;
	let at = |k: i64| (k + offset) as usize;
	let mut v = vec![0usize; 2 * max_d + 3];
	let mut trace: Vec<Vec<usize>> = Vec::new();

	for d in 0..=max_d as i64 {
		trace.push(v.clone());
		let mut k = -d;
		while k <= d {
			let down = k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]);
			let mut x = if down { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
			let mut y = (x as i64 - k) as usize;
			while x < n && y < m && old[x] == new[y] {
				x += 1;
				y += 1;
			}
			v[at(k)] = x;
			if x >= n && y >= m {
				return Some(backtrack(&trace, n, m, offset));
			}
			k += 2;
		}
	}
	None
}

fn backtrack(trace: &[Vec<usize>], n: usize, m: usize, offset: i64) -> Vec<DiffOp> {
	let at = |k: i64| (k + offset) as usize;
	let mut ops = Vec::new();
	let (mut x, mut y) = (n, m);
	for (d, v) in trace.iter().enumerate().rev() {
		let d = d as i64;
		let k = x as i64 - y as i64;
		let prev_k = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
			k + 1
		} else {
			k - 1
		};
		let prev_x = v[at(prev_k)];
		let prev_y = prev_x as i64 - prev_k;
		while x > prev_x && y as i64 > prev_y {
			x -= 1;
			y -= 1;
			ops.push(DiffOp::Equal(x, y));
		}
		if d > 0 {
			if x == prev_x {
				ops.push(DiffOp::Insert(y - 1));
			} else {
				ops.push(DiffOp::Delete(x - 1));
			}
			x = prev_x;
			y = prev_y as usize;
		}
	}
	ops.reverse();
	ops
}

#[cfg(test)]
#[path = "delta_tests.rs"]
mod delta_tests;
