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

//! A scoped build must answer exactly what the whole-repository graph does.

use super::*;
use crate::mcp::fs::graph::{load_graph_blocking, render_traversal, GraphRequest};
use serial_test::serial;

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
	let Outcome::Scoped(scoped) = build(root, relation, name, scope, depth).unwrap() else {
		panic!("{relation:?} {name:?} fell back to the full graph");
	};
	let from_scoped = render(&scoped, relation, name, scope, depth);
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
	let Outcome::Scoped(snapshot) = build(root.path(), GraphRelation::Callers, Some("new"), None, 1).unwrap() else {
		panic!("scoped");
	};
	let text = render(&snapshot, GraphRelation::Callers, Some("new"), None, 1);
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
	let Outcome::Scoped(scoped) = build(root.path(), GraphRelation::Callees, Some("run"), None, 1).unwrap() else {
		panic!("the callees of run fell back to the full graph");
	};
	let answer = render(&scoped, GraphRelation::Callees, Some("run"), None, 1);
	assert!(answer.contains("src/lib.ts:1  function  get"), "{answer}");
}

#[test]
fn whole_word_matching_ignores_longer_identifiers() {
	let automaton = AhoCorasick::builder()
		.match_kind(MatchKind::Standard)
		.build(["foo", "foobar"])
		.unwrap();
	let mentioned = |bytes: &[u8]| mentioned_names(&automaton, bytes, 2);
	assert_eq!(mentioned(b"call foo();"), vec![0]);
	assert_eq!(mentioned(b"foobar"), vec![1]);
	assert!(mentioned(b"my_foo(); foos(); xfoo").is_empty());
	assert_eq!(mentioned(b"a.foo foobar"), vec![0, 1]);
}

#[test]
fn files_are_found_by_their_name_and_their_directory() {
	let tokens = file_tokens("src/net/http/server.go");
	assert_eq!(tokens, BTreeSet::from(["server".to_string(), "http".to_string()]));
	assert_eq!(file_tokens("lib.rs"), BTreeSet::from(["lib".to_string()]));
	assert_eq!(file_tokens("src/foo.d.ts"), BTreeSet::from(["foo".to_string()]));
	assert_eq!(
		file_tokens("src/components/index.ts"),
		BTreeSet::from(["index".to_string(), "components".to_string()])
	);
}
