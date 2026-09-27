# MCP Tools Reference

Every tool Octofs exposes, what it does, and how it works internally. The
model-facing contract (schemas, descriptions, defaults) lives in
`src/mcp/server.rs` — `Params` structs and `#[tool(...)]` blocks; this document
explains the behavior behind those schemas. All tools share one execution
pipeline (see [architecture.md](architecture.md)):

```
#[tool] method (server.rs)
  → McpToolCall { tool_name, parameters, tool_id, workdir }
  → request_ctx::with_request_context(view_cache, …)
  → execute_* fn in src/mcp/fs/          — pure: everything arrives via the call
  → result string → append_hints()       — drains queued guidance into the response
```

Relative paths resolve against the session workdir (see
[architecture.md](architecture.md#state)); every file tool also accepts
`ssh://` / `sftp://` URLs transparently (see [remote.md](remote.md)).

---

## `view` — read files, list directories, search content

**Modules:** `fs/core.rs`, `fs/file_ops.rs`, `fs/directory.rs`, `fs/search.rs`, `fs/delta.rs`

One tool, three read modes, selected by the parameters:

- **File read** (`path`) — lines render as `N:hh|content`, where `N:hh` is the
  line id edit tools target (see [Line ids](architecture.md#line-ids)). Ranged
  reads (`start`/`end`, negative counts from EOF) always return the requested
  lines.
- **Directory listing** (a directory `path`) — recursive and gitignore-aware,
  one `path\tNL\t~Nt` (lines, ~tokens) entry per file.
- **Content search** (`content`, optionally `regex: true`, with `context` lines
  around each match) — literal or regex matching over a file or tree, bounded
  by `pattern` globs.

**Delta views.** Re-viewing a *whole* file returns only the hunks changed since
the last full view of that file in the same session (or an unchanged marker).
The `ViewCache` (`fs/delta.rs`) stores the last content the model saw; a
Myers line diff renders changed hunks with fresh ids. Ranged views and searches
bypass the cache by design — only whole-file views update it. Octofs's own
writes keep the cache truthful via `delta::note_write` / `note_create`, so an
edit followed by a whole-file view does not replay the file.

**Context-economy contract** (encoded in the tool description): reuse returned
content, read a complete block in one call, never re-read overlapping ranges;
re-read only when the file may have changed or output was truncated. Oversized
output is truncated to a token budget (`utils/truncation.rs`) with the cut
point reported.

---

## `text_editor` — create, str_replace, delete, undo

**Modules:** `fs/core.rs`, `fs/text_editing.rs`, `fs/file_ops.rs`

A multi-command tool (`command` + operands):

- **create** — new file only; fails if it exists, parent directories are made.
- **str_replace** — raw file text (real newlines, no `N:hh|` prefixes);
  `old_text` must match exactly once, or set `replace_all: true`. No/multiple
  matches return an error listing candidate line ids for `batch_edit`.
  Matching is progressive (fuzzy-anchored) and happens in LF space; the file's
  dominant line ending is recorded and CRLF is restored on write.
- **delete** — remove a file (not a directory).
- **undo_edit** — revert the last edit to `path`; up to 10 levels per file,
  in-memory only, delete included. Powered by `FILE_HISTORY`: a content
  snapshot is saved before every write.

Every write acquires the per-file async lock (`text_editing.rs`) and updates
the delta cache.

---

## `batch_edit` — atomic multi-edit on one file

**Modules:** `fs/core.rs`, `fs/text_editing.rs`

Applies up to 50 `insert`/`replace` operations to one file atomically.

- **Targets are line ids** (`"12:a3"`) from `view` or edit output. Each target
  is verified against the current file content (`verify_line_id`,
  `utils/line_hash.rs`) *before anything is written* — a stale id fails the
  whole call with the current content around the target, relocation
  candidates, and a ranged-view suggestion.
- All targets refer to the **original** file and must not overlap; insert
  after line N and replace of line N do not conflict.
- Insert anchors `0` (file start) and `-1` (after last line) are plain
  integers — the only places where position alone is safe.
- The result is a diff with **fresh ids** for follow-up edits — no re-view
  needed. Removed lines, and the middle of a long added block, render as an id
  range; a trailing `shift:` line reports how original line numbers below each
  edit moved.

Because verification precedes the write, a failed batch leaves the file
untouched.

---

## `extract_lines` — copy a range between files

**Modules:** `fs/core.rs`

Copies a line range from one file and appends it into another — moving code
between files, splitting modules — without retyping it. The source is left
untouched; to *move*, follow with a `batch_edit` that removes the range.
Targets follow the same `N:hh` verification and locking discipline as
`batch_edit`.

---

## `shell` — run commands, with background promotion

**Modules:** `fs/shell.rs`, `fs/background.rs`

Runs `sh -c` (`cmd /C` on Windows) in the current workdir. Local machine only —
a remote (`ssh://`) workdir disables `shell` (bail, not a hint). No stdin, no
TTY: interactive commands are unusable by design.

**Lifecycle.** A command runs in a ~10 s foreground window with MCP progress
notifications as heartbeat (so a silent build isn't cancelled by an idle
timeout). Anything still running at the boundary is promoted — the *same
process* becomes a background job; the tool call returns immediately with a
`ResourceLink` to `octofs://jobs/<id>` (named `shell: <first 80 chars>`), and a
wait task fires the completion callback exactly once when it exits: a
subscription notification first, a peer `resources/updated` push as fallback.
Reading the resource returns status plus the output tail (capped at
`MAX_TAIL_BYTES` = 30 000). The guidance to the model: do not poll, sleep,
re-run or `ps` — start the next independent step or end the turn.

**Misuse gate** (`detect_shell_misuse`). The gate is nuanced; preserve this
contract when touching it:

- rejected outright: `sed -i`, content writes into the workdir (`cat >`,
  `echo >`, `tee`), a read program (cat/grep/find/ls) starting any command —
  alone, in a chain or as a pipeline head — file reads belong to `view`;
- run with a hint: a write to a scratch path outside the workdir;
- pipelines are not split, so a read as a later pipe stage passes; read-only
  `sed`/`awk` pass;
- `ssh host 'cmd'` bodies are checked recursively.

**Output hygiene.** Output is terminal-clean — ANSI escapes and progress
redraws stripped, repeated lines collapsed with a count; pipe through
`od -c`/`xxd` for exact bytes. A non-zero exit returns as an error carrying the
output. Distinct commands run concurrently; an identical command in the same
directory is rejected while it runs. Children run in their own process group,
registered via `register_child`, so SIGTERM/stdio-EOF cleanup kills them.

---

## `workdir` — switch working directory

**Modules:** `fs/workdir.rs`, `SessionWorkdir` in `server.rs`

Switches the working directory for later calls (`path`), or reverts to the
session root (`reset: true`). Every tool resolves relative paths against it.
Returns a structured `WorkdirResult` (set / reset / get). A remote `ssh://`
workdir is allowed — it makes relative paths resolve remotely and disables
`shell`. Don't call it just to check the directory.

---

## Resources & subscriptions

**Modules:** `fs/background.rs`, handlers in `server.rs`

- `octofs://jobs/<id>` — one resource per background job: status plus output
  tail; output is also captured durably from process start, not only after
  promotion.
- `subscribe` / `listen` / `unsubscribe` — completion notifications for job
  resources, delivered on the client's subscription stream when present.
