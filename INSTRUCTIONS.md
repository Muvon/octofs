# Octofs Development Instructions

## Core Principles

### Code Quality & Architecture
- **DRY Principle**: Don't repeat yourself - reuse existing patterns
- **KISS Principle**: Keep it simple, stupid - avoid over-engineering
- **Zero Warnings**: All code must pass `cargo clippy` without warnings
- **Fail Fast**: Validate inputs early and return clear error messages
- **Pragmatic Design**: Simple, maintainable code that others can understand

### Tool Design Philosophy
- **Single Responsibility**: Each tool does one thing well
- **Composability**: Tools work together seamlessly
- **Predictability**: Consistent behavior across all operations
- **Safety**: Proper error handling and validation

## Project Structure

### Core Modules

- `src/main.rs` - Entry point and server initialization
- `src/cli.rs` - CLI argument parsing with clap
- `src/mcp/` - Model Context Protocol server
  - `server.rs` - MCP protocol handler and tool registration
  - `mod.rs` - Module exports
  - `fs/` - Filesystem tools implementation
    - `core.rs` - Core file operations (read, write, create)
    - `text_editing.rs` - Text editing operations (str_replace, batch_edit)
    - `directory.rs` - Directory operations (list, search, traverse)
    - `ast_grep.rs` - AST-based code analysis and refactoring
    - `shell.rs` - Shell command execution with background support
    - `workdir.rs` - Working directory context management
    - `file_ops.rs` - File operation utilities
    - `functions.rs` - Tool function definitions and schemas
    - `fs_tests.rs` - Filesystem tests
    - `mod.rs` - Module exports
  - `shared_utils.rs` - Shared utility functions
  - `hint_accumulator.rs` - Hint accumulation for tool feedback
- `src/utils/` - Utility functions
  - `glob.rs` - Glob pattern matching and expansion
  - `truncation.rs` - Content truncation logic for large files
  - `mod.rs` - Module exports
- `Cargo.toml` - Project dependencies and metadata
- `rustfmt.toml` - Code formatting configuration

### Tool Organization

Each filesystem tool is implemented as a separate function in `src/mcp/fs/functions.rs` with:
- Clear input validation
- Proper error handling
- Consistent output formatting
- Integration with MCP protocol

## MCP Tools

### File Operations

**view** - Read files, view directories, and search file content
- Read single file or multiple files
- View directory structure with filtering
- Search file content with ripgrep
- Support for line ranges and context

**text_editor** - Create, edit, and manage file content
- Create new files
- Replace exact string matches
- Undo last edit
- Atomic operations

**batch_edit** - Perform multiple atomic edits on a single file
- Multiple insert/replace operations
- Original line number references
- Atomic execution
- Diff output

**extract_lines** - Copy lines from source to target file
- Extract line ranges
- Append to target file
- Preserve source file

### Code Analysis

**ast_grep** - Search and refactor code using AST patterns
- Pattern matching with metavariables
- Code refactoring with rewrites
- Language-specific parsing
- Context-aware matching

**view_signatures** - Extract function signatures and declarations
- Function signatures
- Class definitions
- Type declarations
- No implementation bodies

### Shell & System

**shell** - Execute commands with background process support
- Command execution
- Background process support
- Output capture
- Error handling

**workdir** - Get or set working directory context
- Get current working directory
- Set new working directory
- Reset to session root
- Relative path resolution

## Development Workflow

### MANDATORY BUILD COMMANDS

```bash
# Development build
cargo build

# Check code quality
cargo check --message-format=short

# Run tests
cargo test

# Lint with clippy
cargo clippy

# Format code
cargo fmt
```

### Code Quality Standards

- **Zero clippy warnings** - All code must pass `cargo clippy` without warnings
- **Minimal dependencies** - Reuse existing dependencies before adding new ones
- **Error handling** - Use proper `Result<T>` types and meaningful error messages
- **Testing** - Unit tests for individual components, integration tests for workflows

### Testing Approach

- **Unit tests** for individual components (in `fs_tests.rs`)
- **Integration tests** for full workflows
- **Manual testing** with real projects during development

## Common Patterns

### Error Handling

```rust
// ✅ GOOD: Proper error handling with context
pub async fn execute(command: Commands) -> Result<()> {
    match command {
        Commands::Mcp => {
            let server = McpServer::new().await?;
            server.run().await
        }
    }
}

// ✅ GOOD: Clear error messages
anyhow::bail!("File not found: {}", path.display());

// ❌ AVOID: Unwrapping without context
let content = fs::read_to_string(path).unwrap();
```

### File Operations

```rust
// ✅ GOOD: Use proper error handling
let content = tokio::fs::read_to_string(&path).await
    .context("Failed to read file")?;

// ✅ GOOD: Validate inputs early
if path.is_absolute() && !path.starts_with(&workdir) {
    anyhow::bail!("Path must be within working directory");
}

// ❌ AVOID: Assuming file operations succeed
let _ = fs::write(path, content);
```

### Async Operations

```rust
// ✅ GOOD: Use tokio for async file operations
let content = tokio::fs::read_to_string(path).await?;

// ✅ GOOD: Proper async error handling
tokio::spawn(async move {
    if let Err(e) = process_file(&path).await {
        eprintln!("Error: {}", e);
    }
});

// ❌ AVOID: Blocking operations in async context
let content = std::fs::read_to_string(path)?;
```

## Adding New Tools

### Steps to Add a New Tool

1. **Define the tool function** in `src/mcp/fs/functions.rs`
   - Clear input validation
   - Proper error handling
   - Consistent output formatting

2. **Add tool schema** in `src/mcp/fs/functions.rs`
   - Input parameters with descriptions
   - Output format specification
   - Example usage

3. **Register the tool** in `src/mcp/server.rs`
   - Add to tool list
   - Map to handler function
   - Update tool descriptions

4. **Add tests** in `src/mcp/fs/fs_tests.rs`
   - Unit tests for core functionality
   - Error case handling
   - Integration with other tools

5. **Update documentation**
   - Add to README.md
   - Update INSTRUCTIONS.md if needed
   - Add inline code comments

### Tool Implementation Template

```rust
pub async fn my_tool(
    params: MyToolParams,
    workdir: &Path,
) -> Result<MyToolResponse> {
    // Validate inputs
    if params.path.is_empty() {
        anyhow::bail!("Path cannot be empty");
    }

    // Resolve path relative to workdir
    let resolved_path = resolve_path(&params.path, workdir)?;

    // Perform operation
    let result = perform_operation(&resolved_path).await?;

    // Return result
    Ok(MyToolResponse {
        success: true,
        data: result,
    })
}
```

## Performance Guidelines

### File Operations
- **Progressive file counting** during directory traversal
- **Lazy loading** of file content for large files
- **Intelligent truncation** for oversized content
- **Batch operations** for multiple file changes

### Code Analysis
- **AST parsing** only when needed
- **Pattern caching** for repeated searches
- **Incremental analysis** for large codebases

### Shell Execution
- **Process pooling** for background tasks
- **Output streaming** for long-running commands
- **Resource cleanup** after command completion

## Common Patterns

### Path Resolution

```rust
// ✅ GOOD: Resolve paths relative to workdir
fn resolve_path(path: &str, workdir: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(workdir.join(path).canonicalize()?)
    }
}
```

### Content Truncation

```rust
// ✅ GOOD: Truncate large content intelligently
fn truncate_content(content: &str, max_lines: usize) -> String {
    content
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}
```

### Error Context

```rust
// ✅ GOOD: Add context to errors
let content = tokio::fs::read_to_string(&path)
    .await
    .context(format!("Failed to read file: {}", path.display()))?;
```

## Quick Start Checklist

1. **Code Quality**: Always run `cargo clippy` before finalizing code
2. **Testing**: Write tests for new functionality
3. **Documentation**: Update README.md and INSTRUCTIONS.md
4. **Error Handling**: Use proper `Result<T>` types
5. **Validation**: Validate inputs early and fail fast
6. **Performance**: Consider performance implications of changes
7. **Compatibility**: Ensure changes don't break existing tools

## Development Patterns

### Adding New File Operations

1. Implement function in `src/mcp/fs/core.rs` or appropriate module
2. Add tool wrapper in `src/mcp/fs/functions.rs`
3. Register in `src/mcp/server.rs`
4. Add tests in `src/mcp/fs/fs_tests.rs`
5. Update README.md with usage examples

### Adding New Utilities

1. Create module in `src/utils/`
2. Implement functionality with clear API
3. Add tests in module
4. Export from `src/utils/mod.rs`
5. Use in appropriate tools

## Code Style

### Naming Conventions

- **Functions**: `snake_case` for all functions
- **Types**: `PascalCase` for structs, enums, traits
- **Constants**: `SCREAMING_SNAKE_CASE` for constants
- **Variables**: `snake_case` for all variables

### Comments

- **Why not What**: Explain intent, not obvious operations
- **Module-level**: Document module purpose and usage
- **Complex logic**: Explain non-obvious algorithms
- **Avoid**: Obvious comments like `// increment counter`

### Formatting

- Use `cargo fmt` for consistent formatting
- Follow `rustfmt.toml` configuration
- Max line length: 100 characters (configured in rustfmt.toml)

## Troubleshooting

### Build Issues

```bash
# Clean build
cargo clean && cargo build

# Check for clippy warnings
cargo clippy -- -D warnings

# Run tests
cargo test
```

### Runtime Issues

```bash
# Enable debug logging
RUST_LOG=debug octofs

# Check error messages
octofs 2>&1 | grep -i error
```

## Resources

- [MCP Specification](https://modelcontextprotocol.io/)
- [Tokio Documentation](https://tokio.rs/)
- [Rust Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [AST Grep Documentation](https://ast-grep.github.io/)
