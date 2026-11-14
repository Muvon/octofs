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

//! Java outline extraction tests.

use super::Java;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, CodeRelationship, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&Java {}.get_ts_language()).expect("java grammar");
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

fn first_node<'tree>(tree: &'tree Tree, kind: &str) -> Node<'tree> {
	*nodes_of_kind(tree, kind)
		.first()
		.unwrap_or_else(|| panic!("no {kind} node in source"))
}

fn demo_project() -> FileRegistry {
	let files: Vec<String> = [
		"demo/src/main/java/com/example/app/Service.java",
		"demo/src/main/java/com/example/app/Widget.java",
	]
	.iter()
	.map(|file| file.to_string())
	.collect();
	FileRegistry::new(&files)
}

#[test]
fn classes_methods_and_constructors_are_items() {
	let src = "package p;\n\npublic class Foo {\n\tprivate int x;\n\n\tpublic Foo() {}\n\n\tvoid bar() {}\n}\n";
	let items = outline("a.java", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("class", "Foo"), ("constructor", "Foo"), ("method", "bar")]
	);
}

#[test]
fn a_methods_name_is_not_its_return_type() {
	let src = "class A {\n\tString name() {\n\t\treturn \"\";\n\t}\n}\n";
	let items = outline("a.java", src).unwrap();
	assert_eq!(items.len(), 2);
	assert_eq!(items[1].name, "name");
}

#[test]
fn graph_symbol_kinds_match_the_outline_and_extension_is_java() {
	assert_eq!(Java {}.get_symbol_kinds(), Java {}.get_outline_kinds());
}

#[test]
fn imports_are_stripped_down_to_their_dotted_path() {
	for (source, expected) in [
		("import java.util.List;\n", "java.util.List"),
		("import static java.util.Arrays.asList;\n", "java.util.Arrays.asList"),
		("import java.util.*;\n", "java.util.*"),
	] {
		let tree = parse(source);
		let (imports, exports) = Java {}.extract_imports_exports(first_node(&tree, "import_declaration"), source);
		assert_eq!(imports, vec![expected], "imports for {source:?}");
		assert!(exports.is_empty(), "exports for {source:?}");
	}
}

#[test]
fn a_package_declaration_is_exported_with_a_package_prefix() {
	let source = "package com.example.app;\n";
	let tree = parse(source);
	let (imports, exports) = Java {}.extract_imports_exports(first_node(&tree, "package_declaration"), source);
	assert!(imports.is_empty());
	assert_eq!(exports, vec!["package:com.example.app"]);
}

#[test]
fn only_public_types_are_exported_and_the_kind_prefixes_the_name() {
	for (source, kind, expected) in [
		("public class Service {}\n", "class_declaration", vec!["class:Service"]),
		(
			"public interface Shape {}\n",
			"interface_declaration",
			vec!["interface:Shape"],
		),
		("public enum Status { OK }\n", "enum_declaration", vec!["enum:Status"]),
		(
			"public record Point(int x, int y) {}\n",
			"record_declaration",
			vec!["record:Point"],
		),
		(
			"public @interface Marker {}\n",
			"annotation_type_declaration",
			vec!["annotation:Marker"],
		),
		("class Service {}\n", "class_declaration", vec![]),
	] {
		let tree = parse(source);
		let (imports, exports) = Java {}.extract_imports_exports(first_node(&tree, kind), source);
		assert!(imports.is_empty(), "imports for {source:?}");
		assert_eq!(exports, expected, "exports for {source:?}");
	}
}

#[test]
fn only_public_methods_are_exported() {
	let source = "class Service {\n\tpublic void run() {}\n\tvoid hidden() {}\n}\n";
	let tree = parse(source);
	let methods = nodes_of_kind(&tree, "method_declaration");
	let (_, exports) = Java {}.extract_imports_exports(methods[0], source);
	assert_eq!(exports, vec!["method:run"]);
	let (_, exports) = Java {}.extract_imports_exports(methods[1], source);
	assert!(exports.is_empty());
}

#[test]
fn method_invocations_keep_their_receiver_as_a_qualifier() {
	let source = "class Service {\n\tvoid run() {\n\t\thelper();\n\t\tthis.repo.save(name);\n\t}\n}\n";
	let tree = parse(source);
	let targets: Vec<CallTarget> = nodes_of_kind(&tree, "method_invocation")
		.into_iter()
		.flat_map(|node| Java {}.extract_function_calls(node, source))
		.collect();
	assert_eq!(
		targets,
		vec![
			CallTarget {
				name: "helper".to_string(),
				qualifier: None,
			},
			CallTarget {
				name: "save".to_string(),
				qualifier: Some("this::repo".to_string()),
			},
		]
	);
}

#[test]
fn object_creation_reports_the_constructed_type_as_the_call_target() {
	let source = "class Service {\n\tvoid run() {\n\t\tWidget w = new Widget();\n\t}\n}\n";
	let tree = parse(source);
	assert_eq!(
		Java {}.extract_function_calls(first_node(&tree, "object_creation_expression"), source),
		vec![CallTarget {
			name: "Widget".to_string(),
			qualifier: None,
		}]
	);
}

#[test]
fn a_method_reference_is_not_reported_as_a_call() {
	let source = "class Service {\n\tSupplier<String> s = Service::create;\n}\n";
	let tree = parse(source);
	assert!(Java {}
		.extract_function_calls(first_node(&tree, "method_reference"), source)
		.is_empty());
}

#[test]
fn a_class_reports_its_superclass_then_each_implemented_interface() {
	let source = "class Service extends Base implements Runnable, Closeable {}\n";
	let tree = parse(source);
	assert_eq!(
		Java {}.extract_type_relations(first_node(&tree, "class_declaration"), source),
		vec![
			(TypeRelationKind::Extends, "Base".to_string()),
			(TypeRelationKind::Implements, "Runnable".to_string()),
			(TypeRelationKind::Implements, "Closeable".to_string()),
		]
	);
}

#[test]
fn interfaces_extend_while_enums_and_records_implement() {
	for (source, kind, expected) in [
		(
			"interface Shape extends Drawable, Sized {}\n",
			"interface_declaration",
			vec![
				(TypeRelationKind::Extends, "Drawable".to_string()),
				(TypeRelationKind::Extends, "Sized".to_string()),
			],
		),
		(
			"enum Status implements Serializable { OK }\n",
			"enum_declaration",
			vec![(TypeRelationKind::Implements, "Serializable".to_string())],
		),
		(
			// The generic argument is dropped by `simple_type_name`.
			"record Point(int x, int y) implements Comparable<Point> {}\n",
			"record_declaration",
			vec![(TypeRelationKind::Implements, "Comparable".to_string())],
		),
		("class Plain {}\n", "class_declaration", vec![]),
	] {
		let tree = parse(source);
		assert_eq!(
			Java {}.extract_type_relations(first_node(&tree, kind), source),
			expected,
			"relations for {source:?}"
		);
	}
}

#[test]
fn a_dotted_import_resolves_to_the_matching_source_file() {
	assert_eq!(
		Java {}.resolve_import(
			"com.example.app.Service",
			"demo/src/main/java/com/example/app/Widget.java",
			&demo_project()
		),
		Some("demo/src/main/java/com/example/app/Service.java".to_string())
	);
}

#[test]
fn a_wildcard_import_names_every_file_of_the_package_directory() {
	assert_eq!(
		Java {}.resolve_import_files("com.example.app.*", "x/Main.java", &demo_project()),
		vec![
			"demo/src/main/java/com/example/app/Service.java".to_string(),
			"demo/src/main/java/com/example/app/Widget.java".to_string()
		]
	);
	assert_eq!(
		Java {}.resolve_import_files("com.example.app.Widget", "x/Main.java", &demo_project()),
		vec!["demo/src/main/java/com/example/app/Widget.java".to_string()]
	);
	assert!(Java {}
		.resolve_import_files("java.util.*", "x/Main.java", &demo_project())
		.is_empty());
}

#[test]
fn a_wildcard_import_resolves_to_the_first_file_of_the_package_directory() {
	assert_eq!(
		Java {}.resolve_import(
			"com.example.app.*",
			"demo/src/main/java/com/example/app/Widget.java",
			&demo_project()
		),
		Some("demo/src/main/java/com/example/app/Service.java".to_string())
	);
	// A package directory the project does not have stays external.
	assert_eq!(
		Java {}.resolve_import(
			"java.util.*",
			"demo/src/main/java/com/example/app/Widget.java",
			&demo_project()
		),
		None
	);
}

#[test]
fn an_import_must_match_the_whole_package_path_not_just_the_class_name() {
	let files = FileRegistry::new(&[
		"a/src/org/other/Service.java".to_string(),
		"b/src/com/example/Service.java".to_string(),
		"c/src/xcom/example/Service.java".to_string(),
	]);
	let source = "a/App.java";
	assert_eq!(
		Java {}.resolve_import("com.example.Service", source, &files),
		Some("b/src/com/example/Service.java".to_string())
	);
	// `xcom/example/` is not `com/example/`: components must match whole.
	assert_eq!(Java {}.resolve_import("om.example.Service", source, &files), None);
}

#[test]
fn a_call_on_a_non_identifier_receiver_keeps_the_callee_name() {
	let source = "class Service {\n\tvoid run() {\n\t\tgetRepo().save(name);\n\t\tnew Foo().bar();\n\t}\n}\n";
	let tree = parse(source);
	let targets: Vec<CallTarget> = nodes_of_kind(&tree, "method_invocation")
		.into_iter()
		.flat_map(|node| Java {}.extract_function_calls(node, source))
		.collect();
	let pairs: Vec<(&str, Option<&str>)> = targets
		.iter()
		.map(|target| (target.name.as_str(), target.qualifier.as_deref()))
		.collect();
	assert_eq!(pairs, vec![("save", None), ("getRepo", None), ("bar", None)]);
}

#[test]
fn imports_outside_the_project_and_unqualified_names_do_not_resolve() {
	let files = demo_project();
	let source = "demo/src/main/java/com/example/app/Widget.java";
	assert_eq!(Java {}.resolve_import("java.util.List", source, &files), None);
	// No dot at all means no package path to turn into a file path.
	assert_eq!(Java {}.resolve_import("Service", source, &files), None);
}

fn edge<'a>(graph: &'a CodeGraph, source: &str, target: &str, relation: RelationType) -> Option<&'a CodeRelationship> {
	graph
		.relationships
		.iter()
		.find(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn build_graph_links_calls_imports_and_inheritance_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "java",
		contents: contents.to_string(),
	};
	let base = "src/main/java/com/example/Base.java";
	let service = "src/main/java/com/example/app/Service.java";
	let graph = build_graph(vec![
		file(base, "package com.example;\n\npublic class Base {\n\tpublic void ping() {}\n}\n"),
		file(
			service,
			"package com.example.app;\n\nimport com.example.Base;\n\npublic class Service extends Base {\n\tpublic void run() {\n\t\tBase.ping();\n\t}\n}\n",
		),
	]);

	assert!(edge(&graph, service, base, RelationType::Imports).is_some());
	let call = edge(
		&graph,
		&format!("{service}::Service::run"),
		&format!("{base}::Base::ping"),
		RelationType::Calls,
	)
	.expect("cross-file call");
	assert_eq!(call.line, Some(7));
	assert!(edge(
		&graph,
		&format!("{service}::Service"),
		&format!("{base}::Base"),
		RelationType::Extends
	)
	.is_some());
}
