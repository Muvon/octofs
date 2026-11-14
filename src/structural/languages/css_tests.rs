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

//! CSS outline and graph extraction tests.

use super::Css;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::Language;
use crate::structural::outline;
use tree_sitter::{Node, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&Css {}.get_ts_language()).expect("css grammar");
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

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|f| f.to_string()).collect();
	FileRegistry::new(&owned)
}

fn demo_styles() -> FileRegistry {
	registry(&[
		"demo/styles/main.css",
		"demo/styles/base.css",
		"demo/shared/reset.scss",
		"vendor.css",
	])
}

#[test]
fn rule_sets_are_named_by_their_first_selector() {
	let src = ".card {\n\tcolor: red;\n}\n\n#main {\n\tmargin: 0;\n}\n";
	let items = outline("a.css", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![(".card", 1), ("#main", 5)]);
}

#[test]
fn keyframes_are_named_by_their_identifier() {
	let src = "@keyframes spin {\n\tfrom {\n\t\trotate: 0;\n\t}\n}\n";
	let items = outline("a.css", src).unwrap();
	let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
	assert!(names.contains(&"spin"), "items: {items:?}");
}

#[test]
fn extensions_and_symbol_kinds() {
	let lang = Css {};
	assert_eq!(
		lang.get_symbol_kinds(),
		vec!["rule_set", "at_rule", "keyframes_statement", "media_statement"]
	);
}

#[test]
fn every_at_import_spelling_yields_the_imported_path() {
	for (source, expected) in [
		("@import \"base.css\";\n", "base.css"),
		("@import 'base.css';\n", "base.css"),
		("@import url(theme.css);\n", "theme.css"),
		("@import url(\"vendor/reset.css\");\n", "vendor/reset.css"),
	] {
		let tree = parse(source);
		let node = nodes_of_kind(tree.root_node(), "import_statement")[0];
		let (imports, exports) = Css {}.extract_imports_exports(node, source);
		assert_eq!(imports, vec![expected], "imports for {source:?}");
		assert!(exports.is_empty());
	}
}

#[test]
fn a_rule_set_is_neither_an_import_nor_an_export() {
	let source = ".card { color: red; }\n";
	let tree = parse(source);
	let node = nodes_of_kind(tree.root_node(), "rule_set")[0];
	let (imports, exports) = Css {}.extract_imports_exports(node, source);
	assert!(imports.is_empty());
	assert!(exports.is_empty());
}

#[test]
fn imports_resolve_relative_extensionless_bare_and_project_wide() {
	let files = demo_styles();
	let lang = Css {};
	let source = "demo/styles/main.css";
	assert_eq!(
		lang.resolve_import("./base.css", source, &files),
		Some("demo/styles/base.css".to_string())
	);
	assert_eq!(
		lang.resolve_import("../shared/reset", source, &files),
		Some("demo/shared/reset.scss".to_string())
	);
	assert_eq!(
		lang.resolve_import("base.css", source, &files),
		Some("demo/styles/base.css".to_string())
	);
	assert_eq!(
		lang.resolve_import("vendor.css", source, &files),
		Some("vendor.css".to_string())
	);
	assert_eq!(lang.resolve_import("./missing.css", source, &files), None);
	assert_eq!(lang.resolve_import("missing.css", source, &files), None);
}

#[test]
fn graph_links_an_import_to_the_stylesheet_it_names() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "css",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("styles/main.css", "@import \"base.css\";\n.card {\n\tcolor: red;\n}\n"),
		file("styles/base.css", ".base {\n\tmargin: 0;\n}\n"),
	]);
	assert!(graph.relationships.iter().any(|edge| edge.source == "styles/main.css"
		&& edge.target == "styles/base.css"
		&& edge.relation == RelationType::Imports));
	assert!(graph.nodes.contains_key("styles/main.css::.card"));
	// CSS has no calls or type relations.
	assert!(graph
		.relationships
		.iter()
		.all(|edge| matches!(edge.relation, RelationType::Contains | RelationType::Imports)));
}
