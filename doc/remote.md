# Remote filesystem (SSH/SFTP)

Remote is a native capability of every file tool — always compiled in, no
feature flag. Implementation: `src/mcp/fs/remote.rs`.

## Model

Every path parses via `parse_path_source` into `PathSource::Local` or
`PathSource::Remote` (`ssh://`, `sftp://`). `resolve_path_source(path,
workdir)` resolves locals against the workdir; a remote workdir
(`--path ssh://…` or `workdir ssh://…`) makes *relative* paths resolve to
remote sources, joined with `/` by hand — never `PathBuf::join` (it inserts
`\` on Windows and corrupts URLs). IPv6 hosts (`ssh://user@[::1]:2222/path`)
parse.

## URL grammar

- `ssh://user@host:port/path` · `sftp://…` — same handling.
- Defaults: port 22, user `$USER` (or `root`); both overridden by
  `~/.ssh/config` when not explicit in the URL.
- No path, or `/~` / `/~/x`, means the login home (matches `ssh host` /
  `scp host:`); `sftp_path()` maps `/~` → `.` and `/~/x` → `x` because SFTP
  resolves relatives against the login home. Other absolute paths pass through.

## OpenSSH config

Host aliases resolve through the local `~/.ssh/config`, including `Include`
and `Match`. Honoured: `HostName`, `User`, `Port`, `IdentityFile` (entries
accumulate), `IdentityAgent`, and a single `ProxyJump`. Multi-hop ProxyJump
and `ProxyCommand` are rejected with a clear error. Explicit URL values win
over config.

## Authentication

Mirrors the `ssh` CLI: agent first — the host's `IdentityAgent` (e.g.
1Password), else `$SSH_AUTH_SOCK`; then key files: `--ssh-key` if given, the
host's `IdentityFile` entries, then `~/.ssh` defaults (`id_ed25519`, `id_ecdsa`,
`id_rsa` — probed in that order). Passphrase-protected key files are
unsupported — use an agent. On failure the error lists why each method failed.

**RSA keys are not supported at all**: the Rust `rsa` crate has no release
with the Marvin-attack fix (RUSTSEC-2023-0071), so russh is built with
`default-features = false` and the `ring` backend (also because Alpine/musl
release builds have no cmake — `aws-lc-rs` needs it). ed25519/ecdsa keys and
host keys work. Do not "fix" either choice.

## Host keys & connection pool

Host keys are verified against `~/.ssh/known_hosts` with OpenSSH
`accept-new` policy: unknown host recorded (trust-on-first-use), mismatch or
any check error fails closed. Sessions pool per (host, port, user), shared as
`PathSource` values flow through the tools; dead transports are evicted and
replaced. The pool lock is deliberately held across connection setup to avoid
duplicate connections and repeated agent prompts.

## Limits

`shell` always runs locally and bails on a remote workdir — there is no
remote command execution. Everything file-shaped (`view`, `text_editor`,
`batch_edit`, `extract_lines`, directory listings, content search) works
remotely, with remote listing quirks handled in `fs/directory.rs`.
