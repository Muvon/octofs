# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.19.x  | :white_check_mark: |
| < 0.19  | :x:                |

## Reporting a Vulnerability

We take security seriously. If you discover a security vulnerability, please report it responsibly.

### How to Report

1. **DO NOT** open a public issue
2. Email security@muvon.io with:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

### What to Expect

- **Acknowledgment** within 48 hours
- **Assessment** within 7 days
- **Resolution** timeline based on severity
- **Credit** in the security advisory (if desired)

## Security Considerations

Octofs gives the connected MCP client the same access as the user running it. It is not a sandbox:

- **Trust model** — Every tool acts with the server user's permissions. `shell` runs arbitrary commands and the file tools accept absolute paths, so the session root (`--path`, `workdir`) is the default for relative paths, not a containment boundary. Connect only clients you trust, and use OS-level isolation (a container, VM, or dedicated user) when you need confinement.
- **Remote paths** — `ssh://` and `sftp://` paths, including a `workdir` switched to one, connect with the operator's SSH key and agent.
- **Network** — The default stdio transport opens no port. `--bind` serves HTTP: a non-loopback address is refused unless `--auth-token` (or `OCTOFS_AUTH_TOKEN`) is set, and clients must then send `Authorization: Bearer <token>`. A server without a token listens on loopback only and rejects requests that carry a browser `Origin` header.
- **Stale-write detection** — Edits target hash-verified line ids, so an edit against a file that changed underneath fails instead of overwriting it.

## Best Practices

When using Octofs:

- Configure appropriate working directories
- Review batch edits before applying
- Use environment variables for sensitive data
- Keep Octofs updated to the latest version
- Keep `--bind` on `127.0.0.1`; when you must expose it, use a long random `OCTOFS_AUTH_TOKEN` and put the server behind TLS or an SSH tunnel, since it speaks plain HTTP
