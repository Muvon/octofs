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

//! Python outline and graph-extraction tests.

use super::Python;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind, SELF_RECEIVER};
use crate::structural::outline;
use tree_sitter::{Node, Parser};

/// Pre-order walk of the parsed source, calling `visit` on every node.
fn for_each_node(source: &str, mut visit: impl FnMut(Node, &str)) {
	let mut parser = Parser::new();
	parser.set_language(&Python {}.get_ts_language()).unwrap();
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
		let (found_imports, found_exports) = Python {}.extract_imports_exports(node, contents);
		imports.extend(found_imports);
		exports.extend(found_exports);
	});
	(imports, exports)
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
fn classes_and_nested_methods_are_items() {
	let src = "import os\n\n\nclass Foo:\n\tdef bar(self):\n\t\tpass\n\n\ndef baz():\n\tpass\n";
	let items = outline("a.py", src).unwrap();
	let summary: Vec<(&str, &str, usize)> = items
		.iter()
		.map(|i| (i.kind.as_str(), i.name.as_str(), i.start_line))
		.collect();
	assert_eq!(
		summary,
		vec![("class", "Foo", 4), ("function", "bar", 5), ("function", "baz", 9)]
	);
}

#[test]
fn imports_yield_module_names() {
	let (imports, _) =
		imports_exports_in("import os, sys as system\nfrom pkg.mod import a, b\nfrom . import c\nfrom ..up import d\n");
	// `from . import c` names a submodule of the package, so the imported
	// name is part of the path; `from pkg.mod import a` may do the same, so
	// the module comes with one path per name.
	assert_eq!(
		imports,
		vec![
			"os",
			"sys",
			"pkg.mod",
			"~pkg.mod.a",
			"~pkg.mod.b",
			".c",
			"..up",
			"~..up.d"
		]
	);
}

#[test]
fn an_absolute_import_is_found_from_every_directory_above_the_file() {
	let files = registry(&[
		"src/app/__init__.py",
		"src/app/core/run.py",
		"src/app/util.py",
		"src/app/sub/mod.py",
	]);
	let py = Python {};
	// `app` lives in `src/`, two levels above the importing file.
	assert_eq!(
		py.resolve_import("app.util", "src/app/core/run.py", &files),
		Some("src/app/util.py".to_string())
	);
	// `from app import sub`: `app.sub` is a package without `__init__`, so only `app` resolves.
	assert_eq!(
		py.resolve_import("app", "src/app/core/run.py", &files),
		Some("src/app/__init__.py".to_string())
	);
	let files = registry(&["pkg/__init__.py", "pkg/sub.py", "pkg/main.py"]);
	let (imports, _) = imports_exports_in("from pkg import sub\n");
	assert_eq!(imports, vec!["pkg", "~pkg.sub"]);
	assert_eq!(
		py.resolve_import("~pkg.sub", "pkg/main.py", &files),
		Some("pkg/sub.py".to_string())
	);
	// A name that is no module (`from typing import List`) makes no external node.
	assert!(py.import_is_optional("~typing.List"));
	assert_eq!(py.resolve_import("~typing.List", "pkg/main.py", &files), None);
}

#[test]
fn a_module_found_only_in_an_intermediate_ancestor_is_a_guess() {
	let files = registry(&["types.py", "a/b/c/main.py", "a/types.py", "a/b/util.py"]);
	let py = Python {};
	// Own directory and the workdir root are certain.
	assert!(!py.import_is_guess("util", "a/b/main.py", &files));
	assert!(!py.import_is_guess("types", "x.py", &files));
	// `a/types.py` sits between the importing file and the root.
	assert_eq!(
		py.resolve_import("types", "a/b/c/main.py", &files),
		Some("a/types.py".to_string())
	);
	assert!(py.import_is_guess("types", "a/b/c/main.py", &files));
}

#[test]
fn from_import_of_a_non_module_name_creates_no_external_node() {
	let graph = build_graph(vec![SourceFile {
		path: "app.py".to_string(),
		language: "python",
		contents: "from typing import List\n".to_string(),
	}]);
	let externals: Vec<&String> = graph.nodes.keys().filter(|id| id.starts_with("external:")).collect();
	assert_eq!(externals, vec!["external:typing"]);
}

#[test]
fn a_dots_only_from_import_yields_one_path_per_imported_name() {
	let (imports, _) =
		imports_exports_in("from . import a, b as c\nfrom .. import (\n\td,\n\te,\n)\nfrom . import *\n");
	assert_eq!(imports, vec![".a", ".b", "..d", "..e", "."]);
}

#[test]
fn from_dot_import_resolves_each_name_to_a_module_or_package() {
	let files = registry(&[
		"pkg/x.py",
		"pkg/y/__init__.py",
		"top.py",
		"pkg/sub/mod.py",
		"pkg/other.py",
	]);
	let py = Python {};
	assert_eq!(
		py.resolve_import(".x", "pkg/main.py", &files),
		Some("pkg/x.py".to_string())
	);
	assert_eq!(
		py.resolve_import(".y", "pkg/main.py", &files),
		Some("pkg/y/__init__.py".to_string())
	);
	assert_eq!(
		py.resolve_import("..other", "pkg/sub/mod.py", &files),
		Some("pkg/other.py".to_string())
	);
	assert_eq!(
		py.resolve_import(".mod", "pkg/sub/other.py", &files),
		Some("pkg/sub/mod.py".to_string())
	);
}

#[test]
fn an_import_never_resolves_to_the_importing_file() {
	let files = registry(&["pkg/__init__.py", "pkg/mod.py"]);
	let py = Python {};
	assert_eq!(py.resolve_import(".", "pkg/__init__.py", &files), None);
	assert_eq!(py.resolve_import("pkg", "pkg/__init__.py", &files), None);
	assert_eq!(py.resolve_import(".mod", "pkg/mod.py", &files), None);
}

#[test]
fn module_level_definitions_are_exports_even_when_decorated() {
	let src = "def top():\n\tpass\n\n@deco\nclass Wrapped:\n\tdef method(self):\n\t\tpass\n";
	let (_, exports) = imports_exports_in(src);
	assert_eq!(exports, vec!["top", "Wrapped"]);
}

#[test]
fn calls_keep_qualifier() {
	let mut calls = Vec::new();
	for_each_node("foo()\nself.bar()\n", |node, contents| {
		calls.extend(Python {}.extract_function_calls(node, contents));
	});
	assert_eq!(
		calls,
		vec![
			CallTarget {
				name: "foo".to_string(),
				qualifier: None
			},
			CallTarget {
				name: "bar".to_string(),
				qualifier: Some(SELF_RECEIVER.to_string())
			},
		]
	);
}

#[test]
fn base_classes_are_extends_relations() {
	let mut relations = Vec::new();
	for_each_node(
		"class A(B, mod.C, Generic[T]):\n\tpass\n\nclass Plain:\n\tpass\n",
		|node, contents| {
			relations.extend(Python {}.extract_type_relations(node, contents));
		},
	);
	assert_eq!(
		relations,
		vec![
			(TypeRelationKind::Extends, "B".to_string()),
			(TypeRelationKind::Extends, "C".to_string()),
			(TypeRelationKind::Extends, "Generic".to_string()),
		]
	);
}

#[test]
fn methods_are_owned_by_their_class() {
	let mut owners = Vec::new();
	for_each_node(
		"class Foo:\n\tdef bar(self):\n\t\tpass\n\ndef free():\n\tpass\n",
		|node, contents| {
			if node.kind() == "function_definition" {
				owners.push(Python {}.extract_symbol_owner(node, contents));
			}
		},
	);
	assert_eq!(owners, vec![Some("Foo".to_string()), None]);
}

#[test]
fn a_name_imported_from_the_package_resolves_to_its_init_file() {
	let files = registry(&["pkg/__init__.py", "pkg/main.py", "pkg/real.py"]);
	let py = Python {};
	// `from . import helper`: no `helper` module, so the name lives in `__init__.py`.
	assert_eq!(
		py.resolve_import(".helper", "pkg/main.py", &files),
		Some("pkg/__init__.py".to_string())
	);
	assert_eq!(
		py.resolve_import(".real", "pkg/main.py", &files),
		Some("pkg/real.py".to_string())
	);
	// Inside `__init__.py` itself it would be a self-edge.
	assert_eq!(py.resolve_import(".helper", "pkg/__init__.py", &files), None);
	// A dotted module that does not exist stays unresolved.
	assert_eq!(py.resolve_import(".a.b", "pkg/main.py", &files), None);
}

#[test]
fn relative_imports_walk_up_by_dots() {
	let files = registry(&["pkg/util.py", "pkg/sub/__init__.py", "top.py", "pkg/__init__.py"]);
	let py = Python {};
	assert_eq!(
		py.resolve_import(".util", "pkg/main.py", &files),
		Some("pkg/util.py".to_string())
	);
	assert_eq!(
		py.resolve_import(".sub", "pkg/main.py", &files),
		Some("pkg/sub/__init__.py".to_string())
	);
	assert_eq!(
		py.resolve_import("..top", "pkg/main.py", &files),
		Some("top.py".to_string())
	);
	assert_eq!(
		py.resolve_import(".", "pkg/main.py", &files),
		Some("pkg/__init__.py".to_string())
	);
	assert_eq!(py.resolve_import("...top", "pkg/main.py", &files), None);
}

#[test]
fn absolute_imports_try_package_then_project_root() {
	let files = registry(&["pkg/util.py", "pkg/deep/mod.py", "other.py"]);
	let py = Python {};
	assert_eq!(
		py.resolve_import("util", "pkg/main.py", &files),
		Some("pkg/util.py".to_string())
	);
	assert_eq!(
		py.resolve_import("pkg.deep.mod", "pkg/main.py", &files),
		Some("pkg/deep/mod.py".to_string())
	);
	assert_eq!(
		py.resolve_import("other", "pkg/main.py", &files),
		Some("other.py".to_string())
	);
	assert_eq!(py.resolve_import("os", "pkg/main.py", &files), None);
}

#[test]
fn graph_links_calls_imports_and_inheritance_across_files() {
	let graph = build_graph(vec![
		SourceFile {
			path: "pkg/util.py".to_string(),
			language: "python",
			contents: "def helper():\n    pass\n\n\nclass Base:\n    pass\n".to_string(),
		},
		SourceFile {
			path: "pkg/main.py".to_string(),
			language: "python",
			contents:
				"from .util import helper, Base\n\n\nclass Child(Base):\n    pass\n\n\ndef run():\n    helper()\n"
					.to_string(),
		},
	]);
	assert!(has_edge(
		&graph,
		"pkg/main.py::run",
		"pkg/util.py::helper",
		RelationType::Calls
	));
	assert!(has_edge(&graph, "pkg/main.py", "pkg/util.py", RelationType::Imports));
	assert!(has_edge(
		&graph,
		"pkg/main.py::Child",
		"pkg/util.py::Base",
		RelationType::Extends
	));
}

#[test]
fn a_call_that_names_a_class_reaches_the_class_it_constructs() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "python",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"pkg/widget.py",
			"class Widget:\n    def __init__(self, size):\n        self.size = size\n",
		),
		file("other/widget.py", "class Widget:\n    pass\n"),
		file(
			"app.py",
			"from pkg.widget import Widget\n\ndef build():\n    return Widget(1)\n",
		),
	]);
	assert!(has_edge(
		&graph,
		"app.py::build",
		"pkg/widget.py::Widget",
		RelationType::Calls
	));
	assert!(!has_edge(
		&graph,
		"app.py::build",
		"other/widget.py::Widget",
		RelationType::Calls
	));
}
