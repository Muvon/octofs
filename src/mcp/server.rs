use std::sync::{Arc, RwLock};

use rmcp::{
	handler::server::{wrapper::Parameters, ServerHandler},
	model::{
		Implementation, InitializeRequestParams, InitializeResult, ProtocolVersion,
		ServerCapabilities, ServerInfo,
	},
	schemars,
	service::RequestContext,
	tool, tool_handler, tool_router, RoleServer,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::fs;
use super::hint_accumulator;
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

/// MCP server with per-session working directory isolation.
#[derive(Debug, Clone)]
pub struct OctofsServer {
	/// Per-session working directory state.
	workdir: Arc<SessionWorkdir>,
}

impl OctofsServer {
	/// Create a new server instance with the given session root directory.
	pub fn new() -> Self {
		let root = super::get_session_root_directory();
		Self {
			workdir: Arc::new(SessionWorkdir::new(root)),
		}
	}

	/// Create a new server instance with an explicit root directory.
	/// Used by HTTP mode to create fresh instances per session.
	pub fn with_root(root: PathBuf) -> Self {
		Self {
			workdir: Arc::new(SessionWorkdir::new(root)),
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
		description = "Read files, view directories, and search file content. Unified read-only tool. Listing a directory returns each file with its line count and estimated token cost (`path\tNL\t~Nt`) — use it to scope unfamiliar trees and budget reads before opening files."
	)]
	async fn view(&self, Parameters(params): Parameters<ViewParams>) -> Result<String, String> {
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "view".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_view(&call).await.map_err(|e| e.to_string())?;
		Ok(append_hints(result))
	}

	#[tool(
		description = "Perform text editing operations on files: create, str_replace, delete, undo_edit."
	)]
	async fn text_editor(
		&self,
		Parameters(params): Parameters<TextEditorParams>,
	) -> Result<String, String> {
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "text_editor".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_text_editor(&call)
			.await
			.map_err(|e| e.to_string())?;
		Ok(append_hints(result))
	}

	#[tool(description = "Perform multiple insert/replace operations on a SINGLE file atomically.")]
	async fn batch_edit(
		&self,
		Parameters(params): Parameters<BatchEditParams>,
	) -> Result<String, String> {
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "batch_edit".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_batch_edit(&call)
			.await
			.map_err(|e| e.to_string())?;
		Ok(append_hints(result))
	}

	#[tool(description = "Copy lines from a source file and append them into a target file.")]
	async fn extract_lines(
		&self,
		Parameters(params): Parameters<ExtractLinesParams>,
	) -> Result<String, String> {
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "extract_lines".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_extract_lines(&call)
			.await
			.map_err(|e| e.to_string())?;
		Ok(append_hints(result))
	}

	#[tool(description = "Execute a command in the shell.")]
	async fn shell(&self, Parameters(params): Parameters<ShellParams>) -> Result<String, String> {
		let workdir = self.workdir.get_current();
		let call = McpToolCall {
			tool_name: "shell".to_string(),
			parameters: serde_json::to_value(&params).unwrap_or_default(),
			tool_id: String::new(),
			workdir,
		};
		let result = fs::execute_shell_command(&call)
			.await
			.map_err(|e| e.to_string())?;
		Ok(append_hints(result))
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
		Parameters(params): Parameters<WorkdirParams>,
	) -> Result<String, String> {
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
			.with_protocol_version(ProtocolVersion::V_2025_03_26)
			.with_instructions(
				"This server provides filesystem tools: view (read files/dirs), \
				 text_editor (create/str_replace/delete/undo), batch_edit (multi-op line edits), \
				 extract_lines (copy lines between files), shell (execute commands), \
				 workdir (get/set working directory)."
					.to_string(),
			)
	}

	/// Extract workdir from experimental capabilities during initialize handshake.
	/// For HTTP mode, each session can specify its initial working directory.
	async fn initialize(
		&self,
		request: InitializeRequestParams,
		_context: RequestContext<RoleServer>,
	) -> Result<InitializeResult, rmcp::ErrorData> {
		// Extract workdir from capabilities.experimental.session
		if let Some(experimental) = &request.capabilities.experimental {
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

		Ok(self.get_info())
	}
}

/// Drain any accumulated hints and append them to the tool result.
/// Called after tool execution to surface misuse guidance to the LLM.
fn append_hints(mut result: String) -> String {
	let hints = hint_accumulator::drain_hints();
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
	/// A single file or directory path (e.g. "src/main.rs"). To view several files,
	/// make multiple `view` calls — they run in parallel.
	pub path: String,
	/// First line to show (inclusive). Integer line number (negative counts from the
	/// end: -1 = last line) or a string hash in hash mode. Omit to start at line 1.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: Option<serde_json::Value>,
	/// Last line to show (inclusive). Integer line number (negative counts from the end)
	/// or a string hash in hash mode. Omit to read to the end of the file.
	/// Omit BOTH `start` and `end` to view the whole file.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Filename glob filter for directory listing.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Content search string. By default treated as a literal substring.
	/// Set `regex: true` to interpret as a Rust regex (case-insensitive via `(?i)` prefix,
	/// e.g. `(?i)error`). Only used when path is a directory or a single file.
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
/// An endpoint is either an integer line number or a string hash. The JSON type
/// disambiguates the two — no range strings, no ambiguity for all-digit hashes.
fn line_endpoint_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
	serde_json::from_value(serde_json::json!({
		"description": "A line endpoint: an integer line number (negative counts from the end, -1 = last line), or a string hash in hash mode (e.g. \"a3bd\").",
		"oneOf": [
			{ "type": "integer", "format": "int64" },
			{ "type": "string" }
		],
		"examples": [10, -1, "a3bd"]
	}))
	.expect("static schema is valid JSON")
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
	/// The operation to perform: create, str_replace, undo_edit
	pub command: TextEditorCommand,
	/// REQUIRED. Path to the file to operate on.
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
	/// Start line in the ORIGINAL file. Integer line number or a string hash.
	/// For `insert` this is the anchor: 0 = file start, -1 = after last line, N = after line N.
	/// For `replace` this is the first line of the range to replace.
	#[schemars(schema_with = "line_endpoint_schema")]
	pub start: serde_json::Value,
	/// Last line of the range to replace (inclusive), for `replace` only.
	/// Omit for a single-line replace (defaults to `start`). Ignored for `insert`.
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub end: Option<serde_json::Value>,
	/// Raw content to insert or replace with.
	pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BatchEditParams {
	/// Path to the file to edit
	pub path: String,
	/// Array of operations for batch_edit on SINGLE file. Max 50 operations.
	#[schemars(length(max = 50))]
	pub operations: Vec<BatchEditOperation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ExtractLinesParams {
	/// Path to the source file to extract lines from
	pub from_path: String,
	/// First line to copy (inclusive). Integer line number or a string hash.
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_start: serde_json::Value,
	/// Last line to copy (inclusive). Integer line number or a string hash.
	/// Omit to copy a single line (defaults to `from_start`).
	#[serde(default)]
	#[schemars(schema_with = "line_endpoint_schema")]
	pub from_end: Option<serde_json::Value>,
	/// Path to the target file where extracted lines will be appended
	pub append_path: String,
	/// Where to append in the target: 0 = beginning, -1 = end, N = after line N
	/// (integer), or a string hash in hash mode.
	#[schemars(schema_with = "line_endpoint_schema")]
	pub append_line: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ShellParams {
	/// The shell command to execute
	pub command: String,
	/// Run command in background and return PID
	#[serde(default)]
	pub background: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WorkdirParams {
	/// Absolute path or path relative to current workdir to switch into.
	/// Required unless `reset: true`. Do not pass `"."` — that is a no-op.
	#[serde(default)]
	pub path: Option<String>,
	/// If true, revert to the original session working directory.
	#[serde(default)]
	pub reset: Option<bool>,
}

// ── Server implementation ───────────────────────────────────────────────────────
