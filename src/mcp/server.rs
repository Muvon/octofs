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
use serde_json::Value;
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
		description = "Read files, view directories, and search file content. Unified read-only tool."
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
		description = "Perform text editing operations on files: create, str_replace, undo_edit."
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

	#[tool(description = "Get or set the working directory used by all MCP tools.")]
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
				 text_editor (create/str_replace/undo), batch_edit (multi-op line edits), \
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
	/// One or more file/directory paths. Single path for file viewing or directory listing; multiple paths for multi-file viewing (max 50).
	#[serde(deserialize_with = "deserialize_string_or_vec")]
	#[schemars(length(min = 1, max = 50))]
	pub paths: Vec<String>,
	/// Line range for file viewing. Accepts:
	/// - Single range: `[start, end]` (1-indexed, inclusive) — applied to single file or all files
	/// - Per-file ranges: `[[start, end], [start, end], ...]` — one range per path
	///   Accepts line numbers or line identifiers from previous `view` output.
	#[serde(default)]
	pub lines: Option<Vec<Value>>,
	/// Filename glob filter for directory listing.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Content search string (fixed-string match). Only used when path is a directory.
	#[serde(default)]
	pub content: Option<String>,
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

/// Deserialize a value that can be either a single string or an array of strings.
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::de;

	struct StringOrVec;

	impl<'de> de::Visitor<'de> for StringOrVec {
		type Value = Vec<String>;

		fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
			formatter.write_str("a string or an array of strings")
		}

		fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
			Ok(vec![value.to_string()])
		}

		fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
			Ok(vec![value])
		}

		fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
			let mut vec = Vec::new();
			while let Some(elem) = seq.next_element::<String>()? {
				vec.push(elem);
			}
			Ok(vec)
		}
	}

	deserializer.deserialize_any(StringOrVec)
}

/// Text editor operation. The `command` field discriminates between variants,
/// and each variant declares exactly the fields it needs — so `old_text`/`new_text`
/// are schema-level required for `str_replace` and absent for `create`/`undo_edit`.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum TextEditorParams {
	/// Create a new file with the given content. Fails if the file already exists.
	Create {
		/// Path to the file to create.
		path: String,
		/// Content to write to the new file.
		content: String,
	},
	/// Replace an exact text match in an existing file.
	StrReplace {
		/// Path to the file to edit.
		path: String,
		/// Text to find (must match exactly, including whitespace).
		old_text: String,
		/// Replacement text.
		new_text: String,
	},
	/// Undo the last edit to a file (up to 10 levels of history).
	UndoEdit {
		/// Path to the file to revert.
		path: String,
	},
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchEditOperationType {
	Insert,
	Replace,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BatchEditOperation {
	/// Type of operation: 'insert' (after line) or 'replace' (line range)
	pub operation: BatchEditOperationType,
	/// Line numbers from ORIGINAL file content.
	pub line_range: BatchEditLineRange,
	/// Raw content to insert or replace with.
	pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum BatchEditLineRange {
	/// Single line number for insert (0=beginning, N=after line N, -1=after last line)
	Single(i64),
	/// Line range [start, end] for replace (1-indexed, inclusive)
	Range(Vec<i64>),
	/// Single hash identifier for insert (hash mode: insert after line with this hash)
	Hash(String),
	/// Hash range [start_hash, end_hash] for replace (hash mode)
	HashRange(Vec<String>),
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
	/// Two-element array [start, end] with 1-indexed line numbers (inclusive)
	#[schemars(length(min = 2, max = 2))]
	pub from_range: Vec<i64>,
	/// Path to the target file where extracted lines will be appended
	pub append_path: String,
	/// Position where to append: 0=beginning, -1=end, N=after line N (1-indexed)
	pub append_line: i64,
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
	/// Optional path to set as new working directory
	#[serde(default)]
	pub path: Option<String>,
	/// If true, reset to original session working directory
	#[serde(default)]
	pub reset: Option<bool>,
}

// ── Server implementation ───────────────────────────────────────────────────────
