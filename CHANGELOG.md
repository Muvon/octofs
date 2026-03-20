# Changelog

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
