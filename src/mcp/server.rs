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

//! MCP server implementation using the official rmcp SDK.
//!
//! Tool methods return `Result<String, String>` which rmcp auto-converts:
//! - `Ok(text)` → `CallToolResult::success` with text content
//! - `Err(text)` → `CallToolResult::error` with text content (tool-level error)

use std::sync::Arc;

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
	/// One or more file/directory paths. Single path for file viewing or directory listing; multiple paths for multi-file viewing (max 50).
	#[serde(deserialize_with = "deserialize_string_or_vec")]
	#[schemars(length(min = 1, max = 50))]
	pub paths: Vec<String>,
	/// Line range [start, end] for single file viewing. Accepts line numbers (1-indexed, inclusive) or line identifiers from previous `view` output.
	#[serde(default)]
	#[schemars(length(min = 2, max = 2))]
	pub lines: Option<Vec<Value>>,
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

// ── Mode-specific schema builders ──────────────────────────────────────────────
// Build JSON schemas by patching the base runtime-type schema with mode-specific
// field types. No phantom types needed — just JSON surgery.

/// Build a `view` input schema with `lines` typed for the active mode.
fn view_schema(hash_mode: bool) -> Arc<serde_json::Map<String, Value>> {
	let mut schema = (*schema_for::<ViewParams>()).clone();
	let props = schema
		.get_mut("properties")
		.and_then(|v| v.as_object_mut())
		.expect("view schema must have properties");

	// Allow paths to be a single string or an array of strings
	let paths_schema = serde_json::json!({
		"anyOf": [
			{
				"type": "string",
				"description": "Single file or directory path."
			},
			{
				"type": "array",
				"description": "One or more file/directory paths (max 50).",
				"items": { "type": "string" },
				"minItems": 1,
				"maxItems": 50
			}
		],
		"description": "One or more file/directory paths. Single string or array. Single path for file viewing or directory listing; multiple paths for multi-file viewing (max 50)."
	});
	props.insert("paths".to_string(), paths_schema);

	let lines_schema = if hash_mode {
		serde_json::json!({
			"type": ["array", "null"],
			"description": "Line range [start, end] for single file viewing. Accepts 4-char hex hash identifiers from previous `view` output or integer line numbers (1-indexed, inclusive).",
			"items": { "anyOf": [{"type": "string"}, {"type": "integer"}] },
			"minItems": 2,
			"maxItems": 2
		})
	} else {
		serde_json::json!({
			"type": ["array", "null"],
			"description": "Line range [start, end] for single file viewing (1-indexed, inclusive). Supports negative indexing: -1 = last line.",
			"items": { "type": "integer" },
			"minItems": 2,
			"maxItems": 2
		})
	};
	props.insert("lines".to_string(), lines_schema);
	Arc::new(schema)
}

/// Build a `batch_edit` input schema with `line_range` typed for the active mode.
fn batch_edit_schema(hash_mode: bool) -> Arc<serde_json::Map<String, Value>> {
	let mut schema = (*schema_for::<BatchEditParams>()).clone();

	let line_range_schema = if hash_mode {
		serde_json::json!({
			"anyOf": [
				{
					"anyOf": [{"type": "string"}, {"type": "integer"}],
					"description": "Insert after this line. Use hash string from `view` output, or integer (0 = beginning, -1 = after last line)."
				},
				{
					"type": "array",
					"description": "Range [start, end] for replace. Use hash strings from `view` output or integer line numbers.",
					"items": { "anyOf": [{"type": "string"}, {"type": "integer"}] },
					"minItems": 2,
					"maxItems": 2
				}
			]
		})
	} else {
		serde_json::json!({
			"anyOf": [
				{
					"type": "integer",
					"description": "Single line number for insert (0=beginning, N=after line N, -1=after last line)."
				},
				{
					"type": "array",
					"description": "Line range [start, end] for replace (1-indexed, inclusive).",
					"items": { "type": "integer" },
					"minItems": 2,
					"maxItems": 2
				}
			]
		})
	};

	// Patch line_range inside the operation schema.
	// schemars may inline or use $defs — handle both.
	patch_batch_edit_line_range(&mut schema, &line_range_schema);

	// Also update the operation's line_range description
	let lr_desc = if hash_mode {
		"Line identifiers from ORIGINAL file content. Use hash strings from `view` output or integer line numbers."
	} else {
		"Line numbers from ORIGINAL file content."
	};
	patch_batch_edit_line_range_description(&mut schema, lr_desc);

	Arc::new(schema)
}

/// Walk the schema JSON to find and replace the `line_range` definition.
fn patch_batch_edit_line_range(
	schema: &mut serde_json::Map<String, Value>,
	new_line_range: &Value,
) {
	// Strategy: find BatchEditLineRange in $defs and replace it,
	// or find it inlined in the operation properties.
	if let Some(defs) = schema.get_mut("$defs").and_then(|v| v.as_object_mut()) {
		if defs.contains_key("BatchEditLineRange") {
			defs.insert("BatchEditLineRange".to_string(), new_line_range.clone());
			return;
		}
	}

	// Fallback: walk into properties -> operations -> items -> properties -> line_range
	if let Some(ops_schema) = schema
		.get_mut("properties")
		.and_then(|v| v.as_object_mut())
		.and_then(|p| p.get_mut("operations"))
		.and_then(|v| v.as_object_mut())
		.and_then(|o| o.get_mut("items"))
		.and_then(|v| v.as_object_mut())
		.and_then(|i| i.get_mut("properties"))
		.and_then(|v| v.as_object_mut())
	{
		if ops_schema.contains_key("line_range") {
			ops_schema.insert("line_range".to_string(), new_line_range.clone());
		}
	}
}

/// Update the description on the line_range field in the operation schema.
fn patch_batch_edit_line_range_description(
	schema: &mut serde_json::Map<String, Value>,
	description: &str,
) {
	// Try inlined path first
	if let Some(lr) = schema
		.get_mut("properties")
		.and_then(|v| v.as_object_mut())
		.and_then(|p| p.get_mut("operations"))
		.and_then(|v| v.as_object_mut())
		.and_then(|o| o.get_mut("items"))
		.and_then(|v| v.as_object_mut())
		.and_then(|i| i.get_mut("properties"))
		.and_then(|v| v.as_object_mut())
		.and_then(|p| p.get_mut("line_range"))
		.and_then(|v| v.as_object_mut())
	{
		lr.insert(
			"description".to_string(),
			Value::String(description.to_string()),
		);
	}
}

/// Build an `extract_lines` input schema with `from_range` and `append_line` typed for the active
/// mode.
fn extract_lines_schema(hash_mode: bool) -> Arc<serde_json::Map<String, Value>> {
	let mut schema = (*schema_for::<ExtractLinesParams>()).clone();
	let props = schema
		.get_mut("properties")
		.and_then(|v| v.as_object_mut())
		.expect("extract_lines schema must have properties");

	let from_range_schema = if hash_mode {
		serde_json::json!({
			"type": "array",
			"description": "Two-element array [start, end]. Use 4-char hex hash identifiers from `view` output or integer line numbers.",
			"items": { "anyOf": [{"type": "string"}, {"type": "integer"}] },
			"minItems": 2,
			"maxItems": 2
		})
	} else {
		serde_json::json!({
			"type": "array",
			"description": "Two-element array [start, end] with 1-indexed line numbers (inclusive). Supports negative indexing: -1 = last line.",
			"items": { "type": "integer" },
			"minItems": 2,
			"maxItems": 2
		})
	};
	props.insert("from_range".to_string(), from_range_schema);

	let append_line_schema = if hash_mode {
		serde_json::json!({
			"anyOf": [
				{
					"type": "string",
					"description": "Hash identifier from `view` output — insert after the line with this hash."
				},
				{
					"type": "integer",
					"description": "Special positions: 0 = beginning of file, -1 = end of file."
				}
			],
			"description": "Position where to append: hash string (after that line), 0 = beginning, -1 = end."
		})
	} else {
		serde_json::json!({
			"type": "integer",
			"description": "Position where to append: 0 = beginning, -1 = end, N = after line N (1-indexed)."
		})
	};
	props.insert("append_line".to_string(), append_line_schema);

	Arc::new(schema)
}

/// Generate a JSON Schema object for a type, suitable for MCP input_schema.
fn schema_for<T: schemars::JsonSchema + 'static>() -> Arc<serde_json::Map<String, Value>> {
	use rmcp::schemars::generate::SchemaSettings;
	let settings = SchemaSettings::draft2020_12();
	let generator = settings.into_generator();
	let schema = generator.into_root_schema_for::<T>();
	let value = serde_json::to_value(schema).expect("schema serialization");
	match value {
		Value::Object(map) => Arc::new(map),
		_ => unreachable!("schema must be an object"),
	}
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
		let mut server = Self {
			tool_router: Self::tool_router(),
		};
		server.apply_mode_descriptions();
		server
	}

	/// Overwrite tool descriptions and input schemas based on the active line mode.
	/// Each tool gets a precise description and schema matching the current mode — no generic fallbacks.
	fn apply_mode_descriptions(&mut self) {
		use crate::utils::line_hash::is_hash_mode;
		let hash = is_hash_mode();

		if let Some(route) = self.tool_router.map.get_mut("view") {
			route.attr.input_schema = view_schema(hash);
			route.attr.description = Some(if hash {
				r#"Read files, view directories, and search file content. Unified read-only tool.

**File** (path is a file): returns plain text where each line is prefixed with a 4-char hex hash identifier (e.g., `a3bd: code here`). Hashes are derived from line content — unchanged lines keep the same hash across edits.
- Whole file: `{"paths": ["src/main.rs"]}`
- Line range by hash: `{"paths": ["src/main.rs"], "lines": ["a3bd", "c7f2"]}`
- Line range by number also accepted: `{"paths": ["src/main.rs"], "lines": [10, 20]}`

**Multi-file** (max 50): `{"paths": ["src/main.rs", "src/lib.rs"]}`

**Directory** (path is a directory):
- List: `{"paths": ["src/"]}` — filter: `"pattern": "*.rs"`, depth: `"max_depth": 2`
- Search content (ripgrep): `{"paths": ["src"], "content": "fn main"}`
- Hidden files: `"include_hidden": true`

IMPORTANT: Hash prefixes like `a3bd: ` are for reference only. When editing via `text_editor` or `batch_edit`, use raw file content — never include the hash prefix."#
			} else {
				r#"Read files, view directories, and search file content. Unified read-only tool.

**File** (path is a file): returns plain text with 1-indexed line numbers (e.g., `1: code here`).
- Whole file: `{"paths": ["src/main.rs"]}`
- Line range (negative ok: -1 = last): `{"paths": ["src/main.rs"], "lines": [10, 20]}`

**Multi-file** (max 50): `{"paths": ["src/main.rs", "src/lib.rs"]}`

**Directory** (path is a directory):
- List: `{"paths": ["src/"]}` — filter: `"pattern": "*.rs"`, depth: `"max_depth": 2`
- Search content (ripgrep): `{"paths": ["src"], "content": "fn main"}`
- Hidden files: `"include_hidden": true`"#
			}.into());
		}

		if let Some(route) = self.tool_router.map.get_mut("text_editor") {
			route.attr.description = Some(if hash {
				r#"Perform text editing operations on files.

The `command` parameter specifies the operation to perform.
For READ operations use the `view` tool instead.
For line-based edits (insert after hash, replace by hash range), use the separate `batch_edit` tool.

Commands:

`create`: Create new file. Fails if file already exists.
- `{"command": "create", "path": "src/new.rs", "content": "..."}` — creates parent dirs automatically.

`str_replace`: Replace exact string match. Requires exactly 1 match — fails on 0 (no match) or 2+ (ambiguous).
- `{"command": "str_replace", "path": "src/main.rs", "old_text": "fn old()", "new_text": "fn new()"}`
- `old_text` must match exactly (including whitespace). Use raw file content only.
- NEVER include hash prefixes from `view` output (e.g., `a3bd: `) — pass only the actual file content.
- Fuzzy fallback: if exact match fails, tries whitespace-normalized matching and auto-adjusts indentation.
- On failure: shows closest matches with hash identifiers, similarity %, and diagnosis.

`undo_edit`: Revert the last edit on a file. Supports up to 10 undo levels per file.
- `{"command": "undo_edit", "path": "src/main.rs"}`"#
			} else {
				r#"Perform text editing operations on files.

The `command` parameter specifies the operation to perform.
For READ operations use the `view` tool instead.
For line-based edits (insert after line, replace by line range), use the separate `batch_edit` tool.

Commands:

`create`: Create new file. Fails if file already exists.
- `{"command": "create", "path": "src/new.rs", "content": "..."}` — creates parent dirs automatically.

`str_replace`: Replace exact string match. Requires exactly 1 match — fails on 0 (no match) or 2+ (ambiguous).
- `{"command": "str_replace", "path": "src/main.rs", "old_text": "fn old()", "new_text": "fn new()"}`
- `old_text` must match exactly (including whitespace). Use raw content, not escaped.
- Fuzzy fallback: if exact match fails, tries whitespace-normalized matching and auto-adjusts indentation.
- On failure: shows closest matches with line numbers, similarity %, and diagnosis.

`undo_edit`: Revert the last edit on a file. Supports up to 10 undo levels per file.
- `{"command": "undo_edit", "path": "src/main.rs"}`"#
			}.into());
		}

		if let Some(route) = self.tool_router.map.get_mut("batch_edit") {
			route.attr.input_schema = batch_edit_schema(hash);
			route.attr.description = Some(if hash {
				r#"Perform multiple insert/replace operations on a SINGLE file atomically, using ORIGINAL hash identifiers from `view` output.

Use when: 2+ edits on an unmodified file (all line_range hashes reference the file before any changes).
Do NOT use: after any prior edit to the file — hashes will be stale. Re-view first.

CRITICAL: Always `view` the exact hash range before replacing — never assume what is at a hash.

CRITICAL: All line_range values reference the ORIGINAL file content before ANY changes.
Even if operation 1 replaces 1 line with 10 lines, operation 2 still uses the original hashes.
The tool handles offset calculation internally — you never need to adjust for prior operations.

Operations:
- `insert`: line_range = hash string → insert after that line (e.g., `"line_range": "a3bd"`)
  Special: 0 = beginning of file, -1 = after last line
- `replace`: line_range = [start_hash, end_hash] → replace those lines (e.g., `"line_range": ["a3bd", "c7f2"]`)

Key rule — NEVER retype unchanged lines in replace. Only provide content for lines that actually change.

Content is raw file text — NEVER include hash prefixes from `view` output.

Empty content in replace deletes the targeted lines entirely.

Duplicate-line guard: the tool rejects content whose first/last line matches the line
immediately before/after the replacement range. Fix: shrink the range or trim the content.

Max 50 operations per call.

Atomicity: either ALL operations succeed or NONE are applied.

Returns a diff with hash identifiers:
- Context lines: `hash: <text>` (3 lines before/after each change)
- Removed lines: `-hash: <text>`
- Added lines:   `+hash: <text>`
- Multiple ops separated by `---`
Read the diff to verify edits landed correctly — no need for a follow-up `view` call."#
			} else {
				r#"Perform multiple insert/replace operations on a SINGLE file atomically, using ORIGINAL line numbers.

Use when: 2+ edits on an unmodified file (all line numbers reference the file before any changes).
Do NOT use: after any prior edit to the file — line numbers will be stale. Re-view first.

CRITICAL: Always `view` the exact line range before replacing — never assume what is at a line number.

CRITICAL: All line_range values reference the ORIGINAL file content before ANY changes.
Even if operation 1 replaces 1 line with 10 lines, operation 2 still uses the original line numbers.
The tool handles offset calculation internally — you never need to adjust for prior operations.

Operations:
- `insert`: line_range = integer → insert after line N (0 = beginning of file, -1 = after last line)
- `replace`: line_range = [start, end] → remove those lines, insert new content (1-indexed, inclusive)

Negative line numbers count from end: -1 = last line, -2 = second-to-last, etc.

Key rule — NEVER retype unchanged lines in replace. Only provide content for lines that actually change.

Empty content in replace deletes the targeted lines entirely.

Duplicate-line guard: the tool rejects content whose first/last line matches the line
immediately before/after the replacement range. Fix: shrink the range or trim the content.

Max 50 operations per call.

Atomicity: either ALL operations succeed or NONE are applied.

Returns a diff with line numbers:
- Context lines: `NNN: <text>` (3 lines before/after each change)
- Removed lines: `-NNN: <text>`
- Added lines:   `+NNN: <text>`
- Multiple ops separated by `---`
Read the diff to verify edits landed correctly — no need for a follow-up `view` call."#
			}.into());
		}

		if let Some(route) = self.tool_router.map.get_mut("extract_lines") {
			route.attr.input_schema = extract_lines_schema(hash);
			route.attr.description = Some(if hash {
				r#"Copy lines from a source file and append them into a target file. Source is not modified.

Parameters use hash identifiers from `view` output. Output displays extracted content with hash identifiers.

- `from_range`: [start_hash, end_hash] — hash identifiers for the line range to extract
- `append_line`: hash string (insert after that line), 0 = beginning, -1 = end

Examples:
- `{"from_path": "src/utils.rs", "from_range": ["a3bd", "c7f2"], "append_path": "src/new.rs", "append_line": -1}`
- `{"from_path": "config.toml", "from_range": ["d1e5", "f8a0"], "append_path": "new.toml", "append_line": 0}`
- `{"from_path": "main.rs", "from_range": ["b2c4", "e9f1"], "append_path": "module.rs", "append_line": "a1b2"}`"#
			} else {
				r#"Copy lines from a source file and append them into a target file. Source is not modified.

- `from_range`: [start, end] line numbers (1-indexed, inclusive). Supports negative indexing: -1 = last line.
- `append_line`: 0 = beginning, -1 = end, N = after line N (1-indexed)

Examples:
- `{"from_path": "src/utils.rs", "from_range": [10, 25], "append_path": "src/new.rs", "append_line": -1}`
- `{"from_path": "config.toml", "from_range": [1, 5], "append_path": "new.toml", "append_line": 0}`
- `{"from_path": "main.rs", "from_range": [50, 60], "append_path": "module.rs", "append_line": 3}`"#
			}.into());
		}
	}

	/// Read files, view directories, and search file content.
	#[tool(
		name = "view",
		description = "Read files, view directories, and search file content.",
		annotations(read_only_hint = true)
	)]
	async fn view(&self, Parameters(params): Parameters<ViewParams>) -> Result<String, String> {
		with_hints(fs::execute_view(&make_call("view", &params)).await)
	}

	/// Perform text editing operations on files.
	#[tool(
		name = "text_editor",
		description = "Perform text editing operations on files.",
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
		description = "Perform multiple insert/replace operations on a single file atomically.",
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
		description = "Copy lines from a source file and append them into a target file.",
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
