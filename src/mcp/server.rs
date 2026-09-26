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

//! MCP server handlers, session state, and tool parameter schemas.

use std::sync::{Arc, RwLock};

use rmcp::{
	handler::server::{wrapper::Parameters, ServerHandler},
	model::{
		CacheScope, CallToolResult, ContentBlock, Implementation, ListResourcesResult,
		ListToolsResult, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
		ReadResourceResponse, ReadResourceResult, RequestId, Resource, ResourceContents,
		ServerCapabilities, ServerConfig, SubscribeRequestParams, SubscriptionFilter,
		UnsubscribeRequestParams,
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
		description = "Read files, list directories, search content. Lines render as \
			`N:hh|content`; copy `N:hh` verbatim — it is the line id edit tools target. Listings \
			are recursive and gitignore-aware, one `path\tNL\t~Nt` (lines, ~tokens) per file. \
			Re-viewing a whole file returns only the hunks changed since your last view (or an \
			unchanged marker); ranged reads always return the requested lines. Reuse returned \
			content: read a complete block in one call, combine adjacent windows, never re-read \
			overlapping ranges or narrow one just to pick edit targets; re-read only when the file \
			may have changed or output was truncated."
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
		description = "File operations. create: new file only — fails if it exists, parent \
			directories are made. str_replace: raw file text (real newlines, no `N:hh|` \
			prefixes); old_text must match exactly once, or set replace_all: true — on no or \
			multiple matches the error lists candidate line ids for batch_edit. delete: remove a \
			file (not a directory). undo_edit: revert the last edit to `path` (10 levels, delete \
			included). Prefer batch_edit when you already hold line ids."
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
		description = "Apply insert/replace operations (max 50) to one file atomically. Targets \
			are line ids (\"12:a3\") from view or edit output, verified before anything is written; \
			a stale id fails with the current content. All targets refer to the original file and \
			must not overlap. The result diff carries fresh ids for follow-up edits — no re-view \
			needed; removed lines show as an id range, and a trailing `shift:` line says how later \
			original line numbers moved. Insert anchors 0 (file start) and -1 (end) are plain \
			integers. Content is raw text without id prefixes."
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
		description = "Copy a line range from one file and append it into another without \
			retyping it — moving code between files, splitting modules. The source is left \
			untouched; to move, follow with a batch_edit that removes the range."
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
		description = "Run a shell command: builds, tests, git, project CLIs. `sh -c` (`cmd /C` \
			on Windows) in the current workdir, local machine only, no stdin or TTY — never run \
			interactive commands or prefix `cd` to the workdir (`cd <other> && …` is fine one-off; \
			`workdir` switches permanently; a remote ssh:// workdir disables shell). Use `view` to \
			read, list and search files (cat/grep/ls/sed are rejected). Output is terminal-clean \
			(ANSI and progress redraws stripped, repeated lines collapsed with a count; pipe \
			through `od -c` or `xxd` for exact bytes); a non-zero exit returns as an error with \
			the output. A command still running after ~10s moves to the background and returns a \
			job resource; you are notified with the exit code and output tail when it exits. Do \
			NOT poll, sleep, re-run or `ps` it — start the next independent step or end your turn. \
			Distinct commands run concurrently; an identical command in the same directory is \
			rejected while it runs."
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
			session root (`reset: true`). Every tool resolves relative paths against it; do not \
			call it just to check the directory. A remote (ssh://) workdir disables `shell`."
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

/// schemars emits keys no model acts on: `$schema`, the `format` of every integer
/// (`int64`, `uint` — `minimum` already bounds the unsigned ones) and `default: null`
/// on every optional field. Always-loaded tool definitions ride on every request, so
/// they are dropped. Keyword-aware: under `properties`/`$defs` the keys are names,
/// so a parameter called `format` or `default` survives.
fn strip_generator_noise(schema: &mut serde_json::Map<String, serde_json::Value>) {
	schema.remove("$schema");
	schema.remove("format");
	if schema
		.get("default")
		.is_some_and(serde_json::Value::is_null)
	{
		schema.remove("default");
	}
	for (key, value) in schema.iter_mut() {
		match (key.as_str(), value) {
			("properties" | "$defs", serde_json::Value::Object(named)) => {
				for sub in named.values_mut() {
					if let serde_json::Value::Object(sub) = sub {
						strip_generator_noise(sub);
					}
				}
			}
			(_, serde_json::Value::Object(sub)) => strip_generator_noise(sub),
			(_, serde_json::Value::Array(items)) => {
				for item in items {
					if let serde_json::Value::Object(sub) = item {
						strip_generator_noise(sub);
					}
				}
			}
			_ => {}
		}
	}
}

/// 2026-07-28 makes `ttlMs` and `cacheScope` required on list and read results;
/// older protocol versions don't define them. rmcp's generated `list_tools` sets
/// them, so the overrides below must too — a strict 2026-07-28 client (Claude
/// Code's `server/discover` runtime) rejects the result and loads no tools at all.
fn supports_cache_hints(context: &RequestContext<RoleServer>) -> bool {
	context
		.protocol_version()
		.is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

#[tool_handler(router = Self::tool_router())]
impl ServerHandler for OctofsServer {
	fn get_info(&self) -> ServerConfig {
		ServerConfig::new(
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
			"Filesystem tools. File lines render as `N:hh|content`; `N:hh` is the line id edit \
				 tools target, and edit results are diffs with fresh ids, so edits chain without \
				 re-viewing files. Reuse returned content and ids; read complete relevant blocks, \
				 then act on them."
				.to_string(),
		)
	}

	async fn list_tools(
		&self,
		_request: Option<PaginatedRequestParams>,
		context: RequestContext<RoleServer>,
	) -> Result<ListToolsResult, ErrorData> {
		let tools = Self::tool_router()
			.list_all()
			.into_iter()
			.map(|mut tool| {
				let mut schema = tool.input_schema.as_ref().clone();
				for value in schema.values_mut() {
					strip_null_variants(value);
				}
				strip_generator_noise(&mut schema);
				tool.input_schema = Arc::new(schema);
				tool
			})
			.collect();
		let result = ListToolsResult::with_all_items(tools);
		if !supports_cache_hints(&context) {
			return Ok(result);
		}
		// ttl 0 matches rmcp's generated handler; the list is identical for every caller.
		Ok(result.with_ttl_ms(0).with_cache_scope(CacheScope::Public))
	}

	// Background shell jobs are surfaced as resources: each promoted command is
	// `octofs://jobs/<id>`, readable for its status and output tail. On exit the
	// job's wait task emits `resources/updated` for that URI (see the `shell`
	// handler's notifier), so a client learns a build finished without polling.
	async fn list_resources(
		&self,
		_request: Option<PaginatedRequestParams>,
		context: RequestContext<RoleServer>,
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
		let result = ListResourcesResult::with_all_items(resources);
		if !supports_cache_hints(&context) {
			return Ok(result);
		}
		// Jobs come and go and belong to this session: never fresh, never shared.
		Ok(result.with_ttl_ms(0).with_cache_scope(CacheScope::Private))
	}

	async fn read_resource(
		&self,
		request: ReadResourceRequestParams,
		context: RequestContext<RoleServer>,
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
		let result = ReadResourceResult::new(vec![ResourceContents::text(body, uri)]);
		if !supports_cache_hints(&context) {
			return Ok(result.into());
		}
		// A running job's tail changes between reads, and it is this session's job.
		Ok(result
			.with_ttl_ms(0)
			.with_cache_scope(CacheScope::Private)
			.into())
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
	/// File or directory. `a|b` searches several roots with `content` (max 32); several
	/// files take parallel view calls. Remote: ssh://[user@]host[:port]/path (host may be an
	/// ~/.ssh/config alias; `ssh://host` or `ssh://host/~/dir` is the login home).
	pub path: String,
	/// First line (inclusive): line id or integer (negative counts from the end, -1 = last).
	/// Default: line 1.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: Option<serde_json::Value>,
	/// Last line (inclusive): line id or integer (negative counts from the end). Default: end
	/// of file; omit both `start` and `end` for the whole file.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Ripgrep -g globs bounding listings and searches: without `/` a name at any depth,
	/// with `/` the relative path; `*`, `**`, `?`, `[abc]`, `{rs,toml}`, leading `!`; `|`
	/// joins ordered globs (later wins), e.g. `**/*.rs|!target/**`. Applied after
	/// gitignore/hidden filtering; one line, max 4096 bytes, 64 globs.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Locate code: searches the whole file/tree (`start`/`end` ignored) for a literal
	/// substring, or a Rust regex with `regex: true` (`(?i)` case-insensitive; alternation for
	/// related terms).
	#[serde(default)]
	pub content: Option<String>,
	/// Treat `content` as a regex. Default: false.
	#[serde(default)]
	pub regex: Option<bool>,
	/// Directory depth bound. Default: unlimited, except a bare remote listing (no
	/// `pattern`/`content`) stops at the root entries. Searches walk the whole tree.
	#[serde(default)]
	pub max_depth: Option<usize>,
	/// Include dotfiles and dot-directories.
	#[serde(default)]
	pub include_hidden: Option<bool>,
	/// Lines around each `content` match. Default: 0 — ask for enough to answer without
	/// another tiny read.
	#[serde(default)]
	pub context: Option<usize>,
	/// Force the complete file when re-viewing a whole file (normally only changed hunks),
	/// e.g. after losing earlier context. No effect on ranges or searches; output limits
	/// unchanged.
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
	/// Operation to perform.
	pub command: TextEditorCommand,
	/// File path (ssh://user@host:port/path for remote).
	pub path: String,
	/// Content for `create`.
	#[serde(default)]
	pub content: Option<String>,
	/// `str_replace`: exact text to find (required).
	#[serde(default)]
	pub old_text: Option<String>,
	/// `str_replace`: replacement text (required).
	#[serde(default)]
	pub new_text: Option<String>,
	/// `str_replace`: replace every occurrence (rename-style). Default: false.
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
	/// `insert` (after `start`) or `replace` (`start`..`end`).
	pub operation: BatchEditOperationType,
	/// Line id in the ORIGINAL file: first line to replace, or the anchor to insert after
	/// (for `insert` also the integers 0 = file start, -1 = after the last line).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: serde_json::Value,
	/// `replace` only: last line to replace (inclusive), as a line id. Default: `start`.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Raw text to insert or replace with, no line-id prefixes.
	#[serde(deserialize_with = "string_or_lines")]
	pub content: String,
}

/// Accept content as a string or as a list of lines. A list names exactly one
/// block of text, so joining it beats rejecting the call and being re-sent the
/// same edit as a string.
fn string_or_lines<'de, D>(deserializer: D) -> Result<String, D::Error>
where
	D: serde::Deserializer<'de>,
{
	match serde_json::Value::deserialize(deserializer)? {
		serde_json::Value::String(s) => Ok(s),
		serde_json::Value::Array(items) => items
			.iter()
			.map(|item| match item {
				serde_json::Value::String(s) => Ok(s.as_str()),
				other => Err(serde::de::Error::custom(format!(
					"content list must hold strings, found {other}"
				))),
			})
			.collect::<Result<Vec<_>, _>>()
			.map(|lines| lines.join("\n")),
		other => Err(serde::de::Error::custom(format!(
			"content must be a string or a list of lines, found {other}"
		))),
	}
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct BatchEditParams {
	/// File to edit (ssh://user@host:port/path for remote).
	pub path: String,
	/// Operations on this one file (max 50).
	#[schemars(length(max = 50))]
	pub operations: Vec<BatchEditOperation>,
}

/// The tool edits one file, so callers sometimes carry `path` on each operation
/// instead of at the top level. When every operation names the same file that is
/// the file to edit, so it is hoisted rather than refused; disagreeing paths stay
/// an error because no single target is named.
impl<'de> Deserialize<'de> for BatchEditParams {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		#[derive(Deserialize)]
		struct Raw {
			#[serde(default)]
			path: Option<String>,
			operations: Vec<serde_json::Value>,
		}

		let mut raw = Raw::deserialize(deserializer)?;
		let mut per_op: Vec<String> = Vec::new();
		for op in &mut raw.operations {
			if let Some(obj) = op.as_object_mut() {
				if let Some(serde_json::Value::String(p)) = obj.remove("path") {
					per_op.push(p);
				}
			}
		}

		let path = match raw.path {
			Some(p) => p,
			None => {
				let mut unique: Vec<&String> = Vec::new();
				for p in &per_op {
					if !unique.contains(&p) {
						unique.push(p);
					}
				}
				match unique.as_slice() {
					[only] => (*only).clone(),
					[] => return Err(serde::de::Error::missing_field("path")),
					_ => return Err(serde::de::Error::custom(
						"batch_edit edits a single file, but the operations name different paths; \
							 issue one batch_edit per file",
					)),
				}
			}
		};

		let operations = raw
			.operations
			.into_iter()
			.map(serde_json::from_value::<BatchEditOperation>)
			.collect::<Result<Vec<_>, _>>()
			.map_err(serde::de::Error::custom)?;

		Ok(BatchEditParams { path, operations })
	}
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ExtractLinesParams {
	/// Source file (ssh://user@host:port/path for remote).
	pub from_path: String,
	/// First line to copy (inclusive): integer or line id (verified against the source).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_start: serde_json::Value,
	/// Last line to copy (inclusive): integer or line id. Default: `from_start`.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_end: Option<serde_json::Value>,
	/// Target file the lines are appended to (ssh://user@host:port/path for remote).
	pub append_path: String,
	/// Where to append in the target: 0 = beginning, -1 = end, N = after line N, or a line
	/// id (verified against the target).
	#[schemars(schema_with = "line_endpoint_schema")]
	pub append_line: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ShellParams {
	/// Command line for `sh -c`; use non-interactive flags (`-y`, `--no-pager`, `CI=1`) for
	/// anything that might prompt.
	pub command: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WorkdirParams {
	/// Directory to switch into, absolute or relative to the current workdir; required unless
	/// `reset`. Not `"."` (a no-op). Remote: ssh://user@host:port/path; `ssh://host` or
	/// `ssh://host/~/dir` is the login home.
	#[serde(default)]
	pub path: Option<String>,
	/// Revert to the session root.
	#[serde(default)]
	pub reset: Option<bool>,
}

// ── Server implementation ───────────────────────────────────────────────────────
