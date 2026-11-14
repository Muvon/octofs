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

//! Lua outline and graph extraction tests.

use super::Lua;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language};
use crate::structural::outline;
use tree_sitter::{Node, Tree};

fn parse_tree(source: &str) -> Tree {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&Lua {}.get_ts_language()).expect("lua grammar");
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

fn nth_node<'a>(tree: &'a Tree, kind: &str, index: usize) -> Node<'a> {
	nodes_of_kind(tree.root_node(), kind)[index]
}

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|f| f.to_string()).collect();
	FileRegistry::new(&owned)
}

#[test]
fn named_functions_are_items() {
	let src = "function foo()\nend\n\nfunction bar()\nend\n";
	let items = outline("a.lua", src).unwrap();
	let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
	assert_eq!(names, vec!["foo", "bar"]);
}

#[test]
fn name_extensions_and_symbol_kinds() {
	let lua = Lua {};
	assert_eq!(lua.name(), "lua");
	assert_eq!(lua.get_symbol_kinds(), vec!["function_declaration"]);
}

#[test]
fn a_qualified_declaration_splits_into_a_name_and_an_owner() {
	let lua = Lua {};
	for (source, name, owner) in [
		("local function helper()\n\treturn 1\nend\n", "helper", None),
		(
			"function M.encode(v)\n\treturn v\nend\n",
			"encode",
			Some("M".to_string()),
		),
		(
			"function M:decode(v)\n\treturn v\nend\n",
			"decode",
			Some("M".to_string()),
		),
	] {
		let tree = parse_tree(source);
		let node = nth_node(&tree, "function_declaration", 0);
		assert_eq!(lua.extract_declaration_name(node, source), Some(name.to_string()));
		assert_eq!(lua.extract_symbol_owner(node, source), owner);
	}
}

#[test]
fn nested_function_require_is_detected_exactly_once() {
	let source = "function outer()\n\tlocal x = require(\"outer_mod\")\n\tlocal function inner()\n\t\tlocal y = require(\"inner_mod\")\n\tend\nend\n";
	let tree = parse_tree(source);
	let lua = Lua {};
	let imports: Vec<String> = nodes_of_kind(tree.root_node(), "function_call")
		.into_iter()
		.flat_map(|node| lua.extract_imports_exports(node, source).0)
		.collect();
	assert_eq!(imports, vec!["outer_mod".to_string(), "inner_mod".to_string()]);
}

#[test]
fn every_require_spelling_is_recorded_as_an_import() {
	let lua = Lua {};
	let source = "local a = require('json')\nlocal b = require \"pkg.sub\"\nlocal c = require(\"utils\")\n";
	let tree = parse_tree(source);
	for (index, expected) in ["json", "pkg.sub", "utils"].into_iter().enumerate() {
		let (imports, exports) = lua.extract_imports_exports(nth_node(&tree, "function_call", index), source);
		assert_eq!(imports, vec![expected.to_string()]);
		assert!(exports.is_empty());
	}
}

#[test]
fn a_call_that_is_not_require_contributes_no_imports() {
	let source = "local c = other(\"x\")\n";
	let tree = parse_tree(source);
	let (imports, exports) = Lua {}.extract_imports_exports(nth_node(&tree, "function_call", 0), source);
	assert!(imports.is_empty());
	assert!(exports.is_empty());
}

#[test]
fn a_module_level_return_becomes_the_export_list() {
	let lua = Lua {};
	for (source, expected) in [
		("local M = {}\nreturn M\n", vec!["M".to_string()]),
		(
			"local a, b = 1, 2\nreturn a, b\n",
			vec!["a".to_string(), "b".to_string()],
		),
		(
			"return { name = \"x\", size = 2 }\n",
			vec!["name".to_string(), "size".to_string()],
		),
	] {
		let tree = parse_tree(source);
		let (imports, exports) = lua.extract_imports_exports(nth_node(&tree, "return_statement", 0), source);
		assert!(imports.is_empty());
		assert_eq!(exports, expected, "unexpected exports for {source:?}");
	}
}

#[test]
fn a_return_inside_a_function_is_not_a_module_export() {
	let source = "local function f()\n\treturn 1\nend\nreturn f\n";
	let tree = parse_tree(source);
	let lua = Lua {};
	let (_, inner) = lua.extract_imports_exports(nth_node(&tree, "return_statement", 0), source);
	assert!(inner.is_empty());
	let (_, outer) = lua.extract_imports_exports(nth_node(&tree, "return_statement", 1), source);
	assert_eq!(outer, vec!["f".to_string()]);
}

#[test]
fn calls_keep_their_table_as_qualifier_and_require_is_skipped() {
	let source = "obj:method(1)\nobj.field.deep(2)\nplain(3)\nrequire(\"json\")\n";
	let tree = parse_tree(source);
	let lua = Lua {};
	let target = |name: &str, qualifier: Option<&str>| {
		vec![CallTarget {
			name: name.to_string(),
			qualifier: qualifier.map(str::to_string),
		}]
	};
	assert_eq!(
		lua.extract_function_calls(nth_node(&tree, "function_call", 0), source),
		target("method", Some("obj"))
	);
	assert_eq!(
		lua.extract_function_calls(nth_node(&tree, "function_call", 1), source),
		target("deep", Some("obj::field"))
	);
	assert_eq!(
		lua.extract_function_calls(nth_node(&tree, "function_call", 2), source),
		target("plain", None)
	);
	assert!(lua
		.extract_function_calls(nth_node(&tree, "function_call", 3), source)
		.is_empty());
}

#[test]
fn a_node_that_is_not_a_function_call_yields_no_call_targets() {
	let source = "local x = 1\n";
	let tree = parse_tree(source);
	let node = nth_node(&tree, "variable_declaration", 0);
	assert!(Lua {}.extract_function_calls(node, source).is_empty());
}

#[test]
fn imports_resolve_relative_dotted_bare_and_init_forms() {
	let files = registry(&[
		"src/main.lua",
		"src/mod.lua",
		"src/utils/helpers.lua",
		"src/pkg/init.lua",
	]);
	let lua = Lua {};
	assert_eq!(
		lua.resolve_import("./mod", "src/main.lua", &files),
		Some("src/mod.lua".to_string())
	);
	assert_eq!(
		lua.resolve_import("utils.helpers", "src/main.lua", &files),
		Some("src/utils/helpers.lua".to_string())
	);
	assert_eq!(
		lua.resolve_import("pkg", "src/main.lua", &files),
		Some("src/pkg/init.lua".to_string())
	);
	assert_eq!(lua.resolve_import("nonexistent", "src/main.lua", &files), None);
}

#[test]
fn dotted_requires_also_resolve_from_the_repository_root() {
	let files = registry(&[
		"src/main.lua",
		"a/b/c.lua",
		"a/b/d/init.lua",
		"src/a/b/c.lua",
		"pkg/util.lua",
	]);
	let lua = Lua {};
	// The requiring file's own directory wins when it has the module.
	assert_eq!(
		lua.resolve_import("a.b.c", "src/main.lua", &files),
		Some("src/a/b/c.lua".to_string())
	);
	// Otherwise the module is looked up from the root, as `package.path` does.
	assert_eq!(
		lua.resolve_import("a.b.c", "tools/run.lua", &files),
		Some("a/b/c.lua".to_string())
	);
	assert_eq!(
		lua.resolve_import("a.b.d", "src/main.lua", &files),
		Some("a/b/d/init.lua".to_string())
	);
	assert_eq!(
		lua.resolve_import("pkg.util", "deep/er/main.lua", &files),
		Some("pkg/util.lua".to_string())
	);
	assert_eq!(lua.resolve_import("a.b.missing", "src/main.lua", &files), None);
}

#[test]
fn graph_links_calls_and_imports_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "lua",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"lib/main.lua",
			"local util = require(\"util\")\nfunction run()\n\thelper()\nend\n",
		),
		file("lib/util.lua", "function helper()\nend\n"),
	]);
	let has = |source: &str, target: &str, relation: RelationType| {
		graph
			.relationships
			.iter()
			.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
	};
	assert!(has("lib/main.lua", "lib/util.lua", RelationType::Imports));
	assert!(has("lib/main.lua::run", "lib/util.lua::helper", RelationType::Calls));
}
