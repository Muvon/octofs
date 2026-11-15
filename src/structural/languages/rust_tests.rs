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

//! Rust outline extraction tests.

use super::Rust;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, CodeGraph, CodeRelationship, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind, SELF_RECEIVER};
use crate::structural::outline;
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&Rust {}.get_ts_language()).expect("rust grammar");
	parser.parse(source, None).expect("parse")
}

/// The text the declarations of `source` are read from.
fn unwrapped_source(source: &str) -> std::borrow::Cow<'_, str> {
	let mut parser = Parser::new();
	parser.set_language(&Rust {}.get_ts_language()).expect("rust grammar");
	crate::structural::languages::parse_declarations(&Rust {}, &mut parser, source)
		.expect("parse")
		.1
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

fn demo_crate() -> FileRegistry {
	registry(&[
		"demo/src/lib.rs",
		"demo/src/config.rs",
		"demo/src/config/features.rs",
		"demo/src/graph/mod.rs",
		"demo/src/graph/node.rs",
		"demo/src/graph/deep/mod.rs",
		"demo/src/graph/deep/leaf.rs",
	])
}

#[test]
fn functions_structs_enums_traits_and_consts_are_items() {
	let src = "use std::fmt;\n\nconst N: usize = 1;\n\nstruct Point {\n\tx: f64,\n}\n\nenum Kind {\n\tA,\n}\n\ntrait Show {\n\tfn show(&self) {}\n}\n\nfn main() {\n\tprintln!(\"hi\");\n}\n";
	let items = outline("a.rs", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("constant", "N"),
			("struct", "Point"),
			("enum", "Kind"),
			("trait", "Show"),
			("function", "show"),
			("function", "main"),
		]
	);
}

#[test]
fn impl_headers_and_methods_are_items() {
	let src = "impl Point {\n\tfn norm(&self) -> f64 {\n\t\t0.0\n\t}\n}\n";
	let items = outline("a.rs", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	// The impl header is an item too: methods of different impls are
	// otherwise indistinguishable in an outline.
	assert_eq!(summary, vec![("impl", "Point"), ("function", "norm")]);
}

#[test]
fn graph_symbol_kinds_leave_out_impl_blocks() {
	let kinds = Rust {}.get_symbol_kinds();
	assert!(kinds.contains(&"function_item"));
	assert!(!kinds.contains(&"impl_item"));
}

#[test]
fn a_trait_impl_reports_an_implements_relation_owned_by_the_concrete_type() {
	let source = "impl Render for Widget {\n\tfn render(&self) {}\n}\n";
	let tree = parse(source);
	let node = first_node(&tree, "impl_item");
	let lang = Rust {};
	assert_eq!(
		lang.extract_type_relations(node, source),
		vec![(TypeRelationKind::Implements, "Render".to_string())]
	);
	assert_eq!(
		lang.extract_type_relation_source(node, source),
		Some("Widget".to_string())
	);
}

#[test]
fn an_inherent_impl_reports_no_type_relation() {
	let source = "impl Widget {\n\tfn paint(&self) {}\n}\n";
	let tree = parse(source);
	let node = first_node(&tree, "impl_item");
	let lang = Rust {};
	assert!(lang.extract_type_relations(node, source).is_empty());
	assert_eq!(
		lang.extract_type_relation_source(node, source),
		Some("Widget".to_string())
	);
}

#[test]
fn a_trait_with_supertraits_reports_one_extends_relation_per_bound() {
	let source = "trait Render: Draw + Clone {\n\tfn go(&self);\n}\n";
	let tree = parse(source);
	let node = first_node(&tree, "trait_item");
	let lang = Rust {};
	assert_eq!(
		lang.extract_type_relations(node, source),
		vec![
			(TypeRelationKind::Extends, "Draw".to_string()),
			(TypeRelationKind::Extends, "Clone".to_string()),
		]
	);
	assert_eq!(
		lang.extract_type_relation_source(node, source),
		Some("Render".to_string())
	);
}

#[test]
fn a_struct_declares_no_type_relations() {
	let source = "struct Widget { id: u32 }\n";
	let tree = parse(source);
	assert!(Rust {}
		.extract_type_relations(first_node(&tree, "struct_item"), source)
		.is_empty());
}

fn target(name: &str, qualifier: Option<&str>) -> CallTarget {
	CallTarget {
		name: name.to_string(),
		qualifier: qualifier.map(str::to_string),
	}
}

#[test]
fn qualified_and_bare_calls_keep_their_terminal_name_and_qualifier() {
	let source = "fn go() {\n\thelper();\n\tWidget::new();\n\tself.paint();\n\tSelf::build();\n}\n";
	let tree = parse(source);
	let lang = Rust {};
	let targets: Vec<CallTarget> = nodes_of_kind(&tree, "call_expression")
		.into_iter()
		.flat_map(|node| lang.extract_function_calls(node, source))
		.collect();
	assert_eq!(
		targets,
		vec![
			target("helper", None),
			target("new", Some("Widget")),
			target("paint", Some(SELF_RECEIVER)),
			target("build", Some(SELF_RECEIVER)),
		]
	);
}

#[test]
fn macro_invocations_are_reported_as_calls_without_the_bang() {
	let source = "fn go() {\n\tprintln!(\"x\");\n\tcrate::utils::debug_log!(\"y\");\n}\n";
	let tree = parse(source);
	let lang = Rust {};
	let targets: Vec<CallTarget> = nodes_of_kind(&tree, "macro_invocation")
		.into_iter()
		.flat_map(|node| lang.extract_function_calls(node, source))
		.collect();
	assert_eq!(
		targets,
		vec![target("println", None), target("debug_log", Some("crate::utils"))]
	);
}

#[test]
fn a_node_that_is_not_a_call_reports_no_calls() {
	let source = "fn go() {\n\tlet x = 1;\n}\n";
	let tree = parse(source);
	assert!(Rust {}
		.extract_function_calls(first_node(&tree, "let_declaration"), source)
		.is_empty());
}

#[test]
fn use_statements_expand_into_one_fully_qualified_import_per_leaf() {
	for (source, expected) in [
		("use crate::config::Config;\n", vec!["crate::config::Config"]),
		("use crate::a::{B, c::D};\n", vec!["crate::a::B", "crate::a::c::D"]),
		(
			"use crate::a::{b::{C, D}, E};\n",
			vec!["crate::a::b::C", "crate::a::b::D", "crate::a::E"],
		),
		("use std::sync::Arc as StdArc;\n", vec!["std::sync::Arc"]),
		("use crate::utils::*;\n", vec!["crate::utils"]),
	] {
		let tree = parse(source);
		let (imports, exports) = Rust {}.extract_imports_exports(first_node(&tree, "use_declaration"), source);
		assert_eq!(imports, expected, "imports for {source:?}");
		assert!(exports.is_empty(), "exports for {source:?}");
	}
}

#[test]
fn a_pub_use_reexport_still_produces_an_import() {
	for source in ["pub use crate::api::Client;\n", "pub(crate) use crate::api::Client;\n"] {
		let tree = parse(source);
		let (imports, _) = Rust {}.extract_imports_exports(first_node(&tree, "use_declaration"), source);
		assert_eq!(imports, vec!["crate::api::Client"], "for {source:?}");
	}
}

#[test]
fn a_bodyless_mod_becomes_a_self_relative_import_while_a_mod_body_does_not() {
	let source = "pub mod public_mod;\nmod private_mod;\nmod inner { fn a() { let _ = 1; } }\n";
	let tree = parse(source);
	let mods = nodes_of_kind(&tree, "mod_item");
	let lang = Rust {};

	let (imports, exports) = lang.extract_imports_exports(mods[0], source);
	assert_eq!(imports, vec!["self::public_mod"]);
	assert_eq!(exports, vec!["public_mod"]);

	let (imports, exports) = lang.extract_imports_exports(mods[1], source);
	assert_eq!(imports, vec!["self::private_mod"]);
	assert!(exports.is_empty());

	let (imports, exports) = lang.extract_imports_exports(mods[2], source);
	assert!(imports.is_empty());
	assert!(exports.is_empty());
}

#[test]
fn only_pub_items_are_reported_as_exports() {
	for (source, kind, expected) in [
		("pub struct Widget { id: u32 }\n", "struct_item", vec!["Widget"]),
		("pub enum Color { Red }\n", "enum_item", vec!["Color"]),
		("pub trait Render { fn go(&self); }\n", "trait_item", vec!["Render"]),
		("pub fn go() {}\n", "function_item", vec!["go"]),
		("pub const LIMIT: usize = 1;\n", "const_item", vec!["LIMIT"]),
		("struct Hidden { id: u32 }\n", "struct_item", vec![]),
		("fn hidden() {}\n", "function_item", vec![]),
	] {
		let tree = parse(source);
		let (imports, exports) = Rust {}.extract_imports_exports(first_node(&tree, kind), source);
		assert!(imports.is_empty(), "imports for {source:?}");
		assert_eq!(exports, expected, "exports for {source:?}");
	}
}

#[test]
fn symbol_owner_walks_up_to_the_impl_or_trait_but_not_to_a_module() {
	let source = "impl Widget {\n\tfn paint(&self) {}\n}\ntrait Render {\n\tfn go(&self) {}\n}\nmod inner {\n\tfn free() {}\n}\n";
	let tree = parse(source);
	let functions = nodes_of_kind(&tree, "function_item");
	let lang = Rust {};
	assert_eq!(
		lang.extract_symbol_owner(functions[0], source),
		Some("Widget".to_string())
	);
	assert_eq!(
		lang.extract_symbol_owner(functions[1], source),
		Some("Render".to_string())
	);
	assert_eq!(lang.extract_symbol_owner(functions[2], source), None);
}

#[test]
fn a_crate_path_resolves_to_the_deepest_matching_module_file() {
	assert_eq!(
		Rust {}.resolve_import("crate::config::features::Item", "demo/src/main.rs", &demo_crate()),
		Some("demo/src/config/features.rs".to_string())
	);
}

#[test]
fn a_crate_path_falls_back_to_a_module_directory_mod_rs() {
	assert_eq!(
		Rust {}.resolve_import("crate::graph::Missing", "demo/src/lib.rs", &demo_crate()),
		Some("demo/src/graph/mod.rs".to_string())
	);
}

#[test]
fn a_single_segment_crate_path_falls_back_to_the_crate_root() {
	assert_eq!(
		Rust {}.resolve_import("crate::Url", "demo/src/graph/node.rs", &demo_crate()),
		Some("demo/src/lib.rs".to_string())
	);
}

#[test]
fn a_crate_item_declared_in_the_source_file_itself_resolves_to_that_file() {
	// The trait has no way to say "no file": `None` would become an external
	// node, so the source file is returned and the graph layer drops the
	// self-targeted edge.
	for source in ["demo/src/lib.rs", "demo/src/main.rs"] {
		let files = registry(&["demo/src/lib.rs", "demo/src/main.rs"]);
		let crate_root = if source.ends_with("lib.rs") {
			source
		} else {
			"demo/src/lib.rs"
		};
		assert_eq!(
			Rust {}.resolve_import("crate::Error", source, &files),
			Some(crate_root.to_string()),
			"{source}"
		);
	}
}

#[test]
fn deeply_nested_use_groups_expand_in_source_order() {
	let depth = 2000;
	let mut source = String::from("use ");
	for level in 0..depth {
		source.push_str(&format!("m{level}::{{"));
	}
	source.push_str("leaf_a, leaf_b");
	source.push_str(&"}".repeat(depth));
	source.push_str(";\n");
	let imports = super::expand_rust_use_paths(&source);
	let prefix: Vec<String> = (0..depth).map(|level| format!("m{level}")).collect();
	let prefix = prefix.join("::");
	assert_eq!(imports, vec![format!("{prefix}::leaf_a"), format!("{prefix}::leaf_b")]);

	assert_eq!(
		super::expand_rust_use_paths("use a::{b::{c, d}, e, f::{g}};"),
		vec!["a::b::c", "a::b::d", "a::e", "a::f::g"]
	);
}

#[test]
fn a_multi_segment_crate_path_that_matches_nothing_resolves_to_nothing() {
	assert_eq!(
		Rust {}.resolve_import("crate::missing::Thing", "demo/src/lib.rs", &demo_crate()),
		None
	);
}

#[test]
fn a_super_path_from_a_module_root_resolves_against_the_parent_module() {
	// graph/mod.rs is module `graph`; its parent is the crate root.
	let files = demo_crate();
	let lang = Rust {};
	assert_eq!(
		lang.resolve_import("super::config::Item", "demo/src/graph/mod.rs", &files),
		Some("demo/src/config.rs".to_string())
	);
	// `node` is a child of graph, not of the crate root: it can only be an
	// item of the root module, whose file is lib.rs.
	assert_eq!(
		lang.resolve_import("super::node", "demo/src/graph/mod.rs", &files),
		Some("demo/src/lib.rs".to_string())
	);
}

#[test]
fn a_super_path_from_a_leaf_file_resolves_against_its_parent_module() {
	let files = demo_crate();
	let lang = Rust {};
	// graph/node.rs is module graph::node; its parent is graph.
	assert_eq!(
		lang.resolve_import("super::deep::leaf::Item", "demo/src/graph/node.rs", &files),
		Some("demo/src/graph/deep/leaf.rs".to_string())
	);
	// An item of the parent module resolves to the parent's own file.
	assert_eq!(
		lang.resolve_import("super::Item", "demo/src/graph/node.rs", &files),
		Some("demo/src/graph/mod.rs".to_string())
	);
}

#[test]
fn repeated_super_segments_climb_one_module_each() {
	let files = demo_crate();
	let lang = Rust {};
	// graph::deep::leaf -> graph::deep -> graph
	assert_eq!(
		lang.resolve_import("super::super::node::Item", "demo/src/graph/deep/leaf.rs", &files),
		Some("demo/src/graph/node.rs".to_string())
	);
	// ... -> crate root
	assert_eq!(
		lang.resolve_import("super::super::super::config", "demo/src/graph/deep/leaf.rs", &files),
		Some("demo/src/config.rs".to_string())
	);
}

#[test]
fn a_self_path_names_a_child_module_never_the_source_file() {
	let files = demo_crate();
	let lang = Rust {};
	// `mod features;` in config.rs: children of config.rs live in config/.
	assert_eq!(
		lang.resolve_import("self::features", "demo/src/config.rs", &files),
		Some("demo/src/config/features.rs".to_string())
	);
	// `mod node;` in a mod.rs: children sit next to it.
	assert_eq!(
		lang.resolve_import("self::node", "demo/src/graph/mod.rs", &files),
		Some("demo/src/graph/node.rs".to_string())
	);
	// `mod graph;` in lib.rs, a directory module.
	assert_eq!(
		lang.resolve_import("self::graph", "demo/src/lib.rs", &files),
		Some("demo/src/graph/mod.rs".to_string())
	);
	// No child module of that name: not the file itself.
	assert_eq!(lang.resolve_import("self::helpers", "demo/src/config.rs", &files), None);
}

#[test]
fn a_module_relative_use_in_a_module_root_resolves_to_the_child_file() {
	let files = demo_crate();
	let lang = Rust {};
	assert_eq!(
		lang.resolve_import("node::Item", "demo/src/graph/mod.rs", &files),
		Some("demo/src/graph/node.rs".to_string())
	);
	assert_eq!(
		lang.resolve_import("deep::leaf::Item", "demo/src/graph/mod.rs", &files),
		Some("demo/src/graph/deep/leaf.rs".to_string())
	);
	assert_eq!(
		lang.resolve_import("config::features::Item", "demo/src/lib.rs", &files),
		Some("demo/src/config/features.rs".to_string())
	);
	// A leaf file has no children next to it.
	assert_eq!(
		lang.resolve_import("node::Item", "demo/src/graph/deep/leaf.rs", &files),
		None
	);
}

#[test]
fn a_crate_path_drops_trailing_item_names_until_a_file_matches() {
	let files = demo_crate();
	let lang = Rust {};
	assert_eq!(
		lang.resolve_import("crate::graph::deep::leaf::Item", "demo/src/graph/node.rs", &files),
		Some("demo/src/graph/deep/leaf.rs".to_string())
	);
	assert_eq!(
		lang.resolve_import("crate::graph::deep::Item::Variant", "demo/src/config.rs", &files),
		Some("demo/src/graph/deep/mod.rs".to_string())
	);
}

#[test]
fn external_crate_paths_stay_unresolved() {
	let files = demo_crate();
	let lang = Rust {};
	for import in ["anyhow::Result", "rayon::prelude", "std::collections::HashMap"] {
		assert_eq!(
			lang.resolve_import(import, "demo/src/graph/mod.rs", &files),
			None,
			"{import}"
		);
	}
}

#[test]
fn an_unqualified_import_resolves_to_a_sibling_file() {
	assert_eq!(
		Rust {}.resolve_import("node", "demo/src/graph/mod.rs", &demo_crate()),
		Some("demo/src/graph/node.rs".to_string())
	);
}

#[test]
fn a_third_party_crate_path_does_not_resolve_to_a_project_file() {
	assert_eq!(
		Rust {}.resolve_import("serde::Serialize", "demo/src/lib.rs", &demo_crate()),
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
fn build_graph_links_calls_imports_and_trait_impls_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "rust",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("src/lib.rs", "pub fn root() {}\n"),
		file("src/util.rs", "pub trait Render {\n\tfn render(&self);\n}\n\npub fn helper() {}\n"),
		file(
			"src/app.rs",
			"use crate::util::{Render, helper};\n\nstruct Widget;\n\nimpl Render for Widget {\n\tfn render(&self) {}\n}\n\nfn run() {\n\thelper();\n}\n",
		),
	]);

	assert!(edge(&graph, "src/app.rs", "src/util.rs", RelationType::Imports).is_some());
	let call = edge(&graph, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).expect("cross-file call");
	assert_eq!(call.line, Some(10));
	assert!(edge(
		&graph,
		"src/app.rs::Widget",
		"src/util.rs::Render",
		RelationType::Implements
	)
	.is_some());
}

#[test]
fn glob_imports_of_the_crate_the_parent_and_the_module_itself_resolve_to_a_file() {
	let files = registry(&["src/lib.rs", "src/a.rs", "src/a/b.rs"]);
	assert_eq!(
		Rust {}.resolve_import("crate", "src/a/b.rs", &files).as_deref(),
		Some("src/lib.rs")
	);
	assert_eq!(
		Rust {}.resolve_import("super", "src/a/b.rs", &files).as_deref(),
		Some("src/a.rs")
	);
	assert_eq!(
		Rust {}.resolve_import("self", "src/a/b.rs", &files).as_deref(),
		Some("src/a/b.rs")
	);
}

#[test]
fn a_path_attribute_names_the_module_file_directly() {
	let source = "#[path = \"other/impl.rs\"]\nmod platform;\n";
	let tree = parse(source);
	let (imports, _) = Rust {}.extract_imports_exports(first_node(&tree, "mod_item"), source);
	let files = registry(&["src/lib.rs", "src/other/impl.rs"]);
	assert_eq!(imports.len(), 1);
	assert_eq!(
		Rust {}.resolve_import(&imports[0], "src/lib.rs", &files).as_deref(),
		Some("src/other/impl.rs")
	);
}

#[test]
fn super_inside_an_inline_module_is_the_enclosing_file() {
	let source = "mod tests {\n\tuse super::*;\n\tuse super::sub::Item;\n\tuse super::super::other::Thing;\n}\n";
	let tree = parse(source);
	let imports: Vec<String> = nodes_of_kind(&tree, "use_declaration")
		.into_iter()
		.flat_map(|node| Rust {}.extract_imports_exports(node, source).0)
		.collect();
	// `use super::*;` names the file itself: no import. `super::sub` is the
	// file's own child module; a second `super` climbs to the parent module.
	assert_eq!(imports, vec!["self::sub::Item", "super::other::Thing"]);
}

#[test]
fn calls_inside_macro_arguments_are_calls() {
	let source = "fn t() {\n\tassert_eq!(compute(), 1);\n\tlet v = vec![make()];\n}\n";
	let tree = parse(source);
	let names: Vec<String> = nodes_of_kind(&tree, "token_tree")
		.into_iter()
		.flat_map(|node| Rust {}.extract_function_calls(node, source))
		.map(|call| call.name)
		.collect();
	assert!(names.contains(&"compute".to_string()), "{names:?}");
	assert!(names.contains(&"make".to_string()), "{names:?}");
}

#[test]
fn a_pub_use_line_may_reexport_and_a_private_use_may_not() {
	assert!(Rust {}.line_may_reexport("pub use inner::Thing;", "inner", "src/lib.rs"));
	assert!(!Rust {}.line_may_reexport("use inner::Thing;", "inner", "src/lib.rs"));
}

#[test]
fn a_use_of_another_workspace_crate_resolves_through_its_directory_name() {
	let files = registry(&[
		"crates/app/src/main.rs",
		"crates/my-core/src/lib.rs",
		"crates/my-core/src/store.rs",
	]);
	let rust = Rust {};
	assert_eq!(
		rust.resolve_import("my_core::store::Store", "crates/app/src/main.rs", &files)
			.as_deref(),
		Some("crates/my-core/src/store.rs")
	);
	assert_eq!(
		rust.resolve_import("my_core::Config", "crates/app/src/main.rs", &files)
			.as_deref(),
		Some("crates/my-core/src/lib.rs")
	);
	assert_eq!(
		rust.resolve_import("serde::Serialize", "crates/app/src/main.rs", &files),
		None
	);
}

#[test]
fn every_alias_in_a_grouped_use_binds_its_own_name() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "rust",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("src/lib.rs", "mod util;\nmod other;\n"),
		file("src/util.rs", "pub fn one() {}\npub fn two() {}\n"),
		// The same names elsewhere rule out the unique-name guess.
		file("src/other.rs", "pub fn one() {}\npub fn two() {}\n"),
		file(
			"src/app.rs",
			"use crate::util::{one as uno, two as dos};\n\nfn run() {\n\tuno();\n\tdos();\n}\n",
		),
	]);
	assert!(edge(&graph, "src/app.rs::run", "src/util.rs::one", RelationType::Calls).is_some());
	assert!(edge(&graph, "src/app.rs::run", "src/util.rs::two", RelationType::Calls).is_some());
}

#[test]
fn a_trait_from_the_standard_library_is_never_a_project_relation() {
	let source = "struct Error;\nimpl std::error::Error for Error {}\nimpl Display for Error {}\n";
	let tree = parse(source);
	let relations: Vec<_> = nodes_of_kind(&tree, "impl_item")
		.into_iter()
		.flat_map(|node| Rust {}.extract_type_relations(node, source))
		.collect();
	assert_eq!(relations, vec![(TypeRelationKind::Implements, "Display".to_string())]);
}

#[test]
fn a_workspace_crate_import_is_a_guess_and_its_edge_is_not_certain() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "rust",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("crates/my-core/src/lib.rs", "pub mod store;\n"),
		file("crates/my-core/src/store.rs", "pub struct Store;\n"),
		file("crates/app/src/main.rs", "use my_core::store::Store;\nfn main() {}\n"),
	]);
	let import = edge(
		&graph,
		"crates/app/src/main.rs",
		"crates/my-core/src/store.rs",
		RelationType::Imports,
	)
	.expect("import edge");
	assert!(import.confidence < 1.0);
}

fn rust_file(path: &str, contents: &str) -> SourceFile {
	SourceFile {
		path: path.to_string(),
		language: "rust",
		contents: contents.to_string(),
	}
}

/// The module tree the inline-path tests call into. The same name in two
/// modules rules out the unique-name guess.
fn modules_declaring_helper(app: &str) -> Vec<SourceFile> {
	vec![
		rust_file("src/lib.rs", "mod util;\nmod other;\nmod app;\n"),
		rust_file("src/util.rs", "pub fn helper() {}\nmod child;\n"),
		rust_file("src/other.rs", "pub fn helper() {}\n"),
		rust_file("src/util/child.rs", "fn go() {\n\tsuper::helper();\n}\n"),
		rust_file("src/app.rs", app),
	]
}

#[test]
fn a_call_qualified_by_a_module_path_resolves_like_an_import_of_that_path() {
	let app = "fn run() {\n\tcrate::util::helper();\n\tself::local();\n\tstd::mem::drop(1);\n}\n\nfn local() {}\n";
	let inline = build_graph(modules_declaring_helper(app));

	let through_path =
		edge(&inline, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).expect("crate-rooted path");
	assert!(edge(&inline, "src/app.rs::run", "src/other.rs::helper", RelationType::Calls).is_none());
	assert!(edge(&inline, "src/app.rs::run", "src/app.rs::local", RelationType::Calls).is_some());
	assert!(edge(
		&inline,
		"src/util/child.rs::go",
		"src/util.rs::helper",
		RelationType::Calls
	)
	.is_some());
	assert!(edge(&inline, "src/app.rs", "src/util.rs", RelationType::Imports).is_some());
	// An unresolved path is no import: the standard library is not a node.
	assert!(inline.nodes.values().all(|node| !node.id.contains("std::mem")));

	let statement = "use crate::util::helper;\n\nfn run() {\n\thelper();\n}\n";
	let imported = build_graph(modules_declaring_helper(statement));
	let through_use =
		edge(&imported, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).expect("the use statement");
	assert_eq!(through_path.confidence, through_use.confidence);
}

#[test]
fn a_type_in_the_qualifier_is_not_a_module_path() {
	let graph = build_graph(modules_declaring_helper(
		"fn run() {\n\tcrate::util::Widget::helper();\n}\n",
	));
	assert!(edge(&graph, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).is_none());
}

#[test]
fn a_use_inside_an_item_macro_is_an_import_with_its_bindings() {
	let source = "cfg_rt! {\n    mod blocking;\n    pub(crate) use blocking::spawn_blocking as spawn;\n    pub(crate) use $path::Skipped;\n}\n";
	let tree = parse(source);
	let macro_body = nodes_of_kind(&tree, "token_tree")[0];
	let (imports, _) = Rust {}.extract_imports_exports(macro_body, source);
	assert_eq!(imports, vec!["blocking::spawn_blocking"]);
	let bindings = Rust {}.extract_import_bindings(macro_body, source, &imports);
	let named: Vec<(&str, Option<&str>)> = bindings
		.iter()
		.map(|binding| (binding.local_name.as_str(), binding.imported_name.as_deref()))
		.collect();
	assert_eq!(named, vec![("spawn", Some("spawn_blocking"))]);
}

#[test]
fn a_caller_inside_a_macro_wrapped_function_is_attributed_to_that_function() {
	let graph = build_graph(vec![
		rust_file("src/lib.rs", "mod util;\nmod app;\n"),
		rust_file("src/util.rs", "cfg_rt! {\n    pub fn helper() {}\n}\n"),
		rust_file(
			"src/app.rs",
			"use crate::util::helper;\ncfg_rt! {\n    cfg_net! {\n        pub fn run() {\n            helper();\n        }\n    }\n}\n",
		),
	]);
	assert!(graph.nodes.contains_key("src/util.rs::helper"));
	assert!(edge(&graph, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).is_some());
}

#[test]
fn mod_and_use_lines_inside_a_macro_block_are_ordinary_declarations() {
	let graph = build_graph(vec![
		rust_file("tokio/src/lib.rs", "cfg_rt! {\n    pub mod runtime;\n}\n"),
		rust_file(
			"tokio/src/runtime/mod.rs",
			"cfg_rt! {\n    mod blocking;\n    pub(crate) use blocking::spawn_blocking;\n}\n",
		),
		rust_file("tokio/src/runtime/blocking.rs", "pub fn spawn_blocking() {}\n"),
	]);
	assert!(edge(
		&graph,
		"tokio/src/lib.rs",
		"tokio/src/runtime/mod.rs",
		RelationType::Imports
	)
	.is_some());
	assert!(edge(
		&graph,
		"tokio/src/runtime/mod.rs",
		"tokio/src/runtime/blocking.rs",
		RelationType::Imports
	)
	.is_some());
}

#[test]
fn a_macro_body_that_is_not_a_block_of_items_keeps_its_calls_and_declares_nothing() {
	let source = "fn helper() -> u32 {\n    1\n}\nfn run() {\n    let v = vec![1, 2];\n    println!(\"{}\", helper());\n}\nbroken! {\n    fn half(\n}\nstatements! {\n    let x = make();\n}\nmacro_rules! m {\n    () => { fn hidden() {} };\n}\n";
	assert!(matches!(unwrapped_source(source), std::borrow::Cow::Borrowed(_)));
	let graph = build_graph(vec![rust_file("src/a.rs", source)]);
	assert!(!graph.nodes.contains_key("src/a.rs::hidden"));
	assert!(!graph.nodes.contains_key("src/a.rs::half"));
	// The call in a format-style macro's arguments is still extracted.
	assert!(edge(&graph, "src/a.rs::run", "src/a.rs::helper", RelationType::Calls).is_some());
}

#[test]
fn a_lazy_static_shaped_body_declares_its_static() {
	let source = "lazy_static! {\n    pub static ref TABLE: u32 = build();\n}\nfn build() -> u32 {\n    1\n}\n";
	// Statics are no graph nodes; the definition itself is covered through
	// symbol navigation. Here the body must be taken as items.
	let unwrapped = unwrapped_source(source);
	assert!(matches!(unwrapped, std::borrow::Cow::Owned(_)));
	assert!(unwrapped.contains("pub static ref TABLE: u32 = build();"));
	assert!(!unwrapped.contains("lazy_static"));
}

#[test]
fn unwrapping_a_macro_block_keeps_every_line_and_column() {
	let source = "cfg_rt! {\n    cfg_net! {\n        pub fn g() {}\n    }\n}\nfn tail() {}\n";
	let unwrapped = unwrapped_source(source);
	assert_eq!(unwrapped.len(), source.len());
	assert_eq!(unwrapped.lines().count(), source.lines().count());
	assert_eq!(unwrapped.lines().nth(2), source.lines().nth(2));
	assert!(!unwrapped.contains("cfg_rt") && !unwrapped.contains("cfg_net"));
}

#[test]
fn a_use_inside_a_function_body_binds_for_the_calls_in_that_function() {
	let graph = build_graph(vec![
		rust_file("src/lib.rs", "mod util;\nmod other;\nmod app;\n"),
		rust_file("src/util.rs", "pub fn helper() {}\n"),
		rust_file("src/other.rs", "pub fn helper() {}\n"),
		rust_file(
			"src/app.rs",
			"fn run() {\n    use crate::util::helper;\n    helper();\n}\n",
		),
	]);
	assert!(edge(&graph, "src/app.rs::run", "src/util.rs::helper", RelationType::Calls).is_some());
	assert!(edge(&graph, "src/app.rs::run", "src/other.rs::helper", RelationType::Calls).is_none());
}

#[test]
fn two_imports_of_one_name_under_opposite_cfg_are_both_candidates() {
	let graph = build_graph(vec![
		rust_file("src/lib.rs", "mod real;\nmod mocks;\nmod app;\n"),
		rust_file("src/real.rs", "pub fn go() {}\n"),
		rust_file("src/mocks.rs", "pub fn go() {}\n"),
		rust_file(
			"src/app.rs",
			"#[cfg(not(test))]\nuse crate::real::go;\n#[cfg(test)]\nuse crate::mocks::go;\n\nfn run() {\n    go();\n}\n",
		),
	]);
	assert!(edge(&graph, "src/app.rs::run", "src/real.rs::go", RelationType::Calls).is_some());
	assert!(edge(&graph, "src/app.rs::run", "src/mocks.rs::go", RelationType::Calls).is_some());
}

#[test]
fn a_name_a_file_declares_and_re_exports_reaches_the_definition_three_hops_away() {
	let graph = build_graph(vec![
		rust_file("src/lib.rs", "mod blocking;\nmod runtime;\nmod app;\n"),
		rust_file(
			"src/blocking.rs",
			"cfg_rt! {\n    #[cfg(not(emscripten))]\n    pub(crate) use crate::runtime::go;\n\n    #[cfg(emscripten)]\n    pub(crate) fn go() {}\n}\n",
		),
		rust_file("src/runtime/mod.rs", "cfg_rt! {\n    mod inner;\n    pub(crate) use inner::go;\n}\n"),
		rust_file("src/runtime/inner/mod.rs", "mod pool;\npub(crate) use pool::go;\n"),
		rust_file("src/runtime/inner/pool.rs", "pub fn go() {}\n"),
		rust_file("src/app.rs", "fn run() {\n    use crate::blocking::go;\n    go();\n}\n"),
	]);
	assert!(edge(
		&graph,
		"src/app.rs::run",
		"src/runtime/inner/pool.rs::go",
		RelationType::Calls
	)
	.is_some());
	assert!(edge(&graph, "src/app.rs::run", "src/blocking.rs::go", RelationType::Calls).is_some());
}
