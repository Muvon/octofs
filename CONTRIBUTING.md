# Contributing to Octofs

Thank you for your interest in contributing! This document provides guidelines for contributing to Octofs.

## Code of Conduct

Be respectful and constructive. We're all here to build something great.

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

## Questions?

- Open an issue for questions
- Join discussions in existing issues

## License

By contributing, you agree that your contributions will be licensed under the Apache-2.0 License.
