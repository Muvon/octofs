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

//! Bash outline and graph extraction tests.

use super::Bash;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::Language;
use crate::structural::outline;
use tree_sitter::{Node, Tree};

fn parse_tree(source: &str) -> Tree {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&Bash {}.get_ts_language()).expect("bash grammar");
	parser.parse(source, None).expect("parse")
}

/// Pre-order, iterative collection of every node of `kind`.
fn nodes_of_kind<'a>(root: Node<'a>, kind: &str) -> Vec<Node<'a>> {
	let mut found = Vec::new();
	let mut cursor = root.walk();
	loop {
		if cursor.node().kind() == kind {
			found.push(cursor.node());
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

fn first_of_kind<'a>(tree: &'a Tree, kind: &str) -> Node<'a> {
	nodes_of_kind(tree.root_node(), kind)[0]
}

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|f| f.to_string()).collect();
	FileRegistry::new(&owned)
}

#[test]
fn named_functions_are_items() {
	let src = "foo() {\n\techo hi\n}\n\nbar() {\n\techo hi\n}\n";
	let items = outline("a.sh", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![("foo", 1), ("bar", 5)]);
}

#[test]
fn plain_commands_are_not_items() {
	assert!(outline("a.sh", "echo hi\nsource ./env.sh\n").unwrap().is_empty());
}

#[test]
fn language_metadata_is_wired_up() {
	let bash = Bash {};
	assert_eq!(bash.name(), "bash");
	assert_eq!(bash.get_symbol_kinds(), vec!["function_definition"]);
}

#[test]
fn source_statements_inside_a_function_become_imports() {
	let source = "setup() {\n\tsource ./lib/helpers.sh\n\t. \"./lib/other.sh\"\n}\n";
	let tree = parse_tree(source);
	let (imports, exports) = Bash {}.extract_imports_exports(first_of_kind(&tree, "function_definition"), source);
	assert_eq!(
		imports,
		vec!["./lib/helpers.sh".to_string(), "./lib/other.sh".to_string()]
	);
	assert!(exports.is_empty());
}

#[test]
fn a_nested_function_source_is_reported_once_by_its_own_node() {
	let source = "outer() {\n\tinner() {\n\t\tsource ./a.sh\n\t}\n}\n";
	let tree = parse_tree(source);
	let bash = Bash {};
	let imports: Vec<String> = nodes_of_kind(tree.root_node(), "function_definition")
		.into_iter()
		.flat_map(|node| bash.extract_imports_exports(node, source).0)
		.collect();
	assert_eq!(imports, vec!["./a.sh".to_string()]);
}

#[test]
fn a_top_level_source_command_is_an_import() {
	let source = "source ./lib/helpers.sh\n. \"./lib/other.sh\"\necho hi\n";
	let tree = parse_tree(source);
	let bash = Bash {};
	let imports: Vec<String> = nodes_of_kind(tree.root_node(), "command")
		.into_iter()
		.flat_map(|node| bash.extract_imports_exports(node, source).0)
		.collect();
	assert_eq!(
		imports,
		vec!["./lib/helpers.sh".to_string(), "./lib/other.sh".to_string()]
	);
}

#[test]
fn a_source_command_inside_a_function_is_left_to_the_function_node() {
	let source = "setup() {\n\tsource ./lib/helpers.sh\n}\n";
	let tree = parse_tree(source);
	let bash = Bash {};
	let (from_command, _) = bash.extract_imports_exports(first_of_kind(&tree, "command"), source);
	assert!(from_command.is_empty());
	// Together the whole file still reports it exactly once.
	let all: Vec<String> = nodes_of_kind(tree.root_node(), "function_definition")
		.into_iter()
		.chain(nodes_of_kind(tree.root_node(), "command"))
		.flat_map(|node| bash.extract_imports_exports(node, source).0)
		.collect();
	assert_eq!(all, vec!["./lib/helpers.sh".to_string()]);
}

#[test]
fn a_non_source_node_reports_no_imports() {
	let source = "echo hi\n";
	let tree = parse_tree(source);
	let (imports, _) = Bash {}.extract_imports_exports(first_of_kind(&tree, "command"), source);
	assert!(imports.is_empty());
}

#[test]
fn a_hyphenated_command_is_a_call_but_a_path_or_flag_is_not() {
	let bash = Bash {};
	for (source, expected) in [
		("my-func arg\n", Some("my-func")),
		("deploy_all\n", Some("deploy_all")),
		("./run-tests.sh\n", None),
		("/usr/bin/my-tool\n", None),
	] {
		let tree = parse_tree(source);
		let calls = bash.extract_function_calls(first_of_kind(&tree, "command"), source);
		assert_eq!(calls.first().map(|call| call.name.as_str()), expected, "{source:?}");
		assert!(calls.iter().all(|call| call.qualifier.is_none()), "{source:?}");
	}
}

#[test]
fn a_command_becomes_a_call_target_but_source_does_not() {
	let bash = Bash {};

	let source = "deploy --now\n";
	let tree = parse_tree(source);
	let calls = bash.extract_function_calls(first_of_kind(&tree, "command"), source);
	assert_eq!(calls.len(), 1);
	assert_eq!(calls[0].name, "deploy");
	assert_eq!(calls[0].qualifier, None);

	for text in ["source ./a.sh\n", ". ./a.sh\n"] {
		let tree = parse_tree(text);
		assert!(bash
			.extract_function_calls(first_of_kind(&tree, "command"), text)
			.is_empty());
	}

	// A leading environment assignment must not be mistaken for the command.
	let prefixed = "DEBUG=1 deploy\n";
	let tree = parse_tree(prefixed);
	let calls = bash.extract_function_calls(first_of_kind(&tree, "command"), prefixed);
	assert_eq!(calls[0].name, "deploy");
}

#[test]
fn a_non_command_node_yields_no_calls() {
	let source = "run() {\n\techo hi\n}\n";
	let tree = parse_tree(source);
	assert!(Bash {}
		.extract_function_calls(first_of_kind(&tree, "function_definition"), source)
		.is_empty());
}

#[test]
fn relative_and_absolute_sources_resolve_against_the_registry() {
	let bash = Bash {};
	let files = registry(&["scripts/build.sh", "scripts/lib/helpers.sh"]);
	assert_eq!(
		bash.resolve_import("./lib/helpers.sh", "scripts/build.sh", &files)
			.as_deref(),
		Some("scripts/lib/helpers.sh")
	);
	assert_eq!(
		bash.resolve_import("scripts/lib/helpers.sh", "scripts/build.sh", &files)
			.as_deref(),
		Some("scripts/lib/helpers.sh")
	);
	assert_eq!(bash.resolve_import("./nowhere.sh", "scripts/build.sh", &files), None);
}

#[test]
fn graph_links_calls_and_imports_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "bash",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"scripts/build.sh",
			"build() {\n\tsource ./lib/helpers.sh\n\thelper\n}\n",
		),
		file("scripts/lib/helpers.sh", "helper() {\n\techo hi\n}\n"),
	]);
	let has = |source: &str, target: &str, relation: RelationType| {
		graph
			.relationships
			.iter()
			.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
	};
	assert!(has("scripts/build.sh", "scripts/lib/helpers.sh", RelationType::Imports));
	assert!(has(
		"scripts/build.sh::build",
		"scripts/lib/helpers.sh::helper",
		RelationType::Calls
	));
}
