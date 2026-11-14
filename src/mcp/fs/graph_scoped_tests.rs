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

//! A scoped query must answer exactly what the whole-repository graph does.

use super::*;
use crate::mcp::fs::graph::{render_paths, render_traversal, walk_supported, GraphRequest, MAX_LISTED_RESULTS};
use crate::structural::graph::{build_graph, SourceFile};
use serial_test::serial;

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap()
		.block_on(future)
}

/// The scoped answer rendered as text, and its cap notices.
fn scoped(
	root: &Path,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
) -> (String, Vec<String>) {
	scoped_with_budget(root, relation, name, scope, depth, None)
}

/// The callers of the `crowd` name under a time budget.
fn scoped_within(root: &Path, budget: Option<Duration>) -> (String, Vec<String>) {
	scoped_with_budget(root, GraphRelation::Callers, Some("shared"), None, 1, budget)
}

fn scoped_with_budget(
	root: &Path,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
	budget: Option<Duration>,
) -> (String, Vec<String>) {
	let answer = block_on(async {
		let index = open(root).await?;
		build(index, relation, name, None, scope, depth, budget).await
	});
	match answer.unwrap() {
		Answer::Graph { snapshot, notices, .. } => (render(&snapshot, relation, name, scope, depth), notices),
		Answer::Message(text) => panic!("{relation:?} {name:?} was not traced: {text}"),
	}
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
	let root = tempfile::tempdir().unwrap();
	for (path, contents) in files {
		let full = root.path().join(path);
		std::fs::create_dir_all(full.parent().unwrap()).unwrap();
		std::fs::write(full, contents).unwrap();
	}
	root
}

fn render(
	snapshot: &GraphSnapshot,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
) -> String {
	let request = GraphRequest {
		relation,
		name,
		to: None,
		path: None,
		depth: None,
	};
	let (relations, direction) = relation.traversal().unwrap();
	render_traversal(snapshot, &request, scope, relations, direction, depth).unwrap()
}

/// The graph of every supported file of `root`: the oracle a scoped answer
/// is compared with. Only tests build it.
fn whole_repository(root: &Path) -> GraphSnapshot {
	let files: Vec<SourceFile> = walk_supported(root)
		.unwrap()
		.files
		.into_iter()
		.map(|(path, language)| SourceFile {
			contents: std::fs::read_to_string(root.join(&path)).unwrap(),
			path,
			language,
		})
		.collect();
	let graph = build_graph(files);
	let index = GraphIndex::new(&graph);
	GraphSnapshot {
		graph,
		index,
		unread: Vec::new(),
	}
}

/// The answer from the whole-repository graph and from the scoped build; both
/// must be the same text, and `expected` must be in it so an empty answer on
/// both sides cannot pass.
fn assert_same(
	root: &Path,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
	expected: &[&str],
) {
	let full = whole_repository(root);
	let from_full = render(&full, relation, name, scope, depth);
	let (from_scoped, _) = scoped(root, relation, name, scope, depth);
	assert_eq!(from_full, from_scoped, "{relation:?} {name:?} depth {depth}");
	for needle in expected {
		assert!(from_full.contains(needle), "{needle} missing from:\n{from_full}");
	}
}

/// The `path` answer of a scoped search under a time budget, and its notices.
fn scoped_path(
	root: &Path,
	name: &str,
	to: &str,
	scope: Option<&str>,
	depth: usize,
	budget: Option<Duration>,
) -> (String, Vec<String>) {
	let answer = block_on(async {
		let index = open(root).await?;
		build(index, GraphRelation::Path, Some(name), Some(to), scope, depth, budget).await
	});
	match answer.unwrap() {
		Answer::Graph { snapshot, notices, .. } => (
			render_path(&snapshot, name, to, scope, depth, !notices.is_empty()),
			notices,
		),
		Answer::Message(text) => (text, Vec::new()),
	}
}

fn render_path(
	snapshot: &GraphSnapshot,
	name: &str,
	to: &str,
	scope: Option<&str>,
	depth: usize,
	partial: bool,
) -> String {
	let request = GraphRequest {
		relation: GraphRelation::Path,
		name: Some(name),
		to: Some(to),
		path: None,
		depth: None,
	};
	render_paths(snapshot, &request, scope, depth, partial).unwrap()
}

/// The chains from the whole-repository graph and from the scoped search must
/// be the same text; `expected` must be in it.
fn assert_same_path(root: &Path, name: &str, to: &str, depth: usize, expected: &[&str]) {
	let from_full = render_path(&whole_repository(root), name, to, None, depth, false);
	let (from_scoped, notices) = scoped_path(root, name, to, None, depth, None);
	assert!(notices.is_empty(), "{notices:?}");
	assert_eq!(from_full, from_scoped, "path {name} -> {to} depth {depth}");
	for needle in expected {
		assert!(from_full.contains(needle), "{needle} missing from:\n{from_full}");
	}
}

const NOISE: (&str, &str) = ("src/noise.ts", "export function unrelated() { return 1; }\n");

#[test]
#[serial]
fn typescript_alias_imports_and_two_levels_of_callers() {
	let root = project(&[
		("src/a.ts", "export function helper() {}\n"),
		(
			"src/b.ts",
			"import { helper as h } from './a';\nexport function run() {\n\th();\n}\n",
		),
		(
			"src/c.ts",
			"import { run } from './b';\nexport function top() {\n\trun();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("helper"),
		None,
		1,
		&["src/b.ts:2  function  run"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("helper"),
		None,
		2,
		&["src/b.ts:2  function  run", "src/c.ts:2  function  top"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("top"),
		None,
		2,
		&["run", "helper"],
	);
	assert_same(
		root.path(),
		GraphRelation::Imports,
		None,
		Some("src/c.ts"),
		2,
		&["src/b.ts", "src/a.ts"],
	);
	assert_same(
		root.path(),
		GraphRelation::ImportedBy,
		None,
		Some("src/a.ts"),
		2,
		&["src/b.ts", "src/c.ts"],
	);
}

#[test]
#[serial]
fn go_package_imports_resolve_through_every_file_of_the_package() {
	let root = project(&[
		("src/net/http/server.go", "package http\n\nfunc Serve() {}\n"),
		("src/net/http/client.go", "package http\n\nfunc Do() {}\n"),
		(
			"cmd/app/main.go",
			"package main\n\nimport \"net/http\"\n\nfunc main() {\n\thttp.Do()\n}\n",
		),
		("cmd/other/other.go", "package other\n\nfunc Quiet() {}\n"),
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("Do"),
		None,
		1,
		&["cmd/app/main.go:5  function  main"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("main"),
		None,
		1,
		&["src/net/http/client.go:3  function  Do"],
	);
	for file in ["src/net/http/server.go", "src/net/http/client.go"] {
		assert_same(
			root.path(),
			GraphRelation::ImportedBy,
			None,
			Some(file),
			1,
			&["cmd/app/main.go"],
		);
	}
	// One import edge per package, not one per file.
	assert_same(
		root.path(),
		GraphRelation::Imports,
		None,
		Some("cmd/app/main.go"),
		1,
		&["src/net/http/client.go"],
	);
}

#[test]
#[serial]
fn rust_use_paths_and_module_calls() {
	let root = project(&[
		("src/util.rs", "pub fn helper() {\n\tinner();\n}\n\nfn inner() {}\n"),
		(
			"src/main.rs",
			"mod util;\nuse crate::util::helper;\n\nfn main() {\n\thelper();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("helper"),
		None,
		1,
		&["src/main.rs:4  function  main"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("main"),
		None,
		2,
		&["helper", "inner"],
	);
	assert_same(
		root.path(),
		GraphRelation::ImportedBy,
		None,
		Some("src/util.rs"),
		1,
		&["src/main.rs"],
	);
}

#[test]
#[serial]
fn python_from_dot_import_names_resolve_to_the_package_init() {
	let root = project(&[
		("pkg/__init__.py", "def helper():\n    pass\n"),
		("pkg/main.py", "from . import helper\n\n\ndef run():\n    helper()\n"),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("helper"),
		None,
		1,
		&["pkg/main.py:4  function  run"],
	);
	assert_same(
		root.path(),
		GraphRelation::Imports,
		None,
		Some("pkg/main.py"),
		1,
		&["pkg/__init__.py"],
	);
	assert_same(
		root.path(),
		GraphRelation::ImportedBy,
		None,
		Some("pkg/__init__.py"),
		1,
		&["pkg/main.py"],
	);
}

#[test]
#[serial]
fn type_relations_in_both_directions() {
	let root = project(&[
		("src/base.ts", "export class Base {}\n"),
		(
			"src/mid.ts",
			"import { Base } from './base';\nexport class Mid extends Base {}\n",
		),
		(
			"src/leaf.ts",
			"import { Mid } from './mid';\nexport class Leaf extends Mid {}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Subtypes,
		Some("Base"),
		None,
		2,
		&["Mid", "Leaf"],
	);
	assert_same(
		root.path(),
		GraphRelation::Supertypes,
		Some("Leaf"),
		None,
		2,
		&["Mid", "Base"],
	);
}

#[test]
#[serial]
fn a_name_in_too_many_definitions_is_listed_from_a_declarations_scan() {
	let files: Vec<(String, String)> = (0..12)
		.map(|n| (format!("src/m{n}.rs"), "pub fn new() {}\n".to_string()))
		.collect();
	let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	let root = project(&refs);
	let (text, _) = scoped(root.path(), GraphRelation::Callers, Some("new"), None, 1);
	assert!(text.starts_with("`new` has 12 definitions"), "{text}");
}

#[test]
#[serial]
fn re_exports_and_default_exports_keep_their_callers() {
	let root = project(&[
		("src/a.ts", "export function f() {}\n"),
		("src/index.ts", "export { f } from './a';\n"),
		(
			"src/user.ts",
			"import { f } from './index';\nexport function g() {\n\tf();\n}\n",
		),
		("src/d.ts", "export default function dflt() {}\n"),
		(
			"src/e.ts",
			"import dflt from './d';\nexport function h() {\n\tdflt();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("f"),
		None,
		1,
		&["src/user.ts:2  function  g"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("dflt"),
		None,
		1,
		&["src/e.ts:2  function  h"],
	);
	assert_same(
		root.path(),
		GraphRelation::ImportedBy,
		None,
		Some("src/a.ts"),
		1,
		&["src/index.ts"],
	);
}

#[test]
#[serial]
fn a_python_package_init_re_export_leads_callers_and_paths_to_the_definition() {
	// `reverse` is defined twice, so the call cannot be matched by name
	// alone: only the import through `pkg/__init__.py` points at one of them.
	let root = project(&[
		("pkg/base.py", "def reverse():\n    pass\n"),
		("pkg/__init__.py", "from .base import reverse\n"),
		("other/base.py", "def reverse():\n    pass\n"),
		(
			"app/shortcuts.py",
			"from pkg import reverse\n\n\ndef resolve_url():\n    reverse()\n\n\ndef redirect():\n    resolve_url()\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("reverse"),
		Some("pkg/base.py"),
		1,
		&["app/shortcuts.py:4  function  resolve_url"],
	);
	assert_same_path(
		root.path(),
		"redirect",
		"reverse",
		5,
		&["app/shortcuts.py:8 redirect -calls-> app/shortcuts.py:4 resolve_url -calls-> pkg/base.py:1 reverse"],
	);
}

#[test]
#[serial]
fn a_typescript_index_re_export_leads_callers_and_paths_to_the_definition() {
	let root = project(&[
		("src/lib/a.ts", "export function f() {}\nexport function w() {}\n"),
		("src/lib/index.ts", "export { f } from './a';\nexport * from './a';\n"),
		("src/other/b.ts", "export function f() {}\nexport function w() {}\n"),
		(
			"src/user.ts",
			"import { f, w } from './lib';\nexport function g() {\n\tf();\n}\nexport function h() {\n\tw();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("f"),
		Some("src/lib/a.ts"),
		1,
		&["src/user.ts:2  function  g"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("w"),
		Some("src/lib/a.ts"),
		1,
		&["src/user.ts:5  function  h"],
	);
	assert_same_path(root.path(), "g", "f", 5, &["src/user.ts:2 g -calls-> src/lib/a.ts:1 f"]);
}

#[test]
#[serial]
fn a_re_exporter_in_a_parent_directory_leads_callers_and_paths_to_the_definition() {
	let root = project(&[
		("src/lib/a.ts", "export function pd() {}\n"),
		("src/other/b.ts", "export function pd() {}\n"),
		("src/barrel.ts", "export * from './lib/a';\n"),
		(
			"src/user.ts",
			"import { pd } from './barrel';\nexport function g() {\n\tpd();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("pd"),
		Some("src/lib/a.ts"),
		1,
		&["src/user.ts:2  function  g"],
	);
	assert_same_path(
		root.path(),
		"g",
		"pd",
		5,
		&["src/user.ts:2 g -calls-> src/lib/a.ts:1 pd"],
	);
}

#[test]
#[serial]
fn an_aliased_re_export_is_followed_under_its_new_name() {
	let root = project(&[
		("src/lib/c.ts", "export function orig() {}\n"),
		("src/other/d.ts", "export function orig() {}\n"),
		("src/lib/exports.ts", "export { orig as renamed } from './c';\n"),
		(
			"src/user.ts",
			"import { renamed } from './lib/exports';\nexport function g() {\n\trenamed();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("orig"),
		Some("src/lib/c.ts"),
		1,
		&["src/user.ts:2  function  g"],
	);
}

#[test]
#[serial]
fn a_chain_of_two_re_exports_leads_callers_to_the_definition() {
	let root = project(&[
		("src/lib/a.ts", "export function deep() {}\n"),
		("src/other/b.ts", "export function deep() {}\n"),
		("src/lib/first.ts", "export * from './a';\n"),
		("src/second.ts", "export * from './lib/first';\n"),
		(
			"src/user.ts",
			"import { deep } from './second';\nexport function g() {\n\tdeep();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("deep"),
		Some("src/lib/a.ts"),
		1,
		&["src/user.ts:2  function  g"],
	);
}

#[test]
#[serial]
fn a_c_header_with_two_definitions_of_a_name_gives_callers_and_a_path_through_both() {
	// The shape of Linux `list.h`: an attribute macro before the return
	// type, `new` as a parameter name, and the same function defined on both
	// sides of an `#ifdef`.
	let header = "#ifdef CONFIG_DEBUG_LIST\nstatic __always_inline bool __list_add_valid(struct list_head *new,\n\t\t\t\t\t     struct list_head *prev)\n{\n\treturn true;\n}\n#else\nstatic inline bool __list_add_valid(struct list_head *new,\n\t\t\t\tstruct list_head *prev)\n{\n\treturn true;\n}\n#endif\n\nstatic __always_inline void __list_add(struct list_head *new, struct list_head *prev)\n{\n\tif (!__list_add_valid(new, prev))\n\t\treturn;\n}\n\nstatic inline void list_add_tail(struct list_head *new, struct list_head *head)\n{\n\t__list_add(new, head->prev);\n}\n";
	let root = project(&[("include/list.h", header), NOISE]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("__list_add_valid"),
		None,
		1,
		&["include/list.h:15  function  __list_add"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("__list_add"),
		None,
		1,
		&[
			"include/list.h:2  function  __list_add_valid",
			"include/list.h:8  function  __list_add_valid",
		],
	);
	assert_same_path(
		root.path(),
		"list_add_tail",
		"__list_add_valid",
		5,
		&[
			"include/list.h:21 list_add_tail",
			"include/list.h:2 __list_add_valid",
			"include/list.h:8 __list_add_valid",
		],
	);
}

#[test]
#[serial]
fn rust_pub_use_keeps_its_callers() {
	let root = project(&[
		("src/util.rs", "pub fn helper() {}\n"),
		// A second `helper` keeps the unique-name fallback from answering:
		// only the plain `use` binding can pick the right one.
		("src/other.rs", "pub fn helper() {}\n"),
		("src/lib.rs", "pub mod util;\npub mod other;\npub use util::helper;\n"),
		("src/main.rs", "use crate::helper;\n\nfn main() {\n\thelper();\n}\n"),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("helper"),
		None,
		1,
		&["src/main.rs:3  function  main"],
	);
}

#[test]
#[serial]
fn python_module_alias_calls_resolve_like_the_full_graph() {
	let root = project(&[
		("pkg/__init__.py", ""),
		("pkg/mod.py", "def f():\n    pass\n"),
		("app.py", "import pkg.mod as m\n\n\ndef run():\n    m.f()\n"),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("f"),
		None,
		1,
		&["app.py:4  function  run"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("run"),
		None,
		1,
		&["pkg/mod.py:1  function  f"],
	);
}

#[test]
#[serial]
fn a_package_qualified_call_reaches_the_function_not_a_method_of_the_same_name() {
	let root = project(&[
		(
			"src/net/http/server.go",
			"package http\n\ntype Server struct{}\n\nfunc (s *Server) ListenAndServe() error {\n\treturn nil\n}\n\nfunc ListenAndServe(addr string) error {\n\tserver := &Server{}\n\treturn server.ListenAndServe()\n}\n",
		),
		("src/net/http/client.go", "package http\n\nfunc Get() {}\n"),
		(
			"src/cmd/app/main.go",
			"package main\n\nimport \"net/http\"\n\nfunc main() {\n\thttp.ListenAndServe(\":80\")\n}\n",
		),
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("ListenAndServe"),
		None,
		1,
		&["src/cmd/app/main.go:5  function  main"],
	);
	// The call on a receiver is the method, although the function shares its name.
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("ListenAndServe"),
		None,
		1,
		&["src/net/http/server.go:5  function  Server::ListenAndServe"],
	);
}

#[test]
#[serial]
fn a_callee_name_spelled_everywhere_resolves_through_the_imports_of_the_caller() {
	let mut files: Vec<(String, String)> = (0..=MAX_FILES_PER_FOLLOWED_NAME)
		.map(|n| {
			(
				format!("src/noise{n}.ts"),
				format!("export function n{n}() {{\n\tget();\n}}\n"),
			)
		})
		.collect();
	files.push(("src/lib.ts".into(), "export function get() {}\n".into()));
	files.push((
		"src/app.ts".into(),
		"import { get } from './lib';\nexport function run() {\n\tget();\n}\n".into(),
	));
	let borrowed: Vec<(&str, &str)> = files
		.iter()
		.map(|(path, text)| (path.as_str(), text.as_str()))
		.collect();
	let root = project(&borrowed);
	let (answer, _) = scoped(root.path(), GraphRelation::Callees, Some("run"), None, 1);
	assert!(answer.contains("src/lib.ts:1  function  get"), "{answer}");
}

#[test]
fn files_are_found_by_their_name_and_their_directory() {
	let tokens = file_tokens("src/net/http/server.go");
	assert_eq!(tokens, BTreeSet::from(["server".to_string(), "http".to_string()]));
	// A crate root is also what `crate::` names in the importers.
	assert_eq!(
		file_tokens("lib.rs"),
		BTreeSet::from(["lib".to_string(), "crate".to_string()])
	);
	assert_eq!(file_tokens("src/foo.d.ts"), BTreeSet::from(["foo".to_string()]));
	assert_eq!(file_tokens("src/common/util.ts"), BTreeSet::from(["util".to_string()]));
	assert_eq!(
		file_tokens("src/components/index.ts"),
		BTreeSet::from(["index".to_string(), "components".to_string()])
	);
}

#[test]
#[serial]
fn a_name_with_several_definitions_looks_only_at_definition_files_and_their_importers() {
	let root = project(&[
		("src/a.ts", "export function render() {}\n"),
		("src/b.ts", "export function render() {}\n"),
		(
			"src/user_a.ts",
			"import { render } from './a';\nexport function drawA() {\n\trender();\n}\n",
		),
		(
			"src/user_b.ts",
			"import { render } from './b';\nexport function drawB() {\n\trender();\n}\n",
		),
		// Spells the name but imports neither definition: its call resolves to nothing.
		(
			"src/local.ts",
			"function render() {}\nexport function drawLocal() {\n\trender();\n}\n",
		),
		NOISE,
	]);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("render"),
		None,
		1,
		&["src/user_a.ts:2  function  drawA", "src/user_b.ts:2  function  drawB"],
	);
	assert_same(
		root.path(),
		GraphRelation::Callers,
		Some("render"),
		Some("src/a.ts"),
		1,
		&["src/user_a.ts:2  function  drawA"],
	);
}

#[test]
#[serial]
fn go_goroutine_receiver_and_package_calls_are_all_edges() {
	let root = project(&[
		(
			"pkg/svc/svc.go",
			"package svc\n\ntype Worker struct{}\n\nfunc (w *Worker) Run() {}\n\nfunc (w *Worker) Stop() {}\n\nfunc Helper() {}\n",
		),
		(
			"cmd/app/main.go",
			"package main\n\nimport \"app/pkg/svc\"\n\nfunc main() {\n\tw := &svc.Worker{}\n\tgo w.Run()\n\tw.Stop()\n\tsvc.Helper()\n}\n",
		),
		("cmd/other/other.go", "package other\n\nfunc Quiet() {}\n"),
	]);
	assert_same(
		root.path(),
		GraphRelation::Callees,
		Some("main"),
		None,
		1,
		&[
			"pkg/svc/svc.go:5  function  Worker::Run",
			"pkg/svc/svc.go:7  function  Worker::Stop",
			"pkg/svc/svc.go:9  function  Helper",
		],
	);
	for callee in ["Run", "Stop", "Helper"] {
		assert_same(
			root.path(),
			GraphRelation::Callers,
			Some(callee),
			None,
			1,
			&["cmd/app/main.go:5  function  main"],
		);
	}
}

#[test]
#[serial]
fn the_import_line_filter_keeps_importers_and_drops_other_mentions() {
	// (importer, mentions the name without importing it)
	let cases = [
		("ts", "import { x } from './util';\n", "const util = 1;\n"),
		(
			"go",
			"package a\n\nimport \"app/util\"\n",
			"package b\n\nvar u = \"util://example\"\n",
		),
		(
			"java",
			"import app.util.X;\nclass A {}\n",
			"class B { int util = 1; }\n",
		),
		("rb", "require_relative 'util'\n", "util = 1\n"),
		("cpp", "#include \"util.h\"\n", "int util = 1;\n"),
		("py", "from app import util\n", "util = 1\nresult = util.run()\n"),
		("rs", "use crate::util;\n", "let util = 1;\n"),
		("php", "<?php\nuse App\\util;\n", "<?php\n$x = util(1);\n"),
	];
	for (extension, importer, mention) in cases {
		let importer_path = format!("src/importer.{extension}");
		let mention_path = format!("src/mention.{extension}");
		let root = project(&[(importer_path.as_str(), importer), (mention_path.as_str(), mention)]);
		let index = CodeIndexes::global().index(root.path());
		let found = block_on(async {
			index.refresh().await.unwrap();
			index.marked_lines("util", LineKind::Import, None).await.unwrap().files
		});
		let paths: Vec<String> = found.iter().filter_map(|id| index.path_of(*id)).collect();
		assert_eq!(paths, vec![importer_path], "{extension}");
	}
}

#[test]
#[serial]
fn a_grouped_import_keeps_the_continuation_line_that_holds_only_the_name() {
	let root = project(&[
		("pkg/importer.py", "from . import (\n\tother,\n\tutil,\n)\n"),
		("pkg/mention.py", "util = 1\n"),
	]);
	let index = CodeIndexes::global().index(root.path());
	let found = block_on(async {
		index.refresh().await.unwrap();
		index.marked_lines("util", LineKind::Import, None).await.unwrap().files
	});
	let paths: Vec<String> = found.iter().filter_map(|id| index.path_of(*id)).collect();
	assert_eq!(paths, vec!["pkg/importer.py".to_string()]);
}

fn crowd(users: usize) -> tempfile::TempDir {
	let mut files: Vec<(String, String)> = vec![("src/def.ts".into(), "export function shared() {}\n".into())];
	for n in 0..users {
		files.push((
			format!("src/user{n:04}.ts"),
			format!("import {{ shared }} from './def';\nexport function f{n}() {{\n\tshared();\n}}\n"),
		));
	}
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	project(&borrowed)
}

#[test]
#[serial]
fn with_a_path_the_cap_cuts_the_list_and_the_notice_says_how_to_continue() {
	let root = crowd(MAX_PARSED_FILES_PER_QUERY + 50);
	let (text, notices) = scoped(
		root.path(),
		GraphRelation::Callers,
		Some("shared"),
		Some("src/def.ts"),
		1,
	);
	assert!(text.contains("src/user0000.ts:2  function  f0"), "{text}");
	assert!(!text.contains("f649"), "files past the cap must not be parsed");
	let notice = notices.join("\n");
	assert!(
		notice.contains("`shared` and what it leads to span 651 candidate files"),
		"{notice}"
	);
	assert!(notice.contains("narrow it with `path` or `Owner::name`"), "{notice}");
}

#[test]
#[serial]
fn an_unnarrowed_name_spelled_past_the_trace_limit_is_not_traced_and_nothing_is_parsed() {
	let root = crowd(MAX_FILES_SPELLING_TO_TRACE + 50);
	let answer = block_on(async {
		let index = open(root.path()).await?;
		build(index, GraphRelation::Callers, Some("shared"), None, None, 1, None).await
	})
	.unwrap();
	let Answer::Message(text) = answer else {
		panic!("expected the too-common message");
	};
	assert!(text.contains("spelled in about 3051 files"), "{text}");
	assert!(text.contains("`path`") && text.contains("`Owner::name`"), "{text}");
	assert_eq!(CodeIndexes::global().index(root.path()).parse_count(), 0);
}

#[test]
#[serial]
fn a_definition_search_stopped_by_the_cap_is_partial_and_repeating_finds_the_definition() {
	// 700 files spell the name and none looks like a declaration (the
	// definition is an indented method), so the definition in file 650 lies
	// past the 600-file cap of one call.
	let mut files: Vec<(String, String)> = Vec::new();
	for n in 0..700 {
		let contents = if n == 650 {
			"export class K {\n  shared() {}\n}\n".to_string()
		} else {
			format!("export const v{n} = shared;\n")
		};
		files.push((format!("src/m{n:04}.ts"), contents));
	}
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	let root = project(&borrowed);

	let (first, notices) = scoped(root.path(), GraphRelation::Callers, Some("shared"), None, 1);
	assert!(!notices.is_empty(), "the cut was not recorded: {first}");
	assert!(notices.join("\n").contains("700 candidate files"), "{notices:?}");

	let (_, notices) = scoped(root.path(), GraphRelation::Callers, Some("shared"), None, 1);
	assert!(notices.is_empty(), "{notices:?}");
}

#[test]
#[serial]
fn a_path_end_not_found_within_the_cap_is_partial_not_a_final_negative() {
	// `shared` is spelled in 700 files; its only definition (an indented
	// method, so it does not look like one) is past what one call examines.
	let mut files: Vec<(String, String)> = Vec::new();
	for n in 0..700 {
		let contents = if n == 650 {
			"export class K {\n  shared() {}\n}\n".to_string()
		} else {
			format!("export const v{n} = shared;\n")
		};
		files.push((format!("src/m{n:04}.ts"), contents));
	}
	files.push(("src/start.ts".to_string(), "export function start() {}\n".to_string()));
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	let root = project(&borrowed);
	let run = || {
		let answer = block_on(async {
			let index = open(root.path()).await?;
			build(index, GraphRelation::Path, Some("start"), Some("shared"), None, 5, None).await
		})
		.unwrap();
		let Answer::Graph { notices, .. } = answer else {
			panic!("expected a graph");
		};
		notices
	};
	assert!(!run().is_empty(), "the unfinished search for `to` was not reported");
}

#[test]
#[serial]
fn importers_of_a_file_named_by_a_token_spelled_everywhere_are_still_searched() {
	let root = crowd(MAX_FILES_SPELLING_TO_TRACE + 50);
	let answer = block_on(async {
		let index = open(root.path()).await?;
		build(
			index,
			GraphRelation::ImportedBy,
			None,
			None,
			Some("src/def.ts"),
			1,
			None,
		)
		.await
	})
	.unwrap();
	let Answer::Graph { info, snapshot, .. } = answer else {
		panic!("expected a graph");
	};
	// The token is searched like any other: the files that import `def.ts`
	// are found, the same as when the token is rare.
	assert!(info.is_empty(), "{info:?}");
	assert!(
		snapshot
			.graph
			.relationships
			.iter()
			.any(|edge| edge.target == "src/def.ts" && edge.relation == crate::structural::graph::RelationType::Imports),
		"no importer was found"
	);
}

#[test]
#[serial]
fn an_import_scan_that_runs_out_of_time_is_partial_and_repeating_completes_it() {
	// The stem `stem` is spelled in more files than one scan chunk holds.
	let mut files: Vec<(String, String)> = (0..1000)
		.map(|n| (format!("src/m{n:04}.ts"), "// stem\n".to_string()))
		.collect();
	files.push(("src/stem.ts".to_string(), "export const x = 1;\n".to_string()));
	files.push(("src/user_a.ts".to_string(), "import { x } from './stem';\n".to_string()));
	files.push(("src/user_z.ts".to_string(), "import { x } from './stem';\n".to_string()));
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	let root = project(&borrowed);

	let run = || {
		let answer = block_on(async {
			let index = open(root.path()).await?;
			build(
				index,
				GraphRelation::ImportedBy,
				None,
				None,
				Some("src/stem.ts"),
				1,
				Some(Duration::ZERO),
			)
			.await
		})
		.unwrap();
		let Answer::Graph { snapshot, notices, .. } = answer else {
			panic!("expected a graph");
		};
		let importers = snapshot
			.graph
			.relationships
			.iter()
			.filter(|edge| {
				edge.target == "src/stem.ts" && edge.relation == crate::structural::graph::RelationType::Imports
			})
			.count();
		(notices, importers)
	};

	let (first_notices, first_importers) = run();
	assert!(!first_notices.is_empty(), "the unread files were not reported");
	let (mut notices, mut importers) = (first_notices, first_importers);
	for _ in 0..8 {
		if notices.is_empty() {
			break;
		}
		(notices, importers) = run();
	}
	assert!(notices.is_empty(), "repeating did not finish: {notices:?}");
	assert_eq!(importers, 2, "both importers are found once the scan completes");
}

#[test]
#[serial]
fn a_single_definition_is_traced_past_the_cap_and_repeating_completes_the_answer() {
	let users = MAX_PARSED_FILES_PER_QUERY + 50;
	let root = crowd(users);
	let last_caller = format!("function  f{}", users - 1);
	// The list shows 100 results; the closing line still counts every caller.
	let (first, first_notices) = scoped(root.path(), GraphRelation::Callers, Some("shared"), None, 1);
	assert!(!first.contains(&last_caller), "files past the cap must not be parsed");
	let notice = first_notices.join("\n");
	assert!(
		notice.contains("`shared` and what it leads to span 651 candidate files"),
		"{notice}"
	);
	assert!(notice.contains("repeat the query"), "{notice}");
	let mut answer = (first, first_notices);
	for _ in 0..2 {
		if answer.1.is_empty() {
			break;
		}
		answer = scoped(root.path(), GraphRelation::Callers, Some("shared"), None, 1);
	}
	assert!(answer.1.is_empty(), "still partial after repeating: {:?}", answer.1);
	assert!(answer.0.contains(&format!("{users} in all")), "{}", answer.0);
	assert!(answer.0.contains("function  f0"), "{}", answer.0);
}

#[test]
#[serial]
fn an_expired_time_budget_answers_with_what_was_examined_and_repeating_goes_on() {
	let users = 4 * PARSE_CHUNK;
	let root = crowd(users);
	let last_caller = format!("function  f{}", users - 1);
	let mut answer = scoped_within(root.path(), Some(Duration::ZERO));
	assert!(!answer.0.contains(&last_caller), "{}", answer.0);
	let notice = answer.1.join("\n");
	assert!(notice.contains("repeat the query"), "{notice}");
	// The notice counts what the query examined: every file this call parsed.
	let parsed = CodeIndexes::global().index(root.path()).parse_count();
	assert!(
		notice.contains(&format!("{} candidate files, {parsed} examined", users + 1)),
		"{notice}"
	);
	for _ in 0..=users / PARSE_CHUNK {
		if answer.1.is_empty() {
			break;
		}
		answer = scoped_within(root.path(), Some(Duration::ZERO));
	}
	assert!(answer.1.is_empty(), "still partial after repeating: {:?}", answer.1);
	assert!(answer.0.contains(&last_caller), "{}", answer.0);
}

#[test]
#[serial]
fn the_result_list_stops_at_the_cap_and_the_closing_line_counts_the_rest() {
	let users = MAX_LISTED_RESULTS + 30;
	let root = crowd(users);
	let (text, notices) = scoped(root.path(), GraphRelation::Callers, Some("shared"), None, 1);
	assert!(notices.is_empty(), "{notices:?}");
	assert!(text.contains("[+30 more"), "{text}");
	assert!(text.contains("narrow with `path` or `Owner::name`"), "{text}");
}

#[test]
#[serial]
fn a_second_query_reuses_the_parsed_files() {
	let root = project(&[
		("src/a.ts", "export function helper() {}\n"),
		(
			"src/b.ts",
			"import { helper } from './a';\nexport function run() {\n\thelper();\n}\n",
		),
		NOISE,
	]);
	let index = CodeIndexes::global().index(root.path());
	scoped(root.path(), GraphRelation::Callers, Some("helper"), None, 1);
	let after_first = index.parse_count();
	assert!(after_first >= 2, "{after_first}");
	scoped(root.path(), GraphRelation::Callers, Some("helper"), None, 1);
	assert_eq!(index.parse_count(), after_first);
}

#[test]
#[serial]
fn an_edited_file_is_parsed_again_and_nothing_else() {
	let root = project(&[
		("src/a.ts", "export function helper() {}\n"),
		(
			"src/b.ts",
			"import { helper } from './a';\nexport function run() {\n\thelper();\n}\n",
		),
		NOISE,
	]);
	let index = CodeIndexes::global().index(root.path());
	let (before, _) = scoped(root.path(), GraphRelation::Callers, Some("helper"), None, 1);
	assert!(!before.contains("function  again"), "{before}");
	let parsed = index.parse_count();
	std::fs::write(
		root.path().join("src/b.ts"),
		"import { helper } from './a';\nexport function run() {\n\thelper();\n}\nexport function again() {\n\thelper();\n}\n",
	)
	.unwrap();
	let (after, _) = scoped(root.path(), GraphRelation::Callers, Some("helper"), None, 1);
	assert!(after.contains("src/b.ts:5  function  again"), "{after}");
	assert_eq!(index.parse_count(), parsed + 1);
}

#[test]
#[serial]
fn a_path_runs_through_files_of_one_language_and_matches_the_whole_repository() {
	let root = project(&[
		(
			"src/a.ts",
			"import { b } from './b';\nexport function a1() {\n\tb();\n}\n",
		),
		(
			"src/b.ts",
			"import { c } from './c';\nexport function b() {\n\tc();\n}\n",
		),
		("src/c.ts", "export function c() {}\n"),
		NOISE,
	]);
	assert_same_path(
		root.path(),
		"a1",
		"c",
		5,
		&["src/a.ts:2 a1 -calls-> src/b.ts:2 b -calls-> src/c.ts:1 c"],
	);
}

#[test]
#[serial]
fn a_path_follows_go_package_calls_python_package_imports_and_type_edges() {
	let go = project(&[
		(
			"pkg/svc/svc.go",
			"package svc\n\ntype Worker struct{}\n\nfunc (w *Worker) Run() {}\n\nfunc (w *Worker) Stop() {}\n\nfunc Helper() {}\n",
		),
		(
			"cmd/app/main.go",
			"package main\n\nimport \"app/pkg/svc\"\n\nfunc main() {\n\tw := &svc.Worker{}\n\tgo w.Run()\n\tw.Stop()\n\tsvc.Helper()\n}\n",
		),
	]);
	assert_same_path(
		go.path(),
		"main",
		"Helper",
		5,
		&["cmd/app/main.go:5 main -calls-> pkg/svc/svc.go:9 Helper"],
	);

	let python = project(&[
		("pkg/__init__.py", "def helper():\n    pass\n"),
		("pkg/main.py", "from . import helper\n\n\ndef run():\n    helper()\n"),
		NOISE,
	]);
	assert_same_path(
		python.path(),
		"run",
		"helper",
		5,
		&["pkg/main.py:4 run -calls-> pkg/__init__.py:1 helper"],
	);

	let types = project(&[
		("src/base.ts", "export class Base {}\n"),
		(
			"src/mid.ts",
			"import { Base } from './base';\nexport class Mid extends Base {}\n",
		),
		(
			"src/leaf.ts",
			"import { Mid } from './mid';\nexport class Leaf extends Mid {}\n",
		),
		NOISE,
	]);
	assert_same_path(
		types.path(),
		"Leaf",
		"Base",
		5,
		&["src/leaf.ts:2 Leaf -extends-> src/mid.ts:2 Mid -extends-> src/base.ts:1 Base"],
	);
}

#[test]
#[serial]
fn the_shortest_of_two_chains_is_the_answer() {
	let root = project(&[
		("src/t.ts", "export function t() {}\n"),
		(
			"src/a.ts",
			"import { t } from './t';\nexport function a() {\n\tt();\n}\n",
		),
		(
			"src/d.ts",
			"import { t } from './t';\nexport function d() {\n\tt();\n}\n",
		),
		(
			"src/c.ts",
			"import { d } from './d';\nexport function c() {\n\td();\n}\n",
		),
		(
			"src/s.ts",
			"import { a } from './a';\nimport { c } from './c';\nexport function s() {\n\ta();\n\tc();\n}\n",
		),
	]);
	assert_same_path(
		root.path(),
		"s",
		"t",
		5,
		&["src/s.ts:3 s -calls-> src/a.ts:2 a -calls-> src/t.ts:1 t"],
	);
	let (text, _) = scoped_path(root.path(), "s", "t", None, 5, None);
	assert_eq!(text.lines().count(), 1, "{text}");
}

#[test]
#[serial]
fn without_a_chain_the_answer_says_so_and_nothing_cut_the_search_short() {
	let root = project(&[
		("src/a.ts", "export function a() {}\n"),
		("src/b.ts", "export function b() {}\n"),
	]);
	let (text, notices) = scoped_path(root.path(), "a", "b", None, 5, None);
	assert!(text.starts_with("No path from \"a\" to \"b\" within depth 5"), "{text}");
	assert!(notices.is_empty(), "{notices:?}");
	assert_eq!(
		text,
		render_path(&whole_repository(root.path()), "a", "b", None, 5, false)
	);

	let (text, _) = scoped_path(root.path(), "a", "missing", None, 5, None);
	assert!(text.starts_with("No definition of \"missing\""), "{text}");
}

/// `n0` calls `n1`, which calls `n2` and so on, each in a file of its own.
fn relay(length: usize) -> tempfile::TempDir {
	let files: Vec<(String, String)> = (0..=length)
		.map(|n| {
			let body = if n < length {
				format!(
					"import {{ n{} }} from './n{}';\nexport function n{n}() {{\n\tn{}();\n}}\n",
					n + 1,
					n + 1,
					n + 1
				)
			} else {
				format!("export function n{n}() {{}}\n")
			};
			(format!("src/n{n}.ts"), body)
		})
		.collect();
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	project(&borrowed)
}

#[test]
#[serial]
fn an_expired_budget_gives_a_partial_notice_and_repeating_goes_on_to_the_chain() {
	let root = relay(4);
	let (first, notices) = scoped_path(root.path(), "n0", "n4", None, 5, Some(Duration::ZERO));
	assert!(
		!first.contains("within depth"),
		"a cut search must not claim there is no path: {first}"
	);
	assert!(first.contains("found in the files examined so far"), "{first}");
	let notice = notices.join("\n");
	assert!(notice.contains("repeat the query"), "{notice}");
	assert!(notice.contains("level(s) forward and"), "{notice}");

	let mut answer = (first, notices);
	for _ in 0..8 {
		if answer.1.is_empty() {
			break;
		}
		answer = scoped_path(root.path(), "n0", "n4", None, 5, Some(Duration::ZERO));
	}
	assert!(answer.1.is_empty(), "still partial after repeating: {:?}", answer.1);
	assert!(
		answer.0.contains("src/n0.ts:2 n0 -calls-> src/n1.ts:2 n1 -calls-> src/n2.ts:2 n2 -calls-> src/n3.ts:2 n3 -calls-> src/n4.ts:1 n4"),
		"{}",
		answer.0
	);
}

#[test]
#[serial]
fn the_levels_run_out_before_the_chain_and_the_answer_says_how_deep_it_looked() {
	let root = relay(4);
	let (text, notices) = scoped_path(root.path(), "n0", "n4", None, 3, None);
	assert!(
		text.starts_with("No path from \"n0\" to \"n4\" within depth 3"),
		"{text}"
	);
	assert!(notices.is_empty(), "{notices:?}");
}

#[test]
#[serial]
fn a_target_spelled_in_thousands_of_files_is_still_reached_from_the_start() {
	let root = crowd(MAX_FILES_SPELLING_TO_TRACE + 50);
	std::fs::write(
		root.path().join("src/top.ts"),
		"import { f0 } from './user0000';\nexport function top() {\n\tf0();\n}\n",
	)
	.unwrap();
	let (text, notices) = scoped_path(root.path(), "top", "shared", None, 5, None);
	assert!(notices.is_empty(), "{notices:?}");
	assert!(
		text.contains("src/top.ts:2 top -calls-> src/user0000.ts:2 f0 -calls-> src/def.ts:1 shared"),
		"{text}"
	);
}

#[test]
#[serial]
fn a_search_from_the_cheaper_end_leaves_the_wide_side_unparsed() {
	let calls: String = (0..30).map(|n| format!("\tg{n}();\n")).collect();
	let mut files: Vec<(String, String)> = (0..30)
		.map(|n| (format!("src/g{n}.ts"), format!("export function g{n}() {{}}\n")))
		.collect();
	files.push((
		"src/top.ts".into(),
		format!("import {{ mid }} from './mid';\nexport function top() {{\n{calls}\tmid();\n}}\n"),
	));
	files.push((
		"src/mid.ts".into(),
		"import { target } from './target';\nexport function mid() {\n\ttarget();\n}\n".into(),
	));
	files.push(("src/target.ts".into(), "export function target() {}\n".into()));
	let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
	let root = project(&borrowed);
	let index = CodeIndexes::global().index(root.path());
	let (text, notices) = scoped_path(root.path(), "top", "target", None, 5, None);
	assert!(notices.is_empty(), "{notices:?}");
	assert!(
		text.contains("src/top.ts:2 top -calls-> src/mid.ts:2 mid -calls-> src/target.ts:1 target"),
		"{text}"
	);
	let parsed = index.parse_count();
	assert!(parsed < 10, "the thirty callees of `top` were parsed: {parsed}");
}

#[test]
#[serial]
fn an_edit_between_two_path_calls_is_seen() {
	let root = project(&[
		(
			"src/a.ts",
			"import { b } from './b';\nexport function a() {\n\tb();\n}\n",
		),
		("src/b.ts", "export function b() {}\n"),
	]);
	let (before, _) = scoped_path(root.path(), "a", "b", None, 5, None);
	assert!(before.contains("src/a.ts:2 a -calls-> src/b.ts:1 b"), "{before}");

	std::fs::write(
		root.path().join("src/a.ts"),
		"import { b } from './b';\nexport function a() {\n\t// no longer calls it\n}\n",
	)
	.unwrap();
	let (after, notices) = scoped_path(root.path(), "a", "b", None, 5, None);
	assert!(
		after.starts_with("No path from \"a\" to \"b\" within depth 5"),
		"{after}"
	);
	assert!(notices.is_empty(), "{notices:?}");
}
