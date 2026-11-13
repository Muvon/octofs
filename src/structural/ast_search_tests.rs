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

//! Unit tests for AST-pattern search.

use super::{ast_language, miss_diagnostic, search_source, AstMatch};
use crate::structural::language_for_extension;

fn matches(pairs: &[(usize, usize)]) -> Vec<AstMatch> {
	pairs
		.iter()
		.map(|&(line, line_count)| AstMatch { line, line_count })
		.collect()
}

#[test]
fn search_source_matches_rust_javascript_python_and_go() {
	assert_eq!(
		search_source("rust", "fn a() {}\nfn b(x: u8, y: u8) {}\n", "fn $F($$$A) {}"),
		Ok(matches(&[(1, 1), (2, 1)]))
	);
	assert_eq!(
		search_source(
			"javascript",
			"function a() {}\nfunction b(x, y) {}\n",
			"function $F($$$A) {}"
		),
		Ok(matches(&[(1, 1), (2, 1)]))
	);
	// Python: a trailing colon without a body parses but matches nothing —
	// drop the colon or include the body.
	assert_eq!(
		search_source(
			"python",
			"def a():\n    pass\n\n\ndef b(x, y):\n    pass\n",
			"def $F($$$A):\n    $$$B"
		),
		Ok(matches(&[(1, 2), (5, 2)]))
	);
	assert_eq!(
		search_source("go", "func a() {}\n\nfunc b(x uint8, y uint8) {}\n", "func $F($$$A) {}"),
		Ok(matches(&[(1, 1), (3, 1)]))
	);
}
#[test]
fn search_source_matches_the_other_expando_languages() {
	// Languages whose identifiers cannot start with `$` rewrite metavariables
	// to the expando char before parsing; without the rewrite every one of
	// these returns zero matches.
	assert_eq!(
		search_source("ruby", "def a\nend\n", "def $F\nend"),
		Ok(matches(&[(1, 2)]))
	);
	assert_eq!(
		search_source("php", "<?php\nfunction a() {}\n", "function $F() {}"),
		Ok(matches(&[(2, 1)]))
	);
	assert_eq!(
		search_source("swift", "func a() {}\n", "func $F() {}"),
		Ok(matches(&[(1, 1)]))
	);
	assert_eq!(
		search_source("elixir", "def a do\nend\n", "def $F do\nend"),
		Ok(matches(&[(1, 2)]))
	);
	assert_eq!(
		search_source("cpp", "if (a) { a;b;c; }", "if (a) { $$$A }"),
		Ok(matches(&[(1, 1)]))
	);
	assert_eq!(search_source("cpp", "expr->b()", "$A->b()"), Ok(matches(&[(1, 1)])));
	assert_eq!(
		search_source("lua", "print('Hello World')\n", "print($S)"),
		Ok(matches(&[(1, 1)]))
	);
}

#[test]
fn search_source_renders_multi_line_spans() {
	assert_eq!(
		search_source("rust", "fn a() {\n\tlet x = 1;\n}\n", "fn $F() { $$$A }"),
		Ok(matches(&[(1, 3)]))
	);
}
#[test]
fn search_source_offsets_svelte_script_blocks_to_file_rows() {
	let svelte = "<h1>hi</h1>\n<script>\n\tfunction greet() {}\n</script>\n";
	assert_eq!(
		search_source("svelte", svelte, "function $F() {}"),
		Ok(matches(&[(3, 1)]))
	);
}

#[test]
fn search_source_reports_pattern_parse_errors() {
	// `fn $F($$$A) {}` parses in rust but is two top-level nodes in python.
	assert!(search_source("python", "def a():\n    pass\n", "fn $F($$$A) {}").is_err());
	// A bare multi metavariable is invalid everywhere.
	assert!(search_source("rust", "fn a() {}\n", "$$$A").is_err());
}

#[test]
fn search_source_survives_ten_thousand_levels_of_nesting() {
	// ast-grep's traversal is an explicit stack machine; a pathological file
	// must not overflow the real stack, and still match at full depth.
	let deep = format!("fn f() {{ {}1{} }}", "(".repeat(10000), ")".repeat(10000));
	assert_eq!(search_source("rust", &deep, "($A)").map(|m| m.len()), Ok(10000));
}

#[test]
fn miss_diagnostic_names_the_parsed_kind_and_hints() {
	let out = miss_diagnostic("fn $F($$$A) {}", "rust");
	assert!(
		out.contains("No AST matches for pattern \"fn $F($$$A) {}\" in rust."),
		"got: {out}"
	);
	assert!(out.contains("kind=function_item"), "got: {out}");
	assert!(out.contains("parse_error=false"), "got: {out}");
	assert!(out.contains("metavars=$A,$F"), "got: {out}");
	assert!(
		out.contains("`$NAME` matches one node and `$$$NAME` a sequence"),
		"got: {out}"
	);
	assert!(out.contains("Try a plain `content` search"), "got: {out}");
	// A pattern that does not parse reports that instead of a parsed kind.
	let out = miss_diagnostic("fn $F($$$A) {}", "python");
	assert!(out.contains("does not parse as python"), "got: {out}");
	assert!(out.contains("Multiple AST nodes"), "got: {out}");
	assert!(!out.contains('µ'), "expando leaked: {out}");
}

#[test]
fn search_source_matches_plain_c_with_the_c_grammar() {
	// The C++ grammar parses this file as a `declaration`, so ellipsis-body
	// patterns miss; the C grammar parses it as a function_definition.
	assert_eq!(
		search_source(
			"c",
			"int add(int a, int b) { return a + b; }\n",
			"int $NAME($$$ARGS) { $$$BODY }"
		),
		Ok(matches(&[(1, 1)]))
	);
	assert_eq!(
		search_source(
			"c",
			"static void run(void) { work(); }\n",
			"static void $NAME(void) { $$$BODY }"
		),
		Ok(matches(&[(1, 1)]))
	);
}

#[test]
fn ast_language_routes_c_and_h_to_the_c_grammar_only() {
	// AST mode: .c/.h use the C grammar like upstream ast-grep; .cpp and
	// friends stay with the C++ grammar.
	assert_eq!(ast_language("x.c"), Some("c"));
	assert_eq!(ast_language("x.h"), Some("c"));
	assert_eq!(ast_language("x.cpp"), Some("cpp"));
	assert_eq!(ast_language("x.hpp"), Some("cpp"));
	// The outline and symbol modes keep the shared C++ mapping.
	assert_eq!(language_for_extension("c"), Some("cpp"));
	assert_eq!(language_for_extension("h"), Some("cpp"));
}

#[test]
fn pattern_parse_errors_echo_the_user_pattern() {
	// The expando char is internal: errors must quote the `$` pattern.
	let error = search_source("python", "def a():\n    pass\n", "fn $NAME($$$ARGS) { $$$BODY }").unwrap_err();
	assert!(error.contains("fn $NAME($$$ARGS) { $$$BODY }"), "got: {error}");
	assert!(!error.contains('µ'), "got: {error}");
	let error = search_source("php", "<?php\na();\n", "$A = \"$NAME here\";").unwrap_err();
	assert!(error.contains("$A = \"$NAME here\";"), "got: {error}");
	assert!(!error.contains('µ'), "got: {error}");
}

#[test]
fn python_patterns_may_omit_the_body_only_with_the_colon() {
	// A trailing colon without a body parses but matches nothing; dropping
	// the colon (or keeping the body) matches.
	assert_eq!(
		search_source("python", "def a():\n    pass\n", "def $F($$$A):"),
		Ok(vec![])
	);
	assert_eq!(
		search_source("python", "def a():\n    pass\n", "def $F($$$A)"),
		Ok(matches(&[(1, 2)]))
	);
}
