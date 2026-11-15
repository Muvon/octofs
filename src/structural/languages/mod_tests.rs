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

//! Soundness of the per-language line predicates against the real extractors:
//! a text pre-filter built on `line_may_import` / `line_may_inherit` must
//! never reject a file the tree-sitter extractor reports an edge for, and it
//! must reject the lines that only mention the name.

use super::svelte::script_sources;
use super::{get_language, Language, PackageScope, TypeRelationKind};
use tree_sitter::{Node, Parser, Tree};

/// Every node of the tree in document order, visited with an iterative walk.
fn for_each_node(tree: &Tree, mut visit: impl FnMut(Node)) {
	let mut cursor = tree.walk();
	loop {
		visit(cursor.node());
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

fn parse(language: &dyn Language, source: &str) -> Tree {
	let mut parser = Parser::new();
	parser.set_language(&language.get_ts_language()).expect("grammar");
	parser.parse(source, None).expect("parse")
}

fn language(name: &str) -> Box<dyn Language> {
	get_language(name).unwrap_or_else(|| panic!("unknown language {name}"))
}

fn collect_imports(language: &dyn Language, source: &str) -> Vec<String> {
	let tree = parse(language, source);
	let mut imports = Vec::new();
	for_each_node(&tree, |node| {
		imports.extend(language.extract_imports_exports(node, source).0)
	});
	imports
}

fn collect_relations(language: &dyn Language, source: &str) -> Vec<(TypeRelationKind, String)> {
	let tree = parse(language, source);
	let mut relations = Vec::new();
	for_each_node(&tree, |node| {
		relations.extend(language.extract_type_relations(node, source))
	});
	relations
}

/// The pre-filter itself: some line of the file may be an import of `name`.
fn file_may_import(language: &dyn Language, source: &str, name: &str) -> bool {
	source.lines().any(|line| language.line_may_import(line, name))
}

fn file_may_inherit(language: &dyn Language, source: &str, name: &str) -> bool {
	source.lines().any(|line| language.line_may_inherit(line, name))
}

/// The extractor reports `import_path` and the filter keeps the file that
/// spells `name`.
fn assert_import_sound(language_name: &str, source: &str, import_path: &str, name: &str) {
	let language = language(language_name);
	let imports = collect_imports(&*language, source);
	assert!(
		imports.iter().any(|import| import == import_path),
		"{language_name}: extractor reports {imports:?}, not {import_path:?}, for {source:?}"
	);
	assert!(
		file_may_import(&*language, source, name),
		"{language_name}: no line of {source:?} may import {name:?}"
	);
}

fn assert_relation_sound(language_name: &str, source: &str, kind: TypeRelationKind, parent: &str) {
	let language = language(language_name);
	let relations = collect_relations(&*language, source);
	assert!(
		relations.contains(&(kind, parent.to_string())),
		"{language_name}: extractor reports {relations:?}, not {kind:?} {parent:?}, for {source:?}"
	);
	assert!(
		file_may_inherit(&*language, source, parent),
		"{language_name}: no line of {source:?} may name the parent {parent:?}"
	);
}

fn import_lines(language_name: &str, line: &str, name: &str) -> bool {
	language(language_name).line_may_import(line, name)
}

fn inherit_lines(language_name: &str, line: &str, name: &str) -> bool {
	language(language_name).line_may_inherit(line, name)
}

#[test]
fn a_line_needs_the_whole_identifier() {
	assert!(import_lines("go", "import \"net/http\"", "http"));
	assert!(!import_lines("go", "import \"net/https\"", "http"));
	assert!(!import_lines("go", "import \"net/http\"", ""));
	assert!(!import_lines("python", "import httpx", "http"));
}

#[test]
fn keyword_markers_stand_alone() {
	// `use` is a marker, `user` is not.
	assert!(import_lines("rust", "use crate::util;", "util"));
	assert!(!import_lines("rust", "let user = util;", "util"));
	assert!(!import_lines("rust", "let user_util = 1;", "util"));
}

#[test]
fn go_imports_end_in_the_name_before_the_quote() {
	for (source, path, name) in [
		("package main\n\nimport \"net/http\"\n", "net/http", "http"),
		("package main\n\nimport h \"net/http\"\n", "net/http", "http"),
		(
			"package main\n\nimport (\n\t\"os\"\n\t\"net/http\"\n)\n",
			"net/http",
			"http",
		),
		(
			"package main\n\nimport (\n\t_ \"github.com/lib/pq\" // driver\n)\n",
			"github.com/lib/pq",
			"pq",
		),
	] {
		assert_import_sound("go", source, path, name);
	}
}

#[test]
fn a_go_string_that_only_holds_the_name_is_not_an_import() {
	for line in [
		"\tu := \"http://example.com\"",
		"\tresp, err := client.Get(\"http://example.com/x\")",
		"\tvar http = 1",
		"import \"net/httputil\"",
	] {
		assert!(!import_lines("go", line, "http"), "{line}");
	}
	assert!(import_lines("go", "\t\"net/http\"", "http"));
}

#[test]
fn go_embedded_fields_are_found_on_their_own_line() {
	let source = "package main\n\ntype Server struct {\n\tLogger\n}\n";
	assert_relation_sound("go", source, TypeRelationKind::Extends, "Logger");
	for (line, name) in [
		("\t*Logger", "Logger"),
		("\tlog.Logger", "Logger"),
		("\tLogger `json:\"l\"`", "Logger"),
	] {
		assert!(inherit_lines("go", line, name), "{line}");
	}
	assert!(!inherit_lines("go", "\tl := Logger{}", "Logger"));
}

#[test]
fn javascript_imports_end_in_the_name_before_the_quote() {
	for (source, path, name) in [
		("import x from './util';\n", "./util", "util"),
		("import './styles.css';\n", "./styles.css", "styles"),
		("import {\n\ta,\n\tb,\n} from \"./multi\";\n", "./multi", "multi"),
		("export { f } from './reexport';\n", "./reexport", "reexport"),
		("export * from './all';\n", "./all", "all"),
	] {
		assert_import_sound("javascript", source, path, name);
	}
}

#[test]
fn a_script_line_that_only_holds_the_name_is_not_an_import() {
	for line in [
		"const util = require('./util2');",
		"\tconsole.log('util is ready');",
		"const util = 1;",
		"import x from './util/helpers';",
	] {
		assert!(!import_lines("javascript", line, "util"), "{line}");
	}
	assert!(import_lines("javascript", "import x from './util.js';", "util"));
	assert!(import_lines(
		"typescript",
		"export type { T } from \"../types/util\";",
		"util"
	));
}

#[test]
fn javascript_parents_keep_extends_on_the_parent_line() {
	for (source, parent) in [
		("class A extends B {}\n", "B"),
		("class A\n\textends B {\n}\n", "B"),
		("class A extends ns.Base {}\n", "Base"),
		("const C = class extends B {};\n", "B"),
		("class A extends mixin(B) {}\n", "mixin"),
	] {
		assert_relation_sound("javascript", source, TypeRelationKind::Extends, parent);
	}
}

#[test]
fn typescript_imports_end_in_the_name_before_the_quote() {
	for (source, path, name) in [
		("import x from './util';\n", "./util", "util"),
		("import type { T } from \"./types\";\n", "./types", "types"),
		("import {\n\ta,\n\tb,\n} from './multi';\n", "./multi", "multi"),
		("export type { T } from './t';\n", "./t", "t"),
	] {
		assert_import_sound("typescript", source, path, name);
	}
}

#[test]
fn typescript_heritage_lists_keep_continuation_lines() {
	assert_relation_sound(
		"typescript",
		"class A implements B,\n\tC {}\n",
		TypeRelationKind::Implements,
		"C",
	);
	assert_relation_sound(
		"typescript",
		"interface X extends A,\n\tB {}\n",
		TypeRelationKind::Extends,
		"B",
	);
	assert_relation_sound("typescript", "class A extends B {}\n", TypeRelationKind::Extends, "B");
	assert!(!inherit_lines("typescript", "const b = new B();", "B"));
}

#[test]
fn svelte_scripts_follow_the_javascript_and_typescript_predicates() {
	let svelte = language("svelte");
	assert!(svelte.line_may_import("import A from './A.svelte';", "A"));
	assert!(!svelte.line_may_import("const A = 1;", "A"));
	assert!(svelte.line_may_inherit("class X implements A {", "A"));

	for (contents, path, name) in [
		(
			"<script>\n\timport A from './A.svelte';\n</script>\n<p>hi</p>\n",
			"./A.svelte",
			"A",
		),
		(
			"<script lang=\"ts\">\n\timport type {\n\t\tT,\n\t} from \"./types\";\n</script>\n",
			"./types",
			"types",
		),
	] {
		let scripts = script_sources(contents);
		assert!(!scripts.is_empty(), "no script in {contents:?}");
		let mut reported = false;
		for (script_language, script, _, _) in scripts {
			let script_language = language(script_language);
			if collect_imports(&*script_language, &script)
				.iter()
				.any(|import| import == path)
			{
				reported = true;
				assert!(
					file_may_import(&*svelte, contents, name),
					"no line of {contents:?} may import {name:?}"
				);
			}
		}
		assert!(reported, "extractor did not report {path:?} for {contents:?}");
	}
}

#[test]
fn python_imports_and_bases_keep_grouped_continuation_lines() {
	// `from . import (` newline `a,`: the name's line holds only the name.
	assert_import_sound("python", "from . import (\n\ta,\n\tb,\n)\n", ".b", "b");
	assert_import_sound("python", "from pkg.sub import x\n", "pkg.sub", "sub");
	assert_import_sound("python", "import a.b, c\n", "a.b", "b");
	assert_relation_sound(
		"python",
		"class A(\n\tBase,\n\tOther,\n):\n\tpass\n",
		TypeRelationKind::Extends,
		"Other",
	);
}

#[test]
fn a_python_variable_with_the_name_of_a_module_is_not_an_import() {
	for line in [
		"query = Query()",
		"    result = query.filter(x=1)",
		"print(query.run())",
		"def run(query, other):",
	] {
		assert!(!import_lines("python", line, "query"), "{line}");
	}
	for line in ["from app import query", "import query", "    query,"] {
		assert!(import_lines("python", line, "query"), "{line}");
	}
	assert!(!inherit_lines("python", "x = Base()", "Base"));
	assert!(inherit_lines("python", "class A(Base):", "Base"));
}

#[test]
fn rust_nested_use_groups_and_trait_lists_keep_continuation_lines() {
	// `d,` sits alone on its line, without `use` or `::`.
	assert_import_sound("rust", "use a::{\n\tb::c,\n\td,\n};\n", "a::d", "d");
	assert_import_sound("rust", "mod child;\n", "self::child", "child");
	assert_relation_sound(
		"rust",
		"impl<T> Render\n\tfor Widget<T> {}\n",
		TypeRelationKind::Implements,
		"Render",
	);
	assert_relation_sound("rust", "trait A:\n\tB\n\t+ C\n{}\n", TypeRelationKind::Extends, "C");
	assert!(!import_lines("rust", "let util = make();", "util"));
	assert!(!inherit_lines("rust", "let r = Render::new();", "Render"));
}

#[test]
fn java_imports_keep_the_import_keyword_on_the_name_line() {
	for (source, path, name) in [
		("import com.example.Foo;\n", "com.example.Foo", "Foo"),
		(
			"import static com.example.Util.helper;\n",
			"com.example.Util.helper",
			"helper",
		),
		("import com.example.*;\n", "com.example.*", "example"),
	] {
		assert_import_sound("java", source, path, name);
	}
	assert!(!import_lines("java", "Foo foo = new Foo();", "Foo"));
}

#[test]
fn java_implements_lists_keep_continuation_lines() {
	assert_relation_sound(
		"java",
		"class A\n\timplements B,\n\t\tC {\n}\n",
		TypeRelationKind::Implements,
		"C",
	);
}

#[test]
fn cpp_includes_end_in_the_name_before_the_delimiter() {
	for (source, path, name) in [
		("#include \"util.h\"\n", "util.h", "util"),
		("#include <vector>\n", "vector", "vector"),
		("#include <sys/stat.h>\n", "sys/stat.h", "stat"),
		("#  include \"spaced.h\"\n", "spaced.h", "spaced"),
		("#include \"noted.h\" // why it is here\n", "noted.h", "noted"),
	] {
		assert_import_sound("cpp", source, path, name);
	}
	for line in ["int stat = 1;", "printf(\"stat.h>\");", "x = stat(path);"] {
		assert!(!import_lines("cpp", line, "stat"), "{line}");
	}
}

#[test]
fn cpp_base_lists_keep_continuation_lines() {
	assert_relation_sound(
		"cpp",
		"class A :\n\tpublic B,\n\tprivate C {};\n",
		TypeRelationKind::Extends,
		"C",
	);
}

#[test]
fn swift_imports_and_inheritance_keep_their_lines() {
	for (source, path, name) in [
		("import Foundation\n", "Foundation", "Foundation"),
		("@testable import Models\n", "Models", "Models"),
	] {
		assert_import_sound("swift", source, path, name);
	}
	assert_relation_sound(
		"swift",
		"class A:\n\tBase,\n\tProto {\n}\n",
		TypeRelationKind::Implements,
		"Proto",
	);
}

#[test]
fn php_group_use_and_interface_lists_keep_continuation_lines() {
	assert_import_sound(
		"php",
		"<?php\nuse App\\Models\\{\n\tUser,\n\tPost,\n};\n",
		"App/Models/Post",
		"Post",
	);
	assert_import_sound("php", "<?php\nuse App\\Models\\User;\n", "App/Models/User", "User");
	assert_relation_sound(
		"php",
		"<?php\nclass A extends B implements\n\tC,\n\tD {}\n",
		TypeRelationKind::Implements,
		"D",
	);
	assert!(!import_lines("php", "$x = User::find(1);", "User"));
}

#[test]
fn ruby_requires_end_in_the_name_before_the_quote() {
	for (source, path, name) in [
		("require 'json'\n", "json", "json"),
		("require \"active_support/core\"\n", "active_support/core", "core"),
		("require_relative '../lib/helper'\n", "relative:../lib/helper", "helper"),
		("load 'tasks.rb'\n", "tasks.rb", "tasks"),
	] {
		assert_import_sound("ruby", source, path, name);
	}
	assert!(!import_lines("ruby", "json = JSON.parse(text)", "json"));
}

#[test]
fn ruby_mixin_lists_keep_continuation_lines() {
	assert_relation_sound(
		"ruby",
		"class A\n\tinclude M,\n\t\tN\nend\n",
		TypeRelationKind::Implements,
		"N",
	);
	assert_relation_sound("ruby", "class A < B\nend\n", TypeRelationKind::Extends, "B");
}

#[test]
fn lua_requires_end_in_the_name_before_the_quote() {
	for (source, path, name) in [
		("local json = require(\"json\")\n", "json", "json"),
		("local m = require('lib.mod')\n", "lib.mod", "mod"),
		("require(\n\t\"a.b\"\n)\n", "a.b", "b"),
	] {
		assert_import_sound("lua", source, path, name);
	}
	assert!(!import_lines("lua", "local json = 1", "json"));
}

#[test]
fn bash_sources_end_in_the_name_after_the_path() {
	for (source, path, name) in [
		("source ./lib/util.sh\n", "./lib/util.sh", "util"),
		(". ./lib/common.sh\n", "./lib/common.sh", "common"),
		("source \"$DIR/x.sh\"\n", "$DIR/x.sh", "x"),
		("f() {\n\tsource ./lib/inner.sh\n}\n", "./lib/inner.sh", "inner"),
	] {
		assert_import_sound("bash", source, path, name);
	}
	assert!(!import_lines("bash", "echo util is ready", "util"));
}

#[test]
fn elixir_alias_groups_keep_continuation_lines_and_defimpl_its_keyword() {
	assert_import_sound("elixir", "alias MyApp.{\n\tRepo,\n\tUser\n}\n", "MyApp.User", "User");
	for source in [
		"defimpl Printable, for: Integer do\nend\n",
		"defimpl Printable,\n\tfor: Integer do\nend\n",
	] {
		assert_relation_sound("elixir", source, TypeRelationKind::Implements, "Printable");
	}
}

#[test]
fn css_imports_end_in_the_name_before_a_quote_or_parenthesis() {
	for (source, path, name) in [
		("@import \"reset.css\";\n", "reset.css", "reset"),
		("@import 'base.css';\n", "base.css", "base"),
		("@import url(\"theme.css\");\n", "theme.css", "theme"),
		("@import url(print.css);\n", "print.css", "print"),
		("@import 'a.css' screen;\n", "a.css", "a"),
	] {
		assert_import_sound("css", source, path, name);
	}
	assert!(!import_lines("css", ".base { color: red; }", "base"));
}

fn collect_calls(name: &str, source: &str) -> Vec<(String, Option<String>)> {
	let language = language(name);
	let tree = parse(language.as_ref(), source);
	let mut calls = Vec::new();
	for_each_node(&tree, |node| {
		for call in language.extract_function_calls(node, source) {
			calls.push((call.name, call.qualifier));
		}
	});
	calls
}

#[test]
fn every_language_is_found_by_the_name_it_reports() {
	for (name, make) in super::LANGUAGES {
		assert_eq!(make().name(), name);
		assert!(get_language(name).is_some());
	}
	assert!(get_language("markdown").is_none());
}

#[test]
fn the_graph_languages_are_those_that_can_add_symbols() {
	let graph = super::graph_languages();
	for name in ["rust", "typescript", "bash", "css", "svelte", "elixir"] {
		assert!(graph.contains(name), "{name}");
	}
	// JSON declares nothing and Markdown has no language.
	assert!(!graph.contains("json"));
	assert!(!graph.contains("markdown"));
	assert!(super::module_root_files().contains(&"go.mod"));
}

#[test]
fn a_callee_is_split_by_how_its_language_writes_members() {
	use super::{SELF_RECEIVER, UNKNOWN_RECEIVER};
	let pair = |name: &str, qualifier: Option<&str>| (name.to_string(), qualifier.map(str::to_string));

	assert_eq!(
		collect_calls("javascript", "this.run(); a?.b.c(); $.ajax(); box.#hidden();"),
		vec![
			pair("run", Some(SELF_RECEIVER)),
			pair("c", Some("a::b")),
			pair("ajax", Some("$")),
			pair("hidden", Some("box")),
		]
	);
	// A tree walk reaches the outer call before the one in its receiver.
	assert_eq!(
		collect_calls("rust", "fn f() { Self::make(); self.go(); a::b::c(); x.y()?.z(); }"),
		vec![
			pair("make", Some(SELF_RECEIVER)),
			pair("go", Some(SELF_RECEIVER)),
			pair("c", Some("a::b")),
			pair("z", Some(UNKNOWN_RECEIVER)),
			pair("y", Some("x")),
		]
	);
	assert_eq!(
		collect_calls("php", "<?php $this->save(); $repo->find(); Foo::make();"),
		vec![
			pair("save", Some(SELF_RECEIVER)),
			pair("find", Some("repo")),
			pair("make", Some("Foo")),
		]
	);
	assert_eq!(
		collect_calls("lua", "self:go()\nbox.open()\nbox:close()\n"),
		vec![
			pair("go", Some(SELF_RECEIVER)),
			pair("open", Some("box")),
			pair("close", Some("box")),
		]
	);
	assert_eq!(
		collect_calls("cpp", "void f() { this->go(); p->q(); ns::run(); }"),
		vec![
			pair("go", Some(SELF_RECEIVER)),
			pair("q", Some("p")),
			pair("run", Some("ns")),
		]
	);
}

#[test]
fn a_nested_declaration_is_owned_by_the_container_kinds_of_its_language() {
	let owner_of = |name: &str, source: &str, member: &str| {
		let language = language(name);
		let tree = parse(language.as_ref(), source);
		let mut owner = None;
		for_each_node(&tree, |node| {
			if language.get_symbol_kinds().contains(&node.kind())
				&& language.extract_declaration_name(node, source).as_deref() == Some(member)
			{
				owner = language.extract_symbol_owner(node, source);
			}
		});
		owner
	};
	assert_eq!(
		owner_of("rust", "impl Shape for Square { fn area(&self) {} }", "area").as_deref(),
		Some("Square")
	);
	assert_eq!(owner_of("rust", "fn free() {}", "free"), None);
	// `class << self` reopens the enclosing class: its methods belong to it.
	assert_eq!(
		owner_of(
			"ruby",
			"class Box\n  class << self\n    def open; end\n  end\nend\n",
			"open"
		)
		.as_deref(),
		Some("Box")
	);
	assert_eq!(
		owner_of("java", "enum Level { HIGH; int rank() { return 1; } }", "rank").as_deref(),
		Some("Level")
	);
	assert_eq!(
		owner_of("swift", "extension Shape { func grow() {} }", "grow").as_deref(),
		Some("Shape")
	);
	assert_eq!(owner_of("go", "package p\nfunc free() {}\n", "free"), None);
	assert_eq!(
		owner_of("python", "class A:\n    def m(self): pass\n", "m").as_deref(),
		Some("A")
	);
}

#[test]
fn a_trailing_comment_ends_a_name_list_at_the_markers_of_its_language() {
	use super::name_list_line;
	let lua = language("lua");
	let python = language("python");
	assert!(name_list_line(
		"b, c -- more words: here",
		"b",
		"",
		lua.comment_starts()
	));
	assert!(!name_list_line(
		"b, c -- more words: here",
		"b",
		"",
		python.comment_starts()
	));
	assert!(name_list_line(
		"b, c # more words: here",
		"b",
		"",
		python.comment_starts()
	));
	assert!(!name_list_line(
		"b, c # more words: here",
		"b",
		"",
		lua.comment_starts()
	));
}

#[test]
fn only_languages_with_packages_declare_a_package_scope() {
	let expected = [
		("go", PackageScope::Directory),
		("swift", PackageScope::Directory),
		("java", PackageScope::Declared),
		("php", PackageScope::Declared),
		("cpp", PackageScope::NoPackage),
		("elixir", PackageScope::NoPackage),
		("ruby", PackageScope::NoPackage),
		("lua", PackageScope::NoPackage),
		("bash", PackageScope::NoPackage),
		("python", PackageScope::NoPackage),
		("rust", PackageScope::NoPackage),
		("javascript", PackageScope::NoPackage),
		("typescript", PackageScope::NoPackage),
		("tsx", PackageScope::NoPackage),
		("svelte", PackageScope::NoPackage),
	];
	for (name, scope) in expected {
		assert_eq!(get_language(name).unwrap().package_scope(), scope, "{name}");
	}
}
