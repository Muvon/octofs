# Development

Building, testing, and the conventions every change must honour. Deeper
working notes for agents live in `.agents/` (see below).

## Build & test

- Build: `cargo build` · Release: `cargo build --release`
- Dev run: `cargo run -- mcp` (stdio; add `--bind 0.0.0.0:12345` for HTTP;
  `RUST_LOG=debug` for logs)
- Format: `cargo fmt` — hard tabs (`rustfmt.toml`), default 100-col width
- Lint: `cargo clippy --all-features -- -D warnings`
- Test: `cargo test --all-features` (one suite: `cargo test --all-features <name>`)
- Coverage: `make coverage` (needs cargo-llvm-cov + llvm-tools-preview;
  excludes `*_tests.rs`)

This checkout auto-syncs to the dev server — run build/test/clippy there
(`ssh dev`), not on this machine.

## Conventions

- **No inline test modules in production files.** Unit tests live in a sibling
  `<module>_tests.rs`, wired with
  `#[cfg(test)] #[path = "<module>_tests.rs"] mod <module>_tests;`.
  Cross-module integration tests: `src/mcp/fs/fs_tests.rs`; protocol-level
  in-process tests: `src/mcp/server_tests.rs` (rmcp `client` feature is a
  dev-dependency — feature unification keeps it out of the shipped binary).
- **Every `.rs` file starts with the Apache 2.0 header** —
  `Copyright 2026 Muvon Un Limited`; check the year in a new calendar year.
- **Async file I/O only via `tokio::fs`** — never `std::fs` inside `async fn`.
- **Errors:** `anyhow::bail!` for validation exits, `.context()` on
  propagation; no `.unwrap()`/`.expect()` outside tests and OnceLock init.
- **Comments explain why, not what**; a module-level doc comment on every file.
- Don't add a dependency before checking whether an existing one covers the
  need. Don't hand-edit `CHANGELOG.md` or release-bump `Cargo.toml` casually —
  see `.agents/release.md` first.

## Adding or changing an MCP tool

Reference example: the `view` tool — `#[tool]` block in `src/mcp/server.rs`,
execute path in `src/mcp/fs/core.rs` + `file_ops.rs`, tests in `fs_tests.rs`.

1. **`src/mcp/server.rs`** — Params struct (`Deserialize` + `JsonSchema`,
   schemars/serde annotations); an `async fn` method with
   `#[tool(title = …, description = …)]` inside the `#[tool_router]` impl;
   build the `McpToolCall`, run it inside
   `request_ctx::with_request_context(self.view_cache.clone(), …)`, wrap the
   result with `append_hints`.
2. **`src/mcp/fs/`** — `execute_my_tool(call: &McpToolCall) -> Result<String>`
   in the fitting module; re-export from `fs/mod.rs`.
3. **Tests** — sibling `*_tests.rs` for units; `fs_tests.rs` for integration
   (`McpToolCall::test_call` + `tempfile`; `serial_test` when global
   registries are involved); `server_tests.rs` for protocol delivery.
4. Run the build/test/lint commands above on the dev server.

### Contracts to honour

- **Descriptions are model-facing documentation.** The `#[tool]` description
  and param docs are exactly what client LLMs see; a schema change is a docs
  change — update it in the same commit (and this `doc/` if behavior shifts).
- **Purity** — execute fns take everything from `call` (workdir, parameters);
  the only ambient state is the explicit registries (file locks, history, view
  cache, SFTP pool).
- **Paths** — always `resolve_path_source` / `core::resolve_path`, never
  `PathBuf::from(raw_param)`; remote URLs and workdir-relative resolution must
  keep working.
- **Response size** — route large outputs through `utils/truncation.rs`
  (`estimate_tokens`, the token-aware `format_*` helpers) or an explicit tail
  cap (e.g. `MAX_TAIL_BYTES`). Context economy is the product's core promise;
  an unbounded response is a bug.
- **Hints vs errors** — non-fatal guidance goes through
  `request_ctx::push_hint`; hard misuse and invalid input `bail!`.
- **Writes** — acquire the file lock (`text_editing`), snapshot via
  `save_file_history` (undo), keep the delta cache truthful
  (`delta::note_write` / `note_create`), match in LF space and restore the
  file's dominant CRLF ending on write.
- **Long-running work** — follow the shell pattern: durable output capture
  from the start, promote to a background job past the foreground window,
  expose `octofs://jobs/<id>`, notify once on completion.

## Deeper references

- `.agents/architecture.md` — agent-oriented cross-module working notes
- `.agents/tool-authoring.md` — the tool checklist (condensed above)
- `.agents/remote-ssh.md` — read before touching `fs/remote.rs` or path resolution
- `.agents/release.md` — releases, CI, publishing
- [tools.md](tools.md) · [architecture.md](architecture.md) · [remote.md](remote.md)
