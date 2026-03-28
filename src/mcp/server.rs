// Copyright 2025 Muvon Un Limited
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

//! MCP server implementation using the official rmcp SDK.
//!
//! Tool methods return `Result<String, String>` which rmcp auto-converts:
//! - `Ok(text)` → `CallToolResult::success` with text content
//! - `Err(text)` → `CallToolResult::error` with text content (tool-level error)

use rmcp::{
	handler::server::{router::tool::ToolRouter, wrapper::Parameters, ServerHandler},
	model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
	schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::fs;

// ── Tool parameter schemas ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ViewParams {
	/// File path, directory path, or glob pattern. Required unless `paths` is provided.
	#[serde(default)]
	pub path: Option<String>,
	/// Array of file paths for multi-file viewing. Max 50 files.
	#[serde(default)]
	#[schemars(length(max = 50))]
	pub paths: Option<Vec<String>>,
	/// Line range [start, end] for single file viewing (1-indexed, inclusive).
	#[serde(default)]
	#[schemars(length(min = 2, max = 2))]
	pub lines: Option<Vec<i64>>,
	/// Filename glob filter for directory listing.
	#[serde(default)]
	pub pattern: Option<String>,
	/// Content search string (ripgrep). Only used when path is a directory.
	#[serde(default)]
	pub content: Option<String>,
	/// Maximum directory traversal depth.
	#[serde(default)]
	pub max_depth: Option<usize>,
	/// Include hidden files/directories starting with '.'.
	#[serde(default)]
	pub include_hidden: Option<bool>,
	/// Show line numbers in content search results.
	#[serde(default = "default_true")]
	pub line_numbers: Option<bool>,
	/// Context lines around content search matches.
	#[serde(default)]
	pub context: Option<usize>,
}

fn default_true() -> Option<bool> {
	Some(true)
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextEditorCommand {
	Create,
	StrReplace,
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
pub struct AstGrepParams {
	/// The AST pattern to search for
	pub pattern: String,
	/// Optional language of the code
	#[serde(default)]
	pub language: Option<String>,
	/// Optional rewrite pattern for refactoring
	#[serde(default)]
	pub rewrite: Option<String>,
	/// Optional array of file paths to search within
	#[serde(default)]
	pub paths: Option<Vec<String>>,
	/// Optional context lines around matches
	#[serde(default)]
	pub context: Option<usize>,
	/// Apply rewrites to all matches without confirmation
	#[serde(default)]
	pub update_all: Option<bool>,
	/// Output in JSON format
	#[serde(default)]
	pub json_output: Option<bool>,
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

#[derive(Clone)]
pub struct OctofsServer {
	tool_router: ToolRouter<Self>,
}

#[tool_router]
impl OctofsServer {
	pub fn new() -> Self {
		Self {
			tool_router: Self::tool_router(),
		}
	}

	/// Read files, view directories, and search file content.
	#[tool(
		name = "view",
		description = "Read files, view directories, and search file content. Unified read-only tool.

**File** (path is a file): returns plain text with line identifiers (numbers or content-based hashes depending on server --line-mode).
- Whole file: `{\"path\": \"src/main.rs\"}`
- Line range (negative ok: -1 = last): `{\"path\": \"src/main.rs\", \"lines\": [10, 20]}`

**Multi-file** (paths array, max 50): `{\"paths\": [\"src/main.rs\", \"src/lib.rs\"]}`

**Directory** (path is a directory):
- List: `{\"path\": \"src/\"}` — filter: `\"pattern\": \"*.rs\"`, depth: `\"max_depth\": 2`
- Search content (ripgrep): `{\"path\": \"src\", \"content\": \"fn main\"}`
- Hidden files: `\"include_hidden\": true`

In hash mode (--line-mode hash): each line is prefixed with a 4-char hex hash derived from its content (e.g., `a3bd: code here`). These hashes are stable — unchanged lines keep the same hash across edits.",
		annotations(read_only_hint = true)
	)]
	async fn view(&self, Parameters(params): Parameters<ViewParams>) -> Result<String, String> {
		with_hints(fs::execute_view(&make_call("view", &params)).await)
	}

	/// Perform text editing operations on files.
	#[tool(
		name = "text_editor",
		description = "Perform text editing operations on files.

The `command` parameter specifies the operation to perform.
For READ operations use the `view` tool instead.
For line-based edits (insert after line, replace by line range), use the separate `batch_edit` tool.

Commands:

`create`: Create new file. Fails if file already exists.
- `{\"command\": \"create\", \"path\": \"src/new.rs\", \"content\": \"...\"}` — creates parent dirs automatically.

`str_replace`: Replace exact string match. Requires exactly 1 match — fails on 0 (no match) or 2+ (ambiguous).
- `{\"command\": \"str_replace\", \"path\": \"src/main.rs\", \"old_text\": \"fn old()\", \"new_text\": \"fn new()\"}`
- `old_text` must match exactly (including whitespace). Use raw content, not escaped.
- Fuzzy fallback: if exact match fails, tries whitespace-normalized matching and auto-adjusts indentation.
- On failure: shows closest matches with line numbers, similarity %, and diagnosis.

`undo_edit`: Revert the last edit on a file. Supports up to 10 undo levels per file.
- `{\"command\": \"undo_edit\", \"path\": \"src/main.rs\"}`",
		annotations(destructive_hint = true)
	)]
	async fn text_editor(
		&self,
		Parameters(params): Parameters<TextEditorParams>,
	) -> Result<String, String> {
		with_hints(fs::execute_text_editor(&make_call("text_editor", &params)).await)
	}

	/// Perform multiple atomic edits on a single file.
	#[tool(
		name = "batch_edit",
		description = "Perform multiple insert/replace operations on a SINGLE file atomically, using ORIGINAL line numbers or hash identifiers.

Use when: 2+ edits on an unmodified file (all line references point to the file before any changes).
Do NOT use: after any prior edit to the file — line numbers/hashes will be stale.

CRITICAL: Always `view` the exact line range before replacing — never assume what is at a line.
If you edited this file before, re-view it first.

CRITICAL: All line_range values reference the ORIGINAL file content before ANY changes.
Even if operation 1 replaces 1 line with 10 lines, operation 2 still uses the original references.
The tool handles offset calculation internally — you never need to adjust for prior operations.

Operations:
- `insert`: line_range = integer or hash string → insert after that line (0 = beginning of file, -1 = after last line)
- `replace`: line_range = [start, end] (numbers or hash strings) → remove those lines, insert new content

In hash mode (--line-mode hash): line_range accepts 4-char hex hash identifiers from `view` output.
  - insert: `\"line_range\": \"a3bd\"` → insert after the line with hash a3bd
  - replace: `\"line_range\": [\"a3bd\", \"c7f2\"]` → replace lines from hash a3bd to c7f2

Negative line numbers count from end: -1 = last line, -2 = second-to-last, etc.

Key rule — NEVER retype unchanged lines in replace:
❌ Bad: replace [1,3] with \"use std::fs;\\nuse std::io;\\nuse std::path::PathBuf;\" (retyped lines 1-2)
✅ Good: replace [3,3] with \"use std::path::PathBuf;\" (only the line actually changing)

Empty content in replace deletes the targeted lines entirely.

Duplicate-line guard: the tool rejects content whose first/last line matches the line
immediately before/after the replacement range — a common mistake where surrounding
context is accidentally included. Fix: shrink the range or trim the content.

Max 50 operations per call.

Atomicity: either ALL operations succeed or NONE are applied — the file is never left in a partial state.

Returns a diff of all changes made (using line numbers or hashes depending on server mode):
- Context lines: `ID: <text>` (3 lines before/after each change)
- Removed lines: `-ID: <text>`
- Added lines:   `+ID: <text>`
- Multiple ops separated by `---`
Read the diff to verify edits landed correctly — no need for a follow-up `view` call.",
		annotations(destructive_hint = true)
	)]
	async fn batch_edit(
		&self,
		Parameters(params): Parameters<BatchEditParams>,
	) -> Result<String, String> {
		with_hints(fs::execute_batch_edit(&make_call("batch_edit", &params)).await)
	}

	/// Copy lines from source to target file.
	#[tool(
		name = "extract_lines",
		description = "Copy lines from a source file and append them into a target file. Source is not modified.

- `append_line`: 0 = beginning, -1 = end, N = after line N.

Examples:
- `{\"from_path\": \"src/utils.rs\", \"from_range\": [10, 25], \"append_path\": \"src/new.rs\", \"append_line\": -1}`
- `{\"from_path\": \"config.toml\", \"from_range\": [1, 5], \"append_path\": \"new.toml\", \"append_line\": 0}`
- `{\"from_path\": \"main.rs\", \"from_range\": [50, 60], \"append_path\": \"module.rs\", \"append_line\": 3}`",
		annotations(destructive_hint = true)
	)]
	async fn extract_lines(
		&self,
		Parameters(params): Parameters<ExtractLinesParams>,
	) -> Result<String, String> {
		with_hints(fs::execute_extract_lines(&make_call("extract_lines", &params)).await)
	}

	/// Execute a shell command.
	#[tool(
		name = "shell",
		description = "Execute a command in the shell. Returns stdout+stderr combined, with success/failure indication.

Each command runs in its own process — state (cd, exports) does not persist. Chain with `&&`: `cd foo && cargo build`.

Background: set `background: true` to get a PID immediately; kill with `kill <pid>`.

Examples:
- `{\"command\": \"cargo test\"}`
- `{\"command\": \"python -m http.server 8000\", \"background\": true}`
- `{\"command\": \"kill 12345\"}`",
		annotations(destructive_hint = true, open_world_hint = true)
	)]
	async fn shell(&self, Parameters(params): Parameters<ShellParams>) -> Result<String, String> {
		with_hints(fs::execute_shell_command(&make_call("shell", &params)).await)
	}

	/// Search and refactor code using AST patterns.
	#[tool(
		name = "ast_grep",
		description = "Search and refactor code using AST patterns with ast-grep.

Pattern syntax:
- `$VAR` — matches ONE AST node
- `$$$VAR` — matches ZERO or more nodes (use for parameter lists, arguments, body)
- `$_` — wildcard, matches one node without capturing

Patterns are structurally exact — every element you include must be present, every element you omit must be absent.
`fn $F($$$A) { $$$B }` does NOT match `fn foo() -> Bar {}` (missing return type in pattern).

Common patterns:
- `console.log($$$)` — find all console.log calls
- `fn $NAME($$$ARGS) -> $RET { $$$BODY }` — Rust functions with return type
- `def $NAME($$$ARGS): $$$` — Python functions

Refactoring: set `rewrite` to transform matches. Same metavariables carry captured content.
- pattern: `oldFunc($$$ARGS)`, rewrite: `newFunc($$$ARGS)`",
		annotations(read_only_hint = false)
	)]
	async fn ast_grep(
		&self,
		Parameters(params): Parameters<AstGrepParams>,
	) -> Result<String, String> {
		with_hints(fs::execute_ast_grep_command(&make_call("ast_grep", &params)).await)
	}

	/// Get or set working directory context.
	#[tool(
		name = "workdir",
		description = "Get or set the working directory used by all MCP tools (shell, text_editor, etc.).

- Get current: `{}` or `{\"path\": null}`
- Set new: `{\"path\": \"/path/to/dir\"}` (absolute or relative to current working directory)
- Reset to session root: `{\"reset\": true}`

Changes apply to the current thread only. Subsequent tool calls resolve paths relative to this directory.",
		annotations(read_only_hint = false, idempotent_hint = true)
	)]
	async fn workdir(
		&self,
		Parameters(params): Parameters<WorkdirParams>,
	) -> Result<String, String> {
		with_hints(fs::execute_workdir_command(&make_call("workdir", &params)).await)
	}
}

#[tool_handler]
impl ServerHandler for OctofsServer {
	fn get_info(&self) -> ServerInfo {
		ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
			.with_server_info(Implementation::from_build_env())
			.with_protocol_version(ProtocolVersion::V_2025_03_26)
			.with_instructions(
				"This server provides filesystem tools: view (read files/dirs), \
			 text_editor (create/str_replace/undo), batch_edit (multi-op line edits), \
			 extract_lines (copy lines between files), shell (execute commands), \
			 ast_grep (AST-aware code search/refactor), workdir (get/set working directory)."
					.to_string(),
			)
	}
}

// ── Helpers ─────────────────────────────────────────────────────────────────────

/// Build an McpToolCall from typed params (serialized back to JSON for legacy handlers).
fn make_call(name: &str, params: &impl serde::Serialize) -> super::McpToolCall {
	super::McpToolCall {
		tool_name: name.to_string(),
		parameters: strip_nulls(serde_json::to_value(params).unwrap_or_default()),
		tool_id: super::next_tool_id(),
	}
}

/// Remove null-valued keys from a JSON object so existing handlers see absent
/// keys (not `null`) for optional fields that were not provided by the caller.
fn strip_nulls(value: Value) -> Value {
	match value {
		Value::Object(map) => Value::Object(
			map.into_iter()
				.filter(|(_, v)| !v.is_null())
				.map(|(k, v)| (k, strip_nulls(v)))
				.collect(),
		),
		other => other,
	}
}

/// Convert handler result to `Result<String, String>` and append accumulated hints.
fn with_hints(result: anyhow::Result<String>) -> Result<String, String> {
	let hints = super::hint_accumulator::drain_hints();
	let suffix = if hints.is_empty() {
		String::new()
	} else {
		format!("\n\n{}", hints.join("\n\n"))
	};
	match result {
		Ok(mut text) => {
			text.push_str(&suffix);
			Ok(text)
		}
		Err(e) => {
			let mut msg = e.to_string();
			msg.push_str(&suffix);
			Err(msg)
		}
	}
}

impl Default for OctofsServer {
	fn default() -> Self {
		Self::new()
	}
}
