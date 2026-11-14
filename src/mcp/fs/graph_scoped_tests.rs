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
use crate::mcp::fs::graph::{load_graph_blocking, render_traversal, GraphRequest, MAX_LISTED_RESULTS};
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
		build(index, relation, name, scope, depth, budget).await
	});
	match answer.unwrap() {
		Answer::Graph { snapshot, notices } => (render(&snapshot, relation, name, scope, depth), notices),
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
	let full = load_graph_blocking(root).unwrap();
	let from_full = render(&full, relation, name, scope, depth);
	let (from_scoped, _) = scoped(root, relation, name, scope, depth);
	assert_eq!(from_full, from_scoped, "{relation:?} {name:?} depth {depth}");
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
fn rust_pub_use_keeps_its_callers() {
	let root = project(&[
		("src/util.rs", "pub fn helper() {}\n"),
		("src/lib.rs", "pub mod util;\npub use util::helper;\n"),
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
	assert_eq!(file_tokens("lib.rs"), BTreeSet::from(["lib".to_string()]));
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
			index.marked_lines("util", LineKind::Import).await.unwrap()
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
		index.marked_lines("util", LineKind::Import).await.unwrap()
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
		build(index, GraphRelation::Callers, Some("shared"), None, 1, None).await
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
