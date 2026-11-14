// Copyright 2026 Muvon Un Limited
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

//! The set of repository files that import resolution looks paths up in, and
//! the string-only path helpers language resolvers share. Paths are
//! workdir-relative and come from one walk, so lookups never touch the
//! filesystem.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REGISTRY_ID: AtomicU64 = AtomicU64::new(1);

/// Replace backslashes so Windows-style and Unix-style spellings of a path
/// compare equal.
pub fn normalize_separators(path: &str) -> String {
	path.replace('\\', "/")
}

/// Collapse `.` and `..` components of a relative path. A `..` with nothing
/// left to pop is kept, so an escape above the root stays visible instead of
/// resolving to a shorter, wrong path.
pub fn normalize_path(path: &str) -> String {
	let separated = normalize_separators(path);
	let mut components: Vec<&str> = Vec::new();
	for component in separated.split('/') {
		match component {
			"" | "." => {}
			".." => match components.last() {
				Some(&"..") | None => components.push(".."),
				Some(_) => {
					components.pop();
				}
			},
			normal => components.push(normal),
		}
	}
	if components.is_empty() {
		return separated;
	}
	components.join("/")
}

/// Resolve `relative_path` against the directory of `source_file`:
/// `src/main.rs` + `../lib.rs` is `lib.rs`.
pub fn resolve_relative_path(source_file: &str, relative_path: &str) -> String {
	let source_dir = source_file
		.rsplit_once(['/', '\\'])
		.map(|(directory, _)| directory)
		.unwrap_or_default();
	if source_dir.is_empty() {
		normalize_path(relative_path)
	} else {
		normalize_path(&format!("{source_dir}/{relative_path}"))
	}
}

/// Bytes the allocator really holds for a block of `bytes`: a header and the
/// rounding to 16 (glibc), at least 32. Many short strings cost this much
/// each beyond their length.
pub fn heap_chunk(bytes: usize) -> usize {
	if bytes == 0 {
		return 0;
	}
	(bytes + std::mem::size_of::<usize>()).next_multiple_of(16).max(32)
}

/// The directory part of a separator-normalized path (`""` for the root).
fn directory_of(normalized: &str) -> &str {
	normalized.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// File registry for efficient file lookup by path, directory and name. Every
/// path is stored twice (by directory and by name); exact lookups scan the
/// files of one directory.
pub struct FileRegistry {
	id: u64,
	/// Directory (forward slashes, `""` is the root) -> files directly in it.
	files_by_dir: HashMap<String, Vec<String>>,
	/// Base name with extension -> every file with that name.
	files_by_name: HashMap<String, Vec<String>>,
	/// Last directory component -> every directory path ending in it.
	dirs_by_name: HashMap<String, Vec<String>>,
}

impl FileRegistry {
	/// Tells registries apart, so a result computed against one is not used
	/// with another.
	pub fn id(&self) -> u64 {
		self.id
	}

	/// Heap bytes held, by capacity, with the allocator's per-block overhead:
	/// every path is stored by directory and by name.
	pub fn heap_bytes(&self) -> usize {
		let text = |paths: &[String]| paths.iter().map(|path| heap_chunk(path.capacity())).sum::<usize>();
		let grouped = |map: &HashMap<String, Vec<String>>| {
			map.capacity() * (std::mem::size_of::<(String, Vec<String>)>() + 1)
				+ map
					.iter()
					.map(|(key, paths)| {
						heap_chunk(key.capacity())
							+ heap_chunk(paths.capacity() * std::mem::size_of::<String>())
							+ text(paths)
					})
					.sum::<usize>()
		};
		grouped(&self.files_by_dir) + grouped(&self.files_by_name) + grouped(&self.dirs_by_name)
	}

	pub fn new(all_files: &[String]) -> Self {
		let mut files_by_dir: HashMap<String, Vec<String>> = HashMap::new();
		let mut files_by_name: HashMap<String, Vec<String>> = HashMap::new();
		let mut dirs_by_name: HashMap<String, Vec<String>> = HashMap::new();
		let mut seen_dirs: HashSet<String> = HashSet::new();
		for file_path in all_files {
			let normalized = normalize_separators(file_path);
			let (directory, name) = normalized.rsplit_once('/').unwrap_or(("", normalized.as_str()));
			files_by_dir
				.entry(directory.to_string())
				.or_default()
				.push(file_path.clone());
			files_by_name
				.entry(name.to_string())
				.or_default()
				.push(file_path.clone());
			// A directory seen before has had all its ancestors registered too.
			let mut ancestor = directory;
			while !ancestor.is_empty() && seen_dirs.insert(ancestor.to_string()) {
				let last = ancestor.rsplit('/').next().unwrap_or(ancestor);
				dirs_by_name
					.entry(last.to_string())
					.or_default()
					.push(ancestor.to_string());
				ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
			}
		}

		Self {
			id: NEXT_REGISTRY_ID.fetch_add(1, Ordering::Relaxed),
			files_by_dir,
			files_by_name,
			dirs_by_name,
		}
	}

	/// Files directly in `dir` (forward slashes, no trailing slash; `""` is the
	/// root).
	pub fn files_in_dir(&self, dir: &str) -> &[String] {
		self.files_by_dir.get(dir).map(Vec::as_slice).unwrap_or(&[])
	}

	/// Every file whose base name, extension included, equals `file_name`.
	pub fn files_named(&self, file_name: &str) -> &[String] {
		self.files_by_name.get(file_name).map(Vec::as_slice).unwrap_or(&[])
	}

	/// Every directory path whose last component equals `dir_name`.
	pub fn dirs_named(&self, dir_name: &str) -> &[String] {
		self.dirs_by_name.get(dir_name).map(Vec::as_slice).unwrap_or(&[])
	}

	/// Whether a file equal to `path` (after separator normalization) exists.
	pub fn contains(&self, path: &str) -> bool {
		self.find_normalized(path).is_some()
	}

	/// First registry file named `base_path` plus one of `extensions`, tried in
	/// order; an empty extension means `base_path` as is.
	pub fn find_file_with_extensions(&self, base_path: &str, extensions: &[&str]) -> Option<String> {
		extensions.iter().find_map(|extension| {
			if extension.is_empty() {
				self.find_exact_file(base_path)
			} else {
				self.find_exact_file(&format!("{base_path}.{extension}"))
			}
		})
	}

	/// First registry file equal to `target` after separator normalization.
	pub fn find_normalized(&self, target: &str) -> Option<&str> {
		let target = normalize_separators(target);
		self.files_in_dir(directory_of(&target))
			.iter()
			.map(String::as_str)
			.find(|file| {
				if file.contains('\\') {
					normalize_separators(file) == target
				} else {
					*file == target
				}
			})
	}

	/// First registry file byte-equal to `target`.
	pub fn find_verbatim(&self, target: &str) -> Option<&str> {
		let normalized = normalize_separators(target);
		self.files_in_dir(directory_of(&normalized))
			.iter()
			.map(String::as_str)
			.find(|file| *file == target)
	}

	/// Exact file match that tolerates Windows- and Unix-style separators.
	pub fn find_exact_file(&self, target_path: &str) -> Option<String> {
		self.find_normalized(target_path).map(str::to_string)
	}
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
