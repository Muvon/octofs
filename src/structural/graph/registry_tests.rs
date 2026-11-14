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

//! File registry and path helper tests.

use super::*;

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|file| file.to_string()).collect();
	FileRegistry::new(&owned)
}

#[test]
fn files_are_indexed_by_directory_and_name() {
	let registry = registry(&["src/a/mod.rs", "src/a/x.rs", "src\\b\\mod.rs", "mod.rs", "lib/a/y.py"]);
	assert_eq!(registry.files_in_dir("src/a"), ["src/a/mod.rs", "src/a/x.rs"]);
	assert_eq!(registry.files_in_dir("src/b"), ["src\\b\\mod.rs"]);
	assert_eq!(registry.files_in_dir(""), ["mod.rs"]);
	assert!(registry.files_in_dir("src").is_empty());
	assert_eq!(registry.files_named("mod.rs").len(), 3);
	assert!(registry.files_named("nope.rs").is_empty());
}

#[test]
fn directories_are_indexed_by_last_component_with_their_ancestors() {
	let registry = registry(&["src/a/x.rs", "lib/a/y.rs", "src/c/z.rs"]);
	let mut named_a: Vec<&String> = registry.dirs_named("a").iter().collect();
	named_a.sort();
	assert_eq!(named_a, ["lib/a", "src/a"]);
	assert_eq!(registry.dirs_named("src"), ["src"]);
	assert!(registry.dirs_named("x.rs").is_empty());
}

#[test]
fn contains_tolerates_mixed_separators() {
	let registry = registry(&["src\\a.rs", "b.rs"]);
	assert!(registry.contains("src/a.rs"));
	assert!(registry.contains("b.rs"));
	assert!(!registry.contains("src"));
}

#[test]
fn find_file_with_extensions_tries_extensions_in_order() {
	let registry = registry(&["src/utils.rs", "src/utils.js", "src/plain"]);
	assert_eq!(
		registry.find_file_with_extensions("src/utils", &["js", "rs"]),
		Some("src/utils.js".to_string())
	);
	assert_eq!(
		registry.find_file_with_extensions("src/plain", &["rs", ""]),
		Some("src/plain".to_string())
	);
	assert_eq!(registry.find_file_with_extensions("src/missing", &["rs", "js"]), None);
}

#[test]
fn lookup_tolerates_mixed_separators() {
	let registry = registry(&["src/main.rs", "src\\utils\\helper.rs", "lib/config.rs"]);
	assert_eq!(
		registry.find_exact_file("src/utils/helper.rs"),
		Some("src\\utils\\helper.rs".to_string())
	);
	assert_eq!(
		registry.find_exact_file("lib\\config.rs"),
		Some("lib/config.rs".to_string())
	);
	assert_eq!(registry.find_exact_file("src/nope.rs"), None);
}

#[test]
fn verbatim_lookup_does_not_normalize() {
	let registry = registry(&["src\\a.rs", "src/b.rs"]);
	assert_eq!(registry.find_verbatim("src/b.rs"), Some("src/b.rs"));
	assert_eq!(registry.find_verbatim("src/a.rs"), None);
	assert_eq!(registry.find_normalized("src/a.rs"), Some("src\\a.rs"));
}

#[test]
fn first_duplicate_wins() {
	let registry = registry(&["a/x.rs", "a\\x.rs"]);
	assert_eq!(registry.find_exact_file("a/x.rs"), Some("a/x.rs".to_string()));
}

#[test]
fn separators_normalize_to_forward_slashes() {
	assert_eq!(normalize_separators("src\\main.rs"), "src/main.rs");
	assert_eq!(normalize_separators("src\\utils/helper.rs"), "src/utils/helper.rs");
	assert_eq!(normalize_separators("src/utils\\helper.rs"), "src/utils/helper.rs");
	assert_eq!(normalize_separators(""), "");
	assert_eq!(normalize_separators("\\"), "/");
	assert_eq!(normalize_separators("/"), "/");
}

#[test]
fn normalize_path_collapses_dots_and_keeps_escapes() {
	assert_eq!(normalize_path("src/../lib.rs"), "lib.rs");
	assert_eq!(normalize_path("./src/./a.rs"), "src/a.rs");
	assert_eq!(normalize_path("a\\b\\..\\c.rs"), "a/c.rs");
	assert_eq!(normalize_path("../../x.rs"), "../../x.rs");
	assert_eq!(normalize_path("a/../../x.rs"), "../x.rs");
}

#[test]
fn relative_paths_resolve_against_the_source_directory() {
	assert_eq!(resolve_relative_path("src/main.rs", "../lib.rs"), "lib.rs");
	assert_eq!(resolve_relative_path("src/a/main.js", "./util"), "src/a/util");
	assert_eq!(resolve_relative_path("main.js", "./util"), "util");
	assert_eq!(resolve_relative_path("src/main.js", "../../outside"), "../outside");
}
