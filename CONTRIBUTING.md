# Contributing to Octofs

Thank you for your interest in contributing! This document provides guidelines for contributing to Octofs.

## Code of Conduct

Be respectful and constructive. We're all here to build something great. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## How to Contribute

### Reporting Bugs

- Check if the bug has already been reported
- Include clear reproduction steps
- Provide environment details (OS, Rust version, Octofs version)
- Include relevant logs or error messages

### Suggesting Features

- Open an issue with the "Feature Request" template
- Explain the use case and problem you're solving
- Be open to discussion and iteration

### Pull Requests

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Ensure tests pass: `cargo test`
5. Ensure clippy passes: `cargo clippy -- -D warnings`
6. Format code: `cargo fmt`
7. Commit with clear messages
8. Push and open a Pull Request

## Development Setup

```bash
# Clone your fork
git clone https://github.com/YOUR_USERNAME/octofs.git
cd octofs

# Build
cargo build

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run
```

## Code Standards

- **Zero clippy warnings** — All code must pass `cargo clippy`
- **Tests** — Add tests for new functionality
- **Documentation** — Update README.md for user-facing changes
- **Commit messages** — Use clear, descriptive commit messages

## Project Structure

```
src/
├── main.rs           # Entry point
├── cli.rs            # CLI parsing
└── mcp/
    ├── server.rs     # MCP protocol
    └── fs/           # Filesystem tools
```

## Claiming an issue

Issues labelled `good first issue` are scoped so a newcomer can finish them in a few hours; each one names the file, the fix, and the tests to add.

1. Comment on the issue to claim it; a maintainer assigns it to you. Hold one starter issue at a time.
2. Open a PR within 14 days, or comment if you need more time. Assigned starter issues with no activity for 14 days get a `stale-assignment` reminder.
3. Put `Closes #<issue number>` in the PR description.

`first-timers-only` issues are reserved for someone making their first open-source contribution.

## AI-assisted contributions

AI tools are allowed; you are accountable for every line you submit.
Disclose material AI help in the PR description and with an `Assisted-by: <tool>` commit trailer.
You must understand, test, and be able to explain your change without the tool.
Do not let agents open, comment on, or claim issues or PRs autonomously.
`good first issue` and `first-timers-only` issues are reserved for humans learning the codebase.
Comment to claim an issue before opening a PR; unclaimed drive-by PRs may be closed.
Maintainers may close PRs that appear unreviewed or cost more to review than they add.

## Questions?

- Open an issue for questions
- Join discussions in existing issues

## License

By contributing, you agree that your contributions will be licensed under the Apache-2.0 License.
