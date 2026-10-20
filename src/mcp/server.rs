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

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rmcp::{
	handler::server::{wrapper::Parameters, ServerHandler},
	model::{
		CacheScope, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock, CreateTaskResult,
		CustomNotification, DetailedTask, ExperimentalCapabilities, GetTaskParams, GetTaskResult, Implementation,
		JsonObject, ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
		ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, RequestId, Resource, ResourceContents,
		ServerCapabilities, ServerConfig, ServerNotification, SubscribeRequestParams, SubscriptionFilter, Task,
		TaskPayload, TaskStatus, UnsubscribeRequestParams, UpdateTaskParams,
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
/// Each client session has its own workdir, isolated from other sessions.
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

/// Most sessions one connection keeps state for. Octomind multiplexes every
/// session of a long-lived daemon over one connection and MCP has no session-end
/// signal, so the least recently used session is dropped beyond this.
const MAX_TRACKED_SESSIONS: usize = 64;

/// Workdir and delta-view cache of one client session.
#[derive(Debug)]
struct SessionState {
	workdir: SessionWorkdir,
	/// Present only for clients that report a `context_epoch`: a delta is safe
	/// only when the client says when the model lost what it was served.
	view_cache: Option<Arc<fs::delta::ViewCache>>,
	/// Client context generation the view cache was filled under.
	context_epoch: std::sync::atomic::AtomicU64,
	last_used: std::sync::atomic::AtomicU64,
}

/// Sessions served over this connection, keyed by `experimental.session.session_id`;
/// `None` is a client that sends no session context.
#[derive(Debug, Default)]
struct Sessions {
	states: std::sync::Mutex<HashMap<Option<String>, Arc<SessionState>>>,
	clock: std::sync::atomic::AtomicU64,
}

impl Sessions {
	/// State for `id` anchored at `root`. A session whose anchor moved, or whose
	/// client started or stopped reporting a context epoch, starts over; one
	/// whose context epoch changed keeps its workdir but loses its view cache.
	fn get(&self, id: Option<String>, root: PathBuf, context_epoch: Option<u64>) -> Arc<SessionState> {
		use std::sync::atomic::Ordering;
		let now = self.clock.fetch_add(1, Ordering::Relaxed);
		let mut states = self.states.lock().expect("sessions lock poisoned");
		if let Some(state) = states
			.get(&id)
			.filter(|state| state.workdir.root == root && state.view_cache.is_some() == context_epoch.is_some())
		{
			state.last_used.store(now, Ordering::Relaxed);
			if let (Some(cache), Some(epoch)) = (&state.view_cache, context_epoch) {
				if state.context_epoch.swap(epoch, Ordering::Relaxed) != epoch {
					cache.clear();
				}
			}
			return state.clone();
		}
		if !states.contains_key(&id) && states.len() >= MAX_TRACKED_SESSIONS {
			let oldest = states
				.iter()
				.min_by_key(|(_, state)| state.last_used.load(Ordering::Relaxed))
				.map(|(key, _)| key.clone());
			if let Some(oldest) = oldest {
				states.remove(&oldest);
			}
		}
		let state = Arc::new(SessionState {
			workdir: SessionWorkdir::new(root),
			view_cache: context_epoch.map(|_| Arc::new(fs::delta::ViewCache::default())),
			context_epoch: std::sync::atomic::AtomicU64::new(context_epoch.unwrap_or_default()),
			last_used: std::sync::atomic::AtomicU64::new(now),
		});
		states.insert(id, state.clone());
		state
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

/// Claude Code ignores `resources/updated` for the model (it only evicts its read
/// cache), so a job's exit would never wake the session. Its one server-to-model
/// push is a channel: a server declaring the `claude/channel` experimental
/// capability sends `notifications/claude/channel`, and Claude Code injects the
/// content as `<channel source="octofs" job="…">` into the session, starting a turn.
/// It accepts channels only from servers the user enabled (`--channels`, or
/// `--dangerously-load-development-channels server:<name>` for a local server), in
/// interactive sessions, on pre-2026-07-28 connections; other clients ignore it.
const CLAUDE_CHANNEL_CAPABILITY: &str = "claude/channel";
const CLAUDE_CHANNEL_NOTIFICATION: &str = "notifications/claude/channel";

/// Push a finished job to Claude Code as a channel message carrying what
/// `resources/read` returns for it: the exit code and the finished foreground result.
async fn notify_claude_channel(peer: &Peer<RoleServer>, uri: &str) {
	let Some(id) = fs::background::job_id_from_uri(uri) else {
		return;
	};
	let Some(body) = fs::background::resource_text(id) else {
		return;
	};
	let params = serde_json::json!({ "content": body, "meta": { "job": id } });
	let notification = CustomNotification::new(CLAUDE_CHANNEL_NOTIFICATION, Some(params));
	if let Err(error) = peer
		.send_notification(ServerNotification::CustomNotification(notification))
		.await
	{
		warn!("background job channel message for {uri} could not be delivered: {error}");
	}
}

/// Before 2026-07-28 a server may push notifications nobody subscribed to; the
/// 2026-07-28 revision routes every notification through a listen stream, and
/// Claude Code drops channels on such a connection.
fn allows_unsolicited_notifications(context: &RequestContext<RoleServer>) -> bool {
	!context
		.protocol_version()
		.is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

/// SEP-2663 tasks: a client that declared `io.modelcontextprotocol/tasks` gets a
/// promoted shell job as a task whose id is the job id. The client polls
/// `tasks/get` itself and hands the finished result to its model like any tool
/// result, so no completion push is needed — the one path that reaches the model
/// of a 2026-07-28 Claude Code session, which drops every unsolicited message.
/// The extension belongs to the 2026-07-28 revision, so older connections never
/// get a task.
fn client_supports_tasks(context: &RequestContext<RoleServer>) -> bool {
	!allows_unsolicited_notifications(context)
		&& context
			.client_capabilities()
			.is_some_and(|capabilities| capabilities.supports_tasks())
}

/// Jobs run for seconds to hours; a couple of seconds keeps the result prompt
/// without flooding the connection.
const TASK_POLL_INTERVAL_MS: u64 = 2_000;

fn rfc3339(unix: u64) -> String {
	let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix);
	chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The resource-link name and task status message for a job: the command, so a
/// client can describe it ("make reldebug … still running") without re-deriving it.
fn job_label(command: &str) -> String {
	let label: String = command.trim().chars().take(80).collect();
	if label.is_empty() {
		"background shell job".to_string()
	} else {
		format!("shell: {label}")
	}
}

/// Base task fields for a job. Unlimited TTL: jobs stay readable until shutdown.
/// A running job's state is unchanged since creation; its progress rides in the
/// status message.
fn job_task(id: &str, view: &fs::background::JobView, progress: bool) -> Task {
	let updated = view.finished_unix.unwrap_or(view.started_unix);
	Task::new(id, TaskStatus::Working, rfc3339(view.started_unix), rfc3339(updated))
		.with_status_message(job_status_message(view, progress))
		.with_poll_interval_ms(TASK_POLL_INTERVAL_MS)
}

/// What a client shows for a job. While it runs: its PID and the command that
/// kills it — the stop the foreground reply always offered, usable from any
/// shell, alongside `tasks/cancel` — and, with `progress`, the latest output
/// line, so a stuck job is visible without reading its log. No elapsed time:
/// the task's `createdAt` already carries it, and a ticking counter makes every
/// poll of an idle job look like new state to clients that compare statuses.
fn job_status_message(view: &fs::background::JobView, progress: bool) -> String {
	let label = job_label(&view.command);
	if view.status != fs::background::JobStatus::Running {
		return label;
	}
	let stop = if cfg!(target_os = "windows") {
		format!("taskkill /PID {} /T /F", view.pid)
	} else {
		format!("kill -- -{}", view.pid)
	};
	let mut message = format!("{label} · PID {} (stop: {stop})", view.pid);
	if progress {
		if let Some(line) = view.output.lines().rev().map(str::trim).find(|line| !line.is_empty()) {
			let line: String = line.chars().take(120).collect();
			message.push_str(&format!(" · {line}"));
		}
	}
	message
}

/// The result a foreground `shell` call returns for the same exit: the output on
/// success, an error result led by the exit code otherwise.
fn job_result(code: i32, output: &str, hints: &str) -> JsonObject {
	let result = if code == 0 {
		CallToolResult::success(vec![ContentBlock::text(format!("{output}{hints}"))])
	} else {
		CallToolResult::error(vec![ContentBlock::text(format!(
			"Command failed with exit code {code}\n\n{output}{hints}"
		))])
	};
	match serde_json::to_value(result) {
		Ok(serde_json::Value::Object(map)) => map,
		_ => JsonObject::new(),
	}
}

fn unknown_task(id: &str) -> ErrorData {
	ErrorData::invalid_params(format!("unknown task: {id}"), None)
}

/// How often a running foreground `shell` command reports liveness. Well below
/// any sane client idle timeout so a single missed beat cannot cancel the call.
const SHELL_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// MCP server with per-session working directory isolation.
#[derive(Debug, Clone)]
pub struct OctofsServer {
	/// Root for sessions that send no usable `experimental.session.workdir`.
	root: PathBuf,
	/// Workdir and view cache per client session on this connection.
	sessions: Arc<Sessions>,
	/// Listen streams opened by this client, for contract-clean delivery of
	/// background-job completion notifications.
	subscriptions: Arc<SubscriptionRegistry>,
	/// Hints raised by a shell call that became a task, keyed by job id. The
	/// task's result is built after that request ended, so they are kept for it.
	task_hints: Arc<std::sync::Mutex<HashMap<String, String>>>,
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
			root,
			sessions: Arc::new(Sessions::default()),
			subscriptions: Arc::new(SubscriptionRegistry::default()),
			task_hints: Arc::default(),
		}
	}

	/// The calling session's state, from the octomind session context in the
	/// client capabilities (`experimental.session`: `session_id`, `workdir`,
	/// `context_epoch`).
	///
	/// `context_epoch` is the client's context generation: it changes whenever
	/// the client compacts or rewrites content the model already received, so
	/// whole-file views served before it may no longer be in the model's context
	/// and must not be answered with a delta. A client that sends no epoch gives
	/// no such guarantee and is always served whole files.
	///
	/// Resolved on every request: modern clients (2026-07-28) carry capabilities
	/// in each request's `_meta`, and one connection can serve many sessions;
	/// legacy clients set them once in the `initialize` handshake —
	/// `RequestContext::client_capabilities()` resolves both.
	fn session(&self, context: &RequestContext<RoleServer>) -> Arc<SessionState> {
		let session = context
			.client_capabilities()
			.and_then(|capabilities| capabilities.experimental)
			.and_then(|mut experimental| experimental.remove("session"));
		let Some(session) = session else {
			return self.sessions.get(None, self.root.clone(), None);
		};
		let id = session.get("session_id").and_then(|v| v.as_str()).map(str::to_string);
		let root = match session.get("workdir").and_then(|v| v.as_str()) {
			Some(workdir) if Path::new(workdir).is_absolute() && Path::new(workdir).is_dir() => PathBuf::from(workdir),
			workdir => {
				debug!("Session workdir {workdir:?} is not an absolute directory path, using the server root");
				self.root.clone()
			}
		};
		let context_epoch = session.get("context_epoch").and_then(|v| v.as_u64());
		self.sessions.get(id, root, context_epoch)
	}
}

impl Default for OctofsServer {
	fn default() -> Self {
		Self::new()
	}
}

use std::path::{Path, PathBuf};

// The shell description states the soft (default) misuse policy; list_tools
// swaps it for the hard one under --hint-mode hard.
pub(crate) const SHELL_MISUSE_SOFT: &str = "are discouraged and come back with a warning)";
pub(crate) const SHELL_MISUSE_HARD: &str = "are rejected)";

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
			Re-viewing a whole file may return only the hunks changed since your last view (or an \
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
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(session.view_cache.clone(), async move {
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
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "text_editor".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(session.view_cache.clone(), async move {
			let result = fs::execute_text_editor(&call).await.map_err(|e| e.to_string())?;
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
			needed; removed lines, and the middle of a long added block, show as an id range, and \
			a trailing `shift:` line says how later \
			original line numbers moved. Insert anchors 0 (file start) and -1 (end) are plain \
			integers. Content is raw text without id prefixes."
	)]
	async fn batch_edit(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<BatchEditParams>,
	) -> Result<String, String> {
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "batch_edit".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(session.view_cache.clone(), async move {
			let result = fs::execute_batch_edit(&call).await.map_err(|e| e.to_string())?;
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
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "extract_lines".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		request_ctx::with_request_context(session.view_cache.clone(), async move {
			let result = fs::execute_extract_lines(&call).await.map_err(|e| e.to_string())?;
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
			read, list and search files (cat/grep/ls except as a later pipe stage, `sed -i` and \
			echo/cat writes into the workdir are discouraged and come back with a warning). Output \
			is terminal-clean \
			(ANSI and progress redraws stripped, repeated lines collapsed with a count; pipe \
			through `od -c` or `xxd` for exact bytes); a non-zero exit returns as an error with \
			the output. Large recognized test/build output is digested with its full output path appended. \
			A command still running after ~10s moves to the background and returns a \
			job resource showing a progress tail while running; on exit you receive the exit code \
			and the same output as a foreground command. Do \
			NOT poll, sleep, re-run or `ps` it, or start a variant of it (other flags or filters) — \
			the result arrives on its own. Do only work that doesn't need it; to wait for it, reply \
			with a brief status and no tool call; resume dependent work when its completion \
			notification arrives. \
			Unrelated commands run concurrently; an identical command in the same directory is \
			rejected while it runs."
	)]
	async fn shell(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<ShellParams>,
	) -> Result<CallToolResponse, String> {
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "shell".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		// Heartbeat while a command is inside the foreground window, so the MCP
		// idle timeout does not cancel a call that is silent by nature. Anything
		// still running at the boundary is returned as a background resource or a
		// task, so there is no held-open call after promotion.
		let progress_token = context.meta.get_progress_token();
		let peer = context.peer.clone();
		// A background job outlives this call; when it exits, emit
		// `resources/updated` for its resource URI so the client can read the
		// output. Two delivery paths, by what the client set up: 2026-07-28
		// clients that opened a `subscriptions/listen` stream get the
		// notification on it (tagged with their subscription id by the sink);
		// everyone else gets the unsolicited push the pre-2026-07-28 spec
		// allowed. On pre-2026-07-28 connections the exit also goes out as a Claude
		// Code channel message, the only push that reaches its model. A client that
		// takes the job as a task polls for its exit instead, so it gets only the
		// pushes it subscribed to. The peer and registry handles are captured here
		// and hidden behind an opaque callback so the fs layer stays protocol-free.
		let completion_peer = context.peer.clone();
		let subscriptions = self.subscriptions.clone();
		let as_task = client_supports_tasks(&context);
		let push_channel = !as_task && allows_unsolicited_notifications(&context);
		let notifier: fs::shell::BackgroundNotify = Box::new(move |uri| {
			tokio::spawn(async move {
				if !notify_subscriptions(&subscriptions, &uri).await && !as_task {
					notify_peer_resource_updated(&completion_peer, &uri).await;
				}
				if push_channel {
					notify_claude_channel(&completion_peer, &uri).await;
				}
			});
		});
		let name = job_label(&params.command);
		let task_hints = self.task_hints.clone();
		let exec = request_ctx::with_request_context(session.view_cache.clone(), async move {
			let outcome = fs::execute_shell_command(&call, Some(notifier))
				.await
				.map_err(|e| e.to_string())?;
			let Some(uri) = outcome.resource_uri else {
				let text = ContentBlock::text(append_hints(outcome.text));
				return Ok(CallToolResult::success(vec![text]).into());
			};
			if as_task {
				if let Some(id) = fs::background::job_id_from_uri(&uri) {
					if let Some(view) = fs::background::read(id) {
						let hints = append_hints(String::new());
						if !hints.is_empty() {
							if let Ok(mut task_hints) = task_hints.lock() {
								task_hints.insert(id.to_string(), hints);
							}
						}
						return Ok(CreateTaskResult::new(job_task(id, &view, false)).into());
					}
				}
			}
			// A ResourceLink is the protocol-native "watch this" signal: the
			// client follows it generically, with no octofs-specific
			// knowledge, so shell can be served by any MCP server.
			let content = vec![
				ContentBlock::text(append_hints(outcome.text)),
				ContentBlock::resource_link(Resource::new(uri, name)),
			];
			Ok::<CallToolResponse, String>(CallToolResult::success(content).into())
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
		let session = self.session(&context);
		let workdir = session.workdir.get_current();
		let call = McpToolCall {
			tool_name: "workdir".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_workdir_command(&call).await.map_err(|e| e.to_string())?;

		// Update session workdir state based on the structured result
		match &result {
			fs::WorkdirResult::Set { current, .. } => {
				session.workdir.set_current(current.clone());
			}
			fs::WorkdirResult::Reset => {
				session.workdir.reset();
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

			let collapsed_type = obj.get_mut("type").and_then(|t| t.as_array_mut()).and_then(|types| {
				types.retain(|t| t.as_str() != Some("null"));
				(types.len() == 1).then(|| types[0].clone())
			});
			if let Some(single) = collapsed_type {
				obj.insert("type".to_string(), single);
			}

			for key in ["anyOf", "oneOf"] {
				let only = obj.get_mut(key).and_then(|v| v.as_array_mut()).and_then(|variants| {
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
	if schema.get("default").is_some_and(serde_json::Value::is_null) {
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
			// fallback path). `claude/channel` lets Claude Code sessions that
			// enabled octofs as a channel receive job exits (see
			// `notify_claude_channel`). The tasks extension hands promoted shell
			// jobs to clients that declare it (see `client_supports_tasks`).
			ServerCapabilities::builder()
				.enable_experimental_with(ExperimentalCapabilities::from([(
					CLAUDE_CHANNEL_CAPABILITY.to_string(),
					JsonObject::new(),
				)]))
				.enable_tools()
				.enable_resources()
				.enable_resources_subscribe()
				.enable_tasks()
				.build(),
		)
		// Not `Implementation::from_build_env()`: that is a plain fn in rmcp, so its
		// `env!` reads rmcp's crate name and version, not ours.
		.with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
		.with_protocol_version(ProtocolVersion::V_2026_07_28)
		.with_instructions(
			"Filesystem tools. File lines render as `N:hh|content`; `N:hh` is the line id edit \
				 tools target, and edit results are diffs with fresh ids, so edits chain without \
				 re-viewing files. Reuse returned content and ids; read complete relevant blocks, \
				 then act on them. Each response costs a round trip: first decide what you need \
				 next, then request every call that doesn't depend on another's result in that one \
				 response. Edit in as few rounds as possible: gather every change a file needs and apply \
				 them in ONE batch_edit (up to 50 inserts/replaces, targets from the same view); edits \
				 to different files go out together as parallel calls in the same response — never a \
				 round per hunk. Find several names with one `view` search using a regex alternation \
				 (`foo|bar|baz`). Start a long build or test in `shell` and keep working on what doesn't \
				 need its result; its exit arrives as a message."
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
				// The shell description is written for the default soft mode.
				if tool.name == "shell" && crate::mcp::get_hint_mode() == crate::mcp::HintMode::Hard {
					if let Some(description) = tool.description.as_mut() {
						*description = description.replace(SHELL_MISUSE_SOFT, SHELL_MISUSE_HARD).into();
					}
				}
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
	// `octofs://jobs/<id>`, readable for its status and retained output. On exit the
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
		let body = fs::background::resource_text(id)
			.ok_or_else(|| ErrorData::resource_not_found(format!("No such background job: {uri}"), None))?;
		let result = ReadResourceResult::new(vec![ResourceContents::text(body, uri)]);
		if !supports_cache_hints(&context) {
			return Ok(result.into());
		}
		// A running job's tail changes between reads, and it is this session's job.
		Ok(result.with_ttl_ms(0).with_cache_scope(CacheScope::Private).into())
	}

	// 2026-07-28 change notifications are opt-in: the client opens a
	// `subscriptions/listen` stream filtered to what it wants, and the SDK
	// acknowledges the accepted subset before `listen` runs. Accept whatever
	// the client asked for — the SDK intersects it with the capabilities
	// advertised in `get_info` (resources.subscribe gates URI subscriptions).
	fn accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter> {
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
		let uris = sink.accepted().resource_subscriptions.clone().unwrap_or_default();
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

	// SEP-2663: every task is a background shell job, so its state comes straight
	// from the job registry. A running job is `working`; an exited one carries the
	// result a foreground call would have returned, or `cancelled` if a client
	// stopped it.
	async fn get_task(
		&self,
		request: GetTaskParams,
		_context: RequestContext<RoleServer>,
	) -> Result<GetTaskResult, ErrorData> {
		let id = request.task_id;
		let view = fs::background::read(&id).ok_or_else(|| unknown_task(&id))?;
		let payload = match view.status {
			fs::background::JobStatus::Running => TaskPayload::Working,
			fs::background::JobStatus::Exited(_) if view.cancel_requested => TaskPayload::Cancelled,
			fs::background::JobStatus::Exited(code) => {
				let hints = self
					.task_hints
					.lock()
					.ok()
					.and_then(|hints| hints.get(&id).cloned())
					.unwrap_or_default();
				TaskPayload::Completed {
					result: job_result(code, &view.output, &hints),
				}
			}
		};
		Ok(GetTaskResult::new(DetailedTask::new(
			job_task(&id, &view, true),
			payload,
		)))
	}

	// A shell task never asks for input, so there is nothing to answer; the spec
	// has servers ignore responses to requests that are not outstanding.
	async fn update_task(
		&self,
		request: UpdateTaskParams,
		_context: RequestContext<RoleServer>,
	) -> Result<(), ErrorData> {
		fs::background::status(&request.task_id)
			.map(|_| ())
			.ok_or_else(|| unknown_task(&request.task_id))
	}

	async fn cancel_task(
		&self,
		request: CancelTaskParams,
		_context: RequestContext<RoleServer>,
	) -> Result<(), ErrorData> {
		fs::background::cancel(&request.task_id)
			.await
			.ok_or_else(|| unknown_task(&request.task_id))
	}

	// The default `initialize` (legacy clients) and `discover` (2026-07-28
	// clients) implementations handle version negotiation; the session
	// workdir from client capabilities is applied per-request in
	// `OctofsServer::session`, which covers both eras.
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
	/// Lines around each `content` match. Default: 0.
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
					_ => {
						return Err(serde::de::Error::custom(
							"batch_edit edits a single file, but the operations name different paths; \
							 issue one batch_edit per file",
						))
					}
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
