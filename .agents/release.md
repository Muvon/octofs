# Octofs — release & CI

## CI (every push/PR; superseded runs auto-cancel)

- **rust-ci** (reusable `muvon/ci-workflow`): fmt + clippy + tests with `--all-features`; doc/coverage off.
- **musl-build**: `x86_64` + `aarch64` `unknown-linux-musl` built inside `rust:1.98.0-alpine3.23` docker —
  mirrors the release pipeline. A change that needs cmake (or anything Alpine lacks) breaks this job; that is
  why russh uses the `ring` backend, not `aws-lc-rs`.
- **coverage**: pushes badge JSON to the `badges` branch; percentages exclude `*_tests.rs` (same regex as
  `make coverage`).

## Release (tag-driven)

Pushing a semver tag (`[0-9]+.[0-9]+.[0-9]+*`) or `workflow_dispatch` triggers `.github/workflows/release.yml`:

1. **Build matrix** — 6 targets: musl x86_64/aarch64 (Alpine docker), Windows MSVC x2, macOS x2; protoc
   installed on macOS/Windows; release profile is size-optimized (`lto`, `strip`, `opt-level = "s"`,
   `panic = "abort"`).
2. **`.mcpb` bundles** per target (`muvon/ci-workflow/actions/mcpb`) for one-click MCP installs.
3. **Publish chain** — crates.io → GitHub release → npm wrapper `@muvon/octofs` → MCP registry
   (`io.github.Muvon/octofs`) → Homebrew tap dispatch.

So the manual part is: bump `version` in `Cargo.toml`, tag, push the tag — everything else is hands-off
[UNCONFIRMED — confirm exact choreography with the maintainer]. `CHANGELOG.md` reads as generated
(per-commit entries with hashes); don't hand-edit it [UNCONFIRMED].

## Distribution surfaces to keep in sync

README documents install via cargo, Homebrew (`muvon/tap/octofs`), npm, pre-built binaries, and the MCP
registry. User-facing behaviour changes should update README in the same change; `server.json` holds the
published MCP metadata.
