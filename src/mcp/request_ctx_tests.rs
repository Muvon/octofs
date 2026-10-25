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

//! Unit tests for request-scoped hints and view cache access.

use std::sync::Arc;

use tokio::sync::oneshot;

use super::{drain_hints, push_hint, with_request_context, with_view_cache};
use crate::mcp::fs::delta::ViewCache;

#[tokio::test]
async fn drain_hints_dedupes_in_insertion_order_then_empties() {
	with_request_context(None, async {
		push_hint("a");
		push_hint("b");
		push_hint("a");
		assert_eq!(drain_hints(), vec!["a", "b"]);
		assert!(drain_hints().is_empty());
	})
	.await;
}

#[test]
fn outside_scope_drain_hints_is_noop() {
	push_hint("a");
	assert!(drain_hints().is_empty());
}

#[tokio::test]
async fn concurrent_scopes_dont_leak_hints() {
	let (right_pushed_tx, right_pushed_rx) = oneshot::channel::<()>();
	let (left_drained_tx, left_drained_rx) = oneshot::channel::<()>();

	let left = with_request_context(None, async move {
		push_hint("left");
		right_pushed_rx.await.expect("right pushed its hint");
		let hints = drain_hints();
		let _ = left_drained_tx.send(());
		hints
	});
	let right = with_request_context(None, async move {
		push_hint("right");
		let _ = right_pushed_tx.send(());
		left_drained_rx.await.expect("left drained its hints");
		drain_hints()
	});

	let (left_hints, right_hints) = tokio::join!(left, right);
	assert_eq!(left_hints, vec!["left"]);
	assert_eq!(right_hints, vec!["right"]);
}

#[tokio::test]
async fn with_view_cache_returns_none_outside_and_without_cache_some_with_cache() {
	assert!(with_view_cache(|_| ()).is_none());

	with_request_context(None, async {
		assert!(with_view_cache(|_| ()).is_none());
	})
	.await;
	with_request_context(Some(Arc::new(ViewCache::default())), async {
		assert!(with_view_cache(|_| ()).is_some());
	})
	.await;
}
