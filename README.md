# Octofs - Standalone MCP Filesystem Tools Server

Standalone Model Context Protocol (MCP) server providing comprehensive filesystem operations, code analysis, and shell execution tools for AI assistants and development workflows.

## Features

- **File Operations**: View, read, create, edit, and manage files with intelligent truncation
- **Directory Navigation**: List, search, and traverse directories with glob patterns
- **Text Editing**: Batch edits, line-based operations, and atomic multi-file updates
- **Code Analysis**: AST-based pattern matching and refactoring with ast-grep
- **Shell Execution**: Execute commands with background process support
- **Working Directory Management**: Context-aware file operations with workdir switching
- **Gitignore Awareness**: Respects `.gitignore` patterns for clean file discovery
- **MCP Server**: Exposes all tools via Model Context Protocol for AI integration

## Installation

### From Source

```bash
# Build the project
cargo build --release

# Run the binary
./target/release/octofs --help
```

## Usage

### CLI Structure

Octofs provides a single command to start the MCP server:

```bash
octofs
```

### MCP Tools

The server exposes the following tools:

**File Operations:**
- `view` - Read files, view directories, and search file content
- `text_editor` - Create, edit, and manage file content
- `batch_edit` - Perform multiple atomic edits on a single file
- `extract_lines` - Copy lines from source to target file

**Code Analysis:**
- `ast_grep` - Search and refactor code using AST patterns
- `semantic_search` - Find code by functionality (requires indexing)
- `view_signatures` - Extract function signatures and declarations

**Shell & System:**
- `shell` - Execute commands with background process support
- `workdir` - Get or set working directory context

**Directory Operations:**
- Directory listing with glob patterns
- Content search with ripgrep integration
- Hidden file handling

## Configuration

Octofs is configured via environment variables and command-line flags:

```bash
# Set log level
export RUST_LOG=debug

# Start server on specific port (HTTP mode)
octofs --bind 127.0.0.1:3000

# Start server in stdio mode (default, for MCP)
octofs
```

## Storage Locations

Octofs is stateless and operates on the filesystem directly. No persistent storage is required.

## Architecture

### Core Modules

- `src/main.rs` - Entry point and server initialization
- `src/cli.rs` - CLI argument parsing
- `src/mcp/` - MCP server implementation
  - `server.rs` - MCP protocol handler
  - `fs/` - Filesystem tools
    - `core.rs` - Core file operations
    - `text_editing.rs` - Text editing operations
    - `directory.rs` - Directory operations
    - `ast_grep.rs` - AST-based code analysis
    - `shell.rs` - Shell command execution
    - `workdir.rs` - Working directory management
    - `functions.rs` - Tool function definitions
- `src/utils/` - Utility functions
  - `glob.rs` - Glob pattern matching
  - `truncation.rs` - Content truncation logic

## Development

### Build Commands

```bash
# Development build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Check code quality
cargo clippy

# Format code
cargo fmt
```

### Code Quality Standards

- **Zero clippy warnings** - All code must pass `cargo clippy` without warnings
- **Minimal dependencies** - Reuse existing dependencies before adding new ones
- **Error handling** - Use proper `Result<T>` types and meaningful error messages
- **Testing** - Unit tests for individual components, integration tests for workflows

## License

Apache-2.0

## Credits

Developed by Muvon Un Limited.
