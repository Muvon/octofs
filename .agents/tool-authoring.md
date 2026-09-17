# Octofs — adding or changing an MCP tool

Read this first. Reference example: the `view` tool — `#[tool]` block in `src/mcp/server.rs`, execute path in
`src/mcp/fs/core.rs` + `file_ops.rs`, tests in `fs_tests.rs`.

## Checklist

1. **`src/mcp/server.rs`** — Params struct (`Deserialize` + `JsonSchema`, schemars/serde annotations); an
   `async fn` method with `#[tool(title = …, description = …)]` inside the `#[tool_router]` impl; build the
   `McpToolCall`, run it inside `request_ctx::with_request_context(self.view_cache.clone(), …)`, wrap the
   result with `append_hints`.
2. **`src/mcp/fs/`** — `execute_my_tool(call: &McpToolCall) -> Result<String>` in the fitting module
   (`core.rs`/domain module, or a new `*.rs` + `mod` line); re-export from `fs/mod.rs`.
3. **Tests** — sibling `*_tests.rs` for units; `fs_tests.rs` for integration (`McpToolCall::test_call` +
   `tempfile`; `serial_test` when global registries are involved); `server_tests.rs` for protocol-level
   in-process delivery (rmcp `client` feature is a dev-dependency — feature unification keeps it out of the
   shipped binary).
4. Run the Done commands from the root AGENTS.md.

## Contracts to honour

- **Descriptions are model-facing documentation.** The `#[tool]` description and param docs are exactly what
  client LLMs see; they encode parameter semantics, defaults and the reuse discipline (see `view`'s guidance on
  ranges, delta views and when to re-read). Treat a schema change as a docs change and update it in the same
  commit.
- **Purity** — execute fns take everything from `call` (workdir, parameters); the only ambient state is the
  explicit registries (file locks, history, view cache, SFTP pool).
- **Paths** — always `resolve_path_source` / `core::resolve_path`, never `PathBuf::from(raw_param)`; remote
  URLs and workdir-relative resolution must keep working.
- **Response size** — route large outputs through `utils/truncation.rs` (`truncate_mcp_response_global`) or an
  explicit tail cap (see `MAX_TAIL_BYTES` in `background.rs`). The product's core promise is model-context
  economy; an unbounded response is a bug.
- **Hints vs errors** — non-fatal guidance goes through `request_ctx::push_hint` (drained into the response);
  hard misuse and invalid input `bail!` so the call fails.
- **Writes** — acquire the file lock (`text_editing`), snapshot via `save_file_history` (undo), keep the delta
  cache truthful (`delta::note_write` / `note_create`), match in LF space and restore CRLF on write
  (`restore_endings`).
- **Long-running work** — follow the shell pattern: durable output capture from the start, promote to a
  background job past the foreground window, expose `octofs://jobs/<id>`, notify once on completion.
