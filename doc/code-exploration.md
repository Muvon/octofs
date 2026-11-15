# Code exploration

Octofs answers questions about code straight from the files on disk: what a
file declares, where a symbol is defined and used, who calls a function, which
types extend which, which files import which. There is no indexing command and
no database. The only index is a table kept in memory, built on the first query
and refreshed from file metadata on every call, so a file edited by any program
shows up in the next query.

Measured (details in [Measurements](#measurements); benchmark machine: a 12-core
Linux box, release build): the first symbol query of a process builds the index
once. After that, a query stops after 800 ms or 600 parsed files, whichever comes
first. A repeated query whose answer is complete took 3 to 224 ms (median) in the
`path` benchmarks, from tokio to the Linux tree. When a name is spelled in more
files than the cap allows, the answer covers the files examined so far and says
how to continue.

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
grammar; a file with another extension fails with this list. Extensions:

- Rust `rs`; Python `py`; Go `go`; Java `java`; PHP `php`; Ruby `rb`; Lua `lua`;
  Swift `swift`; Svelte `svelte`; JSON `json`; Markdown `md`, `markdown`.
- JavaScript `js`, `mjs`, `jsx`; TypeScript `ts`; TSX `tsx`; Elixir `ex`, `exs`.
- Bash `sh`, `bash`; CSS `css`, `scss`, `sass`.
- C and C++ sources `c`, `cc`, `cpp`, `cxx`, `c++`, `cppm`, `ixx`, `mxx`, `ccm`,
  `cxxm`; headers `h`, `hpp`, `hxx`, `hh`, `h++`, `ipp`, `tpp`, `inl`.

Support differs by feature. Outline works for all of the above. AST search covers
all except CSS, JSON and Markdown (a Svelte file is searched through its
`<script>` blocks). The table below is about the `symbol` tool. An empty cell
says why.

| Language | Definitions, references | Calls (callers, callees) | Imports | Supertypes, subtypes | Constructors | Package scope | Re-exports followed |
|---|---|---|---|---|---|---|---|
| Rust | yes | yes, including calls inside macro arguments and calls written with a module path (`crate::a::f()`) | `use`, `mod`, `#[path]`, `crate::`, `super::`, `self::` (globs included) | yes | none: `Foo::new` is an ordinary function | none: modules, not packages | `pub use` |
| Python | yes | yes | `import`, `from ... import`; absolute imports looked up from the file's directory and each directory above it | yes | none defined | none | an `__init__.py` passes on what it imports; any other file only through `import *` |
| JavaScript | yes | yes | `import`, `require('./x')`, dynamic `import('./x')` | yes | `new Foo` reaches the class | none | `export ... from` (named, aliased, `export *`) |
| TypeScript, TSX | yes (TSX files use the TSX grammar) | yes | as JavaScript | yes | `new Foo` reaches the class | none | as JavaScript |
| Go | yes | yes | `import` | yes: embedded struct fields and interfaces | none: Go has no constructor syntax | by directory | none |
| Java | yes | yes, including calls written with a package path (`com.acme.Util.run()`) | `import`, including static, nested-class and wildcard | yes | constructors; `new Foo()` reaches the class | by declared `package` | none |
| C, C++ | yes | yes | `#include` | yes | constructors | none: no package concept | a header that includes another header (a source file passes nothing on) |
| PHP | yes | yes, including static calls written with a namespace path (`\App\Models\User::find()`) | `use`, `require`, `include` | yes | `new Foo` | by declared namespace | none |
| Ruby | yes | yes; a bare name is a call only when the method is declared in the same file | `require`, `require_relative`, `load` | yes: superclass, `include`, `extend`, `prepend` | `Foo.new` | none | none |
| Swift | yes | yes | `import` | yes | constructors | by directory | `@_exported import` |
| Elixir | yes | yes, including calls written with a full module name (`MyApp.Repo.get(id)`); `defdelegate` counts as a call | `alias`, `import`, `require`, `use` | `defimpl` as an implementation | none | none | none |
| Bash | yes | yes | `source` and `.` | none: Bash has no types | none | none | none |
| Lua | yes | yes | `require` | none: Lua has no declared inheritance | none | none | none |
| CSS | no symbol to navigate | none: no calls | `@import` | none | none | none | none |
| Svelte | the symbols of its `<script>` blocks | through the script, as JavaScript or TypeScript | through the script | through the script | through the script | none | through the script |
| Markdown | headings only | none | none | none | none | none | none |
| JSON | no symbol to navigate | none | none | none | none | none | none |

"Package scope" means that files of one package see each other's names without an
import, so a call to such a name resolves to the other file: Go and Swift treat
the files of one directory as a package, Java and PHP use the declared package or
namespace. A Java or PHP file with no `package` or namespace line belongs to the
unnamed package of its directory.

In Swift, a call that names a type (`Widget()`) reaches the `init` of that type.

A call that names its module or class by path resolves like an import of that
path, with no import line in the calling file, and follows the same
[re-exports](#re-exports):

- Rust: `crate::runtime::spawn(f)`, `super::f()`, `self::f()` and `a::b::f()`.
  Every segment is lowercase; a path with a capitalised segment (`Type::f()`,
  `a::Type::f()`) names a type and is not read as a module path. Paths that start
  with `std`, `core` or `alloc` are the standard library, not a project file.
- Java: `com.acme.Util.run()`, lowercase package segments followed by a
  capitalised class.
- PHP: `\App\Models\User::find()`, a static call on a class written with its
  namespace. A function called by namespace path (`\App\helper()`) is not
  resolved this way.
- Elixir: `MyApp.Repo.get(id)`, a module name of two or more segments.

In Python the dotted path of a call (`pkg.mod.helper()`) is the one its `import`
line names, so the import already resolves it.

In Rust, the items inside a brace macro in item position, such as
`cfg_rt! { ... }`, count as items of the surrounding module, impl or trait when
the macro body parses without errors and holds only items. The outline and every
`symbol` relation see them. Any other macro body stays opaque.

Three resolution rules are worth knowing:

- Go import paths. A path whose first segment has a dot (`example.com/app/store`)
  is matched to a local directory by path suffix. A path without a dot is looked
  up under the directory of each `go.mod` in the workdir (`module myapp` makes
  `myapp/internal/log` the directory `internal/log` next to the `go.mod`);
  otherwise it is taken as the standard library and matches only `src/<path>` or
  `<path>` at the root.
- Python imports found only in a directory between the file and the project root
  are shown as guesses, because a standard-library name can collide with a local
  file there.
- Rust workspace crates are matched by directory name and shown as a guess.

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
defaults to 5. It runs on the same in-memory index as the other relations and
searches from both ends at once, one level at a time, parsing only the files it
needs within the 800 ms and 600-file budget of the call.

```json
{"relation": "path", "name": "main", "to": "inner"}
```

```
src/main.rs:4 main -calls-> src/util.rs:1 helper -calls-> src/util.rs:5 inner
```

"No path ... within depth N" is printed only when the search finished. A search
that was cut by the budget or by its work limit says so, and repeating the query
continues it. Timings per repository are in [Measurements](#measurements).

### Re-exports

A name that a file only re-exports leads to the file that declares it. A file
that declares a name and also re-exports the same name (a stub next to a
`pub use` under the opposite `cfg` attribute) leads to both, and a call to that
name is listed under each. This is followed for `callers`, `callees` and `path`,
up to 3 hops:

- Python: names that an `__init__.py` imports, and `import *`.
- JavaScript and TypeScript: `export ... from` (named, aliased, and `export *`).
- Rust: `pub use` (a `use` line that starts with `pub`).
- C and C++: a header that includes another header passes it on. Only headers do:
  no file includes a source file, so the includes of a source file reach nothing
  further.
- Swift: `@_exported import`.

Re-exports in the other languages are not followed.

### Reading the output

- `(guess)` marks a hop matched by name alone: repository-wide, ambiguous or
  unknown receiver. Hops with stronger evidence are unmarked and listed first.
- A call to a name that has several definitions in the same file (for example a C
  function defined under both `#ifdef` and `#else`) leads to each of them, marked
  `(guess)`.
- A call on a receiver the tool cannot identify, such as `getRepo().save()`, is
  resolved by name only and shown as `(guess)`.
- An absolute Python import found only in a directory between the file and the
  project root, and a Rust import of a workspace crate matched by directory
  name, are shown as guesses.
- A call outside any function lists the file as `path:line  file`.
- A list shows at most 100 results, most certain first, then a closing line
  counts the rest.
- Zero results explain what was looked for and suggest `relation: "references"`.
- Relations are resolved by name and imports, not types. Same-name symbols can be
  confused and dynamic calls missed.
- C and C++: code under preprocessor shapes the parser cannot read can hide a
  call, so an empty or "No path" answer for C or C++ covers only what the parser
  recognised.

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
- **Build.** The name table is built once, on the first symbol query of a process.
  The build is counted after the refresh and does not count against the query
  budget.
- **Parsing.** Only the files a query can need are parsed, level by level for
  `depth`, on a pool of at most 6 threads. A cap of 600 files and 800 ms applies
  per query; cached files are free, and at least one chunk of files is always
  parsed so a repeat gets further.
- **Which files a relation reads.**
  - Definitions of `name`: files that look like declaring it first (the files
    under `path` when given), then the others that spell it. `Owner::name` needs
    both names.
  - `callers`: files spelling the name, the importers of the definition file
    first. With several definitions, only the definition files and their
    importers. The other files of the definition's package are read only when
    they spell the name.
  - `callees`, `supertypes`: the definition file, then each followed name in the
    files it imports, and repository-wide only when at most 200 files spell it.
    `get` and `push` are not searched repository-wide, so a `(guess)` hop to
    such a name can be missing.
  - `subtypes`, `imported_by`: files with a line that names the type or file and
    may be an inheritance or import line in their language. `imported_by` builds
    no graph. The same kind of line scan finds re-exports. These scans run under
    the same 800 ms budget and give a partial answer with the repeat notice.
  - `path`: both ends at once, one level at a time. The forward side follows what
    the bodies of its frontier call, the backward side what spells the names of
    its frontier; each round takes the side whose next level needs fewer unparsed
    files. The search stops when the sides meet, when `depth` is used up, when a
    side has nothing left or when the budget ends. No separate graph is built or
    kept.
- **Memory.** One ceiling of 160 MB of index data is shared by all indexes. At
  most 2 workdirs are indexed at once and idle ones are dropped after 10 minutes.
- **Partial answers.** When a name is spelled in more files than the cap or the
  clock allows, the answer comes from the files examined and a closing line says
  "N candidate files, M examined; repeat the query to examine the rest". Parsed
  files are kept and do not count against the cap again, so repeating the same
  query goes further until the answer is complete and the line disappears.
  Partial answers are never cached. Narrowing with `path` or `Owner::name` ends
  it sooner.
- **Definition and references** of an exact name over a directory parse only the
  files the index says spell the name.

## Measurements

Taken on a 12-core Linux box with a release build, running other jobs at the same
time, so treat them as orders of magnitude. Each entry gives the minimum and the
median over several fresh processes. Times are in milliseconds; memory is peak
resident memory of the process, which is larger than the 160 MB of index data.

`path` queries, first call and repeated call in the same process:

| Repository | From, to | First call | Repeated call | Peak memory (MB) |
|---|---|---|---|---|
| go | `ListenAndServe`, `readRequest` | 200 / 206 | 40 / 44 | 50 / 51 |
| go | `ListenAndServe`, `Errorf` | 319 / 326 | 39 / 42 | 55 / 55 |
| vscode | `createDecorator`, `storeServiceDependency` | 278 / 281 | 72 / 74 | 64 / 65 |
| django | `redirect`, `reverse` | 1124 / 1147 | 32 / 35 | 74 / 75 |
| tokio | `spawn_blocking_inner`, `spawn_thread` | 24 / 30 | 3 / 4 | 15 / 15 |
| cargo | `compile`, `create_bcx` | 76 / 78 | 17 / 19 | 22 / 23 |
| Linux tree | `list_add_tail`, `__list_add_valid` | 1244 / 1253 | 215 / 232 | 158 / 190 |

Sequences of `callers` and `imported_by` queries in one process (first call /
repeated call):

| Repository | Query | First call | Repeated call |
|---|---|---|---|
| vscode | `callers createDecorator` | 845 / 873 | 72 / 179 |
| vscode | `callers dispose` | 77 / 80 | 74 / 79 |
| vscode | `imported_by src/vs/base/common/lifecycle.ts` | 686 / 714 | 1091 / 1132 |
| go | `callers ListenAndServe` | 405 / 419 | 42 / 42 |
| go | `callers Close` | 578 / 595 | 41 / 42 |
| go | `imported_by src/net/http/server.go` | 197 / 208 | 40 / 43 |
| django | `callers get_object_or_404` | 57 / 58 | 34 / 35 |
| django | `callers get` | 127 / 129 | 33 / 37 |
| django | `imported_by django/db/models/query.py` | 155 / 160 | 34 / 35 |
| tokio | `callers spawn_blocking` | 21 / 25 | 3 / 4 |
| tokio | `callers poll` | 62 / 69 | 3 / 3 |
| tokio | `imported_by tokio/src/runtime/mod.rs` | 164 / 166 | 3 / 4 |
| cargo | `callers compile` | 377 / 395 | 18 / 21 |
| cargo | `callers new` | 140 / 149 | 18 / 21 |
| cargo | `imported_by src/util/mod.rs` | 167 / 171 | 18 / 21 |
| Linux tree | `callers kmalloc` | 2069 / 2078 | 1324 / 1370 |
| Linux tree | `callers list_add_tail` | 1155 / 1166 | 210 / 941 |
| Linux tree | `callers init` | 212 / 215 | 212 / 229 |
| Linux tree | `imported_by include/linux/list.h` | 1126 / 1127 | 1016 / 1185 |

Peak memory of those sequences (MB): vscode 239 / 242, go 98 / 100, django 49 / 50,
tokio 37 / 38, cargo 46 / 47, Linux tree 345 / 359. The Linux tree peak ranged
from 280 to 370 MB across runs, depending on machine load.

A repeated query is fast only when its answer is complete. A query that still has
files to examine, such as `callers kmalloc` on the Linux tree, spends its 800 ms
budget again on every repeat, which is why some repeated times above stay high.

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
- C and C++ files whose calls sit under preprocessor shapes the parser cannot read
  can hide a call (see [Reading the output](#reading-the-output)).
- Import resolution does not read manifests: `Cargo.toml`, the `require` lines of
  `go.mod` (only the directory that holds a `go.mod` is used), `composer.json` or
  `pom.xml`. The `paths` aliases of `tsconfig.json` and `jsconfig.json` are not
  read either, so an aliased import (`@/x`) is treated as external.
- Ruby: a call by a bare name to a method defined in a required file is missed,
  because a bare name counts as a call only when the method is declared in the
  same file.
- Elixir: `defdelegate` counts as a call to the delegated function.
- If the index of a workdir would not fit under the 160 MB ceiling, graph
  relations report that, `definition` and `references` still work, and the index
  is built again after a minute or when another workdir releases memory.
- When a definition is found the search stops, and a closing line says how many
  other files spell the name without looking like declarations.
- `view` called with the removed `symbol` parameter or the `r` flag fails with an
  error that names the `symbol` tool.
- A traversal stops at its hop limit and a path search at its path or work limit;
  each says so in the output.
- A name with more than eight definitions is listed instead of followed, from a
  declarations-only scan.
- Output stops under the response budget with a truncation notice.
- Results come from names and imports, not from a type checker. Treat `(guess)`
  rows as leads to confirm with `view`.
