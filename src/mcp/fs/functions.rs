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

// Optimized function definitions module - MCP function specifications with reduced tokens

use super::super::McpFunction;
use super::ast_grep::get_ast_grep_function;
use super::shell::get_shell_function;
use super::workdir::get_workdir_function;
use serde_json::json;

// Define the view function - unified read-only tool for files, directories, and content search
pub fn get_view_function() -> McpFunction {
	McpFunction {
		name: "view".to_string(),
		description:
			"Read files, view directories, and search file content. Unified read-only tool.

			**File** (path is a file): returns plain text with 1-indexed line numbers.
			- Whole file: `{\"path\": \"src/main.rs\"}`
			- Line range (negative ok: -1 = last): `{\"path\": \"src/main.rs\", \"lines\": [10, 20]}`

			**Multi-file** (paths array, max 50): `{\"paths\": [\"src/main.rs\", \"src/lib.rs\"]}`

			**Directory** (path is a directory):
			- List: `{\"path\": \"src/\"}` — filter: `\"pattern\": \"*.rs\"`, depth: `\"max_depth\": 2`
			- Search content (ripgrep): `{\"path\": \"src\", \"content\": \"fn main\"}`
			- Hidden files: `\"include_hidden\": true`"
				.to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"path": {
					"type": "string",
					"description": "File path, directory path, or glob pattern. Required unless `paths` is provided."
				},
				"paths": {
					"type": "array",
					"items": {"type": "string"},
					"maxItems": 50,
					"description": "Array of file paths for multi-file viewing. Max 50 files."
				},
				"lines": {
					"type": "array",
					"items": {"type": "integer"},
					"minItems": 2,
					"maxItems": 2,
					"description": "Line range [start, end] for single file viewing (1-indexed, inclusive). Supports negative indexing: -1 for last line."
				},
				"pattern": {
					"type": "string",
					"description": "Filename glob filter for directory listing (e.g. '*.rs', '*.toml|*.yaml'). Only used when path is a directory."
				},
				"content": {
					"type": "string",
					"description": "Content search string (ripgrep). Only used when path is a directory."
				},
				"max_depth": {
					"type": "integer",
					"description": "Maximum directory traversal depth (default: no limit). Only used when path is a directory."
				},
				"include_hidden": {
					"type": "boolean",
					"default": false,
					"description": "Include hidden files/directories starting with '.' (default: false). Only used when path is a directory."
				},
				"line_numbers": {
					"type": "boolean",
					"default": true,
					"description": "Show line numbers in content search results (default: true)."
				},
				"context": {
					"type": "integer",
					"default": 0,
					"description": "Context lines around content search matches (default: 0)."
				}
			}
		}),
	}
}

// Define the text editor function - edit-only commands
pub fn get_text_editor_function() -> McpFunction {
	McpFunction {
		name: "text_editor".to_string(),
		description: "Perform text editing operations on files.

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
			- `{\"command\": \"undo_edit\", \"path\": \"src/main.rs\"}`"
			.to_string(),
		parameters: json!({
			"type": "object",
			"required": ["command", "path"],
			"properties": {
				"command": {
					"type": "string",
					"enum": ["create", "str_replace", "undo_edit"],
					"description": "The operation to perform: create, str_replace, undo_edit"
				},
				"path": {
					"type": "string",
					"description": "REQUIRED. Path to the file to operate on."
				},
				"content": {
					"type": "string",
					"description": "File content for create command. Raw text with actual whitespace (not escape sequences)"
				},
				"old_text": {
					"type": "string",
					"description": "Text to find (must match exactly including whitespace). REQUIRED for str_replace."
				},
				"new_text": {
					"type": "string",
					"description": "Replacement text. REQUIRED for str_replace. Raw text with actual whitespace (not escape sequences)"
				}
			},
			"if": { "properties": { "command": { "const": "create" } } },
			"then": { "required": ["content"] },
			"else": {
				"if": { "properties": { "command": { "const": "str_replace" } } },
				"then": { "required": ["old_text", "new_text"] }
			}
		}),
	}
}

// Define the extract_lines function
pub fn get_extract_lines_function() -> McpFunction {
	McpFunction {
		name: "extract_lines".to_string(),
		description: "Copy lines from a source file and append them into a target file. Source is not modified.

			- `append_line`: 0 = beginning, -1 = end, N = after line N.

			Examples:
			- `{\"from_path\": \"src/utils.rs\", \"from_range\": [10, 25], \"append_path\": \"src/new.rs\", \"append_line\": -1}`
			- `{\"from_path\": \"config.toml\", \"from_range\": [1, 5], \"append_path\": \"new.toml\", \"append_line\": 0}`
			- `{\"from_path\": \"main.rs\", \"from_range\": [50, 60], \"append_path\": \"module.rs\", \"append_line\": 3}`".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"from_path": {
					"type": "string",
					"description": "Path to the source file to extract lines from"
				},
				"from_range": {
					"type": "array",
					"items": {"type": "integer"},
					"minItems": 2,
					"maxItems": 2,
					"description": "Two-element array [start, end] with 1-indexed line numbers (inclusive)"
				},
				"append_path": {
					"type": "string",
					"description": "Path to the target file where extracted lines will be appended (auto-created if doesn't exist)"
				},
				"append_line": {
					"type": "integer",
					"description": "Position where to append: 0=beginning, -1=end, N=after line N (1-indexed)"
				}
			},
			"required": ["from_path", "from_range", "append_path", "append_line"]
		}),
	}
}

// Define the batch_edit function - extracted from text_editor for simplicity
pub fn get_batch_edit_function() -> McpFunction {
	McpFunction {
		name: "batch_edit".to_string(),
		description: "Perform multiple insert/replace operations on a SINGLE file atomically, using ORIGINAL line numbers.

			Use when: 2+ edits on an unmodified file (all line numbers reference the file before any changes).
			Do NOT use: after any prior edit to the file — line numbers will be stale.

			CRITICAL: Always `view` the exact line range before replacing — never assume what is at a line number.
			Line numbers shift after every edit. If you edited this file before, re-view it first.

			CRITICAL: All line_range values reference the ORIGINAL file content before ANY changes.
			Even if operation 1 replaces 1 line with 10 lines, operation 2 still uses the original line numbers.
			The tool handles offset calculation internally — you never need to adjust for prior operations.

			Operations:
			- `insert`: line_range = integer → insert after line N (0 = beginning of file, -1 = after last line)
			- `replace`: line_range = [start, end] → remove those lines, insert new content

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

			Returns a diff of all changes made:
			- Context lines: `NNN: <text>` (3 lines before/after each change)
			- Removed lines: `-NNN: <text>`
			- Added lines:   `+NNN: <text>`
			- Multiple ops separated by `---`
			Read the diff to verify edits landed correctly — no need for a follow-up `view` call.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"path": {
					"type": "string",
					"description": "Path to the file to edit"
				},
				"operations": {
					"type": "array",
					"items": {
						"type": "object",
						"properties": {
							"operation": {
								"type": "string",
								"enum": ["insert", "replace"],
								"description": "Type of operation: 'insert' (after line) or 'replace' (line range)"
							},
							"line_range": {
								"description": "CRITICAL: Line numbers from ORIGINAL file content (before any modifications). Insert: single integer (0=beginning, N=after line N, -1=after last line). Replace: [start, end] array (1-indexed inclusive, negative ok). DO NOT USE if file was modified — line numbers will be wrong!",
								"oneOf": [
									{
										"type": "integer",
										"description": "Single line number for insert (0=beginning, N=after line N, -1=after last line)"
									},
									{
										"type": "array",
										"items": {"type": "integer"},
										"minItems": 2,
										"maxItems": 2,
										"description": "Line range [start, end] for replace (1-indexed, inclusive, negative ok: -1=last line)"
									}
								]
							},
							"content": {
								"type": "string",
								"description": "Raw content to insert or replace with (no escaping needed — use actual tabs/spaces). Empty string in replace deletes the targeted lines."
							}
						},
						"required": ["operation", "line_range", "content"]
					},
					"maxItems": 50,
					"description": "Array of operations for batch_edit on SINGLE file. All line_range values reference ORIGINAL file content. DO NOT USE after any file modifications!"
				}
			},
			"required": ["path", "operations"]
		}),
	}
}

// Get all available filesystem functions
pub fn get_all_functions() -> Vec<McpFunction> {
	vec![
		get_view_function(),
		get_text_editor_function(),
		get_batch_edit_function(),
		get_extract_lines_function(),
		get_shell_function(),
		get_workdir_function(),
		get_ast_grep_function(),
	]
}
