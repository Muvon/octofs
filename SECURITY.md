# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| < 0.2.0 | :x:                |

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

Octofs is designed with security in mind:

- **Path Validation** — All paths are validated to prevent directory traversal
- **Gitignore Awareness** — Respects `.gitignore` patterns
- **Working Directory** — Operations are scoped to configured directories
- **No Network** — Default stdio transport, no network exposure
- **Shell Safety** — Commands are executed with user permissions only

## Best Practices

When using Octofs:

- Configure appropriate working directories
- Review batch edits before applying
- Use environment variables for sensitive data
- Keep Octofs updated to the latest version
