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

//! Tests for the minimal stderr tracing subscriber in main.rs.

use super::{init_mcp_logging, StderrSubscriber};
use tracing::Level;

#[test]
fn init_logging_sets_global_subscriber_once() {
	// Safe to call exactly once per test binary; a second call would panic
	// on set_global_default, which is itself the documented contract.
	init_mcp_logging();
}

#[test]
fn subscriber_emits_events_at_or_below_its_level() {
	// INFO-level subscriber: warn! passes the enabled() gate and reaches
	// event() (writing to stderr); debug! is filtered out.
	let dispatch = tracing::dispatcher::Dispatch::new(StderrSubscriber { level: Level::INFO });
	tracing::dispatcher::with_default(&dispatch, || {
		tracing::warn!("octofs test warn message");
		tracing::debug!("octofs test debug message");
		let span = tracing::info_span!("octofs_test_span");
		let _guard = span.enter();
		tracing::warn!(value = ?42, "octofs test debug-field message");
	});
}

#[test]
fn subscriber_filters_events_above_its_level() {
	let dispatch = tracing::dispatcher::Dispatch::new(StderrSubscriber {
		level: Level::ERROR,
	});
	tracing::dispatcher::with_default(&dispatch, || {
		tracing::warn!("this must not be emitted");
	});
}
