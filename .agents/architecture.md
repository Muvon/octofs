# Octofs — architecture notes

Reference for cross-cutting work. Read when a change spans more than one module.

## Module map

```
src/
  main.rs              — CLI dispatch (`octofs mcp`), stdio/HTTP startup, SIGTERM/EOF shutdown + child cleanup
  cli.rs               — clap CLI: `octofs mcp [--path PATH] [--bind HOST:PORT] [--ssh-key FILE] [--ssh-timeout SECS]`
  mcp/
    mod.rs             — McpToolCall { tool_name, parameters, tool_id, workdir }, SESSION_ROOT (startup default only)
    server.rs          — OctofsServer: #[tool_router] methods (view, text_editor, batch_edit, extract_lines, shell,
                          workdir), all Params structs, append_hints, MCP resources (octofs://jobs/<id>),
                          subscribe/listen/unsubscribe, progress notifications
    request_ctx.rs     — per-request context: hint queue (push_hint → drained by append_hints on every response)
    fs/
      mod.rs           — module list + re-exports (execute_*, parse_path_source, PathSource)
      core.rs          — resolve_path, undo history (FILE_HISTORY), execute_view/text_editor/batch_edit/extract_lines
      file_ops.rs      — view_file_spec, view_file_with_content_search, create_file_spec
      text_editing.rs  — str_replace_spec (progressive matching), batch_edit_spec, per-file async locking
      directory.rs     — directory listing, gitignore-aware walking, glob filtering, remote listing quirks
      search.rs        — literal/regex matcher with context blocks
      delta.rs         — ViewCache (note_write/note_create/render_whole_file), Myers line diff, hunk rendering
      remote.rs        — PathSource, ssh:// URL parsing, OpenSSH config resolution, SFTP pool, remote file ops
      shell.rs         — execute_shell_command, misuse gate, foreground window, process-group cleanup
      background.rs    — job registry, octofs://jobs/<id> resources, durable output files, completion callback
      workdir.rs       — workdir get/set/reset (WorkdirResult)
    *_tests.rs         — sibling unit tests; fs_tests.rs = cross-module integration; server_tests.rs = in-process
                          protocol delivery tests (rmcp `client` feature, dev-dependency only)
  utils/
    line_hash.rs       — N:hh line ids (FNV1a-8 of content), parse_endpoint, verify_line_id
    truncation.rs      — estimate_tokens, truncate_to_tokens, global response truncation
```

## Execution flow

`server.rs` `#[tool]` method → builds `McpToolCall` (carries the per-session `workdir`) → runs the `execute_*`
function inside `request_ctx::with_request_context(view_cache, …)` → result string → `append_hints()` drains the
hint queue into the response. Tool methods return protocol content; one may attach a `ResourceLink` when a call
promotes a job to background (see shell).

## State

- **SessionWorkdir** — per `OctofsServer` instance (`RwLock<PathBuf>`); HTTP mode creates a fresh server per
  session. `SESSION_ROOT` in `mcp/mod.rs` is only the startup default.
- **FILE_LOCKS** — per-file `tokio::sync::Mutex` inside a `std::sync::Mutex<HashMap>`; keys are canonicalized
  local paths or `PathSource::lock_key()` (host+path) for remote. The outer guard is never held across `.await`.
- **FILE_HISTORY** — content snapshot before every write; powers `undo` (max 10 levels per file, in-memory only).
- **ViewCache** (`delta.rs`) — per session and file, the last content the model saw in full. Whole-file views
  return only hunks changed since; this server's own writes keep the cache in step via `note_write` (only if the
  cached "before" matched) / `note_create`. Ranged views and searches bypass it.
- **SFTP pool** (`remote.rs`) — sessions pooled per (host, port, user); dead transports evicted and replaced;
  pool lock deliberately held across connection setup (avoids duplicate connections and agent prompts).

## Line ids

One format, no mode switch: `N:hh` — 1-indexed position plus a 2-char lowercase hex FNV1a-8 hash of the line's
content (content-only, so a moved line keeps its hash and stale errors can say where content went). `view`
renders `N:hh|content`. Edit targets require ids, verified per-target via `verify_line_id`; a stale id fails
with current content around the target, relocation candidates, and a ranged-view suggestion. Edit results are
diffs with fresh ids plus a trailing `shift:` line, so edits chain without re-viewing. Plain integers remain
where position alone is safe: `view` ranges (negatives count from EOF) and batch_edit insert anchors `0` (file
start) / `-1` (after last line).

## Shell lifecycle

Command runs in the foreground window (`FOREGROUND_TIMEOUT`) with progress notifications. If it outlasts the
window, the same process is promoted to a background job (`background::promote`), the tool call returns a
resource URI `octofs://jobs/<id>`, and a wait task fires the completion callback exactly once — subscriptions
first, peer `resources/updated` notification as fallback. A resource read returns status plus the output tail
(`MAX_TAIL_BYTES` = 30 000). Children run in their own process group; `kill_all_shell_children()` runs on stdio
EOF/SIGTERM and on HTTP ctrl-c.

## Transport

- **stdio** (default): one `OctofsServer`; session root from `--path` or cwd; clean exit on stdin EOF or SIGTERM.
- **HTTP** (`--bind`): axum + rmcp streamable HTTP; fresh `OctofsServer::with_root()` per session; initial
  workdir settable via MCP `initialize` params.

## Error handling shape

`anyhow::bail!` for early validation exits; `.context(...)` on propagated errors; `.unwrap()` only in tests and
OnceLock init. Shell-misuse and remote-workdir-with-local-shell are hard errors (bail), softer guidance goes
through `push_hint` instead of failing the call.
