# cargo-install install captures

Tool version: `cargo 1.99.0 (5f94df478 2026-08-27), rustc 1.99.0`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

Scratch bin crate (src/Cargo.toml.base, src/main.rs) with 13 crates.io deps (anyhow, chrono, clap, rand, regex, reqwest/rustls, serde, serde_json, thiserror, tokio full, tracing, tracing-subscriber, uuid). Both CARGO_HOME (registry and index) and CARGO_TARGET_DIR are scratch dirs, so fresh runs download the index and every crate. main.rs deliberately has an unused import, an unused variable and a never-constructed enum. Reset for fresh = wipe CARGO_HOME, target and Cargo.lock. cargo writes everything to STDERR. The compile-error-* pair came from a first pass where uuid lacked its `serde` feature; they are kept as a realistic 'deps fine, my code broken' case.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| compile-error-build-fresh | `cargo build` | 101/101 | 0 | 15319 | 15319 | Full fresh download+compile noise that ends in 4 `error[E0277]` diagnostics (multi-line, with `note:` spans into ~/.cargo registry sources), 2 warnings and 'could not compile ... due to 4 previous errors; 2 warnings emitted'. |
| compile-error-build-cached | `cargo build` | 101/101 | 0 | 6084 | 6084 | Same compile errors with deps already built: only the user-crate diagnostics. |
| fetch-fresh | `cargo fetch` | 0/0 | 0 | 6350 | 6350 | `cargo fetch` on an empty CARGO_HOME: index update, Locking, and the Downloaded lines only. |
| build-badversion | `cargo build` | 101/101 | 0 | 437 | 437 | `serde = "=99.0.0"`: 'failed to select a version for the requirement', a list of candidate versions and a prerelease `help:`. |
| build-fresh | `cargo build` | 0/0 | 0 | 10287 | 10287 | 'Updating crates.io index', 'Locking 188 packages', `Adding x (available: y)`, `Downloading crates ...`, 151 `Downloaded` lines, 138 `Compiling` lines, 3 warnings in the user crate (rustc diagnostics with ` --> ` arrows and pipe-character gutters), 'generated 3 warnings (run `cargo fix` ...)', then the `Finished` line. |
| build-cached | `cargo build` | 0/0 | 0 | 969 | 969 | Warm target: the 3 user-crate warnings are replayed from cache (no `Compiling` line), then `Finished`. |
