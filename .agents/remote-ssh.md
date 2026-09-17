# Octofs — remote filesystem (SSH/SFTP)

Read before touching `src/mcp/fs/remote.rs` or anything path-resolution-adjacent.

## Model

Every path parses via `parse_path_source` into `PathSource::Local` or `PathSource::Remote` (`ssh://`, `sftp://`).
Remote is a native capability — always compiled in, no feature flag. `resolve_path_source(path, workdir)`
resolves locals against the workdir; a remote workdir (`--path ssh://…`) makes relative paths resolve to remote
sources, joined with `/` by hand — never `PathBuf::join` (it inserts `\` on Windows and corrupts the URL).
IPv6 hosts (`ssh://user@[::1]:2222/path`) parse.

## URL grammar

- `ssh://user@host:port/path` · `sftp://…` — same handling.
- Defaults: port 22, user `$USER` (or `root`); both overridden by `~/.ssh/config` when not explicit in the URL.
- No path, or `/~` / `/~/x`, means the login home (matches `ssh host` / `scp host:`); `sftp_path()` maps `/~` →
  `.` and `/~/x` → `x` because SFTP resolves relatives against the login home. Other absolute paths pass through.

## OpenSSH config

Host aliases resolve through the local `~/.ssh/config`, including `Include` and `Match`. Honoured: `HostName`,
`User`, `Port`, `IdentityFile` (entries accumulate), `IdentityAgent`, and a single `ProxyJump`. Multi-hop
ProxyJump and `ProxyCommand` are rejected with a clear error. Explicit URL values win over config.

## Authentication (mirrors the `ssh` CLI)

Agent first — the host's `IdentityAgent` (e.g. 1Password), else `$SSH_AUTH_SOCK`; then key files: `--ssh-key`
if given, the host's `IdentityFile` entries, then `~/.ssh` defaults (`id_ed25519`, `id_ecdsa`).
Passphrase-protected key files are unsupported — use an agent. On failure the error lists why each method
failed (a swallowed key-load error is indistinguishable from a server-side rejection otherwise).

**RSA keys are not supported at all**: the Rust `rsa` crate has no release with the Marvin-attack fix
(RUSTSEC-2023-0071), so russh is built with `default-features = false` and the `ring` backend (also because
Alpine/musl release builds have no cmake — `aws-lc-rs` needs it). ed25519/ecdsa keys and host keys work.

## Host keys & connection pool

Host keys verified against `~/.ssh/known_hosts` with OpenSSH `accept-new` policy: unknown host recorded
(trust-on-first-use), mismatch or any check error fails closed. Sessions pool per (host, port, user), shared as
`Arc<SftpSession>` — SFTP multiplexes requests by id, so concurrent tool calls to one host pipeline instead of
serializing. Keepalives every 15 s; a dead transport is evicted and replaced (a cached dead session would fail
every call forever). `--ssh-timeout` (default 30 s) bounds connects; `init_sftp_pool` runs once at startup.

## Tool surface

File tools (view, text_editor, batch_edit, extract_lines, workdir) reach remote hosts; **`shell` never does** —
it runs on the local machine and bails with guidance under a remote workdir. `ssh host 'cmd'` inside the shell
tool is checked by the same misuse rules as local commands (recursive gate in `detect_shell_misuse`).
A bare remote directory listing defaults to `max_depth: 1` (each subdirectory costs an SFTP round trip);
`pattern`/`content` searches always walk the whole tree.

## Tests

`remote_tests.rs` covers URL parsing (schemes, defaults, IPv6, home-relative paths), ssh-config parsing,
proxy-jump forms and lock keys — no live SSH server needed. Keep new remote logic testable the same way.
