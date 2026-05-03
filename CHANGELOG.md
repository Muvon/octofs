# Changelog

## [0.4.1] - 2026-05-03

### 📋 Release Summary

This release enhances the filesystem toolset with unified diff outputs for string replacements and improved line range validation for more reliable file editing (b0ce960a, 8a628812). Additionally, the project documentation has been rewritten to provide a clearer overview of the system architecture and usage instructions (86e4b7b0).


### ✨ New Features & Enhancements

- **mcp**: enhance line range validation and schema clarity `8a628812`
- **fs**: add unified diff output to str_replace `b0ce960a`

### 📚 Documentation & Examples

- **instructions**: rewrite for clarity and architecture `86e4b7b0`

## [0.4.0] - 2026-05-02

### 📋 Release Summary

This release introduces enhanced file viewing capabilities with support for multi-range selection and automatic out-of-bounds clamping (6f832c54, 8e54d4d3, cbc22c56, 708ecd53). User experience is improved through a streamlined text editor interface and more robust file locking via path canonicalization (48d2f32f, 79e08c6d, 5a1e6753). Additionally, shell execution is now strictly non-interactive to ensure stability, supported by updated toolchains and improved error handling for complex file operations (cf75acda, d4169097, e5a9ea1f).


### ✨ New Features & Enhancements

- **mcp**: implement structured line range types `708ecd53`
- **mcp**: enforce non-interactive shell execution `cf75acda`
- **mcp**: add multi-range support for file views `6f832c54`
- **fs**: clamp out-of-bounds line ranges `8e54d4d3`
- **mcp**: support per-file line ranges in view `cbc22c56`

### 🔧 Improvements & Optimizations

- **mcp**: flatten text editor command schema `48d2f32f`
- **mcp**: convert TextEditorParams to tagged enum `79e08c6d`
- **fs**: error when line ranges exceed path count `d4169097`

### 🐛 Bug Fixes & Stability

- **fs**: canonicalize paths for file lock keys `5a1e6753`

### 🔄 Other Changes

1 maintenance, dependency, and tooling update not listed individually.

## [0.3.1] - 2026-04-19

### 📋 Release Summary

This release improves working directory management and introduces helpful hints across all tool responses to provide better user guidance (be6c7044, 2a897f1a). Enhancements to file system tools and output formatting provide greater flexibility when viewing files and more consistent results (800c9490, 0c3528a0, 630b2377).


### 🚨 Breaking Changes

⚠️ **Important**: This release contains breaking changes that may require code updates.

- **mcp**: replace thread-local storage with call.workdir `be6c7044`

### ✨ New Features & Enhancements

- **mcp**: append hints to all tool responses `2a897f1a`

### 🔧 Improvements & Optimizations

- **mcp**: use typed WorkdirResult instead of JSON parsing `630b2377`
- remove unused tool_router field from OctofsServer `3c1b7450`
- **fs**: remove space after colon in line number formatting `0c3528a0`

### 🐛 Bug Fixes & Stability

- **fs**: allow null value for lines parameter `800c9490`

### 🔄 Other Changes

1 maintenance, dependency, and tooling update not listed individually.

## [0.3.0] - 2026-04-08

### 📋 Release Summary

This release introduces hash-based line selection and referencing, enabling stable edits through position-aware line identifiers. Content search has been added to file viewing tools, and the MCP protocol now supports hash-based line ranges. The ast_grep tool has been removed in favor of improved path resolution, and search functionality has been migrated to a pure-Rust implementation (953f7735, 41d6585d, 7a46c1bc).


### 🚨 Breaking Changes

⚠️ **Important**: This release contains breaking changes that may require code updates.

- **mcp**: remove ast_grep tool and add path resolution `9c607564`

### ✨ New Features & Enhancements

- **fs**: add hash line selection and dynamic schema `ad9b6456`
- **fs**: add content search to file view tools `55ca8508`
- **line-hash**: implement position-aware hashing `05f3960a`
- **mcp**: support hash-based line ranges `0e9e7aba`
- **cli**: add hash-based line identifiers for stable edits `5cd1a849`

### 🔧 Improvements & Optimizations

- **fs**: replace ripgrep with pure-Rust search `953f7735`
- **mcp**: unify view parameter to paths with ripgrep output `41d6585d`

### 📚 Documentation & Examples

- **readme**: add MCP tools reference and configuration `3cca5e67`

### 🔄 Other Changes

3 maintenance, dependency, and tooling updates not listed individually.

## [0.2.1] - 2026-03-27

### 📋 Release Summary

This release upgrades octofs to use the official rmcp SDK with HTTP transport for more reliable MCP connections (d0945d95) and eliminates race conditions when tools are interrupted (2e2fec9a). The update also streamlines the codebase, removes unused dependencies, and delivers refreshed documentation and branding for a cleaner user experience (c20664df, 69731527, 66e90fe9).


### ✨ New Features & Enhancements

- **mcp**: integrate official rmcp SDK with HTTP transport `d0945d95`

### 🔧 Improvements & Optimizations

- **mcp**: simplify return types and remove McpToolResult `c20664df`
- **mcp**: reformat code for better readability `5a79032e`

### 🐛 Bug Fixes & Stability

- **mcp**: race tool execution against SIGTERM on Unix `2e2fec9a`

### 📚 Documentation & Examples

- **readme**: remove roadmap section `ee3f56f0`
- add comprehensive project documentation and branding `66e90fe9`

### 🔄 Other Changes

1 maintenance, dependency, and tooling update not listed individually.

## [0.2.0] - 2026-03-25

### 📋 Release Summary

This release improves shell session management with reliable process cleanup on shutdown and adds fuzzy matching for safer file replacements. macOS users now have native x86_64 support.


### ✨ New Features & Enhancements

- **shell**: track and kill shell child process groups on shutdown `f197fd41`

### 🔧 Improvements & Optimizations

- **str_replace**: add fuzzy matching and atomic writes `c95c65fe`

### 🐛 Bug Fixes & Stability

- **shell**: replace setsid with process_group to allow signal propagation `29a08556`

### 🔄 Other Changes

- **release**: add x86_64-apple-darwin build target `eb27d212`

### 📊 Release Summary

**Total commits**: 4 across 4 categories

✨ **1** new feature - *Enhanced functionality*
🔧 **1** improvement - *Better performance & code quality*
🐛 **1** bug fix - *Improved stability*
🔄 **1** other change - *Maintenance & tooling*

## [0.1.1] - 2026-03-20

### 📋 Release Summary

This release introduces MCP server configuration for seamless filesystem tool integration. Shell commands now run reliably without hanging on interactive prompts, while text editing operations handle conflicts more accurately.


### ✨ New Features & Enhancements

- **mcp**: add MCP server configuration for octofs filesystem tools `568a4950`

### 🔧 Improvements & Optimizations

- **text_editing**: improve formatting and line wrapping `5550ee62`
- **text_editing**: extract duplicate check logic to helper function `9b43a432`

### 🐛 Bug Fixes & Stability

- **shell**: prevent interactive prompts from hanging shell commands `d937d4d2`
- **mcp**: resolve false conflicts in text editor operations `fd8c8800`

### 🔄 Other Changes

- add pre-commit hooks and rust toolchain config `a41e1675`
- update files `291f2769`

### 📊 Release Summary

**Total commits**: 7 across 4 categories

✨ **1** new feature - *Enhanced functionality*
🔧 **2** improvements - *Better performance & code quality*
🐛 **2** bug fixes - *Improved stability*
🔄 **2** other changes - *Maintenance & tooling*

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-03-15

### 📋 Release Summary

This initial release of octofs delivers a standalone MCP filesystem tools server that lets you view, edit, shell, and search your working directory with confidence. The update fixes two edge-case issues that could cause silent failures when parsing certain file structures.


### 🐛 Bug Fixes & Stability

- **mcp**: add missing jsonrpc field rename for deserialization `0e3409de`
- **fs**: tighten structural noise detection for compound closers `b05dc494`

### 🔄 Other Changes

- upgrade Rust toolchain to 1.92.0 `17d39053`
- Initial release `5c1118bd`
- Initial commit `f1dca141`

### 📊 Release Summary

**Total commits**: 5 across 2 categories

🐛 **2** bug fixes - *Improved stability*
🔄 **3** other changes - *Maintenance & tooling*
