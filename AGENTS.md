# Octofs — MCP Filesystem Tools Server

Standalone Rust binary that exposes filesystem tools (view, text_editor, batch_edit, extract_lines, shell, workdir) over the Model Context Protocol. Runs as stdio or HTTP server. Built on `rmcp` 1.3, `tokio`, `axum`. Apache 2.0, maintained by Muvon Un Limited.

## Project Structure

```
src/
  main.rs                    — Entry point: CLI dispatch, stdio/HTTP server startup, signal handling
  cli.rs                     — Clap CLI: `octofs mcp [--path] [--bind] [--ssh-key] [--ssh-timeout]`
  mcp/
    mod.rs                   — McpToolCall struct, session root directory (OnceLock)
    server.rs                — OctofsServer (rmcp tool impl), SessionWorkdir, all Params structs
    request_ctx.rs           — Per-request hint queue (appended to every tool response)
    shared_utils.rs          — apply_head_truncation helper
    fs/
      mod.rs                 — Re-exports: execute_view, execute_text_editor, execute_batch_edit,
                               execute_extract_lines, execute_shell_command, execute_workdir_command
      core.rs                — resolve_path, file history (undo), execute_view, execute_text_editor,
                               execute_batch_edit, execute_extract_lines
      file_ops.rs            — view_file_spec, view_file_with_content_search, create_file_spec
      text_editing.rs        — str_replace_spec, batch_edit_spec, per-file async locking
      directory.rs           — Directory listing and content search (ignore crate + pure-Rust regex)
      search.rs              — Matcher + search_lines: literal/regex matching with context blocks
      shell.rs               — execute_shell_command, foreground/background, PGID process group cleanup
      workdir.rs             — execute_workdir_command, WorkdirResult
      *_tests.rs             — Unit tests for the matching production module
      fs_tests.rs            — Cross-module integration tests (cfg(test) only)
  utils/
    glob.rs                  — expand_glob_patterns_filtered (gitignore-aware, max 1000 files)
    truncation.rs            — estimate_tokens, truncate_to_tokens, format_content_with_line_numbers,
                               truncate_mcp_response_global
    line_hash.rs             — Composite line ids (N:hh), FNV1a-8 content hashes, verify_line_id
```

## Where to Look

| Task | Start here |
|------|------------|
| Add a new MCP tool | `server.rs` (Params struct + `#[tool]` method) → `fs/core.rs` or new `fs/*.rs` (execute fn) → `fs/mod.rs` (re-export) → `fs/fs_tests.rs` (tests) |
| Modify existing tool logic | `fs/core.rs` (view, text_editor, batch_edit, extract_lines) · `fs/text_editing.rs` (str_replace, batch_edit internals) · `fs/shell.rs` · `fs/workdir.rs` |
| Change tool parameter schema | `server.rs` — Params structs with `#[schemars]` / `#[serde]` annotations |
| File reading / formatting | `fs/file_ops.rs` — all view_file_* functions |
| Directory listing / search | `fs/directory.rs` + `fs/search.rs` |
| Line ids / endpoint parsing | `utils/line_hash.rs` — composite `N:hh` ids, verification, stale errors |
| Content truncation logic | `utils/truncation.rs` — token estimation and smart truncation |
| Glob expansion | `utils/glob.rs` — gitignore-aware, dotfile-filtered |
| Hint messages to LLM | `mcp/request_ctx.rs` — push_hint(), drained after every tool call; shell misuse always rejects (`fs/shell.rs`) |
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

One format, no mode switch: every line is addressed as `N:hh` — 1-indexed position plus a 2-char lowercase hex FNV1a-8 hash of the line's CONTENT (content-only, so a moved line keeps its hash and stale errors can report where content went). View output renders `N:hh|content`.

`utils/line_hash::parse_endpoint` parses each scalar line target into an `Endpoint`: JSON integer (or numeric string) → `Number`, `"N:hh"` string → `Id`. Edit targets require `Id` and are verified against the file via `verify_line_id` — a stale id fails with the current content around the target, relocation candidates (same hash elsewhere), and a ranged-`view` suggestion. There is no whole-file staleness gate; verification is per-target. Edit results are diffs with FRESH ids so edits chain without re-viewing.
- `view`: `path` (single string), optional `start`/`end` (omit both → whole file; `start` only → to EOF). Numbers clamp to bounds; negatives count from EOF; ids resolve by their position part.
- `batch_edit` op: `start` (+ optional `end`), both ids for replace (end omitted → single line). Insert anchor: an id, or `0` = file start / `-1` = after last line (plain integers; other integers rejected — unverifiable).
- `extract_lines`: `from_start`/`from_end` and `append_line` accept numbers or ids (ids verified).

Multi-file view was removed — the caller makes parallel `view` calls instead.

### str_replace Matching

Progressive stages in `text_editing::str_replace_spec`: exact (unique, or every occurrence with `replace_all: true`) → escaped-literal recovery (double-escaped `\n`/`\t` interpreted when the unescaped form matches) → whitespace-normalized fuzzy with indentation adjustment → closest-match diagnostics with line ids. CRLF files: all matching happens in LF space, `restore_endings` puts `\r\n` back on write (batch_edit does the same) — without this, CRLF files silently no-oped on the fuzzy path.

### Hint Accumulator

Any `execute_*` function can call `request_ctx::push_hint("...")` to queue guidance text. After the tool returns, `server.rs::append_hints()` drains the queue and appends hints to the response. Used to surface misuse warnings to the LLM without failing the call.

### Shell Misuse Enforcement

Shell misuse (cat/grep/find/ls/sed/awk instead of dedicated tools) is ALWAYS rejected with an error — no mode switch; this deliberately blocks shell in favour of the dedicated tools. Pipelines are not split (`... | grep` stays allowed); quoted separators (SSH remote commands) don't false-positive. Enforced in `fs/shell.rs::execute_shell_command` via `detect_shell_misuse`.

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
- **Test layout**: production `.rs` files must not contain inline test modules or test bodies. Put unit tests in a sibling `<module>_tests.rs` file and wire it with `#[cfg(test)]`, `#[path = "<module>_tests.rs"]`, and `mod <module>_tests;`. Keep cross-module filesystem integration tests in `fs/fs_tests.rs`.

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
- Define inline `mod tests { ... }` or other test bodies in a production `.rs` file — use a sibling `*_tests.rs` file
