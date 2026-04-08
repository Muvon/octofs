<div align="center">

# 🐙 Octofs

**Give your AI assistant filesystem superpowers**

[![Rust](https://img.shields.io/badge/Rust-1.92+-orange.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-2025--03--26-green.svg)](https://modelcontextprotocol.io)
[![Version](https://img.shields.io/badge/Version-0.2.1-blue.svg)](https://github.com/muvon/octofs)

*The fastest, most capable filesystem MCP server. Built in Rust for AI agents that actually ship.*

[Installation](#installation) • [Quick Start](#quick-start) • [Features](#features) • [Tools Reference](#mcp-tools-reference)

</div>

---

## Why Octofs?

Your AI coding assistant (Cursor, Claude, Windsurf, etc.) is smart—but it's **blind to your filesystem**. Octofs bridges that gap, giving your AI:

- **Eyes** — Read files, search content, explore directories
- **Hands** — Create, edit, batch-modify files atomically
- **Context** — Execute commands, manage working directories

```
┌─────────────────────────────────────────────────────────────┐
│  You: "Refactor all error handling to use anyhow::Context" │
├─────────────────────────────────────────────────────────────┤
│  AI without Octofs:                                         │
│  • "I can't see your project structure"                     │
│  • "Please paste the relevant files"                        │
│  • *Wastes 10 minutes on back-and-forth*                    │
├─────────────────────────────────────────────────────────────┤
│  AI with Octofs:                                            │
│  • Scans entire codebase in milliseconds                    │
│  • Finds all 47 error handling patterns                     │
│  • Suggests atomic batch edits                              │
│  • Applies changes with your approval                       │
└─────────────────────────────────────────────────────────────┘
```

## What Makes It Different

| Feature | Octofs | Others |
|---------|--------|--------|
| **Speed** | Rust-powered, sub-millisecond responses | Python/Node-based, slower |
| **Content Search** | Built-in search with context lines | String matching only |
| **Batch Operations** | Atomic multi-edit on single file | One-at-a-time |
| **Line Modes** | Hash-based (stable across edits) or number-based | Number-only |
| **Transport** | STDIO + HTTP (Streamable HTTP) | STDIO only |
| **Shell Integration** | Background process support | Limited or none |
| **Safety** | Gitignore-aware, path validation | Full filesystem access |

---

## Installation

### From Source

Requires Rust 1.92+.

```bash
# Clone and build
git clone https://github.com/muvon/octofs
cd octofs
cargo build --release

# Binary will be at ./target/release/octofs
# Optionally install globally
cargo install --path .
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/muvon/octofs/releases) for your platform.

---

## Quick Start

### 1. Configure Your AI Assistant

**Cursor** (`~/.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs"
    }
  }
}
```

**Claude Desktop** (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs"
    }
  }
}
```

**Windsurf** (`~/.windsurf/mcp.json`):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs"
    }
  }
}
```

### 2. Restart Your AI Assistant

The MCP server will start automatically when your AI assistant connects.

### 3. Try It

Ask your AI assistant to:
- "Show me the project structure"
- "Read the main.rs file"
- "Search for all uses of `unwrap()` in the codebase"
- "Create a new file called `test.rs`"

---

## Features

### 📁 Filesystem Operations

- **View Files & Directories** — Read single/multiple files, list directories with glob patterns, search content
- **Smart Truncation** — Large files are truncated intelligently to avoid overwhelming context
- **Gitignore-Aware** — Respects `.gitignore` patterns during directory traversal
- **Line Ranges** — Read specific line ranges with negative indexing support (`-1` = last line)

### ✏️ Text Editing

- **Create Files** — Create new files with automatic parent directory creation
- **String Replace** — Replace exact string matches with fuzzy fallback for whitespace
- **Undo** — Revert last edit (up to 10 undo levels per file)
- **Batch Edit** — Perform multiple insert/replace operations atomically on a single file

### 🔍 Code Intelligence

- **Content Search** — Search for strings within files with context lines
- **Line Extraction** — Copy specific line ranges from one file to another

### 🖥️ Shell & System

- **Command Execution** — Run shell commands with output capture
- **Background Processes** — Run long commands in background, get PID for later management
- **Working Directory** — Set/get/reset working directory context for operations

---

## Configuration

### Line Identifier Modes

Octofs supports two modes for identifying lines in files:

#### Number Mode (default)

Lines are identified by 1-indexed line numbers:
```
1: fn main() {
2:     println!("Hello");
3: }
```

Use for: Simple operations, one-off edits.

#### Hash Mode

Lines are identified by 4-character hex hashes derived from content:
```
a3bd: fn main() {
c7f2:     println!("Hello");
e9f1: }
```

Use for: Complex multi-step edits where line numbers would shift. Hashes stay stable across edits.

**Enable hash mode:**
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs",
      "args": ["--line-mode", "hash"]
    }
  }
}
```

### Transport Modes

#### STDIO (default)

Standard input/output transport. Works with all MCP clients.

```bash
octofs  # defaults to STDIO
```

#### HTTP

Streamable HTTP transport for remote access or multi-client scenarios.

```bash
octofs --bind 0.0.0.0:12345
```

Connect clients to `http://localhost:12345/mcp`.

### Working Directory

By default, Octofs operates in the current directory. Specify a different root:

```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs",
      "args": ["--path", "/path/to/your/project"]
    }
  }
}
```

---

## MCP Tools Reference

### `view` — Read files, list directories, search content

**File reading:**
```json
{"paths": ["src/main.rs"]}
{"paths": ["src/main.rs"], "lines": [10, 20]}
{"paths": ["src/main.rs"], "lines": ["a3bd", "c7f2"]}  // hash mode
```

**Multi-file reading (max 50):**
```json
{"paths": ["src/main.rs", "src/lib.rs", "src/cli.rs"]}
```

**Directory listing:**
```json
{"paths": ["src/"]}
{"paths": ["src/"], "pattern": "*.rs"}
{"paths": ["src/"], "max_depth": 2, "include_hidden": true}
```

**Content search:**
```json
{"paths": ["src"], "content": "fn main"}
{"paths": ["src"], "content": "unwrap()", "context": 3}
```

---

### `text_editor` — Create, edit, replace text

**Create file:**
```json
{"command": "create", "path": "src/new.rs", "content": "pub fn new() {}"}
```

**Replace string:**
```json
{
  "command": "str_replace",
  "path": "src/main.rs",
  "old_text": "fn old()",
  "new_text": "fn new()"
}
```

**Undo last edit:**
```json
{"command": "undo_edit", "path": "src/main.rs"}
```

---

### `batch_edit` — Atomic multi-operation edits

Perform multiple insert/replace operations on a single file atomically.

**Insert at beginning:**
```json
{
  "path": "src/main.rs",
  "operations": [
    {"operation": "insert", "line_range": 0, "content": "// Header\n"}
  ]
}
```

**Replace lines:**
```json
{
  "path": "src/main.rs",
  "operations": [
    {"operation": "replace", "line_range": [10, 15], "content": "new code here"}
  ]
}
```

**Hash mode (stable across edits):**
```json
{
  "path": "src/main.rs",
  "operations": [
    {"operation": "replace", "line_range": ["a3bd", "c7f2"], "content": "new code"}
  ]
}
```

---

### `extract_lines` — Copy lines between files

```json
{
  "from_path": "src/utils.rs",
  "from_range": [10, 25],
  "append_path": "src/new.rs",
  "append_line": -1
}
```

---

### `shell` — Execute commands

**Foreground:**
```json
{"command": "cargo test"}
{"command": "cd foo && cargo build"}
```

**Background:**
```json
{"command": "python -m http.server 8000", "background": true}
// Returns PID, kill later with: {"command": "kill 12345"}
```

---

### `workdir` — Manage working directory

**Get current:**
```json
{}
```

**Set new:**
```json
{"path": "/path/to/project"}
```

**Reset to session root:**
```json
{"reset": true}
```

---

## Architecture

```
octofs/
├── src/
│   ├── main.rs              # Entry point, STDIO/HTTP server setup
│   ├── cli.rs               # CLI argument parsing (clap)
│   └── mcp/
│       ├── server.rs        # MCP protocol handler (rmcp SDK)
│       ├── shared_utils.rs  # Shared utilities
│       ├── hint_accumulator.rs  # Tool feedback hints
│       └── fs/              # Filesystem tools
│           ├── core.rs          # view, batch_edit, extract_lines, text_editor
│           ├── text_editing.rs  # str_replace, undo, batch operations
│           ├── directory.rs     # Directory traversal
│           ├── file_ops.rs       # File operations
│           ├── search.rs          # Content search
│           ├── shell.rs           # Command execution
│           ├── workdir.rs         # Working directory management
│           └── fs_tests.rs        # Unit tests
└── src/utils/
    ├── glob.rs              # Glob pattern matching
    ├── line_hash.rs         # Content-based line hashing
    └── truncation.rs        # Smart content truncation
```

**Key components:**

- **rmcp SDK** — Official Rust MCP SDK for protocol handling
- **Tokio** — Async runtime for concurrent operations
- **File locking** — Per-file async locks prevent concurrent write conflicts
- **Undo history** — Up to 10 undo levels per file, thread-safe storage

---

## Development

```bash
# Build
cargo build --release

# Run tests
cargo test

# Lint (zero warnings policy)
cargo clippy

# Format
cargo fmt

# Run locally
cargo run
```

### Running Tests

```bash
# All tests
cargo test

# Specific test
cargo test test_view_file

# With output
cargo test -- --nocapture
```

---

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

**Quick checklist:**
1. Run `cargo fmt` before committing
2. Ensure `cargo clippy` passes with zero warnings
3. Add tests for new functionality
4. Update documentation as needed

---

## Security

See [SECURITY.md](SECURITY.md) for security policy and reporting vulnerabilities.

---

## License

Apache-2.0 — See [LICENSE](LICENSE)

---

## Acknowledgments

- [rmcp](https://github.com/anthropics/rust-sdk) — Official Rust MCP SDK
- [Model Context Protocol](https://modelcontextprotocol.io) — The protocol specification

---

<div align="center">

**Built with 🦀 by [Muvon](https://muvon.io)**

*Star us on GitHub if Octofs helps you ship faster! ⭐*

</div>
