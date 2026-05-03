# Octofs — MCP Filesystem Tools Server

Standalone Rust binary that exposes filesystem tools (view, text_editor, batch_edit, extract_lines, shell, workdir) over the Model Context Protocol. Runs as stdio or HTTP server. Built on `rmcp` 1.3, `tokio`, `axum`. Apache 2.0, maintained by Muvon Un Limited.

## Project Structure

```
src/
  main.rs                    — Entry point: CLI dispatch, stdio/HTTP server startup, signal handling
  cli.rs                     — Clap CLI: `octofs mcp [--path] [--bind] [--line-mode]`
  mcp/
    mod.rs                   — McpToolCall struct, session root directory (OnceLock)
    server.rs                — OctofsServer (rmcp tool impl), SessionWorkdir, all Params structs
    hint_accumulator.rs      — Thread-local hint queue; hints appended to every tool response
    shared_utils.rs          — apply_head_truncation helper
    fs/
      mod.rs                 — Re-exports: execute_view, execute_text_editor, execute_batch_edit,
                               execute_extract_lines, execute_shell_command, execute_workdir_command
      core.rs                — resolve_path, file history (undo), execute_view, execute_text_editor,
                               execute_batch_edit, execute_extract_lines
      file_ops.rs            — view_file_spec, view_file_multi_ranges, view_file_with_content_search,
                               create_file_spec, view_many_files, view_many_files_spec
      text_editing.rs        — str_replace_spec, batch_edit_spec, per-file async locking
      directory.rs           — Directory listing and content search (ignore crate + pure-Rust regex)
      search.rs              — search_content: fixed-string match with context blocks
      shell.rs               — execute_shell_command, foreground/background, PGID process group cleanup
      workdir.rs             — execute_workdir_command, WorkdirResult
      fs_tests.rs            — Integration tests (cfg(test) only)
  utils/
    glob.rs                  — expand_glob_patterns_filtered (gitignore-aware, max 1000 files)
    truncation.rs            — estimate_tokens, truncate_to_tokens, format_content_with_line_numbers,
                               truncate_mcp_response_global
    line_hash.rs             — LineMode (Number|Hash), FNV1a-16 per-line hashes, resolve_hash_to_line
```

## Where to Look

| Task | Start here |
|------|------------|
| Add a new MCP tool | `server.rs` (Params struct + `#[tool]` method) → `fs/core.rs` or new `fs/*.rs` (execute fn) → `fs/mod.rs` (re-export) → `fs/fs_tests.rs` (tests) |
| Modify existing tool logic | `fs/core.rs` (view, text_editor, batch_edit, extract_lines) · `fs/text_editing.rs` (str_replace, batch_edit internals) · `fs/shell.rs` · `fs/workdir.rs` |
| Change tool parameter schema | `server.rs` — Params structs with `#[schemars]` / `#[serde]` annotations |
| File reading / formatting | `fs/file_ops.rs` — all view_file_* functions |
| Directory listing / search | `fs/directory.rs` + `fs/search.rs` |
| Line number vs hash mode | `utils/line_hash.rs` — set at startup via `--line-mode` CLI flag |
| Content truncation logic | `utils/truncation.rs` — token estimation and smart truncation |
| Glob expansion | `utils/glob.rs` — gitignore-aware, dotfile-filtered |
| Hint messages to LLM | `mcp/hint_accumulator.rs` — push_hint(), drained after every tool call |
| Session workdir state | `server.rs` `SessionWorkdir` — per-instance RwLock<PathBuf> |
| Process cleanup on exit | `main.rs` signal handler + `fs/shell.rs` `kill_all_shell_children` |

## How Things Work

### Tool Execution Flow

Every tool call goes: `server.rs #[tool] method` → builds `McpToolCall { tool_name, parameters, workdir }` → calls `execute_*` in `fs/` → result string → `append_hints()` wraps it before returning to MCP client.

`McpToolCall` carries the per-session `workdir: PathBuf` so all `execute_*` functions are pure — they receive context, don't read global state.

### Path Resolution

All paths go through `core::resolve_path(path_str, workdir)`:
- Relative → `workdir.join(path)` (no canonicalize — file may not exist yet)
- Absolute → used as-is

```rust
// ✅ always resolve through workdir
let path = resolve_path(&params.path, &call.workdir);

// ❌ never construct paths directly
let path = PathBuf::from(&params.path);
```

### File Locking

Concurrent writes to the same file are serialized via per-file `tokio::sync::Mutex` stored in a `std::sync::Mutex<HashMap>`. Key is the canonicalized path (falls back to raw string). Lock is acquired in `text_editing::acquire_file_lock` before any write. Never hold the outer `std::sync::Mutex` across an `await`.

### Undo History

`core::save_file_history(path)` snapshots current content into `FILE_HISTORY` (OnceLock Mutex HashMap) before every write. `core::undo_edit(path)` pops the last snapshot. History is in-memory only — lost on restart.

### Line Identifiers

Two modes set once at startup via `--line-mode`:
- `number` (default) — sequential 1-indexed integers
- `hash` — 4-char lowercase hex FNV1a-16 hashes, position-dependent (same content at different lines → different hash)

`utils/line_hash::is_hash_mode()` gates all formatting paths. `batch_edit` and `extract_lines` accept both numbers and hash strings in their range parameters.

### Hint Accumulator

Any `execute_*` function can call `hint_accumulator::push_hint("...")` to queue guidance text. After the tool returns, `server.rs::append_hints()` drains the queue and appends hints to the response. Used to surface misuse warnings to the LLM without failing the call.

### Transport Modes

- **stdio** (default): single `OctofsServer` instance, session root from `--path` or `cwd`
- **HTTP** (`--bind host:port`): `axum` + `rmcp` streamable HTTP; each session gets a fresh `OctofsServer::with_root()` instance; initial workdir can be set via MCP `initialize` params

### Error Handling

```rust
// ✅ anyhow::bail! for early validation exits
anyhow::bail!("Path cannot be empty");

// ✅ .context() to add location to propagated errors
tokio::fs::read_to_string(&path).await
    .context(format!("Failed to read file: {}", path.display()))?;

// ❌ never unwrap in non-test code (except OnceLock init patterns)
fs::read_to_string(path).unwrap();
```

### Async Rules

```rust
// ✅ tokio::fs for all file I/O
tokio::fs::read_to_string(&path).await?;

// ❌ std::fs blocks the async runtime
std::fs::read_to_string(&path)?;
```

### Adding a New Tool — Checklist

1. **`server.rs`** — add `Params` struct (derive `Deserialize`, `JsonSchema`), add `#[tool]` async method on `OctofsServer`
2. **`fs/`** — implement `execute_my_tool(call: &McpToolCall) -> Result<String>` in the appropriate module (`core.rs` for file ops, new file for distinct domains)
3. **`fs/mod.rs`** — re-export the execute function
4. **`fs/fs_tests.rs`** — add tests using `McpToolCall::test_call(...)` and `tempfile`

## Code Style

- **Naming**: `snake_case` functions/variables, `PascalCase` types/enums/traits, `SCREAMING_SNAKE_CASE` constants
- **Comments**: explain *why*, not *what*; module-level doc comment on every file; avoid obvious comments
- **Line length**: 100 chars max (enforced by `rustfmt.toml`)
- **Copyright header**: every `.rs` file must start with the Apache 2.0 header — `Copyright 2026 Muvon Un Limited`. Verify year when modifying files in a new calendar year.

## Validation

- Zero `cargo clippy` warnings — treat warnings as errors
- All tests pass: `cargo test`
- No `std::fs` blocking calls in async paths
- No `.unwrap()` outside of test code or OnceLock init patterns
- New tools have tests in `fs_tests.rs` covering happy path + error cases
- Copyright header present and year correct on every modified `.rs` file

## Gotchas

- `functions.rs` does **not exist** — tool schemas and Params structs live in `server.rs`; execute logic lives in `fs/core.rs` or domain-specific `fs/*.rs` files
- `resolve_path` does **not** canonicalize — the file may not exist yet (e.g. `text_editor create`). Canonicalize only when building lock keys
- `SessionWorkdir` is per-server-instance (HTTP: per-session); `SESSION_ROOT` in `mcp/mod.rs` is the startup default only
- The outer `std::sync::Mutex` in `FILE_LOCKS` must never be held across an `.await` — acquire the inner `tokio::sync::Mutex` first, then drop the outer guard
- Shell children are tracked by PID/PGID; `kill_all_shell_children` is called on SIGTERM/EOF — always register new child processes via `register_child(pid)`
- `ignore` crate (gitignore-aware walker) is used for directory listing — dotfiles and `.gitignore`d paths are excluded by default

## Never

- Add `std::fs` blocking calls inside `async fn` — use `tokio::fs` exclusively
- Use `.unwrap()` in non-test, non-OnceLock-init code
- Skip the copyright header on new `.rs` files
- Add a new dependency without first checking if an existing one covers the need
- Reference `functions.rs` — it was removed; tool definitions are in `server.rs`
