// Copyright 2025 Muvon Un Limited
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Gitignore-aware glob pattern expansion utilities

use anyhow::{anyhow, Result};
use ignore::WalkBuilder;
use std::path::Path;

/// Maximum number of files allowed after glob expansion to prevent command line overflow
const MAX_EXPANDED_FILES: usize = 1000;

/// Expand glob patterns to actual file paths with gitignore and dotfile filtering.
pub fn expand_glob_patterns_filtered(
	patterns: &[String],
	base_dir: Option<&str>,
) -> Result<Vec<String>> {
	let mut expanded_paths = Vec::new();

	let search_dir = if let Some(dir) = base_dir {
		dir.to_string()
	} else {
		let mut extracted_base = None;
		for pattern in patterns {
			let is_absolute = pattern.starts_with('/')
				|| (cfg!(windows)
					&& ((pattern.len() >= 3
						&& pattern.chars().nth(1) == Some(':')
						&& (pattern.chars().nth(2) == Some('\\')
							|| pattern.chars().nth(2) == Some('/')))
						|| pattern.starts_with("\\\\")));

			if is_absolute {
				if let Some(glob_start) = pattern.find("**") {
					let base = &pattern[..glob_start];
					let base = base.trim_end_matches('/').trim_end_matches('\\');
					if !base.is_empty() {
						extracted_base = Some(base.to_string());
						break;
					}
				} else if let Some(glob_start) = pattern.find('*') {
					let base = &pattern[..glob_start];
					let last_separator = base.rfind('/').or_else(|| base.rfind('\\'));
					if let Some(last_sep) = last_separator {
						let base = &base[..last_sep];
						if !base.is_empty() {
							extracted_base = Some(base.to_string());
							break;
						}
					}
				}
			}
		}
		extracted_base.unwrap_or_else(|| ".".to_string())
	};

	let mut builder = WalkBuilder::new(&search_dir);
	builder
		.hidden(false)
		.git_ignore(true)
		.git_global(true)
		.git_exclude(true)
		.require_git(false)
		.follow_links(false)
		.max_depth(None);

	let should_filter_dotfiles = !is_dotfile_or_in_dot_directory(&search_dir);

	let walker = builder.build();
	let mut all_files = Vec::new();

	for result in walker {
		match result {
			Ok(entry) => {
				let path = entry.path();

				if !path.is_file() {
					continue;
				}

				let path_str = path.to_string_lossy();

				if should_filter_dotfiles {
					let relative_path = if let Ok(rel) = path.strip_prefix(&search_dir) {
						rel.to_string_lossy().to_string()
					} else {
						path_str.to_string()
					};

					if is_dotfile_or_in_dot_directory(&relative_path) {
						continue;
					}
				}

				all_files.push(path_str.to_string());
			}
			Err(_) => {
				// Continue walking even if some paths fail
			}
		}
	}

	for pattern in patterns {
		let mut pattern_matches = 0;

		if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
			let glob_pattern = match glob::Pattern::new(pattern) {
				Ok(p) => p,
				Err(e) => return Err(anyhow!("Invalid glob pattern '{}': {}", pattern, e)),
			};

			for file_path in &all_files {
				let normalized_path = file_path.strip_prefix("./").unwrap_or(file_path);

				if glob_pattern.matches(normalized_path) {
					expanded_paths.push(file_path.clone());
					pattern_matches += 1;
				}
			}
		} else {
			let path_obj = Path::new(pattern);
			if path_obj.exists() {
				if path_obj.is_file() {
					if !is_dotfile_or_in_dot_directory(pattern) {
						expanded_paths.push(pattern.clone());
						pattern_matches += 1;
					}
				} else if path_obj.is_dir() {
					let normalized_dir = pattern.trim_end_matches('/').trim_end_matches('\\');
					for file_path in &all_files {
						let file_path_normalized =
							file_path.strip_prefix("./").unwrap_or(file_path);
						if file_path_normalized.starts_with(&format!("{}/", normalized_dir))
							|| file_path_normalized.starts_with(normalized_dir)
								&& !is_dotfile_or_in_dot_directory(file_path)
						{
							expanded_paths.push(file_path.clone());
							pattern_matches += 1;
						}
					}
				}
			}
		}

		let _ = pattern_matches; // suppress unused warning
	}

	expanded_paths.sort();
	expanded_paths.dedup();

	if expanded_paths.len() > MAX_EXPANDED_FILES {
		return Err(anyhow!(
			"Too many files expanded from glob patterns: {} files (max allowed: {}). Consider using more specific patterns to reduce the file count.",
			expanded_paths.len(),
			MAX_EXPANDED_FILES
		));
	}

	Ok(expanded_paths)
}

/// Check if a file path is a dotfile or is inside a dot directory.
fn is_dotfile_or_in_dot_directory(path: &str) -> bool {
	for component in Path::new(path).components() {
		if let Some(name) = component.as_os_str().to_str() {
			if name.starts_with('.') && name != "." && name != ".." {
				return true;
			}
		}
	}
	false
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_is_dotfile_or_in_dot_directory() {
		assert!(!is_dotfile_or_in_dot_directory("src/main.rs"));
		assert!(!is_dotfile_or_in_dot_directory(
			"ui/components/Button.svelte"
		));
		assert!(!is_dotfile_or_in_dot_directory("README.md"));

		assert!(is_dotfile_or_in_dot_directory(".env"));
		assert!(is_dotfile_or_in_dot_directory(".gitignore"));
		assert!(is_dotfile_or_in_dot_directory(".eslintrc.json"));

		assert!(is_dotfile_or_in_dot_directory(".git/config"));
		assert!(is_dotfile_or_in_dot_directory(".vscode/settings.json"));
		assert!(is_dotfile_or_in_dot_directory("src/.hidden/file.rs"));
		assert!(is_dotfile_or_in_dot_directory(".github/workflows/ci.yml"));

		assert!(!is_dotfile_or_in_dot_directory("."));
		assert!(!is_dotfile_or_in_dot_directory(".."));
		assert!(!is_dotfile_or_in_dot_directory("./src/main.rs"));
		assert!(!is_dotfile_or_in_dot_directory("../other/file.rs"));
	}
}
