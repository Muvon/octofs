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

//! Tests for the minimal stderr tracing subscriber and HTTP shutdown in main.rs.

use super::{
	await_http_shutdown_signal, cancel_http_after_shutdown, http_shutdown_includes_sigterm, init_mcp_logging,
	StderrSubscriber,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
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
	let dispatch = tracing::dispatcher::Dispatch::new(StderrSubscriber { level: Level::ERROR });
	tracing::dispatcher::with_default(&dispatch, || {
		tracing::warn!("this must not be emitted");
	});
}

#[test]
fn http_shutdown_listens_for_sigterm_on_unix_only() {
	#[cfg(unix)]
	assert!(http_shutdown_includes_sigterm());
	#[cfg(not(unix))]
	assert!(!http_shutdown_includes_sigterm());
}

#[tokio::test]
async fn http_shutdown_completes_when_interrupt_fires() {
	let interrupt = Arc::new(Notify::new());
	let terminate = Arc::new(Notify::new());
	let interrupt_wait = Arc::clone(&interrupt);
	let terminate_wait = Arc::clone(&terminate);
	let wait = tokio::spawn(async move {
		await_http_shutdown_signal(interrupt_wait.notified(), terminate_wait.notified()).await;
	});
	interrupt.notify_one();
	tokio::time::timeout(Duration::from_secs(2), wait)
		.await
		.expect("interrupt should finish the shutdown wait")
		.expect("shutdown task");
}

#[tokio::test]
async fn http_shutdown_sigterm_follows_platform_decision() {
	// Notify stands in for SIGTERM. The test process is never signalled.
	let interrupt = Arc::new(Notify::new());
	let terminate = Arc::new(Notify::new());
	let interrupt_wait = Arc::clone(&interrupt);
	let terminate_wait = Arc::clone(&terminate);
	let mut wait = tokio::spawn(async move {
		await_http_shutdown_signal(interrupt_wait.notified(), terminate_wait.notified()).await;
	});
	terminate.notify_one();
	if http_shutdown_includes_sigterm() {
		tokio::time::timeout(Duration::from_secs(2), &mut wait)
			.await
			.expect("SIGTERM should finish the shutdown wait")
			.expect("shutdown task");
	} else {
		let timed_out = tokio::time::timeout(Duration::from_millis(200), &mut wait)
			.await
			.is_err();
		wait.abort();
		assert!(timed_out);
	}
}

#[tokio::test]
async fn http_shutdown_cancels_token_after_the_wait() {
	let ct = tokio_util::sync::CancellationToken::new();
	let notify = Arc::new(Notify::new());
	let notify_wait = Arc::clone(&notify);
	let ct_wait = ct.clone();
	let wait = tokio::spawn(async move {
		cancel_http_after_shutdown(notify_wait.notified(), &ct_wait).await;
	});
	assert!(!ct.is_cancelled());
	notify.notify_one();
	tokio::time::timeout(Duration::from_secs(2), wait)
		.await
		.expect("shutdown wait should finish")
		.expect("shutdown task");
	assert!(ct.is_cancelled());
}
