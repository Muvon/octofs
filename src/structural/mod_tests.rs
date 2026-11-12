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

//! Tests for the structural outline API: extension mapping, item extraction,
//! signature line spans and the special markdown/svelte paths.

use super::{is_supported, outline, MAX_SIGNATURE_LINES};

#[test]
fn extension_mapping_covers_all_supported_languages() {
	for (path, expected) in [
		("a.rs", "rust"),
		("a.py", "python"),
		("a.js", "javascript"),
		("a.mjs", "javascript"),
		("a.jsx", "javascript"),
		("a.ts", "typescript"),
		("a.tsx", "typescript"),
		("a.go", "go"),
		("a.java", "java"),
		("a.cpp", "cpp"),
		("a.cc", "cpp"),
		("a.c", "cpp"),
		("a.h", "cpp"),
		("a.cppm", "cpp"),
		("a.php", "php"),
		("a.sh", "bash"),
		("a.bash", "bash"),
		("a.rb", "ruby"),
		("a.lua", "lua"),
		("a.json", "json"),
		("a.swift", "swift"),
		("a.svelte", "svelte"),
		("a.css", "css"),
		("a.scss", "css"),
		("a.sass", "css"),
		("a.ex", "elixir"),
		("a.exs", "elixir"),
		("a.md", "markdown"),
		("a.markdown", "markdown"),
	] {
		assert!(is_supported(path), "{path} should be supported");
		let src = match expected {
			"markdown" => "# h\n",
			_ => "",
		};
		// The language resolves (no unsupported-extension error); an empty
		// file simply has no items.
		assert!(outline(path, src).is_ok(), "{path} should resolve to {expected}");
	}
	// Case-insensitive and URL-shaped labels resolve by their last component.
	assert!(is_supported("SRC/A.RS"));
	assert!(is_supported("ssh://host/dir/a.rs"));
	assert!(!is_supported("a.txt"));
	assert!(!is_supported("Makefile"));
}

#[test]
fn unsupported_extension_error_names_it_and_lists_languages() {
	let err = outline("data.xyz", "").unwrap_err();
	assert!(err.contains("'xyz'"), "got: {err}");
	assert!(err.contains("rust, python"), "got: {err}");

	let no_ext = outline("Makefile", "").unwrap_err();
	assert!(no_ext.contains("no file extension"), "got: {no_ext}");
}

#[test]
fn rust_items_carry_one_based_lines_and_signature_spans() {
	let src = "mod foo {\n\tfn bar() {}\n}\nstruct Baz;\n";
	let items = outline("a.rs", src).unwrap();
	let summary: Vec<(&str, &str, usize, usize)> = items
		.iter()
		.map(|i| (i.kind.as_str(), i.name.as_str(), i.start_line, i.sig_lines))
		.collect();
	assert_eq!(
		summary,
		vec![
			("module", "foo", 1, 1),
			("function", "bar", 2, 1),
			("struct", "Baz", 4, 1),
		]
	);
}

#[test]
fn long_signatures_are_capped_at_max_signature_lines() {
	let src = "fn long<T>(\n\tx: T,\n) where\n\tT: Clone,\n{\n\ttodo!()\n}\n";
	let items = outline("a.rs", src).unwrap();
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].name, "long");
	assert_eq!(items[0].sig_lines, MAX_SIGNATURE_LINES);
}

#[test]
fn python_bodies_open_at_the_colon_line() {
	let src = "class Foo:\n\tdef bar(self):\n\t\tpass\n\ndef baz():\n\tpass\n";
	let items = outline("a.py", src).unwrap();
	let summary: Vec<(&str, &str, usize)> = items
		.iter()
		.map(|i| (i.kind.as_str(), i.name.as_str(), i.start_line))
		.collect();
	assert_eq!(
		summary,
		vec![("class", "Foo", 1), ("function", "bar", 2), ("function", "baz", 5)]
	);
}

#[test]
fn markdown_outline_is_headings_and_ignores_fenced_code() {
	let src = "# Title\n\ntext\n\n## Section\n\n```bash\n# not a heading\n```\n\n### Deep\n";
	let items = outline("a.md", src).unwrap();
	let summary: Vec<(&str, &str, usize)> = items
		.iter()
		.map(|i| (i.kind.as_str(), i.name.as_str(), i.start_line))
		.collect();
	assert_eq!(
		summary,
		vec![
			("heading1", "Title", 1),
			("heading2", "Section", 5),
			("heading3", "Deep", 11)
		]
	);
}

#[test]
fn svelte_outline_comes_from_the_script_block_at_file_coordinates() {
	let src = "<script>\n\tfunction foo() {}\n\n\tconst bar = () => {};\n</script>\n\n<div>hi</div>\n";
	let items = outline("a.svelte", src).unwrap();
	let summary: Vec<(String, usize)> = items.iter().map(|i| (i.name.clone(), i.start_line)).collect();
	assert_eq!(summary, vec![("foo".to_string(), 2), ("bar".to_string(), 4)]);
}

#[test]
fn broken_source_still_yields_recovered_items() {
	// A syntax error must not panic; tree-sitter recovers the intact prefix.
	let src = "fn good() {}\nfn broken( {\n";
	let items = outline("a.rs", src).unwrap();
	assert!(items.iter().any(|i| i.name == "good"), "items: {items:?}");
}

#[test]
fn empty_file_has_no_items() {
	assert!(outline("a.rs", "").unwrap().is_empty());
	assert!(outline("a.md", "").unwrap().is_empty());
}

#[test]
fn nameless_declarations_are_skipped() {
	// An anonymous arrow function declares nothing the outline can name.
	let src = "call(() => {});\n";
	assert!(outline("a.js", src).unwrap().is_empty());
}
