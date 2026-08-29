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

//! End-to-end delivery tests for background-job completion, over a real
//! in-memory MCP connection (duplex transport, full rmcp client + server).
//!
//! Covers both delivery paths a client can end up on:
//! - the 2026-07-28 contract path: a `subscriptions/listen` stream carries the
//!   update, and no unsolicited duplicate is sent;
//! - the legacy fallback: with no listen stream, the unsolicited
//!   `resources/updated` push is delivered to the client handler — what
//!   pre-2026-07-28 clients receive.

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
	CallToolRequestParams, ClientCapabilities, ClientInfo, ContentBlock, Implementation,
	ProtocolVersion, SubscriptionFilter,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::json;
use tokio::sync::Mutex;

use super::OctofsServer;

/// Minimal client that records unsolicited `resources/updated` pushes.
#[derive(Clone, Debug)]
struct RecordingClient {
	unsolicited: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for RecordingClient {
	fn get_info(&self) -> ClientInfo {
		ClientInfo::new(
			ClientCapabilities::default(),
			Implementation::new("octofs-test-client", "0.0.0"),
		)
		.with_protocol_version(ProtocolVersion::V_2026_07_28)
	}

	async fn on_resource_updated(
		&self,
		params: rmcp::model::ResourceUpdatedNotificationParam,
		_context: NotificationContext<RoleClient>,
	) {
		self.unsolicited.lock().await.push(params.uri);
	}
}

/// Connect a client (modern discover lifecycle, as octomind connects) to a
/// fresh server instance over an in-memory duplex transport.
///
/// The returned JoinHandle keeps the server task — and its RunningService —
/// alive for the test's lifetime: dropping a RunningService cancels the
/// connection, and `serve()` blocks until the handshake completes, so it must
/// run concurrently with the client.
async fn connect() -> (
	RunningService<RoleClient, RecordingClient>,
	tokio::task::JoinHandle<()>,
	Arc<Mutex<Vec<String>>>,
) {
	let (client_io, server_io) = tokio::io::duplex(4096);
	let server_task = tokio::spawn(async move {
		// A per-test temp root: background jobs are serialized per working
		// directory, so sibling tests sharing the session root would reject
		// each other's `sleep` jobs. The TempDir lives inside this task, so it
		// is cleaned up only after the connection closes.
		let root = tempfile::tempdir().expect("temp session root");
		let server = OctofsServer::with_root(root.path().to_path_buf())
			.serve(server_io)
			.await
			.expect("serve server");
		server.waiting().await.expect("server connection closed");
	});
	let unsolicited = Arc::new(Mutex::new(Vec::new()));
	let client = tokio::time::timeout(
		Duration::from_secs(5),
		RecordingClient {
			unsolicited: unsolicited.clone(),
		}
		.serve_with_lifecycle(
			client_io,
			ClientLifecycleMode::Auto {
				preferred_versions: vec![ProtocolVersion::V_2026_07_28],
				legacy_version: None,
			},
		),
	)
	.await
	.expect("client handshake within 5s")
	.expect("serve client");
	(client, server_task, unsolicited)
}

/// Start a ~2s job and return the resource link advertised when the test-only
/// foreground window automatically promotes it.
/// The loop form keeps `sleep` inside a loop body — the shell guard's
/// documented leniency — so the job command itself isn't rejected.
async fn start_job(client: &RunningService<RoleClient, RecordingClient>) -> String {
	let serde_json::Value::Object(arguments) = json!({"command": "for i in 1 2; do sleep 1; done"})
	else {
		unreachable!("literal object argument")
	};
	let params = CallToolRequestParams::new("shell").with_arguments(arguments);
	let result = client.call_tool(params).await.expect("call shell");
	result
		.content
		.iter()
		.find_map(|block| match block {
			ContentBlock::ResourceLink(link) => Some(link.uri.clone()),
			_ => None,
		})
		.unwrap_or_else(|| {
			panic!(
				"promoted shell advertises a resource link; got is_error={:?} content={:?}",
				result.is_error, result.content
			)
		})
}

#[tokio::test(flavor = "multi_thread")]
async fn job_completion_is_delivered_on_the_listen_stream() {
	let (client, _server_task, unsolicited) = connect().await;
	let uri = start_job(&client).await;

	let mut filter = SubscriptionFilter::new();
	filter.resource_subscriptions = Some(vec![uri.clone()]);
	let mut subscription =
		tokio::time::timeout(Duration::from_secs(5), client.peer().listen(filter))
			.await
			.expect("listen establishes")
			.expect("listen succeeds");
	assert!(
		subscription
			.acknowledged()
			.resource_subscriptions
			.as_ref()
			.is_some_and(|uris| uris.contains(&uri)),
		"server must acknowledge the requested URI"
	);

	let notification = tokio::time::timeout(Duration::from_secs(15), subscription.next())
		.await
		.expect("update arrives on the stream")
		.expect("stream alive")
		.expect("valid notification");
	match notification {
		rmcp::model::ServerNotification::ResourceUpdatedNotification(update) => {
			assert_eq!(update.params.uri, uri);
		}
		other => panic!("expected a resource update, got {other:?}"),
	}
	assert!(
		unsolicited.lock().await.is_empty(),
		"stream delivery must not duplicate as an unsolicited push"
	);
	subscription.cancel().await.expect("cancel subscription");
}

#[tokio::test(flavor = "multi_thread")]
async fn job_completion_falls_back_to_unsolicited_push_without_a_stream() {
	let (client, _server_task, unsolicited) = connect().await;
	let uri = start_job(&client).await;

	// No listen stream — the server must fall back to the unsolicited push
	// that pre-2026-07-28 clients rely on.
	let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
	loop {
		if unsolicited.lock().await.iter().any(|u| u == &uri) {
			break;
		}
		assert!(
			tokio::time::Instant::now() < deadline,
			"unsolicited push for {uri} never arrived"
		);
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}
