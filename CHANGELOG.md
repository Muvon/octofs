# Changelog

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
