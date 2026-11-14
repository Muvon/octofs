# Code exploration

Octofs answers questions about code straight from the files on disk: what a
file declares, where a symbol is defined and used, who calls a function, which
types extend which, which files import which. There is no indexing command and
no database. The only index is a table kept in memory, built on the first query
and refreshed from file metadata on every call, so a file edited by any program
shows up in the next query.

Measured (figures from the [index description](#how-the-index-behaves)): the
first symbol query of a process builds the name table in 1.1 s on the 67,000-file
Linux tree on an idle 12-core machine. After that, a query parses about 10 MB of
source per second on 6 threads and stops after 800 ms, which is roughly 150 to
400 files. When a name is spelled in more files than that, the answer covers the
files examined so far and says how to continue.

Two tools carry this:

- [`view`](tools.md#view--read-files-list-directories-search-content) has two
  code modes: an outline (`flags: "s"`) and syntax-tree search (`ast: true`).
- [`symbol`](tools.md#symbol--navigate-code-from-a-symbol-name) starts from a
  name and returns definitions, references, callers, callees, supertypes,
  subtypes, imports, importers, or the chain between two symbols.

For the filesystem half (read, search, edit, copy, shell, working directory,
`ssh://` paths) see [tools.md](tools.md) and [remote.md](remote.md). Module
layout is in [architecture.md](architecture.md).

## Which call answers which question

| Question | Call |
|---|---|
| What does this file or directory declare? | `view` with `flags: "s"` |
| Which code has this syntactic shape (every `.unwrap()`, every function)? | `view` with `ast: true` |
| Where is `flush` defined? | `symbol` with `name` |
| Where is `flush` used? | `symbol` with `relation: "references"` |
| Who calls `flush`? What does it call? | `relation: "callers"` / `"callees"` |
| What does `Square` extend or implement? Who extends `Shape`? | `relation: "supertypes"` / `"subtypes"` |
| What does this file import? Who imports it? | `relation: "imports"` / `"imported_by"` |
| How does `main` reach `inner`? | `relation: "path"` with `to` |
| Where does this text appear? | `view` with `content` (plain text, not code-aware) |

## Languages

Outline: Rust, Python, JavaScript, TypeScript, Go, Java, C/C++, PHP, Bash, Ruby,
Lua, JSON, Swift, Svelte, CSS, Elixir, Markdown (`src/structural/languages/`,
`SUPPORTED_LANGUAGES` in `src/structural/mod.rs`). The extension picks the
grammar; a file with another extension fails with this list.

Support differs by feature:

| Feature | Covers | Does not cover |
|---|---|---|
| Outline | all of the above; Markdown yields its headings | |
| `symbol` `definition` / `references` | all except CSS and JSON; Markdown yields headings, Svelte the symbols of its `<script>` blocks | CSS and JSON name no navigable symbol: a single file fails with that guidance, a directory skips them |
| AST search | all except CSS, JSON and Markdown; a Svelte file is searched through its `<script>` blocks | CSS, JSON, Markdown |
| Graph relations (`callers`, `callees`, `supertypes`, `subtypes`, `imports`, `imported_by`, `path`) | the languages whose definition in `src/structural/languages/` extracts the relation | Markdown and JSON contribute nothing to the graph. Bash and Lua define no extends or implements extraction, so `supertypes` and `subtypes` find nothing in them. CSS defines no calls or type relations |

## Outline

`view` with `flags: "s"` on a file or a directory returns the declarations with
their bodies elided. Each line keeps its `N:hh` id (position plus a two-character
hash, see [architecture.md](architecture.md#line-ids)), so an outline line can be
passed to `batch_edit` as an edit target. A directory outline honours `pattern`,
`max_depth` and `include_hidden` and works over `ssh://`. It cannot be combined
with `content`, `start`/`end`, `ranges` or other flags.

Input, for a file whose lines 1 to 4 are `mod foo {`, `fn bar() {}`, `}`,
`struct Baz;`:

```json
{"path": "src/lib.rs", "flags": "s"}
```

Output shape: a header with the path and line count, then one line per
declaration. Line 3, the closing brace, is body and does not appear. The hash
digits depend on the line text and are written `hh` here.

```
src/lib.rs	4
1:hh|mod foo {
2:hh|	fn bar() {}
4:hh|struct Baz;
```

## AST search

`view` with `ast: true` treats `content` as an ast-grep pattern and matches it
against the syntax tree instead of the text. `$NAME` matches one node, `$$$ARGS`
a sequence, `$_` any single node. The pattern must be complete, valid code in the
target language. A Python pattern may omit the body only together with the
colon: `def $F($$$A)` matches, `def $F($$$A):` without a body matches nothing.

It works on a file or a directory, locally or over `ssh://`, and honours
`pattern`, `max_depth`, `include_hidden`, `context` and the `c` and `l` flags.
It is rejected together with `regex`, `start`/`end`/`ranges` and the `s`, `v` and
`o` flags. Zero matches return a diagnostic that names the parsed kind, the
metavariables and the plain-text fallback.

Example, over a file holding `fn a() {}` and `fn b(x: u8, y: u8) {}`:

```json
{"path": "src/lib.rs", "ast": true, "content": "fn $F($$$A) {}"}
```

Both functions come back, each source line prefixed with its `N:hh` id:

```
1:hh|fn a() {}
2:hh|fn b(x: u8, y: u8) {}
```

A match spans the whole node, so a multi-line function renders all its lines;
very long spans are cut with an elision marker.

## Symbol lookups

`symbol` takes a `relation` and, depending on it, a `name`, a `path`, a `to` and
a `depth`. Combinations that make no sense (`to` without `relation: "path"`,
`depth` or `flags` on the wrong relation, `*` in a graph relation, `name` for
`imports`) fail with a guided error.

| `relation` | Returns | Parameters |
|---|---|---|
| `definition` (default) | Source of the symbol, by name | `name` (`*` wildcards, `Owner::name`), `path`, `pattern`, `max_depth`, `include_hidden`, `flags` |
| `references` | Every use of the name, grouped by enclosing symbol | as `definition`, plus `context` |
| `callers` / `callees` | Calls into / out of the symbol | `name`, `path`, `depth` |
| `supertypes` / `subtypes` | Extends and implements, outgoing / incoming | `name`, `path`, `depth` |
| `imports` / `imported_by` | Imports of / importers of the file in `path` | `path` (a file; `name` is rejected), `depth` |
| `path` | Chains from `name` to `to` over calls, extends and implements (not imports) | `name`, `to` (required), `path`, `depth` |

The examples below use one small Rust project. `src/main.rs`:

```rust
mod util;
use crate::util::helper;

fn main() {
	helper();
}
```

`src/util.rs`:

```rust
pub fn helper() {
	inner();
}

fn inner() {}
```

`src/model.rs`:

```rust
pub trait Shape {
	fn area(&self) -> f64;
}

pub struct Square;

impl Shape for Square {
	fn area(&self) -> f64 {
		1.0
	}
}
```

### Definition

Local or over `ssh://`. An exact name renders the whole definition when it is
short enough (32 lines or fewer); a longer one and wildcard matches render a
bounded preview that names the omitted range. The line before the source is the
enclosing symbol. Every source line carries its `N:hh` id, so the result feeds
`batch_edit`. `flags: "c"` gives the match count per file and `"l"` the matching
paths; `o`, `v`, `s` and `r` are rejected.

```json
{"name": "flush", "path": "src/store.rs"}
```

For a file where `flush` is a method of `impl Store`:

```
[impl Store]
6:hh|	pub fn flush(&mut self) {
7:hh|		self.data.clear();
8:hh|	}
```

### References

Local or over `ssh://`. Every use of the name, grouped under the symbol that
contains it; a definition site is tagged `[def]`. `context` adds lines around
each use.

```json
{"name": "flush", "path": "src/store.rs", "relation": "references"}
```

```
[impl Store › fn flush] [def]
2:hh|	pub fn flush(&mut self) {
[impl Store › fn drain]
6:hh|		self.flush();
[fn main]
11:hh|	store.flush();
```

### Callers and callees

Graph relations print one hop per line as `path:line  kind  Owner::name`, grouped
under `depth N:` headers. `depth` defaults to 1 and the maximum is 5.

```json
{"relation": "callers", "name": "inner", "depth": 2}
```

```
depth 1:
  src/util.rs:1  function  helper
depth 2:
  src/main.rs:4  function  main
```

`callees` runs the other way: `{"relation": "callees", "name": "main", "depth": 2}`
lists `src/util.rs:1  function  helper` under `depth 1:` and
`src/util.rs:5  function  inner` under `depth 2:`.

### Supertypes and subtypes

`supertypes` follows outgoing extends and implements edges, `subtypes` the
incoming ones.

```json
{"relation": "supertypes", "name": "Square"}
```

The result contains the row `src/model.rs:1  trait  Shape`. The reverse,
`{"relation": "subtypes", "name": "Shape"}`, lists `Square` in the same row form.

### Imports and imported by

Both take a file in `path` and no `name`. A directory is rejected as "not a
source file".

```json
{"relation": "imports", "path": "src/main.rs"}
```

The result lists `src/util.rs  file`. An import that resolves to no file of the
repository (a dependency) reads `external: <import>`, for example
`external: serde` for `use serde::Serialize;`.

```json
{"relation": "imported_by", "path": "src/util.rs"}
```

The result lists `src/main.rs  file`. Go, Java wildcard and Swift module imports
name a package of several files: the import points at the package's first file in
alphabetical order, and `imported_by` of any file of the package answers through
it. Re-exports through barrel files are not followed by `imported_by`.

### Path between two symbols

`path` searches calls, extends and implements between the definitions of `name`
and `to` and prints each chain on one line. Imports are not followed. `depth`
defaults to 5.

```json
{"relation": "path", "name": "main", "to": "inner"}
```

```
src/main.rs:4 main -calls-> src/util.rs:1 helper -calls-> src/util.rs:5 inner
```

A search that is too shallow to connect the two, or that hits its hop or work
limit, says so instead of reporting that no path exists.

### Reading the output

- `(guess)` marks a hop matched by name alone: repository-wide, ambiguous or
  unknown receiver. Hops with stronger evidence are unmarked and listed first.
- A call outside any function lists the file as `path:line  file`.
- A list shows at most 100 results, most certain first, then a closing line
  counts the rest.
- Zero results explain what was looked for and suggest `relation: "references"`.
- Relations are resolved by name and imports, not types. Same-name symbols can be
  confused and dynamic calls missed.

## How the index behaves

Everything is computed from the files and held in memory. Nothing is written to
disk.

- **What is indexed.** For each workdir, a compact table of which files spell
  which identifiers (a Bloom filter per file) and a cache of parsed files, each
  keyed by its file stamp. What each parsed file's imports resolved to is kept
  with it.
- **Refresh.** Every call re-walks the tree and compares file stamps (relative
  path, size, modification time and, on Unix, change time and inode) without
  reading any file. A file changed by any program is seen by the next call.
- **Build.** The name table is built on the first symbol query of a process
  (1.1 s on the 67,000-file Linux tree on an idle 12-core machine). The build
  does not count against the query budget.
- **Parsing.** Only the files a query can need are parsed, level by level for
  `depth`, on a pool of at most 6 threads. A cap of 600 files and 800 ms applies
  per query; cached files are free, and at least one chunk of files is always
  parsed so a repeat gets further. Measured on 6 threads: about 10 MB of source
  per second, 72 % of it tree-sitter parsing and 25 % symbol extraction, so a
  query examines roughly 150 to 400 files before the clock stops it.
- **Which files a relation reads.**
  - Definitions of `name`: files that look like declaring it first (the files
    under `path` when given), then the others that spell it. `Owner::name` needs
    both names.
  - `callers`: files spelling the name, the importers of the definition file
    first. With several definitions, only the definition files and their
    importers.
  - `callees`, `supertypes`: the definition file, then each followed name in the
    files it imports, and repository-wide only when at most 200 files spell it.
    `get` and `push` are not searched repository-wide, so a `(guess)` hop to
    such a name can be missing.
  - `subtypes`, `imported_by`: files with a line that names the type or file and
    may be an inheritance or import line in their language. `imported_by` builds
    no graph.
  - `path`: always the whole-repository graph, because it needs global
    reachability. Its first call on a large repository takes seconds, and the
    graph stays in memory (about 1.5 GB on the VS Code repository). The last 4
    workdirs are kept, least recently used dropped first.
- **Memory.** One 160 MB ceiling is shared by all indexes. It counts the parsed
  data, the resolved imports, the path table and the cached lookups. At most 2
  workdirs are indexed at once and idle ones are dropped after 10 minutes.
- **Partial answers.** When a name is spelled in more files than the cap or the
  clock allows, the answer comes from the files examined and a closing line says
  "N candidate files, M examined; repeat the query to examine the rest". Parsed
  files are kept and do not count against the cap again, so repeating the same
  query goes further until the answer is complete and the line disappears.
  Partial answers are never cached. Narrowing with `path` or `Owner::name` ends
  it sooner.
- **Definition and references** of an exact name over a directory parse only the
  files the index says spell the name.

## Limits

- Graph relations are local only. An `ssh://` path or remote workdir fails with
  guidance. `definition`, `references`, the outline and AST search do work over
  `ssh://`.
- The graph scope is always the whole session workdir, because name resolution
  needs the whole repository. `path` only picks which definitions of `name` start
  the query, and must be inside the workdir.
- Files over 1 MB are skipped for graph relations (a syntax tree weighs many
  times the text and one parse cannot be interrupted). Unreadable files and files
  that fail to parse are skipped too. All are named in a closing note.
  `definition`, `references` and `view` keep the 5 MB limit.
- A name spelled in more than 3000 files, with no `path` or `Owner::name`, is not
  traced. The reply gives the file count and asks for `path` or `Owner::name`.
  Common names need one of them.
- The whole-repository graph used by `path` holds at most 20,000 files and
  256 MB of source. The rest are counted in a closing note.
- A traversal stops at its hop limit and a path search at its path or work limit;
  each says so in the output.
- A name with more than eight definitions is listed instead of followed, from a
  declarations-only scan.
- Output stops under the response budget with a truncation notice.
- Results come from names and imports, not from a type checker. Treat `(guess)`
  rows as leads to confirm with `view`.
