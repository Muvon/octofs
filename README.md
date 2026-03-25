<div align="center">

# 🐙 Octofs

**Give your AI superpowers over your filesystem**

[![Rust](https://img.shields.io/badge/Rust-1.92+-orange.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Protocol-green.svg)](https://modelcontextprotocol.io)

*The fastest, most capable filesystem MCP server. Built in Rust for AI agents that actually ship.*

[Installation](#installation) • [Features](#features) • [Usage](#usage) • [Integrations](#integrations)

</div>

---

## Why Octofs?

Your AI coding assistant (Cursor, Claude, Windsurf, etc.) is smart—but it's **blind to your filesystem**. Octofs is the bridge that gives your AI eyes, hands, and a brain for code.

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

## ✨ What Makes It Special

| Feature | Octofs | Others |
|---------|--------|--------|
| **Speed** | Rust-powered, sub-millisecond responses | Python-based, slower |
| **AST Intelligence** | Built-in `ast-grep` for semantic code search | String matching only |
| **Batch Operations** | Atomic multi-file edits | One-at-a-time |
| **Safety** | Gitignore-aware, path validation | Full filesystem access |
| **Shell Integration** | Background process support | Limited or none |

## 🚀 Installation

### macOS & Linux (Homebrew coming soon)

```bash
# Install from source (requires Rust 1.92+)
cargo install --path .

# Or clone and build
git clone https://github.com/muvon/octofs
cd octofs && cargo build --release

# Binary will be at ./target/release/octofs
```

### Configure with your AI assistant

**Cursor:**
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs",
      "args": []
    }
  }
}
```

**Claude Desktop:**
```json
{
  "mcpServers": {
    "octofs": {
      "command": "/path/to/octofs"
    }
  }
}
```

## 🎯 Features

### 📁 Filesystem Operations
- **Smart File Reading** — Intelligent truncation for large files, never overwhelms context
- **Directory Traversal** — Glob patterns, gitignore-aware, hidden file handling
- **Batch Editing** — Atomic multi-file operations with diff previews
- **Line Extraction** — Copy specific line ranges between files

### 🔍 Code Intelligence
- **AST Search** — Find code by structure, not just text (`ast-grep` powered)
- **Signature Extraction** — Get function signatures without implementation noise
- **Semantic Search** — Find code by what it does, not what it's called

### 🖥️ Shell & System
- **Command Execution** — Run commands with background process support
- **Working Directory** — Context-aware operations, session isolation
- **Environment Aware** — Respects your shell environment

## 💡 Real-World Use Cases

### "Find and refactor all deprecated API calls"
```
You: Find all uses of the old API and migrate to v2

AI with Octofs:
1. ast-grep search for deprecated patterns
2. View all 23 occurrences across 8 files
3. Generate batch edits with context
4. Apply with your review
```

### "Understand this codebase"
```
You: Give me an overview of the project structure

AI with Octofs:
1. List directory tree with intelligent filtering
2. Extract key module signatures
3. Search for entry points and main flows
4. Generate architecture summary
```

### "Debug this error"
```
You: Why is this test failing?

AI with Octofs:
1. Read the test file and error output
2. Search for related code patterns
3. Check shell for environment issues
4. Suggest fixes with confidence
```

## 🔧 MCP Tools Reference

| Tool | Description | Example |
|------|-------------|---------|
| `view` | Read files, list directories, search content | `view path="src" pattern="*.rs"` |
| `text_editor` | Create, edit, replace text | `text_editor path="main.rs" command="str_replace"` |
| `batch_edit` | Atomic multi-file operations | `batch_edit edits=[{...}, {...}]` |
| `ast_grep` | Structural code search | `ast_grep pattern="fn $NAME($$$ARGS)"` |
| `view_signatures` | Extract function signatures | `view_signatures path="src/lib.rs"` |
| `shell` | Execute commands | `shell command="cargo test"` |
| `workdir` | Manage working directory | `workdir action="set" path="/project"` |

## 🏗️ Architecture

```
octofs/
├── src/
│   ├── main.rs           # Entry point
│   ├── cli.rs            # Command-line interface
│   └── mcp/
│       ├── server.rs     # MCP protocol handler
│       └── fs/           # Filesystem tools
│           ├── core.rs       # File operations
│           ├── text_editing.rs  # Batch edits
│           ├── ast_grep.rs      # Code analysis
│           ├── shell.rs         # Command execution
│           └── workdir.rs       # Context management
└── src/utils/            # Utilities (glob, truncation)
```

**Design Principles:**
- **Zero Warnings** — All code passes `cargo clippy`
- **Fail Fast** — Clear error messages, early validation
- **Async-First** — Tokio-powered for concurrent operations
- **Memory Safe** — Rust guarantees, no leaks

## 🧪 Development

```bash
# Build
cargo build --release

# Test
cargo test

# Lint (zero warnings policy)
cargo clippy

# Format
cargo fmt
```

## 🤝 Integrations

Octofs works seamlessly with:

- **Cursor** — The AI-first code editor
- **Claude Desktop** — Anthropic's AI assistant
- **Windsurf** — The agentic IDE
- **Any MCP-compatible client** — Standard protocol support

## 📊 Comparison with Alternatives

| Tool | Language | AST Support | Batch Edits | Shell | Speed |
|------|----------|-------------|-------------|-------|-------|
| Octofs | Rust | ✅ Built-in | ✅ Atomic | ✅ | ⚡ Native |
| @modelcontextprotocol/server-filesystem | Node.js | ❌ | ❌ | ❌ | Slower |
| custom Python scripts | Python | Partial | Manual | Manual | Slower |

## 📜 License

Apache-2.0 — See [LICENSE](LICENSE)

---

<div align="center">

**Built with 🦀 by [Muvon](https://muvon.io)**

*Star us on GitHub if Octofs helps you ship faster! ⭐*

</div>
