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

//! C++ outline extraction tests.

use super::Cpp;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, CodeRelationship, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&Cpp {}.get_ts_language()).expect("cpp grammar");
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

fn imports_exports_of(source: &str, kind: &str) -> (Vec<String>, Vec<String>) {
	let tree = parse(source);
	Cpp {}.extract_imports_exports(first_node(&tree, kind), source)
}

fn registry(files: &[&str]) -> FileRegistry {
	let owned: Vec<String> = files.iter().map(|file| file.to_string()).collect();
	FileRegistry::new(&owned)
}

#[test]
fn functions_structs_and_namespaces_are_items() {
	let src = "namespace ns {\nint foo() {\n\treturn 0;\n}\nstruct Widget {\n\tint x;\n};\n}\n";
	let items = outline("a.cpp", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("namespace", "ns"), ("function", "foo"), ("struct", "Widget")]
	);
}

#[test]
fn qualified_member_definitions_are_named_by_their_method() {
	let src = "void Widget::resize() {\n}\n";
	let items = outline("a.cpp", src).unwrap();
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].name, "resize");
}

#[test]
fn bodyless_type_references_are_not_items() {
	// `struct stat st;` is a type reference, not a definition.
	let src = "struct stat st;\n";
	assert!(outline("a.cpp", src).unwrap().is_empty());
}

#[test]
fn operator_and_destructor_definitions_keep_their_names() {
	let src = "Foo::~Foo() {\n}\n\nFoo Foo::operator+(const Foo& other) {\n\treturn *this;\n}\n";
	let items = outline("a.cpp", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	// The generic call-target trim would reduce these to `Foo` and
	// `operator`, colliding with the constructor and every other operator.
	assert_eq!(summary, vec![("function", "~Foo"), ("function", "operator+")]);
}

#[test]
fn graph_symbol_kinds_match_the_outline_and_c_extensions_are_claimed() {
	assert_eq!(Cpp {}.get_symbol_kinds(), Cpp {}.get_outline_kinds());
	// C sources and headers are routed to this language, so they need to be
	// claimed for import resolution too.
	for extension in ["c", "h", "cpp", "hpp", "cppm"] {
		assert_eq!(
			crate::structural::language_for_extension(extension),
			Some("cpp"),
			"{extension}"
		);
	}
}

#[test]
fn angle_bracket_and_quoted_includes_both_become_imports() {
	let source = "#include <vector>\n#include \"local/header.h\"\n#include HEADER_MACRO\n";
	let tree = parse(source);
	let includes = nodes_of_kind(&tree, "preproc_include");
	assert_eq!(includes.len(), 3);
	let parsed: Vec<(Vec<String>, Vec<String>)> = includes
		.into_iter()
		.map(|node| Cpp {}.extract_imports_exports(node, source))
		.collect();
	assert_eq!(parsed[0].0, vec!["vector"]);
	assert_eq!(parsed[1].0, vec!["local/header.h"]);
	// A macro-expanded include has no quotes or angle brackets to strip.
	assert!(parsed[2].0.is_empty());
	assert!(parsed.iter().all(|(_, exports)| exports.is_empty()));
}

#[test]
fn function_and_variable_declarations_are_exported_by_name() {
	for (source, kind, expected) in [
		("int main() { return 0; }\n", "function_definition", vec!["main"]),
		("void free_fn(int a);\n", "declaration", vec!["free_fn"]),
		("int counter = 0;\n", "declaration", vec!["counter"]),
		("int p1, p2;\n", "declaration", vec!["p1", "p2"]),
	] {
		let (imports, exports) = imports_exports_of(source, kind);
		assert!(imports.is_empty());
		assert_eq!(exports, expected, "for {source:?}");
	}
}

#[test]
fn types_namespaces_and_macros_are_exported_by_name() {
	for (source, kind, expected) in [
		("class Widget { int w; };\n", "class_specifier", "Widget"),
		("namespace app { }\n", "namespace_definition", "app"),
		("#define LIMIT 10\n", "preproc_def", "LIMIT"),
	] {
		let (_, exports) = imports_exports_of(source, kind);
		assert_eq!(exports, vec![expected], "for {source:?}");
	}
}

#[test]
fn out_of_line_member_definition_exports_its_method_name() {
	let (_, exports) = imports_exports_of("void Widget::resize(int w) { }\n", "function_definition");
	assert_eq!(exports, vec!["resize"]);
}

#[test]
fn calls_keep_their_receiver_or_scope_as_the_qualifier() {
	let source = "void run() {\n\thelper();\n\tWidget::make();\n\tobj.go();\n\tptr->step();\n}\n";
	let tree = parse(source);
	let targets: Vec<(String, Option<String>)> = nodes_of_kind(&tree, "call_expression")
		.into_iter()
		.flat_map(|node| Cpp {}.extract_function_calls(node, source))
		.map(|CallTarget { name, qualifier }| (name, qualifier))
		.collect();
	let expected: Vec<(String, Option<String>)> = [
		("helper", None),
		("make", Some("Widget")),
		("go", Some("obj")),
		("step", Some("ptr")),
	]
	.into_iter()
	.map(|(name, qualifier)| (name.to_string(), qualifier.map(str::to_string)))
	.collect();
	assert_eq!(targets, expected);
}

#[test]
fn non_call_nodes_produce_no_call_targets() {
	let source = "void run() {\n\thelper();\n}\n";
	let tree = parse(source);
	assert!(Cpp {}
		.extract_function_calls(first_node(&tree, "function_definition"), source)
		.is_empty());
}

#[test]
fn base_classes_are_reported_as_extends_regardless_of_access_specifier() {
	let source = "class Derived : public ns::Base<int>, private Other { };\n";
	let tree = parse(source);
	assert_eq!(
		Cpp {}.extract_type_relations(first_node(&tree, "class_specifier"), source),
		vec![
			(TypeRelationKind::Extends, "Base".to_string()),
			(TypeRelationKind::Extends, "Other".to_string()),
		]
	);

	let source = "struct Point : Base { };\n";
	let tree = parse(source);
	assert_eq!(
		Cpp {}.extract_type_relations(first_node(&tree, "struct_specifier"), source),
		vec![(TypeRelationKind::Extends, "Base".to_string())]
	);
}

#[test]
fn types_without_a_base_clause_and_non_type_nodes_have_no_relations() {
	for (source, kind) in [
		("class Plain { };\n", "class_specifier"),
		("int main() { return 0; }\n", "function_definition"),
	] {
		let tree = parse(source);
		assert!(Cpp {}
			.extract_type_relations(first_node(&tree, kind), source)
			.is_empty());
	}
}

#[test]
fn an_out_of_line_definition_is_owned_by_its_qualifying_class() {
	let source = "void Widget::resize(int w) {\n\treturn;\n}\n";
	let tree = parse(source);
	assert_eq!(
		Cpp {}
			.extract_symbol_owner(first_node(&tree, "function_definition"), source)
			.as_deref(),
		Some("Widget")
	);
}

#[test]
fn symbol_owner_falls_back_to_the_enclosing_container_for_inline_methods() {
	let source = "struct Point {\n\tvoid move() { }\n};\n";
	let tree = parse(source);
	assert_eq!(
		Cpp {}
			.extract_symbol_owner(first_node(&tree, "function_definition"), source)
			.as_deref(),
		Some("Point")
	);

	let free_source = "void free_fn() { }\n";
	let free_tree = parse(free_source);
	assert_eq!(
		Cpp {}.extract_symbol_owner(first_node(&free_tree, "function_definition"), free_source),
		None
	);
}

#[test]
fn quoted_includes_resolve_relative_to_the_including_file() {
	let files = registry(&["src/gfx/widget.h", "src/common/defs.h"]);
	let cpp = Cpp {};
	assert_eq!(
		cpp.resolve_import("\"widget.h\"", "src/gfx/main.cpp", &files),
		Some("src/gfx/widget.h".to_string())
	);
	assert_eq!(
		cpp.resolve_import("\"../common/defs.h\"", "src/gfx/main.cpp", &files),
		Some("src/common/defs.h".to_string())
	);
}

#[test]
fn angle_bracket_includes_only_resolve_when_the_header_is_in_the_project() {
	let files = registry(&["gfx/widget.h", "src/main.cpp"]);
	let cpp = Cpp {};
	assert_eq!(
		cpp.resolve_import("<gfx/widget.h>", "src/main.cpp", &files),
		Some("gfx/widget.h".to_string())
	);
	assert_eq!(cpp.resolve_import("<vector>", "src/main.cpp", &files), None);
}

#[test]
fn unquoted_headers_try_the_source_directory_then_exact_then_include_dirs() {
	let cpp = Cpp {};
	assert_eq!(
		cpp.resolve_import("widget.h", "src/gfx/main.cpp", &registry(&["src/gfx/widget.h"])),
		Some("src/gfx/widget.h".to_string())
	);
	assert_eq!(
		cpp.resolve_import("gfx/widget.h", "app/main.cpp", &registry(&["gfx/widget.h"])),
		Some("gfx/widget.h".to_string())
	);
	assert_eq!(
		cpp.resolve_import("widget.h", "app/main.cpp", &registry(&["include/widget.h"])),
		Some("include/widget.h".to_string())
	);
}

#[test]
fn third_party_headers_outside_the_project_do_not_resolve() {
	assert_eq!(
		Cpp {}.resolve_import(
			"absl/strings/str_cat.h",
			"src/main.cpp",
			&registry(&["src/main.cpp", "src/util.h"])
		),
		None
	);
}

fn edge<'a>(graph: &'a CodeGraph, source: &str, target: &str, relation: RelationType) -> Option<&'a CodeRelationship> {
	graph
		.relationships
		.iter()
		.find(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn build_graph_links_calls_includes_and_bases_across_cpp_and_c_files() {
	// `.c` and `.h` sources are routed to this language like `.cpp` ones.
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "cpp",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"src/util.h",
			"inline int helper() {\n\treturn 1;\n}\n\nclass Base {\n};\n",
		),
		file(
			"src/main.cpp",
			"#include \"util.h\"\n\nclass Derived : public Base {\n};\n\nint main() {\n\treturn helper();\n}\n",
		),
		file(
			"src/legacy.c",
			"#include \"util.h\"\n\nint old(void) {\n\treturn helper();\n}\n",
		),
	]);

	assert!(edge(&graph, "src/main.cpp", "src/util.h", RelationType::Imports).is_some());
	assert!(edge(&graph, "src/legacy.c", "src/util.h", RelationType::Imports).is_some());
	let call = edge(&graph, "src/main.cpp::main", "src/util.h::helper", RelationType::Calls).expect("cross-file call");
	assert_eq!(call.line, Some(7));
	assert!(edge(&graph, "src/legacy.c::old", "src/util.h::helper", RelationType::Calls).is_some());
	assert!(edge(
		&graph,
		"src/main.cpp::Derived",
		"src/util.h::Base",
		RelationType::Extends
	)
	.is_some());
}

#[test]
fn c_call_with_a_variable_named_new_is_still_a_call() {
	// `new` is a C++ keyword: the grammar reads this statement as an error
	// node, which must not lose the call.
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "cpp",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"list.h",
			"static inline void list_add_tail(struct list_head *new, struct list_head *head)\n{\n\t__list_add(new, head->prev, head);\n}\n",
		),
		file(
			"add.h",
			"static inline void __list_add(struct list_head *n, struct list_head *prev, struct list_head *next)\n{\n}\n",
		),
	]);

	assert!(edge(
		&graph,
		"list.h::list_add_tail",
		"add.h::__list_add",
		RelationType::Calls
	)
	.is_some());
}

#[test]
fn a_prototype_inside_a_body_is_not_a_call() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "cpp",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("main.c", "void run(void)\n{\n\tint helper(int);\n}\n"),
		file("helper.c", "int helper(int value)\n{\n\treturn value;\n}\n"),
	]);

	assert!(edge(&graph, "main.c::run", "helper.c::helper", RelationType::Calls).is_none());
}
