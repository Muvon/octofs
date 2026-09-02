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
// a RequestContext to that call's task via tokio::task_local. It carries the call's
// hints — tool-misuse guidance collected during THIS call and appended to its result —
// and a handle to the session's delta-view cache so any fs function can reach it
// without threading a parameter through every executor.
// Request-scoped by construction: a hint can never leak into a concurrent HTTP
// session's response, and hints from a failed call die with its scope instead of
// surfacing on the next call.
//
// Staleness is NOT tracked here: edit targets are composite line ids ("N:hh") whose
// content hash is verified against the file at apply time, so an external change is
// caught per-target with a self-healing error instead of a whole-file fingerprint gate.
//
// Outside a scope (unit tests calling executors directly) every accessor degrades to
// a no-op: hints are dropped and views are always served in full.

use std::cell::RefCell;
use std::sync::Arc;

use super::fs::delta::ViewCache;

struct RequestContext {
	hints: RefCell<Vec<String>>,
	view_cache: Arc<ViewCache>,
}

tokio::task_local! {
	static CTX: RequestContext;
}

/// Run `fut` with a fresh request context bound to the session's view cache.
pub async fn with_request_context<F>(view_cache: Arc<ViewCache>, fut: F) -> F::Output
where
	F: std::future::Future,
{
	CTX.scope(
		RequestContext {
			hints: RefCell::new(Vec::new()),
			view_cache,
		},
		fut,
	)
	.await
}

/// Run `f` against the session's view cache. `None` outside a request scope.
pub fn with_view_cache<R>(f: impl FnOnce(&ViewCache) -> R) -> Option<R> {
	CTX.try_with(|ctx| f(&ctx.view_cache)).ok()
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
