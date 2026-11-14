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

//! TypeScript outline and graph-extraction tests.

use super::TypeScript;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser};

/// Pre-order walk of the parsed source, calling `visit` on every node.
fn for_each_node(source: &str, mut visit: impl FnMut(Node, &str)) {
	let mut parser = Parser::new();
	parser.set_language(&TypeScript {}.get_ts_language()).unwrap();
	let tree = parser.parse(source, None).unwrap();
	let mut cursor = tree.walk();
	loop {
		visit(cursor.node(), source);
		if cursor.goto_first_child() {
			continue;
		}
		while !cursor.goto_next_sibling() {
			if !cursor.goto_parent() {
				return;
			}
		}
	}
}

fn imports_exports_in(source: &str) -> (Vec<String>, Vec<String>) {
	let (mut imports, mut exports) = (Vec::new(), Vec::new());
	for_each_node(source, |node, contents| {
		let (found_imports, found_exports) = TypeScript {}.extract_imports_exports(node, contents);
		imports.extend(found_imports);
		exports.extend(found_exports);
	});
	(imports, exports)
}

fn relations_in(source: &str) -> Vec<(TypeRelationKind, String)> {
	let mut relations = Vec::new();
	for_each_node(source, |node, contents| {
		relations.extend(TypeScript {}.extract_type_relations(node, contents))
	});
	relations
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
fn interfaces_enums_and_type_aliases_are_items() {
	let src = "interface Foo {\n\ta: number;\n}\n\nenum Color {\n\tA,\n}\n\ntype Alias = string;\n\nfunction bar(x: number): void {}\n";
	let items = outline("a.ts", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("interface", "Foo"),
			("enum", "Color"),
			("type", "Alias"),
			("function", "bar"),
		]
	);
}

#[test]
fn symbol_kinds_include_declarations_but_not_namespaces() {
	let kinds = TypeScript {}.get_symbol_kinds();
	assert!(kinds.contains(&"class_declaration"));
	assert!(kinds.contains(&"enum_declaration"));
	assert!(!kinds.contains(&"internal_module"));
}

#[test]
fn imports_yield_module_paths_even_for_type_imports() {
	let (imports, _) = imports_exports_in("import type { Foo } from './foo';\nimport Bar from \"bar\";\n");
	assert_eq!(imports, vec!["./foo", "bar"]);
}

#[test]
fn exports_cover_declarations_and_type_exports() {
	let (_, exports) = imports_exports_in("export interface Shape {}\nexport type { A, B };\n");
	assert!(exports.contains(&"Shape".to_string()));
	assert!(exports.contains(&"A".to_string()));
	assert!(exports.contains(&"B".to_string()));
}

#[test]
fn calls_keep_qualifier() {
	let mut calls = Vec::new();
	for_each_node("helper();\nsvc.run();\n", |node, contents| {
		calls.extend(TypeScript {}.extract_function_calls(node, contents));
	});
	assert_eq!(
		calls,
		vec![
			CallTarget {
				name: "helper".to_string(),
				qualifier: None
			},
			CallTarget {
				name: "run".to_string(),
				qualifier: Some("svc".to_string())
			},
		]
	);
}

#[test]
fn class_and_interface_heritage_are_type_relations() {
	let relations =
		relations_in("class A extends B implements C, D<T> {}\ninterface E extends F, G {}\nclass Plain {}\n");
	assert_eq!(
		relations,
		vec![
			(TypeRelationKind::Extends, "B".to_string()),
			(TypeRelationKind::Implements, "C".to_string()),
			(TypeRelationKind::Implements, "D".to_string()),
			(TypeRelationKind::Extends, "F".to_string()),
			(TypeRelationKind::Extends, "G".to_string()),
		]
	);
}

#[test]
fn relative_import_prefers_typescript_files() {
	let files = registry(&["src/a.js", "src/a.ts", "src/models/index.ts", "types/shim.d.ts"]);
	let ts = TypeScript {};
	assert_eq!(
		ts.resolve_import("./a", "src/main.ts", &files),
		Some("src/a.ts".to_string())
	);
	assert_eq!(
		ts.resolve_import("./models", "src/main.ts", &files),
		Some("src/models/index.ts".to_string())
	);
	assert_eq!(
		ts.resolve_import("shim", "src/main.ts", &files),
		Some("types/shim.d.ts".to_string())
	);
	assert_eq!(ts.resolve_import("left-pad", "src/main.ts", &files), None);
}

#[test]
fn an_emitted_js_specifier_resolves_to_the_typescript_source() {
	let files = registry(&[
		"src/a.ts",
		"src/view.tsx",
		"src/esm.mts",
		"src/cjs.cts",
		"src/types.d.ts",
		"src/real.js",
		"src/real.ts",
	]);
	let ts = TypeScript {};
	for (specifier, expected) in [
		("./a.js", "src/a.ts"),
		("./view.js", "src/view.tsx"),
		("./view.jsx", "src/view.tsx"),
		("./esm.mjs", "src/esm.mts"),
		("./cjs.cjs", "src/cjs.cts"),
		("./types.js", "src/types.d.ts"),
		// The literal file wins when it exists.
		("./real.js", "src/real.js"),
	] {
		assert_eq!(
			ts.resolve_import(specifier, "src/main.ts", &files),
			Some(expected.to_string()),
			"{specifier}"
		);
	}
	assert_eq!(ts.resolve_import("./missing.js", "src/main.ts", &files), None);
}

#[test]
fn a_same_named_file_in_another_directory_is_never_a_match() {
	let files = registry(&["lib/deep/helpers.ts"]);
	assert_eq!(TypeScript {}.resolve_import("./helpers", "src/main.ts", &files), None);
}

#[test]
fn abstract_classes_are_symbols() {
	assert!(TypeScript {}.get_symbol_kinds().contains(&"abstract_class_declaration"));
}

#[test]
fn graph_links_calls_imports_inheritance_and_implementation() {
	let graph = build_graph(vec![
		SourceFile {
			path: "a.ts".to_string(),
			language: "typescript",
			contents: "export interface Shape {}\nexport class Base {}\nexport function helper(): void {}\n".to_string(),
		},
		SourceFile {
			path: "b.ts".to_string(),
			language: "typescript",
			contents: "import { Shape, Base, helper } from './a';\nclass Circle extends Base implements Shape {}\nexport function run(): void {\n\thelper();\n}\n"
				.to_string(),
		},
	]);
	assert!(has_edge(&graph, "b.ts::run", "a.ts::helper", RelationType::Calls));
	assert!(has_edge(&graph, "b.ts", "a.ts", RelationType::Imports));
	assert!(has_edge(&graph, "b.ts::Circle", "a.ts::Base", RelationType::Extends));
	assert!(has_edge(
		&graph,
		"b.ts::Circle",
		"a.ts::Shape",
		RelationType::Implements
	));
}
