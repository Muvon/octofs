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

// Per-request task-local context.
//
// Every fs tool call runs inside `with_request_context` (see server.rs), which scopes
// a RequestContext to that call's task via tokio::task_local. Two things live here:
//
//   hints  — tool-misuse guidance collected during THIS call and appended to its
//            result. Request-scoped by construction: a hint can never leak into a
//            concurrent HTTP session's response, and hints from a failed call die
//            with its scope instead of surfacing on the next call.
//   stamps — a handle to the SESSION's file fingerprints: (mtime, len) recorded when
//            the model last saw a file's content. Edit tools call ensure_not_stale to
//            fail fast when a file changed on disk since it was viewed — the same
//            timestamp check vim's `checktime` and VSCode's dirty-file tracking use.
//
// Outside a scope (unit tests calling executors directly) every accessor degrades to
// a no-op: hints are dropped, staleness is not enforced.

use anyhow::{bail, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Session-wide file fingerprints keyed by canonical path (see `lock_key_for`).
pub type FileStamps = Arc<Mutex<HashMap<String, (SystemTime, u64)>>>;

struct RequestContext {
	hints: RefCell<Vec<String>>,
	stamps: FileStamps,
}

tokio::task_local! {
	static CTX: RequestContext;
}

/// Run `fut` with a fresh request context bound to the given session stamps.
pub async fn with_request_context<F>(stamps: FileStamps, fut: F) -> F::Output
where
	F: std::future::Future,
{
	CTX.scope(
		RequestContext {
			hints: RefCell::new(Vec::new()),
			stamps,
		},
		fut,
	)
	.await
}

/// Push a hint for the current tool call. No-op outside a request scope.
pub fn push_hint(hint: &str) {
	let _ = CTX.try_with(|ctx| ctx.hints.borrow_mut().push(hint.to_string()));
}

/// Drain this call's hints, deduplicated in insertion order.
pub fn drain_hints() -> Vec<String> {
	CTX.try_with(|ctx| {
		let mut hints = ctx.hints.borrow_mut();
		let mut seen = std::collections::HashSet::new();
		hints.drain(..).filter(|h| seen.insert(h.clone())).collect()
	})
	.unwrap_or_default()
}

/// (mtime, len) is what editors use for external-change detection: cheap (one stat)
/// and reliable — a content hash would need a full read on every check.
fn fingerprint(path: &Path) -> Option<(SystemTime, u64)> {
	let meta = std::fs::metadata(path).ok()?;
	Some((meta.modified().ok()?, meta.len()))
}

/// Record `path`'s current fingerprint as "the model has seen this content".
/// Called after a successful file view and after every successful write.
pub fn record_stamp(path: &Path) {
	let key = crate::mcp::fs::text_editing::lock_key_for(path);
	let _ = CTX.try_with(|ctx| {
		let mut stamps = ctx.stamps.lock().expect("stamps poisoned");
		match fingerprint(path) {
			Some(fp) => {
				stamps.insert(key, fp);
			}
			None => {
				stamps.remove(&key);
			}
		}
	});
}

/// Forget `path`'s fingerprint (file deleted).
pub fn forget_stamp(path: &Path) {
	let key = crate::mcp::fs::text_editing::lock_key_for(path);
	let _ = CTX.try_with(|ctx| ctx.stamps.lock().expect("stamps poisoned").remove(&key));
}

/// Fail if `path` changed on disk since the model last saw it.
/// Paths with no recorded stamp pass — editing without a prior view stays legal.
pub fn ensure_not_stale(path: &Path) -> Result<()> {
	let key = crate::mcp::fs::text_editing::lock_key_for(path);
	let recorded = CTX
		.try_with(|ctx| {
			ctx.stamps
				.lock()
				.expect("stamps poisoned")
				.get(&key)
				.copied()
		})
		.ok()
		.flatten();
	let Some(recorded) = recorded else {
		return Ok(());
	};
	if fingerprint(path) != Some(recorded) {
		bail!(
			"File changed on disk since it was last viewed (external edit — another program or a shell command). Run `view` on '{}' again, then retry.",
			path.display()
		);
	}
	Ok(())
}
