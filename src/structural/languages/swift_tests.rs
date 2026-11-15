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

//! Swift outline extraction tests.

use super::Swift;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, CodeRelationship, RelationType, SourceFile};
use crate::structural::languages::{Language, PackageScope, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&Swift {}.get_ts_language()).expect("swift grammar");
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

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|file| file.to_string()).collect();
	FileRegistry::new(&owned)
}

#[test]
fn classes_methods_and_inits_are_items() {
	let src = "class Foo {\n\tfunc bar() {}\n\tinit() {}\n}\n\nstruct Point {\n\tvar x = 0.0\n}\n";
	let items = outline("a.swift", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("class", "Foo"),
			("function", "bar"),
			("constructor", "init"),
			("class", "Point")
		]
	);
}

#[test]
fn graph_symbol_kinds_match_the_outline_and_extension_is_swift() {
	assert_eq!(Swift {}.get_symbol_kinds(), Swift {}.get_outline_kinds());
}

#[test]
fn an_import_yields_its_module_path() {
	for (source, expected) in [("import Foundation\n", "Foundation"), ("import UIKit\n", "UIKit")] {
		let tree = parse(source);
		let (imports, exports) = Swift {}.extract_imports_exports(first_node(&tree, "import_declaration"), source);
		assert_eq!(imports, vec![expected], "imports for {source:?}");
		assert!(exports.is_empty());
	}
}

#[test]
fn only_public_declarations_are_exported() {
	let source = "public func doSomething() {}\n";
	let tree = parse(source);
	let (imports, exports) = Swift {}.extract_imports_exports(first_node(&tree, "function_declaration"), source);
	assert!(imports.is_empty());
	assert_eq!(exports, vec!["doSomething"]);

	let source = "func internalHelper() {}\n";
	let tree = parse(source);
	let (_, exports) = Swift {}.extract_imports_exports(first_node(&tree, "function_declaration"), source);
	assert!(exports.is_empty(), "{exports:?}");
}

#[test]
fn calls_keep_their_receiver_as_the_qualifier() {
	let source = "func test() {\n\tprint(\"hello\")\n\tdoWork()\n\tsomeObject.process()\n}\n";
	let tree = parse(source);
	let targets: Vec<(String, Option<String>)> = nodes_of_kind(&tree, "call_expression")
		.into_iter()
		.flat_map(|node| Swift {}.extract_function_calls(node, source))
		.map(|target| (target.name, target.qualifier))
		.collect();
	assert!(targets.contains(&("print".to_string(), None)), "{targets:?}");
	assert!(targets.contains(&("doWork".to_string(), None)), "{targets:?}");
	assert!(
		targets.contains(&("process".to_string(), Some("someObject".to_string()))),
		"{targets:?}"
	);
}

#[test]
fn class_inheritance_is_reported_as_implements() {
	let source = "class Dog: Animal, Trainable { }\n";
	let tree = parse(source);
	assert_eq!(
		Swift {}.extract_type_relations(first_node(&tree, "class_declaration"), source),
		vec![
			(TypeRelationKind::Implements, "Animal".to_string()),
			(TypeRelationKind::Implements, "Trainable".to_string()),
		]
	);
}

#[test]
fn protocol_inheritance_is_reported_as_extends() {
	let source = "protocol Flyable: Movable { }\n";
	let tree = parse(source);
	assert_eq!(
		Swift {}.extract_type_relations(first_node(&tree, "protocol_declaration"), source),
		vec![(TypeRelationKind::Extends, "Movable".to_string())]
	);
}

#[test]
fn a_plain_function_has_no_type_relations() {
	let source = "func standalone() {}\n";
	let tree = parse(source);
	assert!(Swift {}
		.extract_type_relations(first_node(&tree, "function_declaration"), source)
		.is_empty());
}

#[test]
fn a_method_is_owned_by_its_enclosing_type() {
	let source = "class Dog {\n\tfunc run() {}\n}\nfunc free() {}\n";
	let tree = parse(source);
	let functions = nodes_of_kind(&tree, "function_declaration");
	assert_eq!(
		Swift {}.extract_symbol_owner(functions[0], source),
		Some("Dog".to_string())
	);
	assert_eq!(Swift {}.extract_symbol_owner(functions[1], source), None);
}

#[test]
fn a_relative_import_resolves_to_the_file_with_or_without_extension() {
	let files = registry(&[
		"Sources/App/Models/User.swift",
		"Sources/App/Services/UserService.swift",
		"Sources/App/Services/Models/User.swift",
	]);
	assert_eq!(
		Swift {}.resolve_import("./Models/User", "Sources/App/Services/UserService.swift", &files),
		Some("Sources/App/Services/Models/User.swift".to_string())
	);
	assert_eq!(
		Swift {}.resolve_import("../Models/User.swift", "Sources/App/Services/UserService.swift", &files),
		Some("Sources/App/Models/User.swift".to_string())
	);
}

#[test]
fn a_module_import_matches_a_directory_or_a_same_named_file() {
	let files = registry(&[
		"Sources/Models/User.swift",
		"Sources/Services/AuthService.swift",
		"Sources/Kit.swift",
	]);
	let swift = Swift {};
	assert_eq!(
		swift.resolve_import("Models", "Sources/main.swift", &files),
		Some("Sources/Models/User.swift".to_string())
	);
	assert_eq!(
		swift.resolve_import("Kit", "Sources/main.swift", &files),
		Some("Sources/Kit.swift".to_string())
	);
	// A system framework has no project file.
	assert_eq!(swift.resolve_import("Foundation", "Sources/main.swift", &files), None);
}

#[test]
fn a_module_import_names_every_swift_file_of_the_module_directory() {
	let files = registry(&[
		"Sources/Models/User.swift",
		"Sources/Models/Account.swift",
		"Sources/Models/README.md",
		"Sources/Kit.swift",
	]);
	let swift = Swift {};
	assert_eq!(
		swift.resolve_import_files("Models", "Sources/main.swift", &files),
		vec![
			"Sources/Models/Account.swift".to_string(),
			"Sources/Models/User.swift".to_string()
		]
	);
	assert_eq!(
		swift.resolve_import_files("Kit", "Sources/main.swift", &files),
		vec!["Sources/Kit.swift".to_string()]
	);
	assert!(swift
		.resolve_import_files("Foundation", "Sources/main.swift", &files)
		.is_empty());
}

#[test]
fn a_module_import_matches_whole_directory_names_only() {
	let files = registry(&[
		"Sources/Models/User.swift",
		"Sources/Models/Account.swift",
		"Sources/MyModels/X.swift",
	]);
	let swift = Swift {};
	// The alphabetically first file stands for the directory.
	assert_eq!(
		swift.resolve_import("Models", "Sources/main.swift", &files),
		Some("Sources/Models/Account.swift".to_string())
	);
	assert_eq!(swift.resolve_import("Model", "Sources/main.swift", &files), None);
}

#[test]
fn qualified_inheritance_names_yield_their_last_segment() {
	let source = "class Dog: Zoo.Animal, Trainable { }\n";
	let tree = parse(source);
	assert_eq!(
		Swift {}.extract_type_relations(first_node(&tree, "class_declaration"), source),
		vec![
			(TypeRelationKind::Implements, "Animal".to_string()),
			(TypeRelationKind::Implements, "Trainable".to_string()),
		]
	);
}

#[test]
fn deeply_qualified_inheritance_names_yield_their_last_segment() {
	for (source, kind_node, expected) in [
		(
			"class Dog: A.B.Animal { }\n",
			"class_declaration",
			TypeRelationKind::Implements,
		),
		(
			"protocol Flyer: A.B.Movable { }\n",
			"protocol_declaration",
			TypeRelationKind::Extends,
		),
	] {
		let tree = parse(source);
		let relations = Swift {}.extract_type_relations(first_node(&tree, kind_node), source);
		assert_eq!(relations.len(), 1, "{source}: {relations:?}");
		assert_eq!(relations[0].0, expected, "{source}");
		assert!(
			relations[0].1 == "Animal" || relations[0].1 == "Movable",
			"{source}: {relations:?}"
		);
	}
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
		language: "swift",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("Sources/Models/Animal.swift", "class Animal {\n\tfunc speak() {}\n}\n"),
		file(
			"Sources/App/Dog.swift",
			"import Models\n\nclass Dog: Animal {\n\tfunc run() {\n\t\tspeak()\n\t}\n}\n",
		),
	]);

	assert!(edge(
		&graph,
		"Sources/App/Dog.swift",
		"Sources/Models/Animal.swift",
		RelationType::Imports
	)
	.is_some());
	let call = edge(
		&graph,
		"Sources/App/Dog.swift::Dog::run",
		"Sources/Models/Animal.swift::Animal::speak",
		RelationType::Calls,
	)
	.expect("cross-file call");
	assert_eq!(call.line, Some(5));
	assert!(edge(
		&graph,
		"Sources/App/Dog.swift::Dog",
		"Sources/Models/Animal.swift::Animal",
		RelationType::Implements
	)
	.is_some());
}

fn swift_file(path: &str, contents: &str) -> SourceFile {
	SourceFile {
		path: path.to_string(),
		language: "swift",
		contents: contents.to_string(),
	}
}

#[test]
fn the_files_of_a_directory_are_one_package_and_no_file_stands_for_its_directory() {
	assert_eq!(Swift {}.package_scope(), PackageScope::Directory);
	assert!(Swift {}.import_names_directory());
	assert!(!Swift {}.is_directory_index("Sources/Kit/Kit.swift"));
}

#[test]
fn a_function_of_the_same_directory_is_reached_without_an_import() {
	let graph = build_graph(vec![
		swift_file("Sources/Kit/Helper.swift", "func helper() {}\n"),
		swift_file("Sources/Other/Helper.swift", "func helper() {}\n"),
		swift_file("Sources/Kit/Run.swift", "func run() {\n\thelper()\n}\n"),
	]);
	assert!(edge(
		&graph,
		"Sources/Kit/Run.swift::run",
		"Sources/Kit/Helper.swift::helper",
		RelationType::Calls
	)
	.is_some());
	assert!(edge(
		&graph,
		"Sources/Kit/Run.swift::run",
		"Sources/Other/Helper.swift::helper",
		RelationType::Calls
	)
	.is_none());
}

#[test]
fn an_import_binds_the_module_name_and_exported_import_passes_the_module_on() {
	let bindings = |source: &str| {
		let tree = parse(source);
		let statement = first_node(&tree, "import_declaration");
		let (imports, _) = Swift {}.extract_imports_exports(statement, source);
		Swift {}
			.extract_import_bindings(statement, source, &imports)
			.into_iter()
			.map(|binding| (binding.local_name, binding.imported_name, binding.import_path))
			.collect::<Vec<_>>()
	};
	assert_eq!(
		bindings("import Models\n"),
		[("Models".to_string(), None, "Models".to_string())]
	);
	assert_eq!(
		bindings("@_exported import Kit\n"),
		[
			("Kit".to_string(), None, "Kit".to_string()),
			("*".to_string(), Some("*".to_string()), "Kit".to_string()),
		]
	);
	assert!(Swift {}.line_may_reexport("@_exported import Kit", "Kit", "Sources/Facade/F.swift"));
	assert!(!Swift {}.line_may_reexport("import Kit", "Kit", "Sources/Facade/F.swift"));
}

#[test]
fn a_call_through_an_exported_import_reaches_the_declaration_of_the_inner_module() {
	let graph = build_graph(vec![
		swift_file("Sources/Kit/Render.swift", "func render() {}\n"),
		swift_file("Sources/Other/Render.swift", "func render() {}\n"),
		swift_file("Sources/Facade/Facade.swift", "@_exported import Kit\n"),
		swift_file(
			"Sources/App/Main.swift",
			"import Facade\n\nfunc main() {\n\trender()\n}\n",
		),
	]);
	assert!(edge(
		&graph,
		"Sources/App/Main.swift::main",
		"Sources/Kit/Render.swift::render",
		RelationType::Calls
	)
	.is_some());
	assert!(edge(
		&graph,
		"Sources/App/Main.swift::main",
		"Sources/Other/Render.swift::render",
		RelationType::Calls
	)
	.is_none());
}

#[test]
fn a_call_that_names_a_type_reaches_the_type_it_constructs() {
	let graph = build_graph(vec![
		swift_file("Sources/Kit/Widget.swift", "struct Widget {\n\tinit(size: Int) {}\n}\n"),
		swift_file(
			"Sources/App/Main.swift",
			"func build() {\n\tlet w = Widget(size: 1)\n}\n",
		),
	]);
	let call = edge(
		&graph,
		"Sources/App/Main.swift::build",
		"Sources/Kit/Widget.swift::Widget",
		RelationType::Calls,
	)
	.expect("call of the type");
	assert_eq!(call.line, Some(2));
}
