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

use std::sync::{Arc, RwLock};

use rmcp::{
	handler::server::{wrapper::Parameters, ServerHandler},
	model::{
		CallToolResult, ContentBlock, Implementation, ListResourcesResult, ListToolsResult,
		PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
		ReadResourceResult, RequestId, Resource, ResourceContents, ServerCapabilities, ServerInfo,
		SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
	},
	schemars,
	service::{Peer, RequestContext, SubscriptionContext, SubscriptionSink},
	tool, tool_handler, tool_router, ErrorData, RoleServer,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::fs;
use super::request_ctx;
use super::McpToolCall;

/// Per-session working directory state.
/// Each server instance has its own workdir, isolated from other sessions.
#[derive(Debug)]
pub struct SessionWorkdir {
	/// The session root directory (set at session creation, never changes).
	pub root: PathBuf,
	/// The current working directory (can be changed via workdir tool).
	pub current: RwLock<Option<PathBuf>>,
}

impl SessionWorkdir {
	pub fn new(root: PathBuf) -> Self {
		Self {
			root,
			current: RwLock::new(None),
		}
	}

	/// Get the current working directory, or the root if not set.
	pub fn get_current(&self) -> PathBuf {
		self.current
			.read()
			.ok()
			.and_then(|guard| guard.clone())
			.unwrap_or_else(|| self.root.clone())
	}

	/// Set the current working directory.
	pub fn set_current(&self, path: PathBuf) {
		if let Ok(mut guard) = self.current.write() {
			*guard = Some(path);
		}
	}

	/// Reset to the session root.
	pub fn reset(&self) {
		if let Ok(mut guard) = self.current.write() {
			*guard = None;
		}
	}
}

/// Active `subscriptions/listen` streams opened by this session's client.
///
/// Sinks are registered when a client listens and removed when the stream
/// ends (client cancellation, transport close, or graceful teardown).
/// Background-job completion is delivered through matching sinks — the
/// 2026-07-28 contract path — and falls back to an unsolicited push for
/// clients that never opened a stream (see the `shell` tool's notifier).
#[derive(Debug, Default)]
struct SubscriptionRegistry {
	/// The guard is never held across an `.await`: sinks are cloned out first.
	sinks: std::sync::Mutex<Vec<SubscriptionSink>>,
}

impl SubscriptionRegistry {
	fn register(&self, sink: SubscriptionSink) {
		if let Ok(mut sinks) = self.sinks.lock() {
			sinks.push(sink);
		}
	}

	fn unregister(&self, id: &RequestId) {
		if let Ok(mut sinks) = self.sinks.lock() {
			sinks.retain(|sink| sink.id() != id);
		}
	}

	/// Sinks whose accepted filter covers `uri`, cloned out so no lock is held
	/// while sending.
	fn sinks_for(&self, uri: &str) -> Vec<SubscriptionSink> {
		self.sinks
			.lock()
			.map(|sinks| {
				sinks
					.iter()
					.filter(|sink| {
						sink.accepted()
							.resource_subscriptions
							.as_ref()
							.is_some_and(|uris| uris.iter().any(|u| u == uri))
					})
					.cloned()
					.collect()
			})
			.unwrap_or_default()
	}
}

fn background_job_finished(uri: &str) -> bool {
	fs::background::job_id_from_uri(uri)
		.and_then(fs::background::status)
		.is_some_and(|status| matches!(status, fs::background::JobStatus::Exited(_)))
}

/// Deliver a completion to every matching live subscription. Failed sinks are
/// removed immediately so they cannot suppress the legacy peer fallback.
async fn notify_subscriptions(subscriptions: &SubscriptionRegistry, uri: &str) -> bool {
	let mut delivered = false;
	for sink in subscriptions.sinks_for(uri) {
		let id = sink.id().clone();
		match sink.notify_resource_updated(uri).await {
			Ok(()) => delivered = true,
			Err(error) => {
				debug!("subscription delivery for {uri} failed: {error}");
				subscriptions.unregister(&id);
			}
		}
	}
	delivered
}

async fn notify_peer_resource_updated(peer: &Peer<RoleServer>, uri: &str) {
	if let Err(error) = peer
		.notify_resource_updated(rmcp::model::ResourceUpdatedNotificationParam::new(uri))
		.await
	{
		warn!(
			"background job notification for {uri} could not be delivered: {error}; \
			 completion remains available through the job resource"
		);
	}
}

/// How often a running foreground `shell` command reports liveness. Well below
/// any sane client idle timeout so a single missed beat cannot cancel the call.
const SHELL_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// MCP server with per-session working directory isolation.
#[derive(Debug, Clone)]
pub struct OctofsServer {
	/// Per-session working directory state.
	workdir: Arc<SessionWorkdir>,
	/// Whether the session context (workdir) has been applied from client
	/// capabilities. Applied once on the first tool call so a later `workdir`
	/// tool change is not overwritten by subsequent requests.
	session_applied: Arc<std::sync::atomic::AtomicBool>,
	/// Listen streams opened by this client, for contract-clean delivery of
	/// background-job completion notifications.
	subscriptions: Arc<SubscriptionRegistry>,
	/// Last full content served per file, so repeat whole-file views can
	/// return only what changed.
	view_cache: Arc<fs::delta::ViewCache>,
}

impl OctofsServer {
	/// Create a new server instance with the given session root directory.
	pub fn new() -> Self {
		Self::with_root(super::get_session_root_directory())
	}

	/// Create a server instance rooted at an explicit directory. HTTP sessions
	/// each build their own instance this way; tests use it to isolate the
	/// per-cwd background-job guard from sibling tests.
	pub fn with_root(root: PathBuf) -> Self {
		Self {
			workdir: Arc::new(SessionWorkdir::new(root)),
			session_applied: Arc::new(std::sync::atomic::AtomicBool::new(false)),
			subscriptions: Arc::new(SubscriptionRegistry::default()),
			view_cache: Arc::new(fs::delta::ViewCache::default()),
		}
	}

	/// Apply the octomind session context from client capabilities
	/// (`experimental.session.workdir`) on the first tool call.
	///
	/// Works for both protocol eras: modern clients (2026-07-28) carry
	/// capabilities in every request's `_meta`, legacy clients set them during
	/// the `initialize` handshake — `RequestContext::client_capabilities()`
	/// resolves both.
	fn ensure_session_workdir(&self, context: &RequestContext<RoleServer>) {
		if self
			.session_applied
			.swap(true, std::sync::atomic::Ordering::SeqCst)
		{
			return;
		}
		let Some(capabilities) = context.client_capabilities() else {
			return;
		};
		let Some(experimental) = &capabilities.experimental else {
			return;
		};
		if let Some(session_obj) = experimental.get("session") {
			if let Some(workdir_str) = session_obj.get("workdir").and_then(|v| v.as_str()) {
				let path = std::path::PathBuf::from(workdir_str);
				if path.is_absolute() && path.is_dir() {
					self.workdir.set_current(path.clone());
					debug!("Session workdir set from capabilities: {}", path.display());
				} else {
					debug!(
						"Session workdir '{}' is not an absolute directory path, ignoring",
						workdir_str
					);
				}
			}
		}
	}
}

impl Default for OctofsServer {
	fn default() -> Self {
		Self::new()
	}
}

use std::path::PathBuf;

#[tool_router]
impl OctofsServer {
	#[tool(
		title = "View",
		annotations(
			read_only_hint = true,
			destructive_hint = false,
			idempotent_hint = true,
			open_world_hint = false
		),
		description = "Read files, list directories, search content. Lines render as `N:hh|content`; \
			`N:hh` is the line id edit tools take as targets — copy it verbatim. Directory listings \
			show each file as `path\tNL\t~Nt` (lines, estimated tokens) for budgeting reads. For \
			content search across several roots, separate them in `path` with `|`. `pattern` takes \
			ripgrep -g globs (`**`, leading `!`). Re-viewing a whole file returns only the hunks \
			changed since your last view (or an unchanged marker); `full: true` forces the complete \
			content."
	)]
	async fn view(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<ViewParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(self.view_cache.clone(), async move {
			let result = fs::execute_view(&call).await.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		title = "Text Editor",
		annotations(
			read_only_hint = false,
			destructive_hint = true,
			idempotent_hint = false,
			open_world_hint = false
		),
		description = "File operations: create, str_replace, delete, undo_edit. str_replace takes raw \
			file text (real newlines, no `N:hh|` prefixes); old_text must match exactly once, or set \
			replace_all: true. On no or multiple matches the error lists candidate line ids for \
			batch_edit. Prefer batch_edit when you already hold line ids."
	)]
	async fn text_editor(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<TextEditorParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "text_editor".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(self.view_cache.clone(), async move {
			let result = fs::execute_text_editor(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		title = "Batch Edit",
		annotations(
			read_only_hint = false,
			destructive_hint = true,
			idempotent_hint = false,
			open_world_hint = false
		),
		description = "Apply several insert/replace operations to one file atomically. Targets are \
			line ids (\"12:a3\") from view or edit output, verified before anything is written; a \
			stale id fails with the current content. The result diff carries fresh ids for follow-up \
			edits — no re-view needed; removed lines show as an id range, and a trailing `shift:` \
			line says how original line numbers after each edit moved. Insert anchors 0 (file \
			start) and -1 (end) are plain integers. Content is raw text without id prefixes."
	)]
	async fn batch_edit(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<BatchEditParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "batch_edit".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(self.view_cache.clone(), async move {
			let result = fs::execute_batch_edit(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		title = "Extract Lines",
		annotations(
			read_only_hint = false,
			destructive_hint = false,
			idempotent_hint = false,
			open_world_hint = false
		),
		description = "Copy lines from a source file and append them into a target file."
	)]
	async fn extract_lines(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<ExtractLinesParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "extract_lines".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(self.view_cache.clone(), async move {
			let result = fs::execute_extract_lines(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		title = "Shell",
		annotations(
			read_only_hint = false,
			destructive_hint = true,
			idempotent_hint = false,
			open_world_hint = true
		),
		description = "Run a shell command: builds, tests, git, project CLIs. Use `view` to read, \
			list and search files (cat/grep/ls/sed are rejected). Output is terminal-clean: ANSI \
			and progress redraws stripped, repeated lines collapsed with a count; pipe through \
			`od -c` or `xxd` for byte-exact output. A command still running after ~10s moves to \
			the background and returns a job resource; you are notified automatically with the \
			exit code and output tail when it exits. Do NOT poll, sleep, re-run or `ps` it — start \
			the next independent step or end your turn. Distinct commands run concurrently; an \
			identical command in the same directory is rejected while it is running."
	)]
	async fn shell(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<ShellParams>,
	) -> Result<CallToolResult, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "shell".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		// Heartbeat while a command is inside the foreground window, so the MCP
		// idle timeout does not cancel a call that is silent by nature. Anything
		// still running at the boundary is returned as a background resource, so
		// there is no held-open call or polling after promotion.
		let progress_token = context.meta.get_progress_token();
		let peer = context.peer.clone();
		// A background job outlives this call; when it exits, emit
		// `resources/updated` for its resource URI so the client can read the
		// output. Two delivery paths, by what the client set up: 2026-07-28
		// clients that opened a `subscriptions/listen` stream get the
		// notification on it (tagged with their subscription id by the sink);
		// everyone else gets the unsolicited push the pre-2026-07-28 spec
		// allowed. The peer and registry handles are captured here and hidden
		// behind an opaque callback so the fs layer stays protocol-free.
		let completion_peer = context.peer.clone();
		let subscriptions = self.subscriptions.clone();
		let notifier: fs::shell::BackgroundNotify = Box::new(move |uri| {
			tokio::spawn(async move {
				if !notify_subscriptions(&subscriptions, &uri).await {
					notify_peer_resource_updated(&completion_peer, &uri).await;
				}
			});
		});
		// The resource link carries the command as its name, so a client can
		// describe the job ("make reldebug … still running") without re-deriving
		// it — e.g. when preserving pending jobs across a context compaction.
		let job_label: String = params.command.trim().chars().take(80).collect();
		let exec = request_ctx::with_request_context(self.view_cache.clone(), async move {
			let outcome = fs::execute_shell_command(&call, Some(notifier))
				.await
				.map_err(|e| e.to_string())?;
			let mut content = vec![ContentBlock::text(append_hints(outcome.text))];
			if let Some(uri) = outcome.resource_uri {
				// A ResourceLink is the protocol-native "watch this" signal: the
				// client follows it generically, with no octofs-specific
				// knowledge, so shell can be served by any MCP server.
				let name = if job_label.is_empty() {
					"background shell job".to_string()
				} else {
					format!("shell: {job_label}")
				};
				content.push(ContentBlock::resource_link(Resource::new(uri, name)));
			}
			Ok::<_, String>(CallToolResult::success(content))
		});
		tokio::pin!(exec);
		let mut beats = 0.0_f64;
		loop {
			tokio::select! {
				biased;
				done = &mut exec => return done,
				_ = tokio::time::sleep(SHELL_HEARTBEAT_INTERVAL) => {
					let Some(token) = progress_token.clone() else { continue };
					beats += 1.0;
					let _ = peer
						.notify_progress(
							rmcp::model::ProgressNotificationParam::new(token, beats)
								.with_message("command still running"),
						)
						.await;
				}
			}
		}
	}

	#[tool(
		title = "Working Directory",
		annotations(
			read_only_hint = false,
			destructive_hint = false,
			idempotent_hint = true,
			open_world_hint = false
		),
		description = "Switch the working directory for later calls (`path`) or revert to the \
			session root (`reset: true`). Do not call it just to check the directory; every tool \
			resolves relative paths against it."
	)]
	async fn workdir(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<WorkdirParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "workdir".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_workdir_command(&call)
			.await
			.map_err(|e| e.to_string())?;

		// Update session workdir state based on the structured result
		match &result {
			fs::WorkdirResult::Set { current, .. } => {
				self.workdir.set_current(current.clone());
			}
			fs::WorkdirResult::Reset => {
				self.workdir.reset();
			}
			fs::WorkdirResult::Get { .. } => {}
		}

		Ok(result.to_json_string())
	}
}

/// Collapse `"type": [T, "null"]` to `T` and `anyOf: [X, {"type": "null"}]` to
/// `X` throughout a tool schema, merging the surviving branch into the parent
/// so field-level keys (description) win.
///
/// schemars emits those nullable forms for every `Option<T>` parameter. Serving
/// stacks build their tool-call grammar from `"type"` and some read it as a
/// plain string: given the array form they fall through to the string branch and
/// constrain the model to emit the argument as text — `"3"` instead of `3` —
/// which rmcp then rejects while deserializing parameters. Measured on Alibaba
/// Model Studio's Qwen path and on CoreWeave; the same weights on Parasail,
/// Chutes and DeepInfra are unaffected.
///
/// Lossless: optionality is carried by `required`. Genuine multi-branch unions
/// (the `string | integer` line endpoints) have nothing to collapse to and are
/// left alone — `parse_endpoint` accepts either form anyway.
fn strip_null_variants(value: &mut serde_json::Value) {
	match value {
		serde_json::Value::Object(obj) => {
			for nested in obj.values_mut() {
				strip_null_variants(nested);
			}

			let collapsed_type =
				obj.get_mut("type")
					.and_then(|t| t.as_array_mut())
					.and_then(|types| {
						types.retain(|t| t.as_str() != Some("null"));
						(types.len() == 1).then(|| types[0].clone())
					});
			if let Some(single) = collapsed_type {
				obj.insert("type".to_string(), single);
			}

			for key in ["anyOf", "oneOf"] {
				let only = obj
					.get_mut(key)
					.and_then(|v| v.as_array_mut())
					.and_then(|variants| {
						variants.retain(|v| v.get("type").and_then(|t| t.as_str()) != Some("null"));
						(variants.len() == 1).then(|| variants[0].clone())
					});
				if let Some(serde_json::Value::Object(only)) = only {
					obj.remove(key);
					for (k, v) in only {
						obj.entry(k).or_insert(v);
					}
				}
			}
		}
		serde_json::Value::Array(items) => {
			for item in items.iter_mut() {
				strip_null_variants(item);
			}
		}
		_ => {}
	}
}

#[tool_handler(router = Self::tool_router())]
impl ServerHandler for OctofsServer {
	fn get_info(&self) -> ServerInfo {
		ServerInfo::new(
			// Resources (read/list) advertise automatically promoted shell jobs
			// as handles. `subscribe` is advertised because resource updates are
			// deliverable both ways: on a `subscriptions/listen` stream (the
			// 2026-07-28 contract path, see `listen` below) and as the
			// unsolicited push legacy clients expect (the `shell` notifier's
			// fallback path).
			ServerCapabilities::builder()
				.enable_tools()
				.enable_resources()
				.enable_resources_subscribe()
				.build(),
		)
		.with_server_info(Implementation::from_build_env())
		.with_protocol_version(ProtocolVersion::V_2026_07_28)
		.with_instructions(
			"This server provides filesystem tools: view (read files/dirs), \
				 text_editor (create/str_replace/delete/undo), batch_edit (multi-op line edits), \
				 extract_lines (copy lines between files), shell (execute commands), \
				 workdir (get/set working directory). File lines are rendered as `N:hh|content`; \
				 the `N:hh` prefix is the line id edit tools take as targets. Edit results are \
				 diffs with fresh ids, so edits can be chained without re-viewing files."
				.to_string(),
		)
	}

	async fn list_tools(
		&self,
		_request: Option<PaginatedRequestParams>,
		_context: RequestContext<RoleServer>,
	) -> Result<ListToolsResult, ErrorData> {
		let tools = Self::tool_router()
			.list_all()
			.into_iter()
			.map(|mut tool| {
				let mut schema = tool.input_schema.as_ref().clone();
				for value in schema.values_mut() {
					strip_null_variants(value);
				}
				tool.input_schema = Arc::new(schema);
				tool
			})
			.collect();
		Ok(ListToolsResult::with_all_items(tools))
	}

	// Background shell jobs are surfaced as resources: each promoted command is
	// `octofs://jobs/<id>`, readable for its status and output tail. On exit the
	// job's wait task emits `resources/updated` for that URI (see the `shell`
	// handler's notifier), so a client learns a build finished without polling.
	async fn list_resources(
		&self,
		_request: Option<PaginatedRequestParams>,
		_context: RequestContext<RoleServer>,
	) -> Result<ListResourcesResult, ErrorData> {
		let resources = fs::background::list()
			.into_iter()
			.map(|job| {
				let state = match job.status() {
					fs::background::JobStatus::Running => "running",
					fs::background::JobStatus::Exited(_) => "finished",
				};
				Resource::new(
					fs::background::resource_uri(&job.id),
					format!("background shell job ({state}): {}", job.command),
				)
			})
			.collect();
		Ok(ListResourcesResult::with_all_items(resources))
	}

	async fn read_resource(
		&self,
		request: ReadResourceRequestParams,
		_context: RequestContext<RoleServer>,
	) -> Result<ReadResourceResponse, ErrorData> {
		let uri = request.uri;
		let id = fs::background::job_id_from_uri(&uri)
			.ok_or_else(|| ErrorData::resource_not_found(format!("Not a job URI: {uri}"), None))?;
		let view = fs::background::read(id).ok_or_else(|| {
			ErrorData::resource_not_found(format!("No such background job: {uri}"), None)
		})?;
		let status = match view.status {
			fs::background::JobStatus::Running => "running".to_string(),
			fs::background::JobStatus::Exited(code) => format!("exited with code {code}"),
		};
		let truncated = if view.truncated {
			"\n[earlier output dropped — showing the last 30000 bytes]"
		} else {
			""
		};
		let body = format!(
			"job {id}\ncommand: {}\nstatus: {status}{truncated}\n\n{}",
			view.command, view.output
		);
		Ok(ReadResourceResult::new(vec![ResourceContents::text(body, uri)]).into())
	}

	// 2026-07-28 change notifications are opt-in: the client opens a
	// `subscriptions/listen` stream filtered to what it wants, and the SDK
	// acknowledges the accepted subset before `listen` runs. Accept whatever
	// the client asked for — the SDK intersects it with the capabilities
	// advertised in `get_info` (resources.subscribe gates URI subscriptions).
	fn accepted_subscription_filter(
		&self,
		requested: &SubscriptionFilter,
	) -> Option<SubscriptionFilter> {
		Some(requested.clone())
	}

	// Keep the sink registered for the life of the stream so background-job
	// completion can be delivered on it. `cancelled` resolves on client
	// cancellation, transport close, or graceful teardown — however the
	// stream ends, the sink is removed. A sink whose stream died between
	// registration and delivery self-reports as closed on send.
	async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
		let sink = context.sink().clone();
		let id = sink.id().clone();
		let uris = sink
			.accepted()
			.resource_subscriptions
			.clone()
			.unwrap_or_default();
		self.subscriptions.register(sink.clone());

		// Registration happens before this check. If the job exits concurrently,
		// either its completion path sees this sink or this replay sees the exited
		// state (possibly both, which is safe for a change notification).
		for uri in uris.into_iter().filter(|uri| background_job_finished(uri)) {
			if let Err(error) = sink.notify_resource_updated(uri.clone()).await {
				debug!("late subscription replay for {uri} failed: {error}");
				self.subscriptions.unregister(&id);
				notify_peer_resource_updated(&context.request_context().peer, &uri).await;
			}
		}
		context.cancelled().await;
		self.subscriptions.unregister(&id);
		Ok(())
	}

	// Legacy (pre-2026-07-28) clients that see `subscribe` advertised may run
	// the `resources/subscribe` handshake. Normal delivery is the unsolicited
	// push sent on job exit. If that happened before this subscription arrived,
	// replay it now from the retained job state.
	async fn subscribe(
		&self,
		request: SubscribeRequestParams,
		context: RequestContext<RoleServer>,
	) -> Result<(), ErrorData> {
		if background_job_finished(&request.uri) {
			notify_peer_resource_updated(&context.peer, &request.uri).await;
		}
		Ok(())
	}

	async fn unsubscribe(
		&self,
		_request: UnsubscribeRequestParams,
		_context: RequestContext<RoleServer>,
	) -> Result<(), ErrorData> {
		Ok(())
	}

	// The default `initialize` (legacy clients) and `discover` (2026-07-28
	// clients) implementations handle version negotiation; the session
	// workdir from client capabilities is applied per-request in
	// `ensure_session_workdir`, which covers both eras.
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;

/// Drain this request's hints and append them to the tool result.
/// Called after tool execution (inside the request scope) to surface guidance to the LLM.
fn append_hints(mut result: String) -> String {
	let hints = request_ctx::drain_hints();
	if !hints.is_empty() {
		result.push_str("\n\n");
		for hint in hints {
			result.push_str("⚠️ ");
			result.push_str(&hint);
			result.push('\n');
		}
	}
	result
}
// ── Tool parameter schemas ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ViewParams {
	/// File or directory path. With content search, `|` separates several roots (max 32).
	/// To read several files, make parallel `view` calls. Remote: ssh://[user@]host[:port]/path
	/// — host may be an ~/.ssh/config alias; `ssh://host` or `ssh://host/~/dir` is the login home.
	pub path: String,
	/// First line to show (inclusive). Integer line number (negative counts from the
	/// end: -1 = last line) or a line id like "12:a3". Omit to start at line 1.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: Option<serde_json::Value>,
	/// Last line to show (inclusive). Integer line number (negative counts from the end)
	/// or a line id like "20:f1". Omit to read to the end of the file.
	/// Omit BOTH `start` and `end` to view the whole file.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Ripgrep -g glob filter for listings and content search: without `/` it matches
	/// filenames at any depth, with `/` the relative path. Supports `*`, `**`, `?`, `[abc]`,
	/// `{rs,toml}`, leading `!`; `|` joins ordered globs (later wins), e.g. `**/*.rs|!target/**`.
	/// Applied after gitignore/hidden filtering. Single line, max 4096 bytes, 64 globs.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Content search: literal substring, or a Rust regex with `regex: true` (`(?i)` for
	/// case-insensitive). Searches the whole file/tree; `start`/`end` are ignored.
	#[serde(default)]
	pub content: Option<String>,
	/// When true, `content` is a regex pattern instead of a literal substring. Default: false.
	#[serde(default)]
	pub regex: Option<bool>,
	/// Maximum directory traversal depth.
	#[serde(default)]
	pub max_depth: Option<usize>,
	/// Include hidden files/directories starting with '.'.
	#[serde(default)]
	pub include_hidden: Option<bool>,
	/// Context lines around content search matches.
	#[serde(default)]
	pub context: Option<usize>,
	/// Whole-file views of a file you already viewed return only the changed hunks
	/// (or an unchanged marker). Set true to force the complete content, e.g. after
	/// losing earlier context.
	#[serde(default)]
	pub full: Option<bool>,
}

/// JSON schema for a single line endpoint (`start`/`end`/`append_line`/op `start`/`end`).
///
/// One shape everywhere: a line id string "N:hh" copied from view output (verified
/// against the file before edits), or a plain integer line number where positions are
/// allowed (view ranges, insert anchors 0/-1). `anyOf` (not `oneOf`) is used because
/// it has strictly wider cross-stack support.
fn line_endpoint_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
	let schema = serde_json::json!({
		"description": "Line id \"N:hh\" from view/edit output, or an integer line number (negative counts from the end). Edit targets require ids.",
		"anyOf": [
			{ "type": "string" },
			{ "type": "integer", "format": "int64" }
		],
		"examples": ["12:a3", -1]
	});
	serde_json::from_value(schema).expect("static schema is valid JSON")
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextEditorCommand {
	Create,
	StrReplace,
	Delete,
	UndoEdit,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TextEditorParams {
	/// The operation to perform: create, str_replace, delete, undo_edit
	pub command: TextEditorCommand,
	/// REQUIRED. Path to the file to operate on.
	/// Supports ssh://user@host:port/path for remote filesystem access.
	pub path: String,
	/// File content for create command.
	#[serde(default)]
	pub content: Option<String>,
	/// Text to find (must match exactly). REQUIRED for str_replace.
	#[serde(default)]
	pub old_text: Option<String>,
	/// Replacement text. REQUIRED for str_replace.
	#[serde(default)]
	pub new_text: Option<String>,
	/// str_replace only: replace ALL occurrences of old_text (rename-style edits).
	/// Default false — old_text must then match exactly once.
	#[serde(default)]
	pub replace_all: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchEditOperationType {
	Insert,
	Replace,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BatchEditOperation {
	/// Type of operation: 'insert' (after a line) or 'replace' (a line range)
	pub operation: BatchEditOperationType,
	/// Target in the ORIGINAL file, as a line id from view output (e.g. "12:a3").
	/// For `replace` this is the first line of the range to replace.
	/// For `insert` this is the anchor to insert after — a line id, or the integers
	/// 0 (file start) / -1 (after last line).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: serde_json::Value,
	/// Last line of the range to replace (inclusive), as a line id (e.g. "20:f1"),
	/// for `replace` only. Omit for a single-line replace (defaults to `start`).
	/// Ignored for `insert`.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Raw content to insert or replace with (no line-id prefixes).
	pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BatchEditParams {
	/// Path to the file to edit. Supports ssh://user@host:port/path for remote access.
	pub path: String,
	/// Array of operations for batch_edit on SINGLE file. Max 50 operations.
	#[schemars(length(max = 50))]
	pub operations: Vec<BatchEditOperation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ExtractLinesParams {
	/// Path to the source file to extract lines from.
	/// Supports ssh://user@host:port/path for remote filesystem access.
	pub from_path: String,
	/// First line to copy (inclusive). Integer line number or a line id like "12:a3"
	/// (ids are verified against the source file).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_start: serde_json::Value,
	/// Last line to copy (inclusive). Integer line number or a line id.
	/// Omit to copy a single line (defaults to `from_start`).
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_end: Option<serde_json::Value>,
	/// Path to the target file where extracted lines will be appended.
	/// Supports ssh://user@host:port/path for remote filesystem access.
	pub append_path: String,
	/// Where to append in the target: 0 = beginning, -1 = end, N = after line N
	/// (integer), or a line id like "12:a3" (verified against the target file).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub append_line: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ShellParams {
	/// The shell command to execute
	pub command: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WorkdirParams {
	/// Absolute path or path relative to current workdir to switch into.
	/// Required unless `reset: true`. Do not pass `"."` — that is a no-op.
	/// Supports ssh://user@host:port/path for remote filesystem access;
	/// `ssh://host` or `ssh://host/~/dir` is the login home.
	#[serde(default)]
	pub path: Option<String>,
	/// If true, revert to the original session working directory.
	#[serde(default)]
	pub reset: Option<bool>,
}

// ── Server implementation ───────────────────────────────────────────────────────
