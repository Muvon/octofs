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

//! Go outline extraction tests.

use super::Go;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, CodeRelationship, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&Go {}.get_ts_language()).expect("go grammar");
	parser.parse(source, None).expect("parse")
}

/// Every node of `kind` in document order, found with an iterative walk.
fn nodes_of_kind<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
	let mut found = Vec::new();
	let mut cursor = tree.walk();
	loop {
		let node = cursor.node();
		if node.kind() == kind {
			found.push(node);
		}
		if cursor.goto_first_child() {
			continue;
		}
		while !cursor.goto_next_sibling() {
			if !cursor.goto_parent() {
				return found;
			}
		}
	}
}

fn nth_node<'tree>(tree: &'tree Tree, kind: &str, index: usize) -> Node<'tree> {
	nodes_of_kind(tree, kind)
		.get(index)
		.copied()
		.unwrap_or_else(|| panic!("no {kind} node #{index} in source"))
}

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|file| file.to_string()).collect();
	FileRegistry::new(&owned)
}

fn target(name: &str, qualifier: Option<&str>) -> CallTarget {
	CallTarget {
		name: name.to_string(),
		qualifier: qualifier.map(str::to_string),
	}
}

#[test]
fn functions_methods_and_type_specs_are_items() {
	let src = "package main\n\nfunc main() {}\n\nfunc (s *T) Method() {}\n\ntype Widget struct {\n\tX int\n}\n";
	let items = outline("a.go", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("function", "main"), ("method", "Method"), ("type", "Widget")]
	);
}

#[test]
fn grouped_type_declarations_yield_one_item_per_spec() {
	let src = "package p\n\ntype (\n\tA struct{ X int }\n\tB int\n)\n";
	let items = outline("a.go", src).unwrap();
	let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
	assert_eq!(names, vec!["A", "B"]);
}

#[test]
fn graph_symbol_kinds_match_the_outline_and_extension_is_go() {
	assert_eq!(Go {}.get_symbol_kinds(), Go {}.get_outline_kinds());
}

#[test]
fn a_method_receiver_becomes_the_symbol_owner() {
	for source in [
		"package main\n\nfunc (s *Server) Handle() {}\n",
		"package main\n\nfunc (s Server) Handle() {}\n",
		"package main\n\nfunc (s *pkg.Server) Handle() {}\n",
	] {
		let tree = parse(source);
		let method = nth_node(&tree, "method_declaration", 0);
		assert_eq!(
			Go {}.extract_symbol_owner(method, source),
			Some("Server".to_string()),
			"receiver in {source:?}"
		);
	}
}

#[test]
fn a_generic_receiver_reports_the_bare_type_as_the_owner() {
	let source = "package main\n\nfunc (s *Store[T]) Handle() {}\n";
	let tree = parse(source);
	assert_eq!(
		Go {}.extract_symbol_owner(nth_node(&tree, "method_declaration", 0), source),
		Some("Store".to_string())
	);
}

#[test]
fn a_package_level_function_has_no_symbol_owner() {
	let source = "package main\n\nfunc Run() {}\n";
	let tree = parse(source);
	assert_eq!(
		Go {}.extract_symbol_owner(nth_node(&tree, "function_declaration", 0), source),
		None
	);
}

#[test]
fn a_single_import_yields_its_quoted_path_with_or_without_an_alias() {
	for (source, expected) in [
		("package main\n\nimport \"fmt\"\n", "fmt"),
		(
			"package main\n\nimport alias \"example.com/pkg/thing\"\n",
			"example.com/pkg/thing",
		),
	] {
		let tree = parse(source);
		let (imports, exports) = Go {}.extract_imports_exports(nth_node(&tree, "import_declaration", 0), source);
		assert_eq!(imports, vec![expected.to_string()]);
		assert!(exports.is_empty());
	}
}

#[test]
fn a_grouped_import_block_yields_every_path_and_skips_comments() {
	let source = "package main\n\nimport (\n\t\"os\"\n\tstr \"strings\"\n\t// a standalone comment line\n\t_ \"github.com/lib/pq\" // driver comment\n)\n";
	let tree = parse(source);
	let (imports, _) = Go {}.extract_imports_exports(nth_node(&tree, "import_declaration", 0), source);
	assert_eq!(imports, vec!["os", "strings", "github.com/lib/pq"]);
}

#[test]
fn only_uppercase_function_and_method_names_are_exported() {
	let source = "package main\n\nfunc Exported() {}\n\nfunc unexported() {}\n";
	let tree = parse(source);
	let (_, exported) = Go {}.extract_imports_exports(nth_node(&tree, "function_declaration", 0), source);
	assert_eq!(exported, vec!["Exported".to_string()]);
	let (_, unexported) = Go {}.extract_imports_exports(nth_node(&tree, "function_declaration", 1), source);
	assert!(unexported.is_empty());

	let method_source = "package main\n\nfunc (s *Server) Handle() {}\n";
	let method_tree = parse(method_source);
	let (_, method_exports) =
		Go {}.extract_imports_exports(nth_node(&method_tree, "method_declaration", 0), method_source);
	assert_eq!(method_exports, vec!["Handle".to_string()]);
}

#[test]
fn exported_type_var_and_const_names_are_reported() {
	// A type/var/const name lives one level down inside its spec node.
	for (source, kind, expected) in [
		("package main\n\ntype Server struct{}\n", "type_declaration", "Server"),
		("package main\n\nvar Public = 1\n", "var_declaration", "Public"),
		("package main\n\nconst Public = 1\n", "const_declaration", "Public"),
	] {
		let tree = parse(source);
		let (_, exports) = Go {}.extract_imports_exports(nth_node(&tree, kind, 0), source);
		assert_eq!(exports, vec![expected], "exports for {kind}");
	}

	let source = "package main\n\ntype server struct{}\n";
	let tree = parse(source);
	let (_, exports) = Go {}.extract_imports_exports(nth_node(&tree, "type_declaration", 0), source);
	assert!(exports.is_empty(), "{exports:?}");
}

#[test]
fn calls_keep_their_receiver_as_the_qualifier() {
	let source = "package main\n\nfunc main() {\n\thelper()\n\tfmt.Println(1)\n\tpkg.Sub.Deep()\n}\n";
	let tree = parse(source);
	let targets: Vec<CallTarget> = nodes_of_kind(&tree, "call_expression")
		.into_iter()
		.flat_map(|node| Go {}.extract_function_calls(node, source))
		.collect();
	assert_eq!(
		targets,
		vec![
			target("helper", None),
			target("Println", Some("fmt")),
			target("Deep", Some("pkg::Sub")),
		]
	);
}

#[test]
fn a_node_that_is_not_a_call_expression_yields_no_call_targets() {
	let source = "package main\n\nfunc main() {\n\tx := 1\n\t_ = x\n}\n";
	let tree = parse(source);
	assert!(Go {}
		.extract_function_calls(nth_node(&tree, "short_var_declaration", 0), source)
		.is_empty());
}

#[test]
fn an_embedded_struct_field_becomes_an_extends_relation() {
	let source = "package main\n\ntype Server struct {\n\tLogger\n\tmu sync.Mutex\n\tName string\n}\n";
	let tree = parse(source);
	let go = Go {};
	assert_eq!(
		go.extract_type_relations(nth_node(&tree, "field_declaration", 0), source),
		vec![(TypeRelationKind::Extends, "Logger".to_string())]
	);
	// Named fields are ordinary composition, not embedding.
	assert!(go
		.extract_type_relations(nth_node(&tree, "field_declaration", 1), source)
		.is_empty());
	assert!(go
		.extract_type_relations(nth_node(&tree, "field_declaration", 2), source)
		.is_empty());
	// Go has no syntactic `implements`, so the type node itself has no relations.
	assert!(go
		.extract_type_relations(nth_node(&tree, "type_spec", 0), source)
		.is_empty());
}

#[test]
fn a_relative_import_resolves_to_a_file_in_the_target_directory() {
	let files = registry(&[
		"pkg/main.go",
		"pkg/util/helper.go",
		"internal/models/user.go",
		"README.md",
	]);
	let go = Go {};
	assert_eq!(
		go.resolve_import("./util", "pkg/main.go", &files),
		Some("pkg/util/helper.go".to_string())
	);
	assert_eq!(
		go.resolve_import("../internal/models", "pkg/main.go", &files),
		Some("internal/models/user.go".to_string())
	);
}

#[test]
fn a_package_import_matches_a_directory_path_suffix() {
	let files = registry(&["pkg/main.go", "vendor/github.com/lib/pq/pq.go"]);
	assert_eq!(
		Go {}.resolve_import("github.com/lib/pq", "pkg/main.go", &files),
		Some("vendor/github.com/lib/pq/pq.go".to_string())
	);
}

#[test]
fn a_package_import_also_matches_on_the_last_path_segment_alone() {
	// The resolver falls back to the final segment, so an unrelated module
	// path resolves to a local directory of the same name.
	let files = registry(&["pkg/main.go", "internal/models/user.go"]);
	assert_eq!(
		Go {}.resolve_import("example.com/other/models", "pkg/main.go", &files),
		Some("internal/models/user.go".to_string())
	);
}

#[test]
fn an_unknown_third_party_import_resolves_to_nothing() {
	let files = registry(&["pkg/main.go", "internal/models/user.go"]);
	assert_eq!(
		Go {}.resolve_import("example.com/nope/missing", "pkg/main.go", &files),
		None
	);
}

#[test]
fn a_standard_library_import_never_matches_a_directory_that_merely_ends_with_its_name() {
	let go = Go {};
	let files = registry(&["cmd/main.go", "repos/repo.go", "vendor/ios/ios.go"]);
	assert_eq!(go.resolve_import("os", "cmd/main.go", &files), None);
	// Only the Go source tree names such a directory: `src/os` or `os`.
	let files = registry(&["cmd/main.go", "src/os/os.go"]);
	assert_eq!(
		go.resolve_import("os", "cmd/main.go", &files),
		Some("src/os/os.go".to_string())
	);
	let files = registry(&["cmd/main.go", "src/net/http/server.go"]);
	assert_eq!(
		go.resolve_import("net/http", "cmd/main.go", &files),
		Some("src/net/http/server.go".to_string())
	);
	// A project directory of the same name somewhere else is not the library.
	let files = registry(&["cmd/main.go", "pkg/log/log.go", "internal/errors/errors.go"]);
	assert_eq!(go.resolve_import("log", "cmd/main.go", &files), None);
	assert_eq!(go.resolve_import("errors", "cmd/main.go", &files), None);
}

#[test]
fn the_longest_directory_suffix_of_the_import_path_wins() {
	let files = registry(&["cmd/main.go", "store/other.go", "app/internal/store/store.go"]);
	assert_eq!(
		Go {}.resolve_import("example.com/app/internal/store", "cmd/main.go", &files),
		Some("app/internal/store/store.go".to_string())
	);
}

#[test]
fn a_package_import_picks_the_alphabetically_first_non_test_file() {
	let files = registry(&["cmd/main.go", "lib/z.go", "lib/a_test.go", "lib/b.go"]);
	assert_eq!(
		Go {}.resolve_import("example.com/x/lib", "cmd/main.go", &files),
		Some("lib/b.go".to_string())
	);
	assert_eq!(
		Go {}.resolve_import("./../lib", "cmd/main.go", &files),
		Some("lib/b.go".to_string())
	);
}

#[test]
fn a_package_import_names_every_non_test_file_of_the_package() {
	let files = registry(&["cmd/main.go", "lib/z.go", "lib/a_test.go", "lib/b.go", "lib/notes.md"]);
	assert_eq!(
		Go {}.resolve_import_files("example.com/x/lib", "cmd/main.go", &files),
		vec!["lib/b.go".to_string(), "lib/z.go".to_string()]
	);
	assert_eq!(
		Go {}.resolve_import_files("./../lib", "cmd/main.go", &files),
		vec!["lib/b.go".to_string(), "lib/z.go".to_string()]
	);
	let only_tests = registry(&["cmd/main.go", "lib/a_test.go", "lib/b_test.go"]);
	assert_eq!(
		Go {}.resolve_import_files("example.com/x/lib", "cmd/main.go", &only_tests),
		vec!["lib/a_test.go".to_string(), "lib/b_test.go".to_string()]
	);
	assert!(Go {}
		.resolve_import_files("example.com/nope", "cmd/main.go", &files)
		.is_empty());
}

#[test]
fn a_call_through_a_package_import_finds_the_function_in_any_file_of_the_package() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "go",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("src/net/http/server.go", "package http\n\nfunc Serve() {}\n"),
		file("src/net/http/client.go", "package http\n\nfunc Do() {}\n"),
		file(
			"cmd/app/main.go",
			"package main\n\nimport \"net/http\"\n\nfunc main() {\n\thttp.Do()\n}\n",
		),
	]);
	// One import edge per package, to its canonical (first) file; the other
	// files answer through the package map.
	assert!(edge(
		&graph,
		"cmd/app/main.go",
		"src/net/http/client.go",
		RelationType::Imports
	)
	.is_some());
	assert!(edge(
		&graph,
		"cmd/app/main.go",
		"src/net/http/server.go",
		RelationType::Imports
	)
	.is_none());
	assert_eq!(
		graph.package_of.get("src/net/http/server.go").map(String::as_str),
		Some("src/net/http/client.go")
	);
	assert!(edge(
		&graph,
		"cmd/app/main.go::main",
		"src/net/http/client.go::Do",
		RelationType::Calls
	)
	.is_some());
}

fn edge<'a>(graph: &'a CodeGraph, source: &str, target: &str, relation: RelationType) -> Option<&'a CodeRelationship> {
	graph
		.relationships
		.iter()
		.find(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn build_graph_links_calls_imports_and_struct_embedding_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "go",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("util/util.go", "package util\n\ntype Logger struct{}\n\nfunc Helper() {}\n"),
		file(
			"cmd/main.go",
			"package main\n\nimport \"example.com/app/util\"\n\ntype Server struct {\n\tutil.Logger\n}\n\nfunc main() {\n\tutil.Helper()\n}\n",
		),
	]);

	assert!(edge(&graph, "cmd/main.go", "util/util.go", RelationType::Imports).is_some());
	let call = edge(&graph, "cmd/main.go::main", "util/util.go::Helper", RelationType::Calls).expect("cross-file call");
	assert_eq!(call.line, Some(10));
	assert!(edge(
		&graph,
		"cmd/main.go::Server",
		"util/util.go::Logger",
		RelationType::Extends
	)
	.is_some());
}

#[test]
fn embedding_in_the_same_file_extends_the_embedded_type_and_never_itself() {
	let graph = build_graph(vec![SourceFile {
		path: "app/app.go".to_string(),
		language: "go",
		contents: "package app\n\ntype Logger struct{}\n\ntype Store interface{}\n\ntype Server struct {\n\tLogger\n}\n\ntype Cache struct {\n\t*Logger\n}\n\ntype Backend interface {\n\tStore\n\tClose() error\n}\n".to_string(),
	}]);

	assert!(edge(
		&graph,
		"app/app.go::Server",
		"app/app.go::Logger",
		RelationType::Extends
	)
	.is_some());
	assert!(edge(&graph, "app/app.go::Cache", "app/app.go::Logger", RelationType::Extends).is_some());
	assert!(edge(
		&graph,
		"app/app.go::Backend",
		"app/app.go::Store",
		RelationType::Extends
	)
	.is_some());
	assert!(edge(
		&graph,
		"app/app.go::Logger",
		"app/app.go::Logger",
		RelationType::Extends
	)
	.is_none());
}

#[test]
fn a_function_of_the_same_package_is_reached_without_an_import() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "go",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("p/helper.go", "package p\n\nfunc Helper() {}\n"),
		file("q/helper.go", "package q\n\nfunc Helper() {}\n"),
		file("p/run.go", "package p\n\nfunc Run() {\n\tHelper()\n}\n"),
	]);
	assert!(edge(&graph, "p/run.go::Run", "p/helper.go::Helper", RelationType::Calls).is_some());
	assert!(edge(&graph, "p/run.go::Run", "q/helper.go::Helper", RelationType::Calls).is_none());
}

#[test]
fn a_dotless_import_is_the_modules_own_package_when_a_go_mod_holds_the_rest() {
	let go = Go {};
	// `module myapp`: `myapp/internal/log` is the directory next to the go.mod.
	let files = registry(&["go.mod", "cmd/main.go", "internal/log/log.go"]);
	assert_eq!(
		go.resolve_import("myapp/internal/log", "cmd/main.go", &files),
		Some("internal/log/log.go".to_string())
	);
	// The standard-library `log` is no project directory.
	assert_eq!(go.resolve_import("log", "cmd/main.go", &files), None);
	// A go.mod in a subdirectory roots the module there.
	let files = registry(&["svc/go.mod", "svc/cmd/main.go", "svc/store/store.go"]);
	assert_eq!(
		go.resolve_import("svc/store", "svc/cmd/main.go", &files),
		Some("svc/store/store.go".to_string())
	);
	// The standard library still never matches a directory elsewhere.
	let files = registry(&["go.mod", "cmd/main.go", "third_party/os/os.go"]);
	assert_eq!(go.resolve_import("os", "cmd/main.go", &files), None);
	assert_eq!(go.resolve_import("net/http", "cmd/main.go", &files), None);
}
