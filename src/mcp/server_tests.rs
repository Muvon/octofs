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
	CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams,
	ClientCapabilities, ClientConfig, ContentBlock, DetailedTask, ErrorCode, GetTaskParams,
	Implementation, ProtocolVersion, ReadResourceRequestParams, SubscribeRequestParams,
	SubscriptionFilter, TaskPayload, TaskStatus,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::json;
use tokio::sync::Mutex;

use super::{OctofsServer, Sessions, MAX_TRACKED_SESSIONS};

/// Minimal client that records unsolicited `resources/updated` pushes and Claude
/// Code channel messages, optionally declaring the SEP-2663 tasks extension.
#[derive(Clone, Debug)]
struct RecordingClient {
	unsolicited: Arc<Mutex<Vec<String>>>,
	channel: Arc<Mutex<Vec<serde_json::Value>>>,
	protocol_version: ProtocolVersion,
	tasks: bool,
}

impl ClientHandler for RecordingClient {
	fn get_info(&self) -> ClientConfig {
		let capabilities = if self.tasks {
			ClientCapabilities::builder().enable_tasks().build()
		} else {
			ClientCapabilities::default()
		};
		ClientConfig::new(
			capabilities,
			Implementation::new("octofs-test-client", "0.0.0"),
		)
		.with_protocol_version(self.protocol_version.clone())
	}

	async fn on_resource_updated(
		&self,
		params: rmcp::model::ResourceUpdatedNotificationParam,
		_context: NotificationContext<RoleClient>,
	) {
		self.unsolicited.lock().await.push(params.uri);
	}

	async fn on_custom_notification(
		&self,
		notification: rmcp::model::CustomNotification,
		_context: NotificationContext<RoleClient>,
	) {
		if notification.method == "notifications/claude/channel" {
			self.channel
				.lock()
				.await
				.push(notification.params.unwrap_or_default());
		}
	}
}

/// Connect a client with the requested protocol lifecycle to a fresh server
/// instance over an in-memory duplex transport.
///
/// The returned JoinHandle keeps the server task — and its RunningService —
/// alive for the test's lifetime: dropping a RunningService cancels the
/// connection, and `serve()` blocks until the handshake completes, so it must
/// run concurrently with the client.
async fn connect_with_lifecycle(
	protocol_version: ProtocolVersion,
	lifecycle: ClientLifecycleMode,
	tasks: bool,
) -> (
	RunningService<RoleClient, RecordingClient>,
	tokio::task::JoinHandle<()>,
	Arc<Mutex<Vec<String>>>,
) {
	let (client_io, server_io) = tokio::io::duplex(4096);
	let server_task = tokio::spawn(async move {
		// A per-test temp root keeps identical background commands from sibling
		// tests independent. The TempDir lives inside this task, so it is cleaned
		// up only after the connection closes.
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
			channel: Arc::new(Mutex::new(Vec::new())),
			protocol_version,
			tasks,
		}
		.serve_with_lifecycle(client_io, lifecycle),
	)
	.await
	.expect("client handshake within 5s")
	.expect("serve client");
	(client, server_task, unsolicited)
}

async fn connect() -> (
	RunningService<RoleClient, RecordingClient>,
	tokio::task::JoinHandle<()>,
	Arc<Mutex<Vec<String>>>,
) {
	connect_with_lifecycle(
		ProtocolVersion::V_2026_07_28,
		ClientLifecycleMode::Auto {
			preferred_versions: vec![ProtocolVersion::V_2026_07_28],
			legacy_version: None,
		},
		false,
	)
	.await
}

async fn connect_legacy() -> (
	RunningService<RoleClient, RecordingClient>,
	tokio::task::JoinHandle<()>,
	Arc<Mutex<Vec<String>>>,
) {
	connect_with_lifecycle(
		ProtocolVersion::V_2025_11_25,
		ClientLifecycleMode::Initialize,
		false,
	)
	.await
}

async fn connect_with_tasks() -> (
	RunningService<RoleClient, RecordingClient>,
	tokio::task::JoinHandle<()>,
	Arc<Mutex<Vec<String>>>,
) {
	connect_with_lifecycle(
		ProtocolVersion::V_2026_07_28,
		ClientLifecycleMode::Auto {
			preferred_versions: vec![ProtocolVersion::V_2026_07_28],
			legacy_version: None,
		},
		true,
	)
	.await
}

/// A ~2s command in each platform's native bounded-wait form; the POSIX loop
/// keeps `sleep` inside the shell guard's documented leniency.
fn two_second_command() -> &'static str {
	if cfg!(target_os = "windows") {
		"ping -n 3 127.0.0.1"
	} else {
		"for i in 1 2; do sleep 1; done"
	}
}

/// Start a ~2s job and return the resource link advertised when the test-only
/// foreground window automatically promotes it.
async fn start_job(client: &RunningService<RoleClient, RecordingClient>) -> String {
	let serde_json::Value::Object(arguments) = json!({"command": two_second_command()}) else {
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

async fn wait_for_job_exit(uri: &str) {
	let id = crate::mcp::fs::background::job_id_from_uri(uri).expect("job URI");
	let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
	loop {
		if matches!(
			crate::mcp::fs::background::status(id),
			Some(crate::mcp::fs::background::JobStatus::Exited(_))
		) {
			return;
		}
		assert!(
			tokio::time::Instant::now() < deadline,
			"background job {uri} never finished"
		);
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}

async fn wait_for_unsolicited(unsolicited: &Arc<Mutex<Vec<String>>>, uri: &str) {
	let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
	loop {
		if unsolicited
			.lock()
			.await
			.iter()
			.any(|notified| notified == uri)
		{
			return;
		}
		assert!(
			tokio::time::Instant::now() < deadline,
			"unsolicited push for {uri} never arrived"
		);
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
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
	wait_for_unsolicited(&unsolicited, &uri).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_job_is_replayed_to_a_late_listen_stream() {
	let (client, _server_task, _unsolicited) = connect().await;
	let uri = start_job(&client).await;
	wait_for_job_exit(&uri).await;

	let mut filter = SubscriptionFilter::new();
	filter.resource_subscriptions = Some(vec![uri.clone()]);
	let mut subscription =
		tokio::time::timeout(Duration::from_secs(5), client.peer().listen(filter))
			.await
			.expect("late listen establishes")
			.expect("late listen succeeds");
	let notification = tokio::time::timeout(Duration::from_secs(5), subscription.next())
		.await
		.expect("finished job is replayed")
		.expect("stream alive")
		.expect("valid notification");
	match notification {
		rmcp::model::ServerNotification::ResourceUpdatedNotification(update) => {
			assert_eq!(update.params.uri, uri);
		}
		other => panic!("expected a resource update, got {other:?}"),
	}
	subscription.cancel().await.expect("cancel subscription");
}

#[tokio::test(flavor = "multi_thread")]
#[allow(deprecated)]
async fn finished_job_is_replayed_to_a_late_legacy_subscription() {
	let (client, _server_task, unsolicited) = connect_legacy().await;
	let uri = start_job(&client).await;
	wait_for_job_exit(&uri).await;
	// Drain the original completion push first. Otherwise it could arrive after
	// `clear` and make the replay assertion pass without the subscribe handler.
	wait_for_unsolicited(&unsolicited, &uri).await;
	unsolicited.lock().await.clear();

	client
		.subscribe(SubscribeRequestParams::new(uri.clone()))
		.await
		.expect("legacy subscription succeeds");
	wait_for_unsolicited(&unsolicited, &uri).await;
}

/// Claude Code wakes its model only on channel messages, so on a pre-2026-07-28
/// connection a job's exit also goes out as one, carrying the job resource text.
#[tokio::test(flavor = "multi_thread")]
async fn job_completion_is_pushed_as_a_claude_channel_message_on_legacy_connections() {
	let (client, _server_task, unsolicited) = connect_legacy().await;
	let info = client.peer_info().expect("server info");
	assert!(
		info.capabilities
			.experimental
			.as_ref()
			.is_some_and(|experimental| experimental.contains_key("claude/channel")),
		"the claude/channel capability must be advertised"
	);
	let uri = start_job(&client).await;
	wait_for_unsolicited(&unsolicited, &uri).await;

	let id = crate::mcp::fs::background::job_id_from_uri(&uri).expect("job URI");
	let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
	let message = loop {
		if let Some(message) = client.service().channel.lock().await.first().cloned() {
			break message;
		}
		assert!(
			tokio::time::Instant::now() < deadline,
			"channel message for {uri} never arrived"
		);
		tokio::time::sleep(Duration::from_millis(100)).await;
	};
	assert_eq!(message["meta"]["job"], id, "{message}");
	let content = message["content"].as_str().expect("text content");
	assert!(content.starts_with(&format!("job {id}\n")), "{content}");
	assert!(content.contains("status: exited with code 0"), "{content}");
}

/// 2026-07-28 carries no unsolicited notifications and Claude Code drops channels
/// on such a connection, so none is sent there.
#[tokio::test(flavor = "multi_thread")]
async fn no_claude_channel_message_on_2026_07_28_connections() {
	let (client, _server_task, unsolicited) = connect().await;
	let uri = start_job(&client).await;
	wait_for_unsolicited(&unsolicited, &uri).await;
	// The channel message would follow the resources/updated push from the same task.
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(client.service().channel.lock().await.is_empty());
}

/// Call `shell` as a tasks client and return the id of the task the promoted job
/// was handed over as.
async fn start_task(client: &RunningService<RoleClient, RecordingClient>, command: &str) -> String {
	let serde_json::Value::Object(arguments) = json!({ "command": command }) else {
		unreachable!("literal object argument")
	};
	let params = CallToolRequestParams::new("shell").with_arguments(arguments);
	match client.call_tool_once(params).await.expect("call shell") {
		CallToolResponse::Task(created) => {
			assert_eq!(created.task.status, TaskStatus::Working);
			assert!(created.task.poll_interval_ms.is_some(), "{created:?}");
			created.task.task_id
		}
		other => panic!("a promoted job is handed over as a task; got {other:?}"),
	}
}

async fn wait_for_terminal_task(
	client: &RunningService<RoleClient, RecordingClient>,
	id: &str,
) -> DetailedTask {
	let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
	loop {
		let result = client
			.get_task(GetTaskParams::new(id))
			.await
			.expect("tasks/get");
		if result.task.status().is_terminal() {
			return result.task;
		}
		assert!(
			tokio::time::Instant::now() < deadline,
			"task {id} never finished"
		);
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}

fn completed_result(task: DetailedTask) -> CallToolResult {
	let TaskPayload::Completed { result } = task.payload else {
		panic!("expected a completed task, got {task:?}");
	};
	serde_json::from_value(serde_json::Value::Object(result)).expect("result is a CallToolResult")
}

fn result_text(result: &CallToolResult) -> String {
	result
		.content
		.iter()
		.find_map(|block| block.as_text().map(|text| text.text.clone()))
		.expect("text result")
}

/// A client that declared the tasks extension gets the promoted job as a task
/// and its result by polling, with no completion pushed at it.
#[tokio::test(flavor = "multi_thread")]
async fn promoted_job_is_handed_to_a_tasks_client_as_a_task() {
	let (client, _server_task, unsolicited) = connect_with_tasks().await;
	let info = client.peer_info().expect("server info");
	assert!(
		info.capabilities.supports_tasks(),
		"the tasks extension must be advertised"
	);
	let id = start_task(&client, two_second_command()).await;

	let task = wait_for_terminal_task(&client, &id).await;
	assert!(
		task.task
			.status_message
			.as_deref()
			.is_some_and(|message| message.starts_with("shell: ")),
		"{task:?}"
	);
	let result = completed_result(task);
	assert_ne!(result.is_error, Some(true), "{result:?}");

	// A push would follow the exit the poll just observed.
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(unsolicited.lock().await.is_empty());
	assert!(client.service().channel.lock().await.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn failed_job_task_result_is_an_error_led_by_the_exit_code() {
	let (client, _server_task, _unsolicited) = connect_with_tasks().await;
	let id = start_task(
		&client,
		"for i in 1 2; do sleep 1; done; echo built; exit 3",
	)
	.await;

	let result = completed_result(wait_for_terminal_task(&client, &id).await);
	assert_eq!(result.is_error, Some(true), "{result:?}");
	let text = result_text(&result);
	assert!(
		text.starts_with("Command failed with exit code 3"),
		"{text}"
	);
	assert!(text.contains("built"), "{text}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn cancelled_task_stops_the_job_and_reports_cancelled() {
	let (client, _server_task, _unsolicited) = connect_with_tasks().await;
	let id = start_task(
		&client,
		"for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do sleep 1; done",
	)
	.await;

	let started = tokio::time::Instant::now();
	client
		.cancel_task(CancelTaskParams::new(id.clone()))
		.await
		.expect("tasks/cancel");
	let task = wait_for_terminal_task(&client, &id).await;
	assert_eq!(task.status(), TaskStatus::Cancelled, "{task:?}");
	assert!(
		started.elapsed() < Duration::from_secs(10),
		"cancel must stop the job, not wait it out"
	);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_task_id_is_invalid_params() {
	let (client, _server_task, _unsolicited) = connect_with_tasks().await;
	for error in [
		client
			.get_task(GetTaskParams::new("0-0"))
			.await
			.expect_err("tasks/get"),
		client
			.cancel_task(CancelTaskParams::new("0-0"))
			.await
			.expect_err("tasks/cancel"),
	] {
		match error {
			rmcp::ServiceError::McpError(error) => {
				assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "{error:?}")
			}
			other => panic!("expected an MCP error, got {other:?}"),
		}
	}
}

/// 2026-07-28 requires `ttlMs` and `cacheScope` on list and read results. A
/// strict client (Claude Code's `server/discover` runtime) rejects results
/// without them and never loads octofs's tools.
#[tokio::test(flavor = "multi_thread")]
async fn results_carry_cache_hints_on_2026_07_28() {
	let (client, _server_task, _unsolicited) = connect().await;

	let tools = client.list_tools(None).await.expect("list tools");
	assert_eq!(tools.ttl_ms, Some(0));
	assert_eq!(tools.cache_scope, Some(CacheScope::Public));

	let uri = start_job(&client).await;
	let resources = client.list_resources(None).await.expect("list resources");
	assert_eq!(resources.ttl_ms, Some(0));
	assert_eq!(resources.cache_scope, Some(CacheScope::Private));

	let read = client
		.read_resource(ReadResourceRequestParams::new(uri))
		.await
		.expect("read job resource");
	assert_eq!(read.ttl_ms, Some(0));
	assert_eq!(read.cache_scope, Some(CacheScope::Private));
}

/// The hints don't exist before 2026-07-28, so legacy results stay unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_results_carry_no_cache_hints() {
	let (client, _server_task, _unsolicited) = connect_legacy().await;

	let tools = client.list_tools(None).await.expect("list tools");
	assert_eq!(tools.ttl_ms, None);
	assert_eq!(tools.cache_scope, None);

	let uri = start_job(&client).await;
	let resources = client.list_resources(None).await.expect("list resources");
	assert_eq!(resources.ttl_ms, None);
	assert_eq!(resources.cache_scope, None);

	let read = client
		.read_resource(ReadResourceRequestParams::new(uri))
		.await
		.expect("read job resource");
	assert_eq!(read.ttl_ms, None);
	assert_eq!(read.cache_scope, None);
}

/// A job link reads through `view` exactly as through `resources/read`: models try
/// `view` on it first, and a client's resource reader may be deferred.
#[tokio::test(flavor = "multi_thread")]
async fn view_reads_a_job_link_like_resources_read() {
	let (client, _server_task, _unsolicited) = connect_legacy().await;
	let uri = start_job(&client).await;
	wait_for_job_exit(&uri).await;

	let read = client
		.read_resource(ReadResourceRequestParams::new(uri.clone()))
		.await
		.expect("read job resource");
	let expected = match &read.contents[0] {
		rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
		other => panic!("job resource is text, got {other:?}"),
	};
	assert!(
		expected.contains("status: exited with code 0"),
		"{expected}"
	);

	let view = |path: String| {
		let serde_json::Value::Object(arguments) = json!({ "path": path }) else {
			unreachable!("literal object argument")
		};
		client.call_tool(CallToolRequestParams::new("view").with_arguments(arguments))
	};
	let result = view(uri.clone()).await.expect("call view");
	assert_ne!(result.is_error, Some(true), "{:?}", result.content);
	let text = result
		.content
		.iter()
		.find_map(|block| match block {
			ContentBlock::Text(text) => Some(text.text.clone()),
			_ => None,
		})
		.expect("text result");
	assert_eq!(text, expected);

	let missing = view("octofs://jobs/0-0".to_string())
		.await
		.expect("call view");
	assert_eq!(missing.is_error, Some(true), "unknown job id is an error");
}

// A model that names one edit in a shape the schema did not anticipate is still
// naming one edit; these coercions keep that from costing a round-trip.
#[test]
fn batch_edit_accepts_content_as_a_list_of_lines() {
	let params: super::BatchEditParams = serde_json::from_value(serde_json::json!({
		"path": "src/lib.rs",
		"operations": [{
			"operation": "replace",
			"start": "7:56",
			"end": "7:56",
			"content": ["use a::B;", "use c::D;"],
		}],
	}))
	.expect("list content is joined");
	assert_eq!(params.operations[0].content, "use a::B;\nuse c::D;");
}

#[test]
fn batch_edit_hoists_a_path_carried_on_every_operation() {
	let params: super::BatchEditParams = serde_json::from_value(serde_json::json!({
		"operations": [
			{"operation": "replace", "start": "7:56", "content": "a", "path": "src/lib.rs"},
			{"operation": "insert", "start": 0, "content": "b", "path": "src/lib.rs"},
		],
	}))
	.expect("a single agreed path is the target");
	assert_eq!(params.path, "src/lib.rs");
	assert_eq!(params.operations.len(), 2);
}

#[test]
fn batch_edit_rejects_operations_naming_different_files() {
	let err = serde_json::from_value::<super::BatchEditParams>(serde_json::json!({
		"operations": [
			{"operation": "replace", "start": "7:56", "content": "a", "path": "src/one.rs"},
			{"operation": "replace", "start": "9:aa", "content": "b", "path": "src/two.rs"},
		],
	}))
	.expect_err("no single target is named");
	assert!(err.to_string().contains("single file"), "got: {err}");
}

#[test]
fn generator_noise_is_stripped_but_parameters_named_like_it_survive() {
	let serde_json::Value::Object(mut schema) = serde_json::json!({
		"$schema": "https://json-schema.org/draft/2020-12/schema",
		"type": "object",
		"properties": {
			"context": {"type": "integer", "format": "uint", "minimum": 0, "default": null},
			"mode": {"type": "string", "default": "fast"},
			"format": {"type": "string", "default": null},
			"start": {"anyOf": [{"type": "string"}, {"type": "integer", "format": "int64"}]},
		},
		"$defs": {"Op": {"type": "integer", "format": "int64"}},
	}) else {
		unreachable!("literal object")
	};
	super::strip_generator_noise(&mut schema);
	assert_eq!(
		serde_json::Value::Object(schema),
		serde_json::json!({
			"type": "object",
			"properties": {
				"context": {"type": "integer", "minimum": 0},
				"mode": {"type": "string", "default": "fast"},
				"format": {"type": "string"},
				"start": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
			},
			"$defs": {"Op": {"type": "integer"}},
		})
	);
}

/// The served tool list carries no generator noise and keeps every parameter.
#[tokio::test(flavor = "multi_thread")]
async fn served_tool_schemas_carry_no_generator_noise() {
	let (client, _server_task, _unsolicited) = connect_legacy().await;
	let tools = client.list_tools(None).await.expect("list tools");
	assert_eq!(tools.tools.len(), 6);
	for tool in &tools.tools {
		let schema = serde_json::to_string(tool.input_schema.as_ref()).expect("schema json");
		assert!(
			!schema.contains("\"$schema\"")
				&& !schema.contains("\"format\"")
				&& !schema.contains("\"default\":null"),
			"{}: {schema}",
			tool.name
		);
	}
	let view = tools
		.tools
		.iter()
		.find(|tool| tool.name == "view")
		.expect("view tool");
	let properties = view.input_schema["properties"]
		.as_object()
		.expect("view properties");
	for name in [
		"path",
		"start",
		"end",
		"pattern",
		"content",
		"regex",
		"max_depth",
		"context",
		"full",
	] {
		assert!(properties.contains_key(name), "view keeps `{name}`");
	}
}

fn session_context(
	peer: &rmcp::Peer<rmcp::RoleServer>,
	id: &str,
	workdir: &std::path::Path,
) -> rmcp::service::RequestContext<rmcp::RoleServer> {
	let mut context =
		rmcp::service::RequestContext::new(rmcp::model::RequestId::Number(1), peer.clone());
	let capabilities: ClientCapabilities = serde_json::from_value(json!({
		"experimental": {"session": {"session_id": id, "workdir": workdir}}
	}))
	.expect("session capabilities");
	context.meta.set_client_capabilities(capabilities);
	context
}

/// Octomind shares one connection between its sessions: each keeps its own
/// anchor, `workdir` override and view cache.
#[tokio::test]
async fn sessions_sharing_a_connection_keep_their_own_workdir() {
	let root = tempfile::tempdir().expect("server root");
	let first = tempfile::tempdir().expect("first workdir");
	let second = tempfile::tempdir().expect("second workdir");
	let nested = first.path().join("nested");
	std::fs::create_dir(&nested).expect("nested dir");
	let server = OctofsServer::with_root(root.path().to_path_buf());
	let (transport, _client) = tokio::io::duplex(4096);
	let service = rmcp::service::serve_directly(server.clone(), transport, None);
	let peer = service.peer();

	let a = server.session(&session_context(peer, "a", first.path()));
	let b = server.session(&session_context(peer, "b", second.path()));
	assert_eq!(a.workdir.get_current(), first.path());
	assert_eq!(b.workdir.get_current(), second.path());
	assert!(!Arc::ptr_eq(&a.view_cache, &b.view_cache));

	a.workdir.set_current(nested.clone());
	let again = server.session(&session_context(peer, "a", first.path()));
	assert_eq!(again.workdir.get_current(), nested);
	let other = server.session(&session_context(peer, "b", second.path()));
	assert_eq!(other.workdir.get_current(), second.path());

	let moved = server.session(&session_context(peer, "a", second.path()));
	assert_eq!(
		moved.workdir.get_current(),
		second.path(),
		"a moved anchor starts over"
	);

	let anonymous = server.session(&rmcp::service::RequestContext::new(
		rmcp::model::RequestId::Number(2),
		peer.clone(),
	));
	assert_eq!(anonymous.workdir.get_current(), root.path());
}

#[test]
fn least_recently_used_session_is_dropped_at_capacity() {
	let sessions = Sessions::default();
	let root = std::path::PathBuf::from("/");
	let first = sessions.get(Some("0".to_string()), root.clone());
	for index in 1..MAX_TRACKED_SESSIONS {
		sessions.get(Some(index.to_string()), root.clone());
	}
	let touched = sessions.get(Some("0".to_string()), root.clone());
	assert!(Arc::ptr_eq(&first, &touched));

	sessions.get(Some("new".to_string()), root);
	let states = sessions.states.lock().expect("sessions lock");
	assert_eq!(states.len(), MAX_TRACKED_SESSIONS);
	assert!(states.contains_key(&Some("0".to_string())));
	assert!(!states.contains_key(&Some("1".to_string())));
}
