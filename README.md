<div align="center">

# 🐙 Octofs

**Give your AI assistant filesystem superpowers**

[![CI](https://github.com/muvon/octofs/actions/workflows/ci.yml/badge.svg)](https://github.com/muvon/octofs/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-1.95+-orange.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-2026--07--28-green.svg)](https://modelcontextprotocol.io)
[![Version](https://img.shields.io/crates/v/octofs.svg)](https://crates.io/crates/octofs)

*Standalone Rust binary that exposes filesystem tools over the Model Context Protocol. Built on `rmcp` 3.0, `tokio`, and `axum`.*

[Installation](#installation) · [Quick Start](#quick-start) · [Features](#features) · [Tools Reference](#mcp-tools-reference) · [Architecture](#architecture)

MCP Registry: [`io.github.Muvon/octofs`](https://github.com/muvon/octofs)

</div>

---

## Why Octofs?

Your AI coding assistant (Cursor, Claude, Windsurf, etc.) is smart — but it's **blind to your filesystem**. Octofs bridges that gap, giving your AI:

- **Eyes** — Read files, search content, explore directories
- **Hands** — Create, edit, batch-modify files atomically
- **Context** — Execute commands, manage working directories
- **Reach** — Transparent SSH/SFTP: every file tool accepts `ssh://` URLs

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
│  • Reads your codebase directly                             │
│  • Finds all error handling patterns                         │
│  • Suggests atomic batch edits                              │
│  • Applies changes with your approval                       │
└─────────────────────────────────────────────────────────────┘
```

## What Makes It Different

| Feature | Octofs | Typical Alternatives |
|---------|--------|---------------------|
| **Implementation** | Compiled Rust binary, no runtime | Python/Node script |
| **Content Search** | Built-in search with context lines | String matching only |
| **Batch Operations** | Atomic multi-edit on single file | One-at-a-time |
| **Line Modes** | Hash-based (stable across edits) or number-based | Number-only |
| **Transport** | STDIO + Streamable HTTP | STDIO only |
| **Shell Integration** | Foreground + background process support | Limited or none |
| **Remote Files** | Transparent SSH/SFTP on every file tool | None |
| **Safety** | Gitignore-aware, stale-write detection, path validation | Full filesystem access |

---

## Installation

### Cargo (crates.io)

```bash
cargo install octofs
```

### Homebrew

```bash
brew install muvon/tap/octofs
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/muvon/octofs/releases) for your platform:

| Platform | Target |
|----------|--------|
| Linux (x86_64) | `x86_64-unknown-linux-musl` |
| Linux (ARM64) | `aarch64-unknown-linux-musl` |
| Windows (x86_64) | `x86_64-pc-windows-msvc` |
| Windows (ARM64) | `aarch64-pc-windows-msvc` |
| macOS (Intel) | `x86_64-apple-darwin` |
| macOS (Apple Silicon) | `aarch64-apple-darwin` |

### MCP Registry

One-click install via the [MCP Registry](https://github.com/muvon/octofs) entry `io.github.Muvon/octofs`.

### From Source

Requires Rust 1.95+.

```bash
git clone https://github.com/muvon/octofs
cd octofs
cargo build --release
# Binary at ./target/release/octofs
```

---

## Quick Start

### 1. Configure Your AI Assistant

The CLI uses a `mcp` subcommand — your config must pass `["mcp"]` as args.

**Cursor** (`~/.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "octofs",
      "args": ["mcp"]
    }
  }
}
```

**Claude Desktop** (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "octofs",
      "args": ["mcp"]
    }
  }
}
```

**Windsurf** (`~/.windsurf/mcp.json`):
```json
{
  "mcpServers": {
    "octofs": {
      "command": "octofs",
      "args": ["mcp"]
    }
  }
}
```

> If Octofs isn't on your `PATH`, use the full path to the binary (e.g. `/usr/local/bin/octofs` or `./target/release/octofs`).

### 2. Restart Your AI Assistant

The MCP server starts automatically when your AI assistant connects.

### 3. Try It

Ask your AI assistant to:
- "Show me the project structure"
- "Read the main.rs file"
- "Search for all uses of `unwrap()` in the codebase"
- "Create a new file called `test.rs`"

---

## Features

### 📁 Filesystem Operations

- **View Files & Directories** — Read a single file (call `view` in parallel for several), list directories with glob patterns, search content
- **Smart Truncation** — Large files are truncated intelligently to avoid overwhelming context windows
- **Gitignore-Aware** — Respects `.gitignore` patterns during directory traversal
- **Line Ranges** — Read specific line ranges with negative indexing (`-1` = last line)
- **Remote Files (SSH/SFTP)** — Every file tool accepts `ssh://user@host:port/path` URLs (see [Remote Filesystem](#remote-filesystem-sshsftp))

### ✏️ Text Editing

- **Create Files** — Create new files with automatic parent directory creation
- **String Replace** — Replace exact string matches with fuzzy fallback for whitespace
- **Delete** — Remove a file (recoverable via undo)
- **Undo** — Revert last edit (up to 10 undo levels per file, in-memory)
- **Batch Edit** — Perform multiple insert/replace operations atomically on a single file
- **Stale-Write Protection** — Edits fail fast if the file changed on disk since it was last viewed (external-edit detection, like an IDE's "file changed on disk" guard)

### 🔍 Code Intelligence

- **Content Search** — Search for strings within files with context lines
- **Line Extraction** — Copy specific line ranges from one file to another

### 🖥️ Shell & System

- **Command Execution** — Run shell commands with output capture
- **Background Processes** — Run long commands in background, get PID for later management
- **Working Directory** — Set/get/reset working directory context for operations

---

## Configuration

### Line Identifiers

Every line is addressed by a composite id `N:hh` — its 1-indexed position plus a
2-character hex hash (FNV-1a) of its content. `view` renders lines as `N:hh|content`:

```
1:a3|fn main() {
2:f1|    println!("Hello");
3:0e|}
```

Edit tools take these ids back as targets and verify the hash against the file
before applying anything. A stale id (the file changed since it was viewed) fails
with the current content around the target and where the expected content moved —
so the model retargets from the error instead of re-reading the file. Edit results
are diffs with fresh ids, so edits chain without re-viewing.

This is the single line-id format — there is no mode switch. Plain line numbers
are still accepted where a position alone is safe: `view` ranges (negative counts
from the end) and the insert anchors `0` (file start) / `-1` (append).

### Shell Misuse Enforcement

Octofs detects shell misuse — commands like `cat`, `grep`, `find`, or `sed` that
should use the dedicated MCP tools instead — and rejects them with an error
explaining which tool to use. The call fails; nothing executes. This is
intentional and not configurable: the dedicated tools give the model line ids,
gitignore-awareness, and remote-host support that raw shell output cannot.
Pipelines (`cargo build 2>&1 | grep error`) remain allowed — only standalone
invocations of those programs are blocked.

Other guidance (out-of-bounds ranges, fuzzy-match notices) is appended to
successful responses as ⚠️ hints without failing the call.

### Transport Modes

#### STDIO (default)

Standard input/output transport. Works with all MCP clients.

```bash
octofs mcp
```

#### HTTP

Streamable HTTP transport for remote access or multi-client scenarios.

```bash
octofs mcp --bind 0.0.0.0:12345
```

Connect clients to `http://localhost:12345/mcp`.

### Working Directory

By default, Octofs operates in the current directory. Specify a different root:

```json
{
  "mcpServers": {
    "octofs": {
      "command": "octofs",
      "args": ["mcp", "--path", "/path/to/your/project"]
    }
  }
}
```

### Remote Filesystem (SSH/SFTP)

All path parameters — and `--path` itself — accept `ssh://` or `sftp://` URLs:

```bash
# Remote session root: relative paths resolve on the remote host
octofs mcp --path ssh://deploy@example.com/var/www/app --ssh-key ~/.ssh/id_ed25519
```

```
view path="ssh://deploy@example.com/etc/nginx/nginx.conf"
```

- **Authentication** — fully automatic, like OpenSSH, honoring `~/.ssh/config`: the agent your config names for the host (`IdentityAgent`, e.g. 1Password) or `$SSH_AUTH_SOCK`, then key files — `--ssh-key` if given, the host's `IdentityFile` entries, then the defaults in `~/.ssh` (`id_ed25519`, `id_ecdsa`). If plain `ssh host` works on your machine, octofs works too — nothing to configure. Passphrase-protected key files are not supported directly; use an agent instead.
- **RSA keys are not supported** — the Rust `rsa` crate has an unfixed timing side-channel (Marvin attack, [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071)), so octofs is built without RSA entirely. Use an ed25519 key instead (`ssh-keygen -t ed25519`); ecdsa also works. RSA-only setups fail with a clear error naming the key.
- **Host keys** — verified against `~/.ssh/known_hosts` with the OpenSSH `accept-new` policy: unknown hosts are recorded on first use, a changed key fails closed.
- **`--ssh-timeout SECS`** — connection timeout (default 30). Connections are pooled per host, kept alive with transport keepalives, and reconnected automatically if they drop.
- **`shell` stays local** — commands always run on the machine where Octofs runs; only file tools (`view`, `text_editor`, `batch_edit`, `extract_lines`, `workdir`) reach remote hosts.

---

## MCP Tools Reference

### `view` — Read files, list directories, search content

**File reading:** (`path` is a single path; `start`/`end` are line numbers or line ids)
```json
{"path": "src/main.rs"}                          // whole file
{"path": "src/main.rs", "start": 10, "end": 20}  // lines 10–20
{"path": "src/main.rs", "start": 42, "end": 42}  // single line
{"path": "src/main.rs", "start": 80}             // line 80 → end of file
{"path": "src/main.rs", "start": -20}            // last 20 lines
{"path": "src/main.rs", "start": "12:a3", "end": "20:f1"}  // line ids from prior output
```

Output renders every line as `N:hh|content` — the `N:hh` prefix is the line id
that `batch_edit` and `extract_lines` take as targets.

To read several files, make multiple `view` calls — they run in parallel.

**Directory listing:**
```json
{"path": "src/"}
{"path": "src/", "pattern": "*.rs"}
{"path": "src/", "max_depth": 2, "include_hidden": true}
```

`pattern` uses the same gitignore-style glob grammar as ripgrep's `-g/--glob`:
without `/` it matches filenames at any depth, while a pattern containing `/`
matches the returned relative path. It supports `*` within one path component,
`**` across directories, `?`, character classes such as `[abc]`, brace alternatives
such as `*.{rs,toml}`, and leading `!` exclusions. Use `|` to pass ordered globs in
one MCP string; later globs take precedence:

```json
{"path": ".", "pattern": "**/*.{rs,toml}|!target/**"}
{"path": ".", "content": "unsafe", "pattern": "src/**/*.rs|!src/generated/**"}
```

The glob filters files discovered by Octofs' gitignore-aware traversal. Use
`include_hidden: true` to include hidden paths; gitignored paths remain excluded.
Patterns are limited to 4096 UTF-8 bytes, 64 `|`-separated rules, and 16 nested
brace levels. They must be a single line and are validated before traversal.
Escape a literal leading `#` or `!` as `\#` or `\!`; otherwise `#` is rejected
instead of silently acting as a gitignore comment. Empty brace alternatives such
as `{,rs}` are rejected because ripgrep does not support them.

**Content search:** (literal by default; set `regex: true` for a Rust regex, `(?i)` = case-insensitive)
```json
{"path": "src", "content": "fn main"}
{"path": "src", "content": "unwrap()", "context": 3}
{"path": "src", "content": "(?i)error", "regex": true}
{"path": "docs|scripts|BENCHMARK.md", "content": "update_benchmark.py", "context": 4}
```

For an rg-style search across several roots, `path` accepts `|`-separated literal
files and directories when `content` is set. The roots are not regexes: as with
rg's positional path arguments, each one must exist. A real path containing `|`
takes precedence over this shorthand. Root lists must be single-line, contain no
duplicates, and are limited to 32 roots and 8192 UTF-8 bytes.

Directory listings annotate each file as `path<TAB>NL<TAB>~Nt` (line count + estimated tokens) so you can budget reads before opening files; binary files show `path<TAB>(binary)`.

---

### `text_editor` — Create, edit, replace text

**Create file:**
```json
{"command": "create", "path": "src/new.rs", "content": "pub fn new() {}"}
```

**Replace string:** (`old_text` must match exactly once)
```json
{
  "command": "str_replace",
  "path": "src/main.rs",
  "old_text": "fn old()",
  "new_text": "fn new()"
}
```

**Replace ALL occurrences** (rename-style edits):
```json
{
  "command": "str_replace",
  "path": "src/main.rs",
  "old_text": "old_name",
  "new_text": "new_name",
  "replace_all": true
}
```

Matching is progressive: exact → escaped-literal recovery (double-escaped `\n`/`\t`
interpreted when the result matches uniquely) → whitespace-normalized fuzzy with
indentation adjustment → rich diagnostics with the closest candidates and their
line ids. CRLF files are matched in LF space and keep their line endings on write.

**Delete file:** (recoverable with `undo_edit`)
```json
{"command": "delete", "path": "src/old.rs"}
```

**Undo last edit:**
```json
{"command": "undo_edit", "path": "src/main.rs"}
```

---

### `batch_edit` — Atomic multi-operation edits

Perform multiple insert/replace operations on a single file atomically.

Each operation has a `start`. For `replace` it's the first line of the range as a
line id copied from `view` output (add `end` for a range; omit it for a single
line). For `insert` it's the anchor to insert after — a line id, or the integers
`0` (file start) / `-1` (after last line). Every id is verified against the file
before anything applies; a stale id fails with the current content so the model
can retarget without a re-view. The result is a diff with fresh ids.

**Insert at beginning:**
```json
{
  "path": "src/main.rs",
  "operations": [
    {"operation": "insert", "start": 0, "content": "// Header\n"}
  ]
}
```

**Replace lines:**
```json
{
  "path": "src/main.rs",
  "operations": [
    {"operation": "replace", "start": "10:4b", "end": "15:c2", "content": "new code here"}
  ]
}
```

---

### `extract_lines` — Copy lines between files

```json
{
  "from_path": "src/utils.rs",
  "from_start": 10,
  "from_end": 25,
  "append_path": "src/new.rs",
  "append_line": -1
}
```

`from_end` is optional (omit to copy a single line). `from_start`, `from_end`, and
`append_line` each accept a line number or a line id (`"12:a3"`, verified against
the file). `append_line` positions the copy in the target: `0` = beginning,
`-1` = end, `N` = after line N.

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
// Returns PID; the response includes the platform-specific kill command
```

> On Windows, shutdown cleanup terminates only direct child processes (no Unix
> process-group semantics); use `taskkill /PID <pid> /T` for process trees.

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
│   ├── main.rs                  # Entry point, STDIO/HTTP server setup, signal handling
│   ├── cli.rs                   # CLI argument parsing (clap): octofs mcp [OPTIONS]
│   ├── mcp/
│   │   ├── mod.rs               # McpToolCall, SessionRoot
│   │   ├── server.rs            # OctofsServer (rmcp tool impl), Params structs, SessionWorkdir
│   │   ├── request_ctx.rs       # Per-request hint queue + stale-file stamps
│   │   └── fs/                  # Filesystem tool implementations
│   │       ├── mod.rs           # Re-exports
│   │       ├── core.rs          # view, text_editor, batch_edit, extract_lines
│   │       ├── file_ops.rs      # view_file_spec, create_file_spec
│   │       ├── text_editing.rs  # str_replace, batch_edit, undo, per-file locking
│   │       ├── directory.rs     # Directory listing + content search
│   │       ├── search.rs        # search_content
│   │       ├── shell.rs         # Command execution, background, process cleanup
│   │       ├── workdir.rs       # Working directory management
│   │       ├── remote.rs        # SSH/SFTP path abstraction, SshHandler, SFTP pool
│   │       └── fs_tests.rs      # Integration tests (cfg(test))
│   └── utils/
│       ├── mod.rs               # Module re-exports
│       ├── line_hash.rs         # Composite line ids (N:hh), FNV-1a hashes, endpoint parsing
│       └── truncation.rs        # Token estimation, smart truncation
```

**Key design decisions:**

- **rmcp SDK** — Official Rust MCP SDK (`rmcp` 3.0) for protocol handling
- **Tokio** — Async runtime; all file I/O uses `tokio::fs`, never blocking `std::fs`
- **File locking** — Per-file `tokio::sync::Mutex` prevents concurrent write conflicts
- **Undo history** — Up to 10 snapshots per file, in-memory (lost on restart)
- **Path resolution** — Relative paths resolve against the session workdir; no canonicalization (files may not exist yet)
- **Stale-write detection** — every edit target's `N:hh` id is verified against the file at apply time; a stale id fails with the current content instead of editing the wrong line

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
cargo run -- mcp
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

- [rmcp](https://crates.io/crates/rmcp) — Official Rust MCP SDK
- [Model Context Protocol](https://modelcontextprotocol.io) — The protocol specification

---

<div align="center">

**Built with 🦀 by [Muvon](https://muvon.io)**

*Star us on GitHub if Octofs helps you ship faster! ⭐*

</div>
