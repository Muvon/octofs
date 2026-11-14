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

//! End-to-end graph building tests: source in, graph out.

use super::*;
use crate::structural::graph::test_language_tests::lookup;

fn source(path: &str, language: &'static str, contents: &str) -> SourceFile {
	SourceFile {
		path: path.to_string(),
		language,
		contents: contents.to_string(),
	}
}

fn edge<'a>(graph: &'a CodeGraph, source: &str, target: &str, relation: RelationType) -> Option<&'a CodeRelationship> {
	graph
		.relationships
		.iter()
		.find(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

fn two_file_project() -> Vec<SourceFile> {
	vec![
		source(
			"src/main.rs",
			"rust",
			"use crate::util::helper;\nuse std::fmt;\nfn run() {\n\thelper();\n}\n",
		),
		source("src/util.rs", "rust", "pub fn helper() {}\n"),
	]
}

#[test]
fn file_and_symbol_nodes_carry_one_based_lines_and_contains_edges() {
	let graph = build_graph_with(two_file_project(), &lookup);

	let file = &graph.nodes["src/main.rs"];
	assert_eq!(file.kind, "source_file");
	assert_eq!(file.name, "main.rs");
	assert_eq!((file.start_line, file.end_line), (1, 5));
	assert_eq!(file.language, "rust");

	let run = &graph.nodes["src/main.rs::run"];
	assert_eq!(run.kind, "function");
	assert_eq!(run.path, "src/main.rs");
	assert_eq!((run.start_line, run.end_line), (3, 5));

	let contains = edge(&graph, "src/main.rs", "src/main.rs::run", RelationType::Contains).expect("contains edge");
	assert_eq!(contains.confidence, 1.0);
}

#[test]
fn imports_resolve_to_files_and_the_rest_become_external_nodes() {
	let graph = build_graph_with(two_file_project(), &lookup);

	let internal = edge(&graph, "src/main.rs", "src/util.rs", RelationType::Imports).expect("file import");
	assert_eq!(internal.confidence, 1.0);

	let external = edge(&graph, "src/main.rs", "external:std::fmt", RelationType::Imports).expect("external import");
	assert_eq!(external.confidence, 1.0);
	let node = &graph.nodes["external:std::fmt"];
	assert_eq!(node.kind, "external");
	assert_eq!(node.name, "std::fmt");
	assert_eq!(node.path, "");

	// A resolved import must not also leave an external node behind.
	assert!(!graph.nodes.contains_key("external:crate::util::helper"));
}

#[test]
fn an_import_that_resolves_to_the_importing_file_adds_no_edge() {
	let graph = build_graph_with(
		vec![source(
			"src/util.rs",
			"rust",
			"use crate::util::Error;\npub struct Error;\n",
		)],
		&lookup,
	);
	assert!(
		graph
			.relationships
			.iter()
			.all(|edge| edge.relation != RelationType::Imports),
		"{:?}",
		graph.relationships
	);
	assert!(!graph.nodes.keys().any(|id| id.starts_with("external:")));
}

#[test]
fn calls_resolve_through_imports() {
	let graph = build_graph_with(two_file_project(), &lookup);

	let call = edge(&graph, "src/main.rs::run", "src/util.rs::helper", RelationType::Calls).expect("call edge");
	assert_eq!(call.confidence, 0.85);
	assert_eq!(call.line, Some(4));
}

#[test]
fn every_edge_endpoint_is_a_node() {
	let graph = build_graph_with(two_file_project(), &lookup);

	for relationship in &graph.relationships {
		assert!(graph.nodes.contains_key(&relationship.source), "{relationship:?}");
		assert!(graph.nodes.contains_key(&relationship.target), "{relationship:?}");
	}
}

#[test]
fn input_order_does_not_change_the_graph() {
	let forward = build_graph_with(two_file_project(), &lookup);
	let mut reversed_input = two_file_project();
	reversed_input.reverse();
	let reversed = build_graph_with(reversed_input, &lookup);

	assert_eq!(forward.nodes, reversed.nodes);
	assert_eq!(forward.relationships, reversed.relationships);
}

#[test]
fn relationships_are_sorted_and_free_of_duplicates() {
	let graph = build_graph_with(
		vec![source("a.rs", "rust", "use std::fmt;\nuse std::fmt;\nfn f() {}\n")],
		&lookup,
	);

	let keys: Vec<_> = graph
		.relationships
		.iter()
		.map(|relationship| (&relationship.source, &relationship.target, relationship.relation))
		.collect();
	let mut sorted = keys.clone();
	sorted.sort();
	sorted.dedup();
	assert_eq!(keys, sorted);
}

#[test]
fn embedded_block_symbols_belong_to_the_host_file() {
	let graph = build_graph_with(
		vec![source("app.host", "host", "// one\n// two\n// three\n// four\n")],
		&lookup,
	);

	let helper = &graph.nodes["app.host::helper"];
	assert_eq!(helper.path, "app.host");
	assert_eq!(helper.start_line, 4);
	let call = edge(&graph, "app.host::run", "app.host::helper", RelationType::Calls).expect("embedded call");
	assert_eq!(call.line, Some(5));
}

#[test]
fn a_language_without_an_implementation_is_recorded_as_skipped() {
	let graph = build_graph(vec![
		source("a.rs", "rust", "fn main() {}\n"),
		source("legacy.cob", "cobol", "IDENTIFICATION DIVISION.\n"),
	]);

	assert!(graph.nodes.contains_key("a.rs"));
	assert!(!graph.nodes.contains_key("legacy.cob"));
	assert_eq!(graph.skipped.len(), 1);
	assert_eq!(graph.skipped[0].path, "legacy.cob");
	assert!(graph.skipped[0].reason.contains("cobol"), "{}", graph.skipped[0].reason);
}

#[test]
fn a_language_without_graph_extraction_contributes_only_its_file_node() {
	let graph = build_graph_with(vec![source("a.bare", "bare", "fn main() { helper(); }\n")], &lookup);

	assert_eq!(graph.nodes.len(), 1);
	assert!(graph.nodes.contains_key("a.bare"));
	assert!(graph.relationships.is_empty());
	assert!(graph.skipped.is_empty());
}

#[test]
fn an_empty_input_builds_an_empty_graph() {
	let graph = build_graph(Vec::new());

	assert!(graph.nodes.is_empty());
	assert!(graph.relationships.is_empty());
	assert!(graph.skipped.is_empty());
}

#[test]
fn relation_names_are_stable() {
	let names: Vec<&str> = [
		RelationType::Contains,
		RelationType::Imports,
		RelationType::Calls,
		RelationType::Extends,
		RelationType::Implements,
	]
	.into_iter()
	.map(RelationType::as_str)
	.collect();
	assert_eq!(names, vec!["contains", "imports", "calls", "extends", "implements"]);
}

#[test]
fn the_reader_supplies_each_files_text_and_a_failed_read_is_skipped() {
	let specs = vec![
		FileSpec {
			path: "a.rs".to_string(),
			language: "rust",
		},
		FileSpec {
			path: "gone.rs".to_string(),
			language: "rust",
		},
	];
	let graph = build_graph_from_reader(specs, &|path: &str| {
		if path == "a.rs" {
			Ok("fn a() {}\n".to_string())
		} else {
			Err(anyhow!("unreadable"))
		}
	});

	assert!(graph.nodes.contains_key("a.rs"));
	assert!(graph.nodes.contains_key("a.rs::a"));
	assert_eq!(graph.skipped.len(), 1);
	assert_eq!(graph.skipped[0].path, "gone.rs");
	assert_eq!(graph.skipped[0].reason, "unreadable");
}
