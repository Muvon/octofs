use std::sync::{Arc, RwLock};

use rmcp::{
	handler::server::{wrapper::Parameters, ServerHandler},
	model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
	schemars,
	service::RequestContext,
	tool, tool_handler, tool_router, RoleServer,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

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
}

impl OctofsServer {
	/// Create a new server instance with the given session root directory.
	pub fn new() -> Self {
		let root = super::get_session_root_directory();
		Self {
			workdir: Arc::new(SessionWorkdir::new(root)),
			session_applied: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
		description = "Read files, view directories, and search file content. Unified read-only tool. \
			File lines are rendered as `N:hh|content` — N is the line number, hh a 2-char content \
			hash. Together `N:hh` is the line id that edit tools (batch_edit, extract_lines) take \
			as targets; copy ids verbatim from this output. Listing a directory returns each file \
			with its line count and estimated token cost (`path\tNL\t~Nt`) — use it to scope \
			unfamiliar trees and budget reads before opening files. For rg-style content search \
			across several roots, separate literal files/directories in `path` with `|`. The \
			`pattern` filter accepts ripgrep -g glob grammar, including `**` and leading `!`."
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
		request_ctx::with_request_context(async move {
			let result = fs::execute_view(&call).await.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		description = "Perform text editing operations on files: create, str_replace, delete, undo_edit. \
			For str_replace pass raw file text in old_text/new_text — real newlines and tabs, no \\n \
			escaping, and no `N:hh|` line-id prefixes. old_text must match the file exactly and \
			uniquely, or pass replace_all: true to replace every occurrence (rename-style edits); \
			on multiple matches or no match, the error lists candidate locations with line ids you \
			can use with batch_edit instead. When you already know the target line ids, prefer \
			batch_edit — it verifies ids against the file and avoids content-match ambiguity."
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
		request_ctx::with_request_context(async move {
			let result = fs::execute_text_editor(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		description = "Perform multiple insert/replace operations on a SINGLE file atomically. \
			Targets are line ids (e.g. \"12:a3\") copied from view or previous edit output; each id \
			is verified against the file before anything is applied, so a stale id fails with the \
			current content instead of editing the wrong line. The result is a diff of every \
			operation with FRESH line ids — use those ids directly for follow-up edits; no need to \
			re-view the file. Insert anchors 0 (file start) and -1 (append after last line) are \
			plain integers. Content is raw text without line-id prefixes."
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
		request_ctx::with_request_context(async move {
			let result = fs::execute_batch_edit(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(description = "Copy lines from a source file and append them into a target file.")]
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
		request_ctx::with_request_context(async move {
			let result = fs::execute_extract_lines(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
		})
		.await
	}

	#[tool(
		description = "Execute a command in the shell — builds, tests, git, the project's own \
			CLIs. Not a file reader: `view` reads files, lists directories, and does rg-style \
			content search, so prefer it over cat/grep/ls/sed for inspection. Output is what a \
			real terminal would display: ANSI escapes and progress-bar redraw frames are \
			removed, and runs of identical consecutive lines collapse to the line plus a \
			repeat count. For byte-exact inspection (line endings, control bytes) pipe through \
			`od -c`, `xxd`, or `cat -v` — their printable output passes through untouched."
	)]
	async fn shell(
		&self,
		context: RequestContext<RoleServer>,
		Parameters(params): Parameters<ShellParams>,
	) -> Result<String, String> {
		self.ensure_session_workdir(&context);
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "shell".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		// Heartbeat while the command runs. The MCP idle timeout cancels a call
		// that reports nothing, and a build or test suite is silent by nature —
		// more so once the model redirects its output to a log to keep the reply
		// small. Without a liveness signal the only way to run anything slow is
		// to detach it and poll, which costs a model round-trip per check and
		// re-sends the whole conversation each time. A progress notification
		// resets the client's idle timer (it still enforces an absolute cap), so
		// a slow foreground command simply works and there is nothing to poll.
		let progress_token = context.meta.get_progress_token();
		let peer = context.peer.clone();
		let exec = request_ctx::with_request_context(async move {
			let result = fs::execute_shell_command(&call)
				.await
				.map_err(|e| e.to_string())?;
			Ok(append_hints(result))
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
		description = "Change the working directory used by subsequent tool calls. \
			Do NOT call this just to check the current directory — all tools accept \
			both relative and absolute paths and resolve relative paths against the \
			session's working directory automatically. Only invoke this tool when you \
			actually need to switch to a different directory (set `path`) or revert \
			to the session root (`reset: true`)."
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

#[tool_handler(router = Self::tool_router())]
impl ServerHandler for OctofsServer {
	fn get_info(&self) -> ServerInfo {
		ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
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

	// The default `initialize` (legacy clients) and `discover` (2026-07-28
	// clients) implementations handle version negotiation; the session
	// workdir from client capabilities is applied per-request in
	// `ensure_session_workdir`, which covers both eras.
}

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
	/// A single file or directory path (e.g. "src/main.rs"). During content search,
	/// `|` may separate literal search roots (e.g. "docs|scripts|README.md"). An existing
	/// path containing `|` takes precedence. Multi-root search is limited to 32 unique,
	/// single-line roots and 8192 UTF-8 bytes. To read several files without content
	/// search, make multiple `view` calls — they run in parallel.
	/// Supports ssh://user@host:port/path or sftp://user@host:port/path for remote
	/// filesystem access.
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
	/// Ripgrep `-g/--glob` compatible filter for directory listing and content search.
	/// A glob without `/` matches filenames at any depth; a glob with `/` matches the
	/// returned relative path. Supports `*`, `**`, `?`, character classes (`[abc]`),
	/// brace alternatives (`*.{rs,toml}`), and leading `!` exclusions. Use `|` to supply
	/// ordered globs in one string, e.g. `**/*.rs|!target/**` (later globs take precedence).
	/// Filtering is applied after gitignore/hidden traversal; use `include_hidden: true`
	/// for hidden paths, while gitignored paths remain excluded. Maximum 4096 UTF-8 bytes,
	/// 64 `|`-separated globs, and 16 nested brace levels. Must be a single line. Escape
	/// a literal leading `#` or `!` as `\#` or `\!`; malformed patterns fail before
	/// directory traversal.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Content search string. By default treated as a literal substring.
	/// Set `regex: true` to interpret as a Rust regex (case-insensitive via `(?i)` prefix,
	/// e.g. `(?i)error`). Only used when path is a directory or a single file.
	/// When set, `start`/`end` are ignored (the whole file/tree is searched).
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
}

/// JSON schema for a single line endpoint (`start`/`end`/`append_line`/op `start`/`end`).
///
/// One shape everywhere: a line id string "N:hh" copied from view output (verified
/// against the file before edits), or a plain integer line number where positions are
/// allowed (view ranges, insert anchors 0/-1). `anyOf` (not `oneOf`) is used because
/// it has strictly wider cross-stack support.
fn line_endpoint_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
	let schema = serde_json::json!({
		"description": "A line endpoint: a line id \"N:hh\" copied from view/edit output (e.g. \"12:a3\") or an integer line number (negative counts from the end, -1 = last line). Edit targets require ids; view ranges accept plain numbers.",
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
	/// Detach the command and return only its PID. Its output is discarded and
	/// there is no completion signal, so the only way to find out what happened
	/// is to make the command redirect to a file and then read that file — each
	/// check costing a full round-trip. Do NOT use this for work whose result
	/// you need: builds, test suites and installs run fine in the foreground
	/// however long they are silent, because the call reports liveness while it
	/// waits. Reserve `background` for processes you deliberately want to
	/// outlive the call, such as a server you are about to make requests to.
	#[serde(default)]
	pub background: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WorkdirParams {
	/// Absolute path or path relative to current workdir to switch into.
	/// Required unless `reset: true`. Do not pass `"."` — that is a no-op.
	/// Supports ssh://user@host:port/path for remote filesystem access.
	#[serde(default)]
	pub path: Option<String>,
	/// If true, revert to the original session working directory.
	#[serde(default)]
	pub reset: Option<bool>,
}

// ── Server implementation ───────────────────────────────────────────────────────
