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

//! Ruby outline and graph extraction tests.

use super::Ruby;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::{CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Tree};

fn parse(source: &str) -> Tree {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&Ruby {}.get_ts_language()).expect("ruby grammar");
	parser.parse(source, None).expect("parse")
}

/// Pre-order, iterative collection of every named node of `kind`. The `class`
/// and `module` keywords are anonymous nodes of the same kind and are skipped.
fn nodes_of_kind<'a>(root: Node<'a>, kind: &str) -> Vec<Node<'a>> {
	let mut found = Vec::new();
	let mut cursor = root.walk();
	loop {
		if cursor.node().is_named() && cursor.node().kind() == kind {
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

fn demo_app() -> FileRegistry {
	registry(&[
		"app/models/invoice.rb",
		"app/models/helpers/util.rb",
		"lib/billing.rb",
		"vendor/gems/money/lib/money.rb",
	])
}

#[test]
fn classes_modules_and_methods_are_items() {
	let src = "module Outer\n\tclass Foo\n\t\tdef bar\n\t\tend\n\tend\n\n\tdef self.baz\n\tend\nend\n";
	let items = outline("a.rb", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("module", "Outer"),
			("class", "Foo"),
			("method", "bar"),
			("method", "baz")
		]
	);
}

#[test]
fn qualified_module_names_are_named_by_their_last_segment() {
	let src = "module Outer::Inner\nend\n";
	let items = outline("a.rb", src).unwrap();
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].name, "Inner");
}

#[test]
fn name_extensions_and_symbol_kinds() {
	let lang = Ruby {};
	assert_eq!(
		lang.get_symbol_kinds(),
		vec!["method", "singleton_method", "class", "module"]
	);
}

#[test]
fn require_and_load_calls_become_imports() {
	for (source, expected) in [
		("require \"json\"\n", "json"),
		("require_relative \"helpers/util\"\n", "relative:helpers/util"),
		("load 'legacy.rb'\n", "legacy.rb"),
	] {
		let tree = parse(source);
		let calls = nodes_of_kind(tree.root_node(), "call");
		let (imports, exports) = Ruby {}.extract_imports_exports(calls[0], source);
		assert_eq!(imports, vec![expected], "imports for {source:?}");
		assert!(exports.is_empty());
	}
}

#[test]
fn an_ordinary_call_is_not_an_import() {
	let source = "puts \"hi\"\n";
	let tree = parse(source);
	let calls = nodes_of_kind(tree.root_node(), "call");
	let (imports, exports) = Ruby {}.extract_imports_exports(calls[0], source);
	assert!(imports.is_empty());
	assert!(exports.is_empty());
}

#[test]
fn require_style_calls_are_not_function_calls() {
	for source in [
		"require \"json\"\n",
		"require_relative \"helpers/util\"\n",
		"load 'legacy.rb'\n",
	] {
		let tree = parse(source);
		let calls = nodes_of_kind(tree.root_node(), "call");
		assert!(
			Ruby {}.extract_function_calls(calls[0], source).is_empty(),
			"{source:?}"
		);
	}
}

#[test]
fn calls_keep_their_receiver_as_a_qualifier() {
	let source = "def go\n  helper()\n  client.send_it(1)\n  Widget.new\nend\n";
	let tree = parse(source);
	let lang = Ruby {};
	let targets: Vec<CallTarget> = nodes_of_kind(tree.root_node(), "call")
		.into_iter()
		.flat_map(|node| lang.extract_function_calls(node, source))
		.collect();
	assert_eq!(
		targets,
		vec![
			CallTarget {
				name: "helper".to_string(),
				qualifier: None,
			},
			CallTarget {
				name: "send_it".to_string(),
				qualifier: Some("client".to_string()),
			},
			// `Widget.new` constructs the class, like `new Widget` elsewhere.
			CallTarget {
				name: "Widget".to_string(),
				qualifier: None,
			},
		]
	);
}

#[test]
fn a_node_that_is_not_a_call_reports_no_calls() {
	let source = "value = 1\n";
	let tree = parse(source);
	let nodes = nodes_of_kind(tree.root_node(), "assignment");
	assert!(Ruby {}.extract_function_calls(nodes[0], source).is_empty());
}

#[test]
fn a_class_with_a_superclass_reports_an_extends_relation() {
	let source = "class Invoice < Base::Record\nend\n";
	let tree = parse(source);
	let classes = nodes_of_kind(tree.root_node(), "class");
	assert_eq!(
		Ruby {}.extract_type_relations(classes[0], source),
		vec![(TypeRelationKind::Extends, "Record".to_string())]
	);
}

#[test]
fn a_plain_class_or_module_declares_no_type_relations() {
	for (source, kind) in [("class Plain\nend\n", "class"), ("module M\nend\n", "module")] {
		let tree = parse(source);
		let nodes = nodes_of_kind(tree.root_node(), kind);
		assert!(Ruby {}.extract_type_relations(nodes[0], source).is_empty(), "{kind}");
	}
}

#[test]
fn include_extend_and_prepend_report_implements_relations_from_the_enclosing_type() {
	let source = "module Billing\n  include Auditable\nend\n\nclass Invoice < Base\n  include Comparable, Util::Taxable\n  extend Finders\n  prepend Logging\n  helper.include Ignored\n\n  class Nested\n    include Inner\n  end\nend\n";
	let tree = parse(source);
	let lang = Ruby {};
	let relations: Vec<Vec<(TypeRelationKind, String)>> = ["module", "class"]
		.iter()
		.flat_map(|kind| nodes_of_kind(tree.root_node(), kind))
		.map(|node| lang.extract_type_relations(node, source))
		.collect();
	assert_eq!(
		relations,
		vec![
			vec![(TypeRelationKind::Implements, "Auditable".to_string())],
			vec![
				(TypeRelationKind::Extends, "Base".to_string()),
				(TypeRelationKind::Implements, "Comparable".to_string()),
				(TypeRelationKind::Implements, "Taxable".to_string()),
				(TypeRelationKind::Implements, "Finders".to_string()),
				(TypeRelationKind::Implements, "Logging".to_string()),
			],
			vec![(TypeRelationKind::Implements, "Inner".to_string())],
		]
	);
}

#[test]
fn a_vendored_gem_match_respects_path_components() {
	let files = registry(&["vendor/gems/money/lib/money.rb", "vendor/gems/x/lib/honey.rb"]);
	let lang = Ruby {};
	assert_eq!(lang.resolve_import("ney", "app/main.rb", &files), None);
	assert_eq!(
		lang.resolve_import("money", "app/main.rb", &files),
		Some("vendor/gems/money/lib/money.rb".to_string())
	);
}

#[test]
fn symbol_owner_walks_up_to_the_enclosing_class_or_module() {
	let source = "module Billing\n  class Invoice\n    def total\n    end\n  end\n\n  def helper\n  end\nend\n";
	let tree = parse(source);
	let methods = nodes_of_kind(tree.root_node(), "method");
	let lang = Ruby {};
	assert_eq!(
		lang.extract_symbol_owner(methods[0], source),
		Some("Invoice".to_string())
	);
	assert_eq!(
		lang.extract_symbol_owner(methods[1], source),
		Some("Billing".to_string())
	);
}

#[test]
fn a_method_in_a_singleton_class_block_belongs_to_the_enclosing_class() {
	let source = "class Config\n  class << self\n    def load\n    end\n  end\nend\n";
	let tree = parse(source);
	let methods = nodes_of_kind(tree.root_node(), "method");
	assert_eq!(
		Ruby {}.extract_symbol_owner(methods[0], source),
		Some("Config".to_string())
	);
}

#[test]
fn a_relative_require_resolves_against_the_requiring_files_directory() {
	let files = demo_app();
	let lang = Ruby {};
	let source = "app/models/invoice.rb";
	assert_eq!(
		lang.resolve_import("relative:helpers/util", source, &files),
		Some("app/models/helpers/util.rb".to_string())
	);
	assert_eq!(
		lang.resolve_import("./helpers/util", source, &files),
		Some("app/models/helpers/util.rb".to_string())
	);
}

#[test]
fn an_absolute_require_falls_back_to_load_paths_then_vendored_gems() {
	let lang = Ruby {};
	let source = "app/models/invoice.rb";
	assert_eq!(
		lang.resolve_import("billing", source, &demo_app()),
		Some("lib/billing.rb".to_string())
	);
	assert_eq!(
		lang.resolve_import("money", source, &demo_app()),
		Some("vendor/gems/money/lib/money.rb".to_string())
	);
	assert_eq!(lang.resolve_import("nonexistent", source, &demo_app()), None);
}

#[test]
fn graph_links_calls_imports_and_superclass_across_files() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "ruby",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"app/main.rb",
			"require_relative \"util\"\n\nclass App < Base\n  def run\n    helper()\n  end\nend\n",
		),
		file("app/util.rb", "def helper\nend\n"),
		file("app/base.rb", "class Base\nend\n"),
	]);
	let has = |source: &str, target: &str, relation: RelationType| {
		graph
			.relationships
			.iter()
			.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
	};
	assert!(has("app/main.rb", "app/util.rb", RelationType::Imports));
	assert!(has("app/main.rb::App::run", "app/util.rb::helper", RelationType::Calls));
	assert!(has("app/main.rb::App", "app/base.rb::Base", RelationType::Extends));
}

#[test]
fn new_reaches_the_class_and_a_bare_statement_reaches_the_method() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "ruby",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file("app/widget.rb", "class Widget\n  def initialize\n  end\nend\n"),
		file(
			"app/main.rb",
			"require_relative(\"widget\")\n\ndef boot\n  prepare\n  Widget.new\nend\n\ndef prepare\nend\n",
		),
	]);
	let has = |source: &str, target: &str| {
		graph
			.relationships
			.iter()
			.any(|edge| edge.source == source && edge.target == target && edge.relation == RelationType::Calls)
	};
	assert!(has("app/main.rb::boot", "app/widget.rb::Widget"));
	assert!(has("app/main.rb::boot", "app/main.rb::prepare"));
}

#[test]
fn parenthesised_requires_bare_statement_calls_and_rb_suffixes_are_handled() {
	let source = "require(\"json\")\nrequire_relative('lib/x.rb')\nrequire name\n";
	let tree = parse(source);
	let imports: Vec<String> = nodes_of_kind(tree.root_node(), "call")
		.into_iter()
		.flat_map(|node| Ruby {}.extract_imports_exports(node, source).0)
		.collect();
	assert_eq!(imports, vec!["json", "relative:lib/x.rb"]);

	let files = registry(&["app/main.rb", "app/lib/x.rb"]);
	assert_eq!(
		Ruby {}
			.resolve_import("relative:lib/x.rb", "app/main.rb", &files)
			.as_deref(),
		Some("app/lib/x.rb")
	);

	let source = "def setup\nend\n\ndef go\n  setup\n  value = other\nend\n";
	let tree = parse(source);
	let names: Vec<String> = nodes_of_kind(tree.root_node(), "identifier")
		.into_iter()
		.flat_map(|node| Ruby {}.extract_function_calls(node, source))
		.map(|call| call.name)
		.collect();
	// `setup` stands alone as a statement; `other` is the value of an assignment.
	assert_eq!(names, vec!["setup"]);
}

#[test]
fn a_bare_name_is_a_call_only_when_a_method_of_that_name_is_in_scope() {
	let bare_calls = |source: &str| -> Vec<String> {
		let tree = parse(source);
		nodes_of_kind(tree.root_node(), "identifier")
			.into_iter()
			.flat_map(|node| Ruby {}.extract_function_calls(node, source))
			.map(|call| call.name)
			.collect()
	};
	// A local assigned in the method hides the method of the same name.
	assert!(bare_calls("def setup\nend\n\ndef go\n  setup = 1\n  setup\nend\n").is_empty());
	// So does a parameter.
	assert!(bare_calls("def setup\nend\n\ndef go(setup)\n  setup\nend\n").is_empty());
	// A name no method of the file declares, in a file that pulls in nothing.
	assert!(bare_calls("def go\n  setup\nend\n").is_empty());
	// A required file does not make a bare name a call: the name may as
	// well be a variable, and only a method the file declares is certain.
	assert!(bare_calls("require_relative \"util\"\n\ndef go\n  setup\nend\n").is_empty());
	// A method of the enclosing class counts.
	assert_eq!(
		bare_calls("class A\n  def setup\n  end\n\n  def go\n    setup\n  end\nend\n"),
		vec!["setup"]
	);
}
