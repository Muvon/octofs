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

//! Unit tests for symbol navigation: definitions, references, breadcrumbs.

use super::super::languages;
use super::{find_definitions, find_references, line_breadcrumbs, NameMatcher};
use crate::structural::outline;

const RUST_SOURCE: &str = "\
use std::io;

pub struct Store {
	data: Vec<u8>,
}

impl Store {
	pub fn flush(&mut self) -> io::Result<()> {
		self.data.clear();
		Ok(())
	}

	fn drain(&mut self) {
		self.flush();
	}
}

fn main() {
	let mut store = Store { data: Vec::new() };
	store.flush();
	Store::drain(&mut store);
}
";

#[test]
fn name_matcher_exact_and_wildcards() {
	let exact = NameMatcher::new("flush").unwrap();
	assert!(exact.matches("flush"));
	assert!(!exact.matches("flushed"));
	assert!(!exact.is_wildcard());

	let prefix = NameMatcher::new("hand*").unwrap();
	assert!(prefix.matches("handle"));
	assert!(prefix.matches("handle_request"));
	// `*` spans zero characters too, as in a shell glob.
	assert!(prefix.matches("hand"));
	assert!(!prefix.matches("sand"));
	assert!(prefix.is_wildcard());

	let suffix = NameMatcher::new("*Config").unwrap();
	assert!(suffix.matches("AppConfig"));
	assert!(suffix.matches("Config"));
	assert!(!suffix.matches("AppConfiguration"));

	let both = NameMatcher::new("*er*").unwrap();
	assert!(both.matches("server"));
	assert!(!both.matches("svr"));
}

#[test]
fn rust_definitions_by_exact_name_and_wildcard() {
	let hits = find_definitions("a.rs", RUST_SOURCE, &NameMatcher::new("flush").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	let hit = &hits[0];
	assert_eq!(hit.line, 8);
	assert_eq!(hit.line_count, 4);
	assert_eq!(hit.column, 1);
	assert_eq!(hit.breadcrumb.as_deref(), Some("impl Store"));

	let hits = find_definitions("a.rs", RUST_SOURCE, &NameMatcher::new("*").unwrap()).unwrap();
	// struct Store, impl Store, flush, drain, main.
	assert_eq!(hits.len(), 5);
}

#[test]
fn definitions_across_python_typescript_and_go() {
	let py = "class Greeter:\n    def greet(self):\n        return 1\n\ndef main():\n    return Greeter()\n";
	let hits = find_definitions("a.py", py, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 2);
	assert_eq!(hits[0].breadcrumb.as_deref(), Some("class Greeter"));

	let ts = "interface Config {\n  name: string;\n}\n\nfunction load(): Config {\n  return { name: \"\" };\n}\n";
	let hits = find_definitions("a.ts", ts, &NameMatcher::new("load").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 5);
	assert_eq!(hits[0].breadcrumb, None);

	let go = "type Server struct {\n\tAddr string\n}\n\nfunc (s *Server) Listen() error {\n\treturn nil\n}\n";
	let hits = find_definitions("a.go", go, &NameMatcher::new("Listen").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 5);
}

#[test]
fn references_flag_definition_sites() {
	let hits = find_references("a.rs", RUST_SOURCE, &NameMatcher::new("flush").unwrap()).unwrap();
	// def line 8, usage in drain (line 14), usage in main (line 20).
	assert_eq!(hits.len(), 3);
	assert!(hits.iter().any(|h| h.line == 8 && h.is_definition));
	assert!(hits.iter().any(|h| h.line == 14 && !h.is_definition));
	assert!(hits.iter().any(|h| h.line == 20 && !h.is_definition));
	// The def-site identifier sits inside `fn flush`, so its breadcrumb is
	// the full chain — matching octocode's reference rendering.
	assert_eq!(
		hits.iter().find(|h| h.line == 8).unwrap().breadcrumb.as_deref(),
		Some("impl Store › fn flush")
	);
	assert_eq!(
		hits.iter().find(|h| h.line == 20).unwrap().breadcrumb.as_deref(),
		Some("fn main")
	);
}

#[test]
fn references_of_a_type_name() {
	let hits = find_references("a.rs", RUST_SOURCE, &NameMatcher::new("Store").unwrap()).unwrap();
	// struct Store (def), impl Store, Store { data: ... } in main, Store::drain.
	assert!(hits.iter().any(|h| h.is_definition && h.line == 3));
	assert_eq!(hits.len(), 4);
}

#[test]
fn line_breadcrumbs_track_innermost_container() {
	let breadcrumbs = line_breadcrumbs("a.rs", RUST_SOURCE).unwrap();
	// Line 1 (`use`) is top level; line 8 opens fn flush inside impl Store.
	assert_eq!(breadcrumbs[0], None);
	assert_eq!(breadcrumbs[7].as_deref(), Some("impl Store › fn flush"));
	assert_eq!(breadcrumbs[10].as_deref(), Some("impl Store › fn flush"));
	// Blank line 12 between methods is still inside impl Store.
	assert_eq!(breadcrumbs[11].as_deref(), Some("impl Store"));
	assert_eq!(breadcrumbs[17].as_deref(), Some("fn main"));
}

#[test]
fn line_breadcrumbs_none_without_containers_or_grammar() {
	assert!(line_breadcrumbs("a.md", "# Heading\n\ntext\n").is_none());
	assert!(line_breadcrumbs("a.css", "body { color: red; }\n").is_none());
	assert!(line_breadcrumbs("a.txt", "fn main() {}\n").is_none());
}

#[test]
fn unsupported_extension_is_an_error_for_a_single_file() {
	let err = find_definitions("a.txt", "fn main() {}", &NameMatcher::new("main").unwrap()).unwrap_err();
	assert!(err.starts_with("No grammar for extension 'txt'."), "{err}");
	assert!(err.contains("Supported languages:"));
	assert!(find_references("a.txt", "x", &NameMatcher::new("x").unwrap()).is_err());
}

#[test]
fn css_and_json_symbol_navigation_is_a_guided_error() {
	// css/json outlines are structural but name no navigable symbols: a
	// single-file lookup says so instead of reporting a plain miss.
	for (label, language) in [("a.css", "css"), ("a.scss", "css"), ("a.json", "json")] {
		let matcher = NameMatcher::new("x").unwrap();
		let err = find_definitions(label, "x", &matcher).unwrap_err();
		assert_eq!(
			err,
			format!(
				"symbol navigation is not supported for {language} files — use flags: \"s\" for an outline or `content` for text"
			)
		);
		assert!(find_references(label, "x", &matcher).is_err());
	}
}

/// Maximum node depth of a parse, computed iteratively (the test itself must
/// not overflow on the deep fixtures it validates).
fn max_tree_depth(language: &str, contents: &str) -> usize {
	let lang = languages::get_language(language).unwrap();
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&lang.get_ts_language()).unwrap();
	let tree = parser.parse(contents, None).unwrap();
	let mut max = 0;
	let mut stack = vec![(tree.root_node(), 1usize)];
	while let Some((node, depth)) = stack.pop() {
		max = max.max(depth);
		let mut cursor = node.walk();
		if cursor.goto_first_child() {
			loop {
				stack.push((cursor.node(), depth + 1));
				if !cursor.goto_next_sibling() {
					break;
				}
			}
		}
	}
	max
}

/// Every structural walk (outline, definitions, references, breadcrumbs)
/// must complete on a pathologically deep parse — tree-sitter itself handles
/// the depth, so only a recursive walker can abort here. Runs in the test
/// thread, not a spawned one with a bigger stack.
fn assert_deep_file_completes_all_walks(label: &str, language: &str, contents: &str) {
	assert!(
		max_tree_depth(language, contents) >= 5_000,
		"fixture must actually nest deeply for {label}"
	);
	outline(label, contents).unwrap();
	let matcher = NameMatcher::new("deep").unwrap();
	for result in [
		find_definitions(label, contents, &matcher),
		find_references(label, contents, &matcher),
	] {
		match result {
			Ok(_) => {}
			// css answers with the guided navigation error — that too proves
			// the walk completed instead of aborting.
			Err(e) if e.starts_with("symbol navigation is not supported") => {}
			Err(e) => panic!("{e}"),
		}
	}
	line_breadcrumbs(label, contents);
}

#[test]
fn deep_nesting_rust_completes_all_walks() {
	let contents = format!("fn deep() {}{}", "{".repeat(10_000), "}".repeat(10_000));
	assert_deep_file_completes_all_walks("a.rs", "rust", &contents);
	// The walks must not merely return: the deep function is still found.
	let matcher = NameMatcher::new("deep").unwrap();
	assert_eq!(find_definitions("a.rs", &contents, &matcher).unwrap().len(), 1);
	assert_eq!(find_references("a.rs", &contents, &matcher).unwrap().len(), 1);
	assert!(outline("a.rs", &contents)
		.unwrap()
		.iter()
		.any(|item| item.name == "deep"));
}

fn deep_nesting_javascript_completes_all_walks() {
	let contents = format!("function deep() {}{}", "{".repeat(10_000), "}".repeat(10_000));
	assert_deep_file_completes_all_walks("a.js", "javascript", &contents);
	let matcher = NameMatcher::new("deep").unwrap();
	assert_eq!(find_definitions("a.js", &contents, &matcher).unwrap().len(), 1);
}

#[test]
fn deep_nesting_css_completes_all_walks() {
	// Nested rule sets: the css grammar supports nesting, so `a{a{a{...`
	// parses as rule_set > block > rule_set > ... — one level per brace.
	let contents = format!("{}{}", "a{".repeat(10_000), "}".repeat(10_000));
	assert_deep_file_completes_all_walks("a.css", "css", &contents);
}

#[test]
fn hundred_thousand_sibling_file_walks_within_a_generous_bound() {
	// 100k top-level nodes: the pre-fix `child(i)` walk was O(siblings²)
	// (minutes in debug); the cursor walk is linear. The bound is loose so a
	// loaded machine cannot flake it — a regression lands orders of magnitude
	// above it, not near it.
	let contents = "// filler\n".repeat(100_000);
	let matcher = NameMatcher::new("zzz_absent").unwrap();
	let start = std::time::Instant::now();
	let definitions = find_definitions("a.rs", &contents, &matcher).unwrap();
	let references = find_references("a.rs", &contents, &matcher).unwrap();
	let breadcrumbs = line_breadcrumbs("a.rs", &contents);
	assert!(definitions.is_empty());
	assert!(references.is_empty());
	assert!(breadcrumbs.is_some());
	assert!(
		start.elapsed().as_secs() < 60,
		"walks took {:?} on a 100k-sibling file",
		start.elapsed()
	);
}

#[test]
fn cpp_names_are_the_declared_symbol_not_the_return_type() {
	let src = "int add(int a, int b) {\n\treturn a + b;\n}\n";
	for label in ["a.c", "a.cpp"] {
		let hits = find_definitions(label, src, &NameMatcher::new("add").unwrap()).unwrap();
		assert_eq!(hits.len(), 1, "{label}");
		assert_eq!(hits[0].line, 1, "{label}");
		let hits = find_definitions(label, src, &NameMatcher::new("int").unwrap()).unwrap();
		assert!(hits.is_empty(), "the return type is not the name: {label}");
		let references = find_references(label, src, &NameMatcher::new("add").unwrap()).unwrap();
		assert_eq!(references.len(), 1, "{label}");
		assert_eq!(references[0].breadcrumb.as_deref(), Some("fn add"), "{label}");
	}
}

#[test]
fn cpp_qualified_operator_and_destructor_names() {
	let src = "int Foo::bar() {\n\treturn 0;\n}\n\nFoo::~Foo() {\n}\n\nFoo Foo::operator+(const Foo& other) {\n\treturn *this;\n}\n";
	for name in ["bar", "~Foo", "operator+"] {
		let hits = find_definitions("a.cpp", src, &NameMatcher::new(name).unwrap()).unwrap();
		assert_eq!(hits.len(), 1, "looking for {name}");
	}
}

#[test]
fn definition_names_across_go_php_swift_and_elixir() {
	let go = "type Server struct {\n\tAddr string\n}\n\nfunc (s *Server) Listen() error {\n\treturn nil\n}\n";
	let hits = find_definitions("a.go", go, &NameMatcher::new("Listen").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 5);

	let php = "<?php\nclass Greeter {\n\tpublic function greet(): void {\n\t\techo \"hi\";\n\t}\n}\n";
	let hits = find_definitions("a.php", php, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 3);
	assert_eq!(hits[0].breadcrumb.as_deref(), Some("class Greeter"));

	let swift = "class Greeter {\n\tfunc greet() {}\n}\n";
	let hits = find_definitions("a.swift", swift, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].breadcrumb.as_deref(), Some("class Greeter"));

	let elixir = "defmodule Greeter do\n\tdef greet do\n\t\t:ok\n\tend\nend\n";
	let hits = find_definitions("a.ex", elixir, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 2);
	let references = find_references("a.ex", elixir, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert!(references[0].is_definition);
}

#[test]
fn typescript_namespace_containers_form_breadcrumbs() {
	let src = "namespace Util {\n\texport function load(): void {\n\t\treturn;\n\t}\n}\nconst done = true;\n";
	let references = find_references("a.ts", src, &NameMatcher::new("load").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert_eq!(
		references[0].breadcrumb.as_deref(),
		Some("namespace Util › function load")
	);
	let breadcrumbs = line_breadcrumbs("a.ts", src).unwrap();
	assert_eq!(breadcrumbs[2].as_deref(), Some("namespace Util › function load"));
	// The line after the namespace closes is top level again.
	assert_eq!(breadcrumbs[5], None);
}

#[test]
fn record_enum_and_union_containers_form_breadcrumbs() {
	let java = "record Point(int x) {\n\tint get() {\n\t\treturn x;\n\t}\n}\n";
	let references = find_references("a.java", java, &NameMatcher::new("get").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert_eq!(references[0].breadcrumb.as_deref(), Some("record Point › method get"));

	let php = "<?php\nenum Suit {\n\tcase Hearts;\n\tpublic function label(): string {\n\t\treturn \"h\";\n\t}\n}\n";
	let references = find_references("a.php", php, &NameMatcher::new("label").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert_eq!(references[0].breadcrumb.as_deref(), Some("enum Suit › method label"));

	let cpp = "union Value {\n\tint get() {\n\t\treturn 1;\n\t}\n};\n";
	let references = find_references("a.cpp", cpp, &NameMatcher::new("get").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert_eq!(references[0].breadcrumb.as_deref(), Some("union Value › fn get"));
}

#[test]
fn markdown_headings_are_symbol_definitions() {
	let src = "# Installation\n\n## Setup\n\nRun the installer.\n";
	let hits = find_definitions("a.md", src, &NameMatcher::new("Installation").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 1);
	assert!(hits[0].is_definition);
	let hits = find_definitions("a.md", src, &NameMatcher::new("*").unwrap()).unwrap();
	assert_eq!(hits.len(), 2);
	let references = find_references("a.md", src, &NameMatcher::new("Setup").unwrap()).unwrap();
	assert_eq!(references.len(), 1);
	assert!(references[0].is_definition);
}

#[test]
fn svelte_symbols_resolve_in_script_blocks_at_file_coordinates() {
	let src = "<script>\n\tfunction greet() {\n\t\treturn \"hi\";\n\t}\n\n\tlet message = greet();\n</script>\n\n<h1>title</h1>\n";
	let hits = find_definitions("a.svelte", src, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 2);
	let references = find_references("a.svelte", src, &NameMatcher::new("greet").unwrap()).unwrap();
	assert_eq!(references.len(), 2);
	assert!(references.iter().any(|hit| hit.line == 2 && hit.is_definition));
	assert!(references.iter().any(|hit| hit.line == 6 && !hit.is_definition));

	let ts = "<script lang=\"ts\">\n\texport function load(): void {}\n</script>\n";
	let hits = find_definitions("a.svelte", ts, &NameMatcher::new("load").unwrap()).unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].line, 2);
}
