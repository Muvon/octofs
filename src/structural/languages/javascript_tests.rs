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

//! JavaScript outline and graph-extraction tests.

use super::{parse_js_export_statement, parse_js_import_statement_full_path, JavaScript};
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser};

/// Pre-order walk of the parsed source, calling `visit` on every node.
fn for_each_node(source: &str, mut visit: impl FnMut(Node, &str)) {
	let mut parser = Parser::new();
	parser.set_language(&JavaScript {}.get_ts_language()).unwrap();
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

fn calls_in(source: &str) -> Vec<CallTarget> {
	let mut calls = Vec::new();
	for_each_node(source, |node, contents| {
		calls.extend(JavaScript {}.extract_function_calls(node, contents))
	});
	calls
}

fn imports_exports_in(source: &str) -> (Vec<String>, Vec<String>) {
	let (mut imports, mut exports) = (Vec::new(), Vec::new());
	for_each_node(source, |node, contents| {
		let (found_imports, found_exports) = JavaScript {}.extract_imports_exports(node, contents);
		imports.extend(found_imports);
		exports.extend(found_exports);
	});
	(imports, exports)
}

fn relations_in(source: &str) -> Vec<(TypeRelationKind, String)> {
	let mut relations = Vec::new();
	for_each_node(source, |node, contents| {
		relations.extend(JavaScript {}.extract_type_relations(node, contents))
	});
	relations
}

fn registry(paths: &[&str]) -> FileRegistry {
	FileRegistry::new(&paths.iter().map(|path| path.to_string()).collect::<Vec<_>>())
}

fn has_edge(graph: &crate::structural::graph::CodeGraph, source: &str, target: &str, relation: RelationType) -> bool {
	graph
		.relationships
		.iter()
		.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn functions_methods_classes_and_named_arrows_are_items() {
	let src = "import x from \"y\";\n\nfunction foo() {}\n\nclass Bar {\n\tmethod() {}\n}\n\nconst baz = () => {};\n";
	let items = outline("a.js", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("function", "foo"),
			("class", "Bar"),
			("method", "method"),
			("function", "baz")
		]
	);
}

#[test]
fn imports_exports_and_anonymous_arrows_are_not_items() {
	let src = "import x from \"y\";\nexport const k = 1;\ncall(() => {});\n";
	assert!(outline("a.js", src).unwrap().is_empty());
}

#[test]
fn symbol_kinds_match_outline_kinds() {
	assert_eq!(JavaScript {}.get_symbol_kinds(), JavaScript {}.get_outline_kinds());
}

#[test]
fn import_statement_yields_module_path() {
	assert_eq!(
		parse_js_import_statement_full_path("import { a, b } from './mod';"),
		Some(vec!["./mod".to_string()])
	);
	assert_eq!(
		parse_js_import_statement_full_path("import './styles.css';"),
		Some(vec!["./styles.css".to_string()])
	);
	let (imports, _) = imports_exports_in("import * as fs from \"fs\";\nimport def from 'pkg';\n");
	assert_eq!(imports, vec!["fs", "pkg"]);
}

#[test]
fn export_forms_yield_names() {
	assert_eq!(
		parse_js_export_statement("export function foo() {}"),
		Some(vec!["foo".to_string()])
	);
	assert_eq!(
		parse_js_export_statement("export const bar = 1;"),
		Some(vec!["bar".to_string()])
	);
	assert_eq!(
		parse_js_export_statement("export { a, b as c }"),
		Some(vec!["a".to_string(), "b".to_string()])
	);
	assert_eq!(
		parse_js_export_statement("export default class Widget {\n}"),
		Some(vec!["Widget".to_string()])
	);
	assert_eq!(parse_js_export_statement("export default foo;"), None);
}

#[test]
fn calls_keep_qualifier_and_include_constructors() {
	let calls = calls_in("foo();\nservice.run();\nconst x = new Widget();\n");
	assert_eq!(
		calls,
		vec![
			CallTarget {
				name: "foo".to_string(),
				qualifier: None
			},
			CallTarget {
				name: "run".to_string(),
				qualifier: Some("service".to_string())
			},
			CallTarget {
				name: "Widget".to_string(),
				qualifier: None
			},
		]
	);
}

#[test]
fn class_extends_is_a_type_relation() {
	let relations = relations_in("class Child extends Base {}\nclass Plain {}\n");
	assert_eq!(relations, vec![(TypeRelationKind::Extends, "Base".to_string())]);
}

#[test]
fn methods_are_owned_by_their_class() {
	let mut owners = Vec::new();
	for_each_node("class Foo {\n\tbar() {}\n}\nfunction free() {}\n", |node, contents| {
		if node.kind() == "method_definition" || node.kind() == "function_declaration" {
			owners.push(JavaScript {}.extract_symbol_owner(node, contents));
		}
	});
	assert_eq!(owners, vec![Some("Foo".to_string()), None]);
}

#[test]
fn relative_import_resolves_with_extension_and_index() {
	let files = registry(&["src/a.js", "src/lib/index.js", "src/b.ts"]);
	let js = JavaScript {};
	assert_eq!(
		js.resolve_import("./a", "src/main.js", &files),
		Some("src/a.js".to_string())
	);
	assert_eq!(
		js.resolve_import("./lib", "src/main.js", &files),
		Some("src/lib/index.js".to_string())
	);
	assert_eq!(
		js.resolve_import("./b", "src/main.js", &files),
		Some("src/b.ts".to_string())
	);
	assert_eq!(
		js.resolve_import("../src/a.js", "src/main.js", &files),
		Some("src/a.js".to_string())
	);
}

#[test]
fn bare_module_import_resolves_under_src_then_the_root_and_not_for_externals() {
	let files = registry(&["src/helpers.js", "shared/thing/index.js", "node_modules/pkg/index.js"]);
	let js = JavaScript {};
	assert_eq!(
		js.resolve_import("helpers", "app/main.js", &files),
		Some("src/helpers.js".to_string())
	);
	assert_eq!(
		js.resolve_import("shared/thing", "app/main.js", &files),
		Some("shared/thing/index.js".to_string())
	);
	assert_eq!(js.resolve_import("react", "app/main.js", &files), None);
	// No package.json or tsconfig knowledge: an installed package is external.
	assert_eq!(js.resolve_import("pkg", "app/main.js", &files), None);
}

#[test]
fn a_same_named_file_elsewhere_is_never_a_match() {
	let files = registry(&["lib/nested/helpers.js", "other/util.js"]);
	let js = JavaScript {};
	assert_eq!(js.resolve_import("helpers", "app/main.js", &files), None);
	assert_eq!(js.resolve_import("./helpers", "app/main.js", &files), None);
	assert_eq!(js.resolve_import("./util", "src/main.js", &files), None);
}

#[test]
fn graph_links_calls_imports_and_inheritance_across_files() {
	let graph = build_graph(vec![
		SourceFile {
			path: "a.js".to_string(),
			language: "javascript",
			contents: "export function greet() {}\nexport class Base {}\n".to_string(),
		},
		SourceFile {
			path: "b.js".to_string(),
			language: "javascript",
			contents: "import { greet, Base } from './a';\nclass Child extends Base {}\nexport function main() {\n\tgreet();\n}\n"
				.to_string(),
		},
	]);
	assert!(has_edge(&graph, "b.js::main", "a.js::greet", RelationType::Calls));
	assert!(has_edge(&graph, "b.js", "a.js", RelationType::Imports));
	assert!(has_edge(&graph, "b.js::Child", "a.js::Base", RelationType::Extends));
}

#[test]
fn require_and_dynamic_import_with_a_string_are_imports_and_bind_names() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "javascript",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("lib/x.js", "function helper() {}\nmodule.exports = { helper };\n"),
		// A second `helper` rules out the unique-name guess: only the
		// destructured binding can lead the call to lib/x.js.
		file("other/z.js", "function helper() {}\n"),
		file(
			"app/y.js",
			"const { helper: assist } = require('../lib/x');\nfunction run() {\n\tassist();\n}\nasync function load() {\n\treturn import('../lib/x');\n}\nconst name = './computed';\nrequire(name);\n",
		),
	]);

	assert!(has_edge(&graph, "app/y.js", "lib/x.js", RelationType::Imports));
	assert!(has_edge(
		&graph,
		"app/y.js::run",
		"lib/x.js::helper",
		RelationType::Calls
	));
	assert!(!graph
		.relationships
		.iter()
		.any(|edge| edge.relation == RelationType::Imports && edge.target.contains("computed")));
}

#[test]
fn class_field_arrows_and_object_property_functions_are_symbols_that_call() {
	let graph = build_graph(vec![SourceFile {
		path: "a.js".to_string(),
		language: "javascript",
		contents: "function helper() {}\nclass Store {\n\thandle = () => {\n\t\thelper();\n\t};\n}\nconst api = {\n\trun: function () {\n\t\thelper();\n\t},\n};\n"
			.to_string(),
	}]);
	assert!(has_edge(
		&graph,
		"a.js::Store::handle",
		"a.js::helper",
		RelationType::Calls
	));
	assert!(has_edge(&graph, "a.js::run", "a.js::helper", RelationType::Calls));
}

#[test]
fn a_file_that_starts_with_a_byte_order_mark_is_read_like_any_other() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "javascript",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("b.js", "\u{feff}export function a() {}\n"),
		file(
			"c.js",
			"\u{feff}import { a } from './b';\nfunction run() {\n\ta();\n}\n",
		),
	]);
	assert!(has_edge(&graph, "c.js", "b.js", RelationType::Imports));
	assert!(has_edge(&graph, "c.js::run", "b.js::a", RelationType::Calls));
}

#[test]
fn an_import_attribute_does_not_become_part_of_the_module_path() {
	assert_eq!(
		super::parse_js_import_statement_full_path("import data from './d.json' with { type: 'json' };"),
		Some(vec!["./d.json".to_string()])
	);
	assert_eq!(
		super::parse_js_import_statement_full_path("import './styles.css';"),
		Some(vec!["./styles.css".to_string()])
	);
}

#[test]
fn a_default_import_binds_its_name_to_the_imported_file() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "javascript",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"lib/x.js",
			"export default function helper() {}\nexport function other() {}\n",
		),
		file("other/z.js", "function helper() {}\n"),
		file(
			"app/y.js",
			"import helper, { other } from '../lib/x';\nfunction run() {\n\thelper();\n\tother();\n}\n",
		),
	]);
	assert!(has_edge(
		&graph,
		"app/y.js::run",
		"lib/x.js::helper",
		RelationType::Calls
	));
	assert!(has_edge(
		&graph,
		"app/y.js::run",
		"lib/x.js::other",
		RelationType::Calls
	));
}

#[test]
fn a_default_import_calls_the_default_export_whatever_it_is_named() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "javascript",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("lib/v.js", "export default function draw() {}\n"),
		file("other/z.js", "function draw() {}\n"),
		file(
			"app/y.js",
			"import render from '../lib/v';\nfunction run() {\n\trender();\n}\n",
		),
	]);
	assert!(has_edge(&graph, "app/y.js::run", "lib/v.js::draw", RelationType::Calls));
	assert!(!has_edge(
		&graph,
		"app/y.js::run",
		"other/z.js::draw",
		RelationType::Calls
	));
}

#[test]
fn functions_assigned_to_exports_and_prototypes_are_symbols() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "javascript",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![file(
		"lib/x.js",
		"function helper() {}\nexports.f = () => {\n\thelper();\n};\nmodule.exports.g = function () {\n\thelper();\n};\nWidget.prototype.m = function () {\n\thelper();\n};\n",
	)]);
	for name in ["f", "g", "m"] {
		assert!(
			has_edge(
				&graph,
				&format!("lib/x.js::{name}"),
				"lib/x.js::helper",
				RelationType::Calls
			),
			"{name} is a symbol that calls helper"
		);
	}
}
