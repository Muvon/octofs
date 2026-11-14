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

**Modules:** `fs/core.rs`, `fs/file_ops.rs`, `fs/directory.rs`, `fs/search.rs`,
`fs/delta.rs`, `fs/structure.rs`, `fs/ast.rs`; parsing lives in
`src/structural/` (`mod.rs`, `signature_extractor.rs`, `ast_search.rs`, `languages/`)

One tool, several read modes, selected by the parameters:

- **File read** (`path`) — lines render as `N:hh|content`, where `N:hh` is the
  line id edit tools target (see [Line ids](architecture.md#line-ids)). Ranged
  reads (`start`/`end`, negative counts from EOF) always return the requested
  lines.
- **Image read** (a `path` ending in `png`, `jpg`/`jpeg`, `gif` or `webp`) — the file
  comes back as an MCP image block, base64 with its MIME type, up to 5 MB. The
  leading bytes must match the extension or the call fails; `start`/`end` and
  `content` are not applied.
- **Directory listing** (a directory `path`) — recursive and gitignore-aware,
  one `path\tNL\t~Nt` (lines, ~tokens) entry per file.
- **Content search** (`content`, optionally `regex: true`, with `context` lines
  around each match) — literal or regex matching over a file or tree, bounded
  by `pattern` globs. In the default line mode, matches in a file with a
  grammar are preceded by their enclosing symbol (e.g. `[impl Store › fn
  flush]`), once per symbol change; the compact modes (`c`, `l`, `o`), inverted
  searches and files without a grammar render without them
  (`fs/search.rs`, `render_file_matches_with_breadcrumbs`).
- **Structural outline** (`flags: "s"`) — the declarations of a file, or of
  every grammar-supported file under a directory, with their bodies elided.
  Each rendered line keeps its `N:hh` id, so the outline feeds `batch_edit`
  like any other view. Markdown files yield their headings. A directory
  outline honours `pattern`, `max_depth` and `include_hidden`. Rejected
  together with `content`, `start`/`end` and the other flags.
- **AST search** (`ast: true`, with the ast-grep pattern in `content`) — the
  pattern is matched against the syntax tree instead of the text: `$NAME`
  matches one node, `$$$ARGS` a sequence, `$_` any single node. The pattern
  must be complete, valid code in the target language (a Python pattern may
  omit the body only together with the colon: `def $F($$$A):` without a body
  matches nothing, `def $F($$$A)` matches). Works on a file or a directory, locally or over `ssh://`, and
  honours `pattern`, `max_depth`, `include_hidden`, `context` and the `c`/`l`
  flags. Rejected together with `regex`, `start`/`end` and the
  `s`/`v`/`o` flags. CSS, JSON and Markdown files are not AST-searchable.
  Zero matches return a diagnostic naming the parsed kind, the
  metavariables and the plain-text fallback.

The modes are mutually exclusive: `view_existing_path` (`fs/core.rs`)
dispatches `ast` → outline → listing/search and each mode bails on
the other modes' parameters before dispatch. The two structural modes split
pure parsing from I/O — `src/structural/` takes content in and returns
structured items without touching the filesystem, while `fs/structure.rs`
and `fs/ast.rs` own the walk, the reads and the rendering.
Structural reads never register in the delta/view cache. Navigation by symbol
name is the separate [`symbol`](#symbol--navigate-code-from-a-symbol-name) tool.

**Supported languages.** The outline covers Rust, Python, JavaScript,
TypeScript, Go, Java, C/C++, PHP, Bash, Ruby, Lua, JSON, Swift, Svelte, CSS,
Elixir and Markdown (`src/structural/languages/`). The `symbol` tool's definition and
references cover the same set except CSS and JSON; AST search covers the same set except CSS, JSON
and Markdown, and searches a Svelte file through its `<script>` blocks.

**Delta views.** Re-viewing a *whole* file returns only the hunks changed since
the last full view of that file in the same session (or an unchanged marker).
The `ViewCache` (`fs/delta.rs`) stores the last content the model saw; a
Myers line diff renders changed hunks with fresh ids. Ranged views, searches
and the structural read modes bypass the cache by design — only whole-file
views update it. Octofs's own writes keep the cache truthful via
`delta::note_write` / `note_create`, so an edit followed by a whole-file view
does not replay the file.

Deltas are opt-in per client: a session gets a `ViewCache` only when its client
sends `context_epoch` (a non-negative integer) in `experimental.session` on every
request — the 2026-07-28 per-request capabilities in `_meta`. The client bumps the
epoch whenever it compacts or rewrites content the model already received; a
changed epoch clears the session's cache, so the next whole-file view is served in
full. Clients that send no epoch (no way to know what the model still holds) are
always served whole files.

**Context-economy contract** (encoded in the tool description): reuse returned
content, read a complete block in one call, never re-read overlapping ranges;
re-read only when the file may have changed or output was truncated. Oversized
output is truncated to a token budget (`utils/truncation.rs`) with the cut
point reported.

---

## `symbol` — navigate code from a symbol name

**Modules:** `fs/symbols.rs` (validation, `definition`/`references`), `fs/graph.rs` (graph
relations); pure logic in `src/structural/symbols.rs` and `src/structural/graph/`

`relation` selects what a call returns; `name` is the symbol (`Owner::name` also matches) and `path`
scopes it:

| `relation` | Returns | Parameters |
|---|---|---|
| `definition` (default) | Source of the symbol, by name | `name` (`*` wildcards), `path`, `pattern`, `max_depth`, `include_hidden`, `flags` |
| `references` | Every use of the name, grouped by enclosing symbol | as `definition`, plus `context` |
| `callers` / `callees` | Calls into / out of the symbol | `name`, `path`, `depth` |
| `supertypes` / `subtypes` | Extends and implements, outgoing / incoming | `name`, `path`, `depth` |
| `imports` / `imported_by` | Imports of / importers of the file in `path` | `path` (a file; `name` is rejected), `depth` |
| `path` | Chains from `name` to `to` over calls, extends and implements (not imports) | `name`, `to` (required), `path`, `depth` |

Parameter combinations that make no sense (`to` without `relation: "path"`, `depth` or `flags` on the
wrong relation, `*` in a graph relation, `name` for `imports`) fail with a guided error.

**`definition` and `references`** are the text-level lookup: definitions of a name in a file or under
a directory (default the workdir), locally or over `ssh://`. `references` is the same lookup for
uses. `flags: "c"`/`"l"` give per-file counts and matching paths; `o`, `v`, `s` and `r` are
rejected. An exact name renders the whole definition when it is short enough, longer ones and
wildcard matches a bounded preview. Every source line renders as `N:hh|content`, so the ids feed
`batch_edit`. Markdown files yield their headings, Svelte files the symbols of their `<script>`
blocks; CSS and JSON name no navigable symbol (a single file says so, a directory skips them).
Symbol reads never register in the delta/view cache.

**Graph relations** are answered from the code graph in `src/structural/graph/`: nodes are files,
symbols and external imports; edges are `Contains`, `Imports`, `Calls`, `Extends` and `Implements`,
resolved by name in a fixed order (same file, imported files, unique project-wide match), each with a
confidence. Mapping: `callers` = incoming `Calls`; `callees` = outgoing `Calls`; `supertypes` =
outgoing `Extends`+`Implements`; `subtypes` = incoming; `imports` = outgoing `Imports` of a file
node; `imported_by` = incoming; `path` = one multi-source search over `Calls`, `Extends` and `Implements` between the definitions of `name` and `to`.
They are resolved by name and imports, not types, and are local only (an `ssh://` path or remote
workdir fails with guidance).

- **Code index queries.** `callers`, `callees`, `supertypes`, `subtypes`, `imports` and `imported_by` are
  answered from the per-workdir code index (`fs/code_index.rs`): a Bloom filter of the identifiers of every
  file, plus a parsed-file cache under a 160 MB ceiling shared by all indexes (at most 2 workdirs; idle ones
  are dropped after 10 minutes). The ceiling counts the parsed data, what each file's imports resolved to
  (kept per file, dropped when the file or the set of files changes), the path table and the cached
  lookups. Each call refreshes the index from file stamps, so a file changed by any program is seen by
  the next call. Lookups of which
  files spell a name, which have an import or inheritance line, and finished complete answers are cached
  until a file changes. Only the files a rule picks are parsed, level by level for `depth`, on a pool of at
  most 6 threads, at most `MAX_PARSED_FILES_PER_QUERY` (600) files and for at most `QUERY_TIME_BUDGET_MS`
  (800 ms, counted after the refresh and the one-time filter build of a cold process, which the first symbol
  query of a process pays once: 1.1 s on the 67,000-file Linux tree on an idle 12-core machine; at least one chunk of
  files is always parsed so a repeat gets further) per query (cached files are free). Measured on 6 threads:
  about 10 MB of source per second, 72 % of it tree-sitter parsing and 25 % symbol extraction, so a query
  examines roughly 150 to 400 files. The closing line counts over the whole query (every name and level
  followed): its candidates and the files examined so far. Graph parsing skips files over
  `MAX_GRAPH_FILE_BYTES` (1 MB) and names them in a closing note; the view commands keep their 5 MB limit.
  - definitions of `name`: the files that look like declaring it first (the files under `path` when given),
    then the other files spelling it; `Owner::name` needs both names. A name spelled in more than
    `MAX_FILES_SPELLING_TO_TRACE` (3000) files with no `path` or `Owner::name` is not traced: the reply
    says "about N files" (counted from the filters, nothing is read) and to give `path` or `Owner::name`.
  - `callers`: the files spelling the name, the importers of the definition file first; with several
    definitions only the definition files and their importers.
  - `callees`, `supertypes`: the definition file, then each followed name in the files the definition file
    imports, and repository-wide only when at most `MAX_FILES_PER_FOLLOWED_NAME` (200) files spell it
    (`get`, `push` are not searched repository-wide, so a `(guess)` hop to them can be missing).
  - `subtypes`, `imported_by`: files with a line that names the type or the file and may be an inheritance
    or import line in their language (`Language::line_may_inherit` / `line_may_import`: the marker keyword
    of the language, a line holding only names, and for quoted paths the name before the closing quote).
    `imported_by` builds no graph: each candidate's imports are resolved through the file registry once,
    kept with the parsed file, and a candidate stays when one of them names the file (or a package that
    holds it).
  A list shows at most `MAX_LISTED_RESULTS` (100) results, most certain first, then a closing line
  counts the rest.
  When the cap or the time budget cuts the candidates, the answer comes from the files examined and a closing line says
  "N candidate files, M examined; repeat the query to examine the rest". Parsed files are kept and do not
  count against the cap, so repeating the same query examines further files until the answer is complete
  and the line is gone; partial answers are never cached. Narrowing with `path` or `Owner::name` ends it
  sooner. `path` always uses the whole-repository graph (it needs global
  reachability), so its first call on a large repository takes seconds and its graph stays in memory
  (about 1.5 GB on the VS Code repository). A name with more than eight definitions is listed instead of
  followed, from a declarations-only scan. Re-exports through barrel files are not followed by
  `imported_by`. Local `definition` and `references` of an exact name over a directory parse only the
  files the index says spell the name.
- **Packages.** Go imports, Java wildcard imports and Swift module imports name a package of several files:
  one `Imports` edge per importing file targets the package's canonical (alphabetically first) file, and
  `imported_by` of any file of the package answers through it; calls through the import resolve against
  every file of the package.
- **Scope.** The graph covers every supported, gitignore-visible file under the session
  workdir, because name resolution needs the whole repository. `path` only selects which
  definitions of `name` start the query; a `path` outside the workdir fails and says to switch
  workdir. When `name` is ambiguous the output has one section per start definition.
- **Depth.** `depth` defaults to 1 (`path`: 5) and is capped at `MAX_GRAPH_DEPTH` (5).
- **Cache.** `fs/graph.rs` keeps in-memory graphs for the last `MAX_CACHED_WORKDIRS` (4) workdir roots,
  least recently used dropped first. Every call re-walks the tree and hashes (relative path, size,
  mtime, and on unix ctime and inode) of each file, without reading any; a graph is rebuilt only when
  that stamp changes. Concurrent calls for one root wait for a single build. Nothing is stored on disk.
- **Limits.** The cached whole-repository graph (`path`) holds at most
  `MAX_GRAPH_FILES` (20,000) files and `MAX_GRAPH_SOURCE_BYTES` (256 MB);
  the rest are counted in a closing note. A traversal stops at `MAX_REPORTED_HOPS` and a path search at
  `MAX_PATHS` paths or its work limit; each says so in the output instead of reporting an empty result.
- **Skipped files.** Files over `MAX_GRAPH_FILE_BYTES` (1 MB: a syntax tree weighs many times the text and one
  parse cannot be interrupted; `definition`, `references` and `view` keep the 5 MB `MAX_VIEW_FILE_BYTES`), unreadable files and files that fail to
  parse are left out and named in a closing note. Markdown and JSON contribute nothing to the graph and
  are not handed to it.
- **Output.** Hops are grouped under `depth N:` lines as `path:line  kind  Owner::name`; `(guess)`
  marks a hop matched only by a repository-wide name by name alone (repository-wide, ambiguous or unknown receiver); a call outside any function lists the file as `path:line  file`; unresolved imports read `external: <import>`; `path` prints
  each chain as `a -calls-> b`. Output stops under the response budget with a truncation notice.
  Zero results say what was looked for and suggest `relation: "references"`.

---

## `text_editor` — create, str_replace, delete, undo

**Modules:** `fs/core.rs`, `fs/text_editing.rs`, `fs/file_ops.rs`

A multi-command tool (`command` + operands):

- **create** — new file only; fails if it exists, parent directories are made.
- **str_replace** — raw file text (real newlines, no `N:hh|` prefixes);
  `old_text` must match exactly once, or set `replace_all: true`. Multiple
  matches return an error listing their line ids, no match the closest matches, for `batch_edit`.
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

- misuse: `sed -i`, content writes into the workdir (`cat >`, `echo >`,
  `tee`), a read program (cat/head/tail/less/more/grep/egrep/fgrep/rg/find/ls) starting any command — alone, in a
  chain or as a pipeline head — file reads belong to `view`. `--hint-mode hard`
  (default) rejects the whole command unexecuted; `--hint-mode soft` runs it and
  appends the guidance as a warning. Hard suits models that follow tool steering:
  a measured win on GLM-5.3, and gpt-6.1-sol rarely hits it (1 of 150 shell
  calls over six tasks). Try soft for a model that keeps re-sending blocked
  commands — watch the rejection count;
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
