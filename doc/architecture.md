# Architecture

How Octofs is put together: module map, execution flow, shared state, and the
cross-cutting mechanisms (line ids, delta views, shell lifecycle, transport).
For per-tool behavior see [tools.md](tools.md); for remote files
[remote.md](remote.md); for contributing [development.md](development.md).

## Module map

```
src/
  main.rs              — CLI dispatch (`octofs mcp`), stdio/HTTP startup, SIGTERM/EOF shutdown + child cleanup
  cli.rs               — clap CLI: `octofs mcp [--path PATH] [--bind HOST:PORT] [--auth-token TOKEN] [--ssh-key FILE] [--ssh-timeout SECS]`
  http_auth.rs         — HTTP access control: bind policy, bearer-token layer, Host/Origin config
  mcp/
    mod.rs             — McpToolCall { tool_name, parameters, tool_id, workdir }, SESSION_ROOT (startup default only)
    server.rs          — OctofsServer: #[tool_router] methods (view, symbol, text_editor, batch_edit, extract_lines,
                          shell, workdir), all Params structs, append_hints, MCP resources (octofs://jobs/<id>),
                          subscribe/listen/unsubscribe, progress notifications
    request_ctx.rs     — per-request context: hint queue (push_hint → drained by append_hints on every response)
    fs/
      mod.rs           — module list + re-exports (execute_*, parse_path_source, PathSource)
      core.rs          — resolve_path, undo history (FILE_HISTORY), execute_view/text_editor/batch_edit/extract_lines
      file_ops.rs      — view_file_spec, view_file_with_content_search, create_file_spec
      text_editing.rs  — str_replace_spec (progressive matching), batch_edit_spec, per-file async locking
      directory.rs     — directory listing, gitignore-aware walking, glob filtering, remote listing quirks
      search.rs        — literal/regex matcher with context blocks, enclosing-symbol breadcrumbs
      structure.rs     — structural outline mode (flags: "s"): file/directory walk, outline rendering
      symbols.rs       — `symbol` tool: parameter validation, definitions/references (text-level), breadcrumbs
      graph.rs         — `symbol` graph relations: workdir walk, single-entry graph cache, traversal rendering
      ast.rs           — AST-pattern search mode (ast: true): ast-grep patterns over the grammars
      delta.rs         — ViewCache (note_write/note_create/render_whole_file), Myers line diff, hunk rendering
      remote.rs        — PathSource, ssh:// URL parsing, OpenSSH config resolution, SFTP pool, remote file ops
      shell.rs         — execute_shell_command, misuse gate, foreground window, process-group cleanup
      background.rs    — job registry, octofs://jobs/<id> resources, durable output files, completion callback
      workdir.rs       — workdir get/set/reset (WorkdirResult)
    *_tests.rs         — sibling unit tests; fs_tests.rs = cross-module integration; server_tests.rs = in-process
                          protocol delivery tests (rmcp `client` feature, dev-dependency only)
  structural/
    mod.rs             — outline entry point, extension → language map, supported-language list
    signature_extractor.rs — signature line spans per language, Markdown headings
    symbols.rs         — definitions/references by name, per-line enclosing-symbol breadcrumbs
    graph/             — pure code graph: files, symbols, calls/imports/extends/implements, traversal (no I/O)
    ast_search.rs      — ast-grep pattern search over the grammars, expando rewrite, miss diagnostics
    languages/         — one tree-sitter grammar + outline/symbol rules per language
    *_tests.rs         — sibling unit tests (mod, signature_extractor, symbols, ast_search, per language)
  utils/
    line_hash.rs       — N:hh line ids (FNV1a-8 of content), parse_endpoint, verify_line_id
    truncation.rs      — estimate_tokens, format_content_with_line_numbers, format_extracted_content_smart
```

## Execution flow

A `#[tool]` method in `server.rs` builds an `McpToolCall` (carrying the
per-session `workdir`), runs the matching `execute_*` function inside
`request_ctx::with_request_context(view_cache, …)`, and wraps the result
string with `append_hints()`, which drains the per-request hint queue into the
response. The `shell` tool additionally attaches a `ResourceLink` when a call
is promoted to background.

Layering rule: `server.rs` owns everything protocol-shaped (schemas,
descriptions, resources, notifications); `fs/*` is protocol-free — execute
functions are pure, taking everything from `&McpToolCall`. The only ambient
state they touch is the explicit registries below. A background job outlives
its call; its completion path is injected as an opaque callback so the fs layer
never sees MCP types.

`view` dispatches by mode in `fs/core.rs` (`view_existing_path`): `ast` → outline (`flags: "s"`) →
listing/content search. Every mode bails on the other modes' parameters before dispatch, so the modes are
mutually exclusive. The structural modes split pure parsing (`src/structural/`, content in — structured items
out) from I/O and rendering (`fs/structure.rs`, `fs/ast.rs`).

The `symbol` tool is separate. `execute_symbol` (`fs/symbols.rs`) validates the parameter combination for the
`relation`, answers `definition`/`references` itself (local or `ssh://`) and hands the graph relations to
`fs/graph.rs`. That module walks the session workdir, builds the graph with `structural::graph::build_graph`
inside `spawn_blocking`, and renders the hops under the response budget; it is local only.

## State

- **Sessions** (`server.rs`) — one `SessionWorkdir` (`RwLock<PathBuf>`) and
  `ViewCache` per client session, resolved on every request from
  `experimental.session` in the client capabilities: `session_id` keys the
  state, `workdir` is its root (the `workdir` tool's reset target). One
  connection can carry many sessions (octomind multiplexes its sessions over a
  shared client); requests without session context share the server root.
  Least recently used sessions beyond `MAX_TRACKED_SESSIONS` are dropped.
  `SESSION_ROOT` in `mcp/mod.rs` is only the startup default.
- **FILE_LOCKS** — per-file `tokio::sync::Mutex` inside a
  `std::sync::Mutex<HashMap>`; keys are canonicalized local paths or
  `PathSource::lock_key()` (host+path) for remote. The outer guard is never
  held across `.await`.
- **FILE_HISTORY** — content snapshot before every write; powers `undo_edit`
  (max 10 levels per file, in-memory only).
- **Scoped graph builds** (`fs/graph_scoped.rs`) — not cached: `callers`, `callees`, `supertypes`, `subtypes`,
  `imports` and `imported_by` parse only the files that mention the names they follow; `path` uses the cache below.
- **GRAPH_CACHE** (`fs/graph.rs`) — an LRU of up to 4 entries, one code graph per workdir root, each with a
  stamp (hash of relative path, size, mtime, and on unix ctime and inode of every walked file). The stamp is
  recomputed on every `symbol` graph call and a graph is rebuilt only when it differs. `BUILD_LOCKS` holds one
  async lock per root so concurrent cold calls share one build. In-memory only; the std mutexes are never held
  across `.await`.
- **ViewCache** (`delta.rs`) — per client session and file, the last content the
  model saw in full. Whole-file views return only hunks changed since; the
  server's own writes keep the cache in step via `note_write` (only if the
  cached "before" matched) / `note_create`. Ranged views, searches and the
  structural read modes bypass it.
- **SFTP pool** (`remote.rs`) — sessions pooled per (host, port, user); dead
  transports evicted and replaced; the pool lock is deliberately held across
  connection setup to avoid duplicate connections and agent prompts.

## Line ids

One format, no mode switch: `N:hh` — 1-indexed position plus a 2-char
lowercase hex FNV1a-8 hash of the line's content (`utils/line_hash.rs`).
Hashing content (not position) means a moved line keeps its hash and stale-id
errors can say where the content went. `view` renders `N:hh|content`; edit
targets require ids, verified per-target via `verify_line_id`. Edit results
are diffs with fresh ids plus a trailing `shift:` line, so edits chain without
re-viewing. Plain integers remain where position alone is safe: `view` ranges
(negatives count from EOF) and `batch_edit` insert anchors `0` / `-1`.

## Shell lifecycle

A command runs in the foreground window (`FOREGROUND_TIMEOUT`, ~10 s) with
progress notifications as heartbeat. If it outlasts the window, the same
process is promoted to a background job (`background::promote`), the tool call
returns a resource URI `octofs://jobs/<id>`, and a wait task fires the
completion callback exactly once — subscriptions first, peer
`resources/updated` notification as fallback. A resource read returns status
plus the output tail (`MAX_TAIL_BYTES` = 30 000). Children run in their own
process group; `kill_all_shell_children()` runs on stdio EOF/SIGTERM and on
HTTP ctrl-c.

## Transport

- **stdio** (default): one `OctofsServer`; session root from `--path` or cwd;
  clean exit on stdin EOF or SIGTERM.
- **HTTP** (`--bind`): axum + rmcp streamable HTTP; fresh
  `OctofsServer::with_root()` per HTTP session; session workdirs come from the
  client capabilities as in stdio mode. `http_auth.rs` gates access: a
  non-loopback bind needs `--auth-token`/`OCTOFS_AUTH_TOKEN` (bearer check,
  Host allowlist lifted); without a token the server is loopback-only and
  rejects any request carrying an `Origin` header.

## Error handling shape

`anyhow::bail!` for early validation exits; `.context(...)` on propagated
errors; `.unwrap()` only in tests and OnceLock init. Hard misuse (shell
misuse, remote workdir with local shell) fails the call via `bail!`; softer
guidance goes through `push_hint` and still returns a successful result.
