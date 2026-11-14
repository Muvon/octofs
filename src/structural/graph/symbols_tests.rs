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

//! Symbol extraction and edge resolution tests. Resolution tests use
//! hand-made `SymbolFileData`; extraction tests parse real source with the
//! test language.

use super::*;
use crate::structural::graph::test_language_tests::{lookup, TestLanguage};

// Local item shadows the glob import: tests state the files and the language
// lookup, the shim builds the import resolver production code shares.
fn discover_symbol_relationships(files: &[SymbolFileData], lookup: LanguageLookup) -> Vec<CodeRelationship> {
	let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
	let refs: Vec<&SymbolFileData> = files.iter().collect();
	super::discover_symbol_relationships(&refs, &mut ImportResolver::new(&paths, lookup), None)
}

fn declaration(name: &str, kind: &str, start_line: u32, end_line: u32) -> SymbolDecl {
	SymbolDecl {
		name: name.to_string(),
		kind: kind.to_string(),
		owner: None,
		start_line,
		end_line,
	}
}

fn owned_declaration(owner: &str, name: &str, kind: &str, start_line: u32, end_line: u32) -> SymbolDecl {
	SymbolDecl {
		owner: Some(owner.to_string()),
		..declaration(name, kind, start_line, end_line)
	}
}

fn call(line: u32, qualifier: Option<&str>, name: &str) -> (u32, CallTarget) {
	(
		line,
		CallTarget {
			name: name.to_string(),
			qualifier: qualifier.map(str::to_string),
		},
	)
}

fn file(path: &str) -> SymbolFileData {
	SymbolFileData {
		path: path.to_string(),
		language: "rust".to_string(),
		imports: Vec::new(),
		import_bindings: Vec::new(),
		calls: Vec::new(),
		type_relations: Vec::new(),
		symbols: Vec::new(),
		memo: ImportMemo::default(),
	}
}

fn find<'a>(
	edges: &'a [CodeRelationship],
	source: &str,
	target: &str,
	relation: RelationType,
) -> Option<&'a CodeRelationship> {
	edges
		.iter()
		.find(|edge| edge.source == source && edge.target == target && edge.relation == relation)
}

#[test]
fn node_kinds_map_to_coarse_symbol_kinds() {
	assert_eq!(symbol_kind_from_node_kind("function_item"), "function");
	assert_eq!(symbol_kind_from_node_kind("method_definition"), "function");
	assert_eq!(symbol_kind_from_node_kind("class_declaration"), "class");
	assert_eq!(symbol_kind_from_node_kind("struct_item"), "struct");
	assert_eq!(symbol_kind_from_node_kind("trait_item"), "trait");
	assert_eq!(symbol_kind_from_node_kind("interface_declaration"), "interface");
	assert_eq!(symbol_kind_from_node_kind("enum_item"), "enum");
	assert_eq!(symbol_kind_from_node_kind("mod_item"), "module");
	assert_eq!(symbol_kind_from_node_kind("const_item"), "const");
	assert_eq!(symbol_kind_from_node_kind("macro_definition"), "macro");
	assert_eq!(symbol_kind_from_node_kind("type_item"), "type");
	assert_eq!(symbol_kind_from_node_kind("whatever"), "symbol");
}

#[test]
fn duplicate_symbol_names_receive_distinct_stable_ids() {
	let symbols = vec![
		declaration("new", "function", 4, 8),
		declaration("new", "function", 20, 24),
		declaration("run", "function", 30, 35),
		owned_declaration("Service", "run", "function", 40, 45),
	];
	assert_eq!(
		symbol_node_ids("src/lib.rs", &symbols),
		vec![
			"src/lib.rs::new@5",
			"src/lib.rs::new@21",
			"src/lib.rs::run",
			"src/lib.rs::Service::run"
		]
	);
}

/// Reference implementation: the plain linear scan `find_owning_symbol`
/// replaces.
fn linear_owning_symbol(symbols: &[SymbolDecl], line: u32, functions_only: bool) -> Option<usize> {
	symbols
		.iter()
		.enumerate()
		.filter(|(_, symbol)| {
			line >= symbol.start_line && line <= symbol.end_line && (!functions_only || symbol.kind == "function")
		})
		.min_by_key(|(_, symbol)| symbol.end_line.saturating_sub(symbol.start_line))
		.map(|(index, _)| index)
}

#[test]
fn find_owning_symbol_matches_linear_reference_on_nested_overlapping_and_tied_ranges() {
	// inner_a (function) and const_x (const) both span 20..=30: a cross-kind
	// tie. inner_a and inner_b (function) both span 10 and contain line 25: a
	// same-kind tie.
	let symbols = vec![
		declaration("outer", "class", 0, 100),
		declaration("mid", "function", 10, 50),
		declaration("inner_a", "function", 20, 30),
		declaration("inner_b", "function", 15, 25),
		declaration("const_x", "const", 20, 30),
		declaration("sibling", "function", 60, 70),
		declaration("tiny", "function", 22, 22),
	];

	for line in 0..=100u32 {
		for functions_only in [false, true] {
			let order = symbol_start_order(&symbols, functions_only);
			assert_eq!(
				find_owning_symbol(&symbols, &order, line),
				linear_owning_symbol(&symbols, line, functions_only),
				"mismatch at line={line} functions_only={functions_only}"
			);
		}
	}
}

#[test]
fn same_file_unique_global_and_ambiguous_names_resolve_by_scope() {
	// a.rs: run calls helper (same file), unique_fn (only in b.rs) and `new`
	// (in b.rs and c.rs). b.rs and c.rs both declare `Base`.
	let mut a = file("src/a.rs");
	a.calls = ["helper", "unique_fn", "new"]
		.into_iter()
		.map(|name| call(5, None, name))
		.collect();
	a.type_relations = vec![TypeRelationDecl {
		line: 5,
		source_name: Some("run".to_string()),
		kind: TypeRelationKind::Extends,
		target_name: "Base".to_string(),
	}];
	a.symbols = vec![
		declaration("run", "function", 0, 10),
		declaration("helper", "function", 20, 25),
	];
	let mut b = file("src/b.rs");
	b.symbols = vec![
		declaration("unique_fn", "function", 0, 1),
		declaration("new", "function", 2, 3),
		declaration("Base", "struct", 4, 5),
	];
	let mut c = file("src/c.rs");
	c.symbols = vec![
		declaration("new", "function", 0, 1),
		declaration("Base", "struct", 2, 3),
	];

	let edges = discover_symbol_relationships(&[a, b, c], &lookup);

	let same_file = find(&edges, "src/a.rs::run", "src/a.rs::helper", RelationType::Calls).expect("same-file call");
	assert_eq!(same_file.confidence, CONFIDENCE_SAME_FILE);
	assert_eq!(same_file.line, Some(6));

	let global = find(&edges, "src/a.rs::run", "src/b.rs::unique_fn", RelationType::Calls).expect("unique global call");
	assert_eq!(global.confidence, CONFIDENCE_GLOBAL_UNIQUE);

	// `new` is declared twice elsewhere: calls never fan out to both.
	assert!(find(&edges, "src/a.rs::run", "src/b.rs::new", RelationType::Calls).is_none());
	assert!(find(&edges, "src/a.rs::run", "src/c.rs::new", RelationType::Calls).is_none());

	// Type relations do record every candidate, at the ambiguous confidence.
	for target in ["src/b.rs::Base", "src/c.rs::Base"] {
		let edge = find(&edges, "src/a.rs::run", target, RelationType::Extends).expect("ambiguous extends");
		assert_eq!(edge.confidence, CONFIDENCE_GLOBAL_AMBIGUOUS);
		assert_eq!(edge.line, Some(6));
	}
}

#[test]
fn qualified_and_self_calls_resolve_to_the_correct_owner() {
	let mut services = file("src/services.rs");
	services.calls = vec![call(5, Some("self"), "run"), call(25, Some("Worker"), "run")];
	services.symbols = vec![
		owned_declaration("Service", "execute", "function", 0, 10),
		owned_declaration("Service", "run", "function", 12, 16),
		owned_declaration("Worker", "dispatch", "function", 20, 30),
		owned_declaration("Worker", "run", "function", 32, 36),
	];

	let edges = discover_symbol_relationships(&[services], &lookup);

	for (source, target) in [
		("src/services.rs::Service::execute", "src/services.rs::Service::run"),
		("src/services.rs::Worker::dispatch", "src/services.rs::Worker::run"),
	] {
		let edge = find(&edges, source, target, RelationType::Calls).expect("owner-resolved call");
		assert_eq!(edge.confidence, CONFIDENCE_OWNER_IN_FILE);
	}
	assert_eq!(edges.len(), 2);
}

#[test]
fn imported_symbol_alias_resolves_to_original_declaration() {
	let mut source = file("src/main.js");
	source.imports = vec!["./utils".to_string()];
	source.import_bindings = vec![ImportBinding {
		local_name: "h".to_string(),
		imported_name: Some("helper".to_string()),
		import_path: "./utils".to_string(),
	}];
	source.calls = vec![call(2, None, "h")];
	source.symbols = vec![declaration("main", "function", 0, 5)];
	let mut target = file("src/utils.js");
	target.symbols = vec![declaration("helper", "function", 0, 1)];

	let edges = discover_symbol_relationships(&[source, target], &lookup);

	let edge = find(&edges, "src/main.js::main", "src/utils.js::helper", RelationType::Calls).expect("aliased call");
	assert_eq!(edge.confidence, CONFIDENCE_BOUND_IMPORT);
}

#[test]
fn module_qualified_call_resolves_inside_the_matching_imported_file() {
	let mut source = file("src/main.js");
	source.imports = vec!["./utils".to_string(), "./other".to_string()];
	source.calls = vec![call(2, Some("utils"), "helper")];
	source.symbols = vec![declaration("main", "function", 0, 5)];
	let mut utils = file("src/utils.js");
	utils.symbols = vec![declaration("helper", "function", 0, 1)];
	let mut other = file("src/other.js");
	other.symbols = vec![declaration("helper", "function", 0, 1)];

	let edges = discover_symbol_relationships(&[source, utils, other], &lookup);

	let edge = find(&edges, "src/main.js::main", "src/utils.js::helper", RelationType::Calls).expect("module call");
	assert_eq!(edge.confidence, CONFIDENCE_MODULE_QUALIFIED);
	assert!(find(&edges, "src/main.js::main", "src/other.js::helper", RelationType::Calls).is_none());
}

#[test]
fn unknown_receiver_keeps_only_a_capped_unique_match() {
	let mut caller = file("src/a.rs");
	caller.calls = vec![call(1, Some("client"), "send"), call(2, Some("client"), "ping")];
	caller.symbols = vec![declaration("run", "function", 0, 5)];
	let mut b = file("src/b.rs");
	b.symbols = vec![
		declaration("send", "function", 0, 1),
		declaration("ping", "function", 2, 3),
	];
	let mut c = file("src/c.rs");
	c.symbols = vec![declaration("ping", "function", 0, 1)];

	let edges = discover_symbol_relationships(&[caller, b, c], &lookup);

	let send = find(&edges, "src/a.rs::run", "src/b.rs::send", RelationType::Calls).expect("unique send");
	assert!(send.confidence <= CONFIDENCE_UNKNOWN_RECEIVER_CAP);
	assert!(edges.iter().all(|edge| !edge.target.ends_with("::ping")));
}

#[test]
fn recursive_call_is_a_certain_self_edge() {
	let mut recursive = file("src/a.rs");
	recursive.calls = vec![call(2, None, "walk")];
	recursive.symbols = vec![declaration("walk", "function", 0, 5)];

	let edges = discover_symbol_relationships(&[recursive], &lookup);

	let edge = find(&edges, "src/a.rs::walk", "src/a.rs::walk", RelationType::Calls).expect("self edge");
	assert_eq!(edge.confidence, CONFIDENCE_CERTAIN);
}

#[test]
fn repeated_calls_collapse_to_the_earliest_line_at_the_highest_confidence() {
	let mut caller = file("src/a.rs");
	caller.calls = vec![call(9, None, "helper"), call(3, None, "helper")];
	caller.symbols = vec![
		declaration("run", "function", 0, 10),
		declaration("helper", "function", 20, 25),
	];

	let edges = discover_symbol_relationships(&[caller], &lookup);

	assert_eq!(edges.len(), 1);
	assert_eq!(edges[0].line, Some(4));
}

#[test]
fn files_without_a_language_implementation_add_no_edges() {
	let mut orphan = file("src/a.cob");
	orphan.language = "cobol".to_string();
	orphan.calls = vec![call(1, None, "helper")];
	orphan.symbols = vec![
		declaration("run", "function", 0, 5),
		declaration("helper", "function", 6, 8),
	];

	assert!(discover_symbol_relationships(&[orphan], &lookup).is_empty());
}

#[test]
fn extraction_reads_symbols_owners_calls_imports_and_type_relations() {
	let code = "use std::collections::HashMap;

pub struct Config {
	pub name: String,
}

pub trait Named {}

impl Named for Config {}

impl Config {
	pub fn new() -> Self {
		Config { name: String::new() }
	}
}

fn helper() -> u32 {
	42
}

pub fn main_fn() {
	let v = helper();
	println!(\"{}\", v);
}
";
	let data = extract_symbols_from_contents(code, &TestLanguage, &lookup).expect("source should parse");

	assert_eq!(data.imports, vec!["std::collections::HashMap"]);

	let symbol = |name: &str| data.symbols.iter().find(|symbol| symbol.name == name).expect("symbol");
	assert_eq!(symbol("Config").kind, "struct");
	assert_eq!(symbol("Named").kind, "trait");
	let new = symbol("new");
	assert_eq!(new.kind, "function");
	assert_eq!(new.owner.as_deref(), Some("Config"));
	assert_eq!(symbol("helper").owner, None);
	let main_fn = symbol("main_fn");
	assert!(symbol("helper").start_line < main_fn.start_line);
	assert!(main_fn.end_line > main_fn.start_line);

	assert!(data.calls.iter().any(|(_, call)| call.name == "helper"));
	let new_call = data
		.calls
		.iter()
		.find(|(_, call)| call.name == "new")
		.expect("String::new call");
	assert_eq!(new_call.1.qualifier.as_deref(), Some("String"));

	let implementation = data
		.type_relations
		.iter()
		.find(|relation| relation.kind == TypeRelationKind::Implements)
		.expect("Config implements Named");
	assert_eq!(implementation.source_name.as_deref(), Some("Config"));
	assert_eq!(implementation.target_name, "Named");
}

#[test]
fn embedded_block_symbols_and_calls_land_at_file_lines() {
	let host = "// one\n// two\n// three\n// four\n";
	let host_language = lookup("host").expect("host language");

	let data = extract_symbols_from_contents(host, host_language.as_ref(), &lookup).expect("host should parse");

	let helper = data
		.symbols
		.iter()
		.find(|symbol| symbol.name == "helper")
		.expect("helper");
	let run = data.symbols.iter().find(|symbol| symbol.name == "run").expect("run");
	assert_eq!(helper.start_line, 3);
	assert_eq!(run.start_line, 4);
	assert_eq!(data.calls.len(), 1);
	assert_eq!(data.calls[0].0, 4);
}

#[test]
fn embedded_block_in_an_unknown_language_is_dropped_without_failing_the_host() {
	let host = "// one\n";
	let host_language = lookup("host").expect("host language");
	let no_languages = |_: &str| -> Option<Box<dyn Language>> { None };

	let data = extract_symbols_from_contents(host, host_language.as_ref(), &no_languages).expect("host should parse");

	assert!(data.symbols.is_empty());
}

#[test]
fn walk_survives_nesting_that_would_overflow_a_recursive_walk() {
	const NESTING: usize = 20_000;
	let code = format!("fn deep() {}{}", "{ ".repeat(NESTING), "} ".repeat(NESTING));

	let data = extract_symbols_from_contents(&code, &TestLanguage, &lookup).expect("source should parse");

	assert!(data.symbols.iter().any(|symbol| symbol.name == "deep"));
}

fn first_named_child_bindings(
	grammar: tree_sitter::Language,
	language: &str,
	code: &str,
	imports: &[&str],
) -> Vec<ImportBinding> {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&grammar).expect("grammar should load");
	let tree = parser.parse(code, None).expect("code should parse");
	let statement = tree.root_node().named_child(0).expect("import statement");
	let imports: Vec<String> = imports.iter().map(|import| import.to_string()).collect();
	extract_import_bindings(language, statement, code, &imports)
}

#[test]
fn javascript_named_and_namespace_imports_bind_local_names() {
	let named = first_named_child_bindings(
		tree_sitter_javascript::LANGUAGE.into(),
		"javascript",
		"import { a as b, c } from './x';",
		&["./x"],
	);
	let pairs: Vec<(&str, Option<&str>)> = named
		.iter()
		.map(|binding| (binding.local_name.as_str(), binding.imported_name.as_deref()))
		.collect();
	assert_eq!(pairs, vec![("b", Some("a")), ("c", Some("c"))]);
	assert!(named.iter().all(|binding| binding.import_path == "./x"));

	let namespace = first_named_child_bindings(
		tree_sitter_javascript::LANGUAGE.into(),
		"javascript",
		"import * as ns from './x';",
		&["./x"],
	);
	assert_eq!(namespace.len(), 1);
	assert_eq!(namespace[0].local_name, "ns");
	assert_eq!(namespace[0].imported_name, None);
}

#[test]
fn rust_use_alias_binds_the_alias_to_the_imported_name() {
	let bindings = first_named_child_bindings(
		tree_sitter_rust::LANGUAGE.into(),
		"rust",
		"use crate::util::helper as h;",
		&["crate::util::helper"],
	);
	assert_eq!(bindings.len(), 1);
	assert_eq!(bindings[0].local_name, "h");
	assert_eq!(bindings[0].imported_name.as_deref(), Some("helper"));
	assert_eq!(bindings[0].import_path, "crate::util::helper");
}

#[test]
fn rust_use_without_alias_yields_no_bindings() {
	assert!(first_named_child_bindings(
		tree_sitter_rust::LANGUAGE.into(),
		"rust",
		"use std::fmt;",
		&["std::fmt"]
	)
	.is_empty());
}

#[test]
fn a_call_outside_any_function_belongs_to_the_file() {
	let mut script = file("src/main.rs");
	script.calls = vec![call(7, None, "start")];
	script.symbols = vec![declaration("boot", "function", 0, 2)];
	let mut lib = file("src/lib.rs");
	lib.symbols = vec![declaration("start", "function", 0, 1)];

	let edges = discover_symbol_relationships(&[script, lib], &lookup);

	let edge = find(&edges, "src/main.rs", "src/lib.rs::start", RelationType::Calls).expect("file-level call");
	assert_eq!(edge.line, Some(8));
}

#[test]
fn a_type_relation_belongs_to_the_declared_type_not_the_enclosing_symbol() {
	let mut model = file("src/model.rs");
	model.symbols = vec![declaration("Foo", "struct", 0, 1), declaration("Render", "trait", 2, 3)];
	let mut implementation = file("src/imp.rs");
	implementation.symbols = vec![declaration("inner", "module", 0, 9)];
	implementation.type_relations = vec![TypeRelationDecl {
		line: 3,
		source_name: Some("Foo".to_string()),
		kind: TypeRelationKind::Implements,
		target_name: "Render".to_string(),
	}];

	let edges = discover_symbol_relationships(&[model, implementation], &lookup);

	assert!(find(
		&edges,
		"src/model.rs::Foo",
		"src/model.rs::Render",
		RelationType::Implements
	)
	.is_some());
	assert!(find(
		&edges,
		"src/imp.rs::inner",
		"src/model.rs::Render",
		RelationType::Implements
	)
	.is_none());
}

#[test]
fn an_import_is_resolved_once_per_source_file_and_import_string() {
	use std::sync::atomic::{AtomicUsize, Ordering};
	let lookups = AtomicUsize::new(0);
	let counting = |name: &str| {
		lookups.fetch_add(1, Ordering::SeqCst);
		lookup(name)
	};
	let paths = vec!["src/main.rs".to_string(), "src/util.rs".to_string()];
	let mut resolver = ImportResolver::new(&paths, &counting);

	for _ in 0..3 {
		assert_eq!(
			*resolver.resolve("rust", "src/main.rs", "crate::util::helper"),
			vec!["src/util.rs".to_string()]
		);
		assert!(resolver.resolve("rust", "src/main.rs", "std::fmt").is_empty());
	}
	assert_eq!(
		lookups.load(Ordering::SeqCst),
		2,
		"each (file, import) pair costs one language lookup"
	);

	resolver.resolve("rust", "src/other.rs", "crate::util::helper");
	assert_eq!(lookups.load(Ordering::SeqCst), 3, "another source file is a new key");
}

#[test]
fn every_name_only_match_is_at_or_below_the_guess_tier() {
	for tier in [
		CONFIDENCE_GLOBAL_UNIQUE,
		CONFIDENCE_UNKNOWN_RECEIVER_CAP,
		CONFIDENCE_GLOBAL_AMBIGUOUS,
		CONFIDENCE_SAME_FILE_AMBIGUOUS,
		CONFIDENCE_IMPORTED_AMBIGUOUS,
	] {
		assert!(tier <= CONFIDENCE_NAME_ONLY_MAX, "{tier}");
	}
	for tier in [
		CONFIDENCE_IMPORTED_FILE,
		CONFIDENCE_SAME_FILE,
		CONFIDENCE_MODULE_QUALIFIED,
	] {
		assert!(tier > CONFIDENCE_NAME_ONLY_MAX, "{tier}");
	}
}
