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
	/// File or directory path. Pass a single path string for the common case
	/// (e.g. "src/main.rs"); pass an array of paths for multi-file viewing (max 50).
	/// The legacy key `paths` is also accepted.
	#[serde(alias = "paths", deserialize_with = "deserialize_string_or_vec")]
	#[schemars(schema_with = "path_param_schema")]
	pub path: Vec<String>,
	/// Line range(s) to view, as a string or an array of strings:
	///
	/// - Single range: `"START-END"` (e.g. `"10-25"`) or a single line `"42"`.
	/// - Multiple ranges (ONE path): `["1-50", "200-250"]`.
	/// - Per-file ranges (N paths): one range string per path, positionally.
	///
	/// Endpoints are 1-indexed line numbers (negatives count from the end: `"-1"` = last line)
	/// or 4-char hashes in hash mode (e.g. `"a3bd-c7f2"`). A single range string applies to all files.
	#[serde(default)]
	#[schemars(schema_with = "lines_param_schema")]
	pub lines: Option<serde_json::Value>,
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

/// JSON schema for `ViewParams::path`.
///
/// Accepts a single path string (the common case) or an array of paths for
/// multi-file viewing. Runtime parsing also accepts the legacy `paths` key.
fn path_param_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
	serde_json::from_value(serde_json::json!({
		"description": "File or directory path. A single path string (e.g. \"src/main.rs\"), or an array of paths for multi-file viewing (max 50).",
		"oneOf": [
			{ "type": "string" },
			{
				"type": "array",
				"items": { "type": "string" },
				"minItems": 1,
				"maxItems": 50
			}
		],
		"examples": ["src/main.rs", ["src/main.rs", "src/lib.rs"]]
	}))
	.expect("static schema is valid JSON")
}

/// JSON schema for `ViewParams::lines`.
///
/// Hand-written so the two accepted shapes are explicit with concrete examples.
/// Ranges are compact strings ("10-25") instead of nested arrays, which avoids the
/// over-wrapping mistakes LLMs made with the old `[[start,end]]` shape.
///
/// Runtime parsing in `fs::core::parse_lines_param` enforces shape and arity.
fn lines_param_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
	serde_json::from_value(serde_json::json!({
		"description": "Line range(s) for file viewing. Either a single range string or an array of range strings:\n\
			- Single range: \"START-END\" (e.g. \"10-25\") or a single line \"42\". Applied to the single file or all files.\n\
			- Multiple ranges on ONE path: [\"1-50\", \"200-250\"].\n\
			- Per-file ranges with N paths: one range string per path, positionally.\n\
			Endpoints are 1-indexed line numbers (negatives count from the end: \"-1\" = last line) or 4-char hashes in hash mode (e.g. \"a3bd-c7f2\").",
		"oneOf": [
			{
				"description": "A single range string applied to the single file or all files",
				"type": "string",
				"examples": ["10-25", "42", "a3bd-c7f2"]
			},
			{
				"description": "Range strings — multiple ranges (one path) or per-file ranges (many paths)",
				"type": "array",
				"items": { "type": "string" },
				"minItems": 1,
				"examples": [["1-50", "200-250"], ["a3bd-c7f2"]]
			}
		]
	}))
	.expect("static schema is valid JSON")
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
	/// Target in the ORIGINAL file, as a string.
	/// - insert: a single anchor — "0" (file start), "-1" (after last line), "N" (after line N), or a hash.
	/// - replace: a range "START-END" (e.g. "10-25"), a single line "42", or a hash range "a3bd-c7f2" (single hash "a3bd" allowed).
	pub line_range: String,
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
	/// Line range to copy, as a string: "START-END" (1-indexed inclusive, e.g. "10-25"),
	/// a single line "42", or a hash range in hash mode ("a3bd-c7f2").
	pub from_range: String,
	/// Path to the target file where extracted lines will be appended
	pub append_path: String,
	/// Where to append in the target, as a string: "0" = beginning, "-1" = end,
	/// "N" = after line N (1-indexed), or a hash in hash mode.
	pub append_line: String,
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
