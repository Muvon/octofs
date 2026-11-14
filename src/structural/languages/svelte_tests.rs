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

//! Svelte embedded-script and graph tests.

use super::{script_sources, Svelte};
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, RelationType, SourceFile};
use crate::structural::languages::Language;
use tree_sitter::Parser;

fn embedded_sources(source: &str) -> Vec<(&'static str, String, u32)> {
	let mut parser = Parser::new();
	parser.set_language(&Svelte {}.get_ts_language()).unwrap();
	let tree = parser.parse(source, None).unwrap();
	Svelte {}
		.extract_embedded_sources(tree.root_node(), source)
		.into_iter()
		.map(|embedded| (embedded.language, embedded.contents, embedded.start_line))
		.collect()
}

fn registry(paths: &[&str]) -> FileRegistry {
	FileRegistry::new(&paths.iter().map(|path| path.to_string()).collect::<Vec<_>>())
}

fn has_edge(graph: &CodeGraph, source: &str, target: &str, relation: RelationType) -> bool {
	graph
		.relationships
		.iter()
		.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn script_language_follows_the_lang_attribute() {
	let src =
		"<script lang=\"ts\">\nlet a: number = 1;\n</script>\n\n<script context=\"module\">\nlet b = 2;\n</script>\n";
	let sources = embedded_sources(src);
	let languages: Vec<&str> = sources.iter().map(|(language, _, _)| *language).collect();
	assert_eq!(languages, vec!["typescript", "javascript"]);
}

#[test]
fn embedded_source_reports_the_row_it_starts_at() {
	let src = "<h1>Title</h1>\n<script>\nlet a = 1;\n</script>\n";
	let sources = embedded_sources(src);
	assert_eq!(sources.len(), 1);
	assert!(sources[0].1.contains("let a = 1;"));
	assert_eq!(sources[0].2, 1);
}

#[test]
fn embedded_sources_match_script_sources() {
	let src = "<script lang=\"ts\">\nexport let name: string;\n</script>\n<p>{name}</p>\n";
	let from_trait: Vec<(&str, String)> = embedded_sources(src)
		.into_iter()
		.map(|(language, contents, _)| (language, contents))
		.collect();
	let from_outline: Vec<(&str, String)> = script_sources(src)
		.into_iter()
		.map(|(language, contents, _, _)| (language, contents))
		.collect();
	assert_eq!(from_trait, from_outline);
}

#[test]
fn markup_without_script_has_no_embedded_sources() {
	assert!(embedded_sources("<div>hello</div>\n").is_empty());
}

#[test]
fn imports_resolve_like_javascript() {
	let files = registry(&["src/util.ts", "src/Button.svelte"]);
	let svelte = Svelte {};
	assert_eq!(
		svelte.resolve_import("./util", "src/App.svelte", &files),
		Some("src/util.ts".to_string())
	);
	assert_eq!(
		svelte.resolve_import("./Button.svelte", "src/App.svelte", &files),
		Some("src/Button.svelte".to_string())
	);
	assert_eq!(svelte.resolve_import("svelte", "src/App.svelte", &files), None);
}

#[test]
fn script_block_joins_the_graph_as_typescript() {
	// `get_language` knows svelte, so the plain `build_graph` path is enough.
	let graph = build_graph(vec![
		SourceFile {
			path: "src/util.ts".to_string(),
			language: "typescript",
			contents: "export function greet(): void {}\n".to_string(),
		},
		SourceFile {
			path: "src/App.svelte".to_string(),
			language: "svelte",
			contents: "<script lang=\"ts\">\n\timport { greet } from './util';\n\tfunction main() {\n\t\tgreet();\n\t}\n</script>\n\n<p>hi</p>\n"
				.to_string(),
		},
	]);
	assert!(graph.skipped.is_empty());
	assert!(has_edge(
		&graph,
		"src/App.svelte::main",
		"src/util.ts::greet",
		RelationType::Calls
	));
	assert!(has_edge(&graph, "src/App.svelte", "src/util.ts", RelationType::Imports));
	let main = &graph.nodes["src/App.svelte::main"];
	assert_eq!(main.start_line, 3);
}
