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

//! PHP outline and graph-extraction tests.

use super::Php;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, PackageScope, TypeRelationKind, UNKNOWN_RECEIVER};
use crate::structural::outline;
use tree_sitter::{Node, Parser};

/// Pre-order walk of the parsed source, calling `visit` on every node.
fn for_each_node(source: &str, mut visit: impl FnMut(Node, &str)) {
	let mut parser = Parser::new();
	parser.set_language(&Php {}.get_ts_language()).unwrap();
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
		let (found_imports, found_exports) = Php {}.extract_imports_exports(node, contents);
		imports.extend(found_imports);
		exports.extend(found_exports);
	});
	(imports, exports)
}

fn relations_in(source: &str) -> Vec<(TypeRelationKind, String)> {
	let mut relations = Vec::new();
	for_each_node(source, |node, contents| {
		relations.extend(Php {}.extract_type_relations(node, contents))
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
fn functions_classes_and_methods_are_items() {
	let src = "<?php\n\nfunction foo() {}\n\nclass Bar {\n\tpublic function baz() {}\n}\n";
	let items = outline("a.php", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(summary, vec![("function", "foo"), ("class", "Bar"), ("method", "baz")]);
}

#[test]
fn use_statements_yield_slash_separated_namespace_paths() {
	let src = "<?php\nuse App\\Models\\User;\nuse App\\Models\\Post as Article;\nuse function Util\\helper;\nuse App\\Services\\{Mailer, Queue as Jobs};\n";
	let (imports, _) = imports_exports_in(src);
	assert_eq!(
		imports,
		vec![
			"App/Models/User",
			"App/Models/Post",
			"Util/helper",
			"App/Services/Mailer",
			"App/Services/Queue",
		]
	);
}

#[test]
fn declarations_and_namespaces_are_exports() {
	let (_, exports) = imports_exports_in("<?php\nnamespace App;\nclass Foo {}\nfunction bar() {}\n");
	assert_eq!(exports, vec!["App", "Foo", "bar"]);
}

#[test]
fn calls_keep_qualifier() {
	let mut calls = Vec::new();
	for_each_node("<?php\nfoo();\n$obj->method();\nUtil::run();\n", |node, contents| {
		calls.extend(Php {}.extract_function_calls(node, contents));
	});
	assert_eq!(
		calls,
		vec![
			CallTarget {
				name: "foo".to_string(),
				qualifier: None
			},
			CallTarget {
				name: "method".to_string(),
				qualifier: Some("obj".to_string())
			},
			CallTarget {
				name: "run".to_string(),
				qualifier: Some("Util".to_string())
			},
		]
	);
}

#[test]
fn static_and_namespaced_calls_keep_the_method_name_and_class_qualifier() {
	let mut calls = Vec::new();
	for_each_node(
		"<?php\n\\App\\Foo::bar();\nFoo::bar();\n\\App\\helper();\ngetRepo()->save();\n$this->repo->find();\n",
		|node, contents| {
			calls.extend(Php {}.extract_function_calls(node, contents));
		},
	);
	let pairs: Vec<(&str, Option<&str>)> = calls
		.iter()
		.map(|call| (call.name.as_str(), call.qualifier.as_deref()))
		.collect();
	assert_eq!(
		pairs,
		vec![
			("bar", Some("Foo")),
			("bar", Some("Foo")),
			("helper", None),
			("save", Some(UNKNOWN_RECEIVER)),
			("getRepo", None),
			("find", Some("this::repo")),
		]
	);
}

#[test]
fn extends_and_implements_are_type_relations() {
	let relations = relations_in(
		"<?php\nclass A extends \\App\\B implements C, D {}\ninterface I extends J, K {}\nenum E: string implements F {}\n",
	);
	assert_eq!(
		relations,
		vec![
			(TypeRelationKind::Extends, "B".to_string()),
			(TypeRelationKind::Implements, "C".to_string()),
			(TypeRelationKind::Implements, "D".to_string()),
			(TypeRelationKind::Extends, "J".to_string()),
			(TypeRelationKind::Extends, "K".to_string()),
			(TypeRelationKind::Implements, "F".to_string()),
		]
	);
}

#[test]
fn methods_are_owned_by_their_class() {
	let mut owners = Vec::new();
	for_each_node(
		"<?php\nclass Foo {\n\tpublic function bar() {}\n}\nfunction free() {}\n",
		|node, contents| {
			if matches!(node.kind(), "method_declaration" | "function_definition") {
				owners.push(Php {}.extract_symbol_owner(node, contents));
			}
		},
	);
	assert_eq!(owners, vec![Some("Foo".to_string()), None]);
}

#[test]
fn namespace_imports_resolve_through_psr4_style_paths() {
	let files = registry(&["src/App/Config.php", "App/Models/User.php", "lib/other.php"]);
	let php = Php {};
	assert_eq!(
		php.resolve_import("App/Config", "index.php", &files),
		Some("src/App/Config.php".to_string())
	);
	assert_eq!(
		php.resolve_import("App/Models/User", "index.php", &files),
		Some("App/Models/User.php".to_string())
	);
	assert_eq!(php.resolve_import("Vendor/Missing", "index.php", &files), None);
}

#[test]
fn a_namespace_resolves_by_its_path_suffix_wherever_the_source_root_is() {
	let files = registry(&["lib/Domain/Models/User.php", "other/User.php", "src/xModels/User.php"]);
	let php = Php {};
	assert_eq!(
		php.resolve_import("App/Models/User", "index.php", &files),
		Some("lib/Domain/Models/User.php".to_string())
	);
	// A bare class name never matches by file name alone.
	assert_eq!(php.resolve_import("User", "index.php", &files), None);
	assert_eq!(php.resolve_import("App/Missing/User", "index.php", &files), None);
}

#[test]
fn relative_and_bare_file_imports_resolve_next_to_the_source() {
	let files = registry(&["app/Config.php", "app/sub/helpers.php"]);
	let php = Php {};
	assert_eq!(
		php.resolve_import("./sub/helpers.php", "app/main.php", &files),
		Some("app/sub/helpers.php".to_string())
	);
	assert_eq!(
		php.resolve_import("Config.php", "app/main.php", &files),
		Some("app/Config.php".to_string())
	);
}

#[test]
fn graph_links_calls_imports_and_inheritance_across_files() {
	let graph = build_graph(vec![
		SourceFile {
			path: "App/Base.php".to_string(),
			language: "php",
			contents: "<?php\nnamespace App;\nclass Base {}\nfunction helper() {}\n".to_string(),
		},
		SourceFile {
			path: "App/Service.php".to_string(),
			language: "php",
			contents: "<?php\nnamespace App;\nuse App\\Base;\nclass Service extends Base {\n\tpublic function run() {\n\t\thelper();\n\t}\n}\n"
				.to_string(),
		},
	]);
	assert!(has_edge(
		&graph,
		"App/Service.php::Service::run",
		"App/Base.php::helper",
		RelationType::Calls
	));
	assert!(has_edge(
		&graph,
		"App/Service.php",
		"App/Base.php",
		RelationType::Imports
	));
	assert!(has_edge(
		&graph,
		"App/Service.php::Service",
		"App/Base.php::Base",
		RelationType::Extends
	));
}

#[test]
fn new_is_a_call_of_the_class_and_require_include_are_imports() {
	let mut calls = Vec::new();
	for_each_node("<?php\n$a = new \\App\\Foo(1);\n$b = new Bar;\n", |node, contents| {
		calls.extend(Php {}.extract_function_calls(node, contents));
	});
	let names: Vec<&str> = calls.iter().map(|call| call.name.as_str()).collect();
	assert_eq!(names, vec!["Foo", "Bar"]);

	let (imports, _) = imports_exports_in(
		"<?php\nrequire 'a.php';\nrequire_once __DIR__ . '/lib/b.php';\ninclude(\"c.php\");\ninclude_once $computed;\n",
	);
	assert_eq!(imports, vec!["a.php", "./lib/b.php", "c.php"]);
}

#[test]
fn the_namespace_a_file_declares_is_its_package_scope() {
	assert_eq!(Php {}.package_scope(), PackageScope::Declared);
	for source in [
		"<?php\nnamespace App\\Models;\nclass C {}\n",
		"<?php\nnamespace App\\Models {\n\tclass C {}\n}\n",
	] {
		let mut packages = Vec::new();
		for_each_node(source, |node, contents| {
			packages.extend(Php {}.declared_package(node, contents))
		});
		assert_eq!(packages, ["App\\Models"], "{source}");
	}
}

#[test]
fn a_class_of_the_same_namespace_is_reached_without_a_use() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "php",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"src/Models/Widget.php",
			"<?php\nnamespace App\\Models;\nclass Widget {}\n",
		),
		file("other/Widget.php", "<?php\nnamespace Other;\nclass Widget {}\n"),
		file(
			"tests/WidgetTest.php",
			"<?php\nnamespace App\\Models;\nfunction check() {\n\t$w = new Widget();\n}\n",
		),
	]);
	assert!(has_edge(
		&graph,
		"tests/WidgetTest.php::check",
		"src/Models/Widget.php::Widget",
		RelationType::Calls
	));
	assert!(!has_edge(
		&graph,
		"tests/WidgetTest.php::check",
		"other/Widget.php::Widget",
		RelationType::Calls
	));
}

#[test]
fn a_static_call_that_spells_the_namespace_of_its_class_needs_no_use() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "php",
		contents: contents.to_string(),
	};
	let user = "<?php\nnamespace App\\Models;\nclass User {\n\tpublic static function find() {}\n}\n";
	let graph = build_graph(vec![
		file("src/Models/User.php", user),
		file("src/Other/User.php", &user.replace("Models", "Other")),
		file(
			"src/Service.php",
			"<?php\nnamespace App;\nclass Service {\n\tpublic function run() {\n\t\t\\App\\Models\\User::find();\n\t}\n}\n",
		),
	]);
	assert!(has_edge(
		&graph,
		"src/Service.php::Service::run",
		"src/Models/User.php::User::find",
		RelationType::Calls
	));
	assert!(!has_edge(
		&graph,
		"src/Service.php::Service::run",
		"src/Other/User.php::User::find",
		RelationType::Calls
	));
}
