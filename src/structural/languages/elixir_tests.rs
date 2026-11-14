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

//! Elixir outline and graph extraction tests.

use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::{build_graph, RelationType, SourceFile};
use crate::structural::languages::{get_language, CallTarget, Language, TypeRelationKind};
use crate::structural::outline;
use tree_sitter::{Node, Tree};

const SOURCE: &str = r#"
defmodule MyApp.Accounts do
  alias MyApp.{Repo, User}
  import MyApp.Validation
  require Logger
  use MyApp.Telemetry

  defstruct [:name, :email]

  def fetch_user(id) when is_integer(id) do
    Repo.get(User, id)
  end

  defp validate(user), do: MyApp.Validation.call(user)
end

defprotocol MyApp.Renderable do
  def render(value)
end

defimpl MyApp.Renderable, for: MyApp.User do
  def render(user), do: user.name
end
"#;

fn elixir() -> Box<dyn Language> {
	get_language("elixir").expect("Elixir language should be registered")
}

fn parse(source: &str) -> Tree {
	let mut parser = tree_sitter::Parser::new();
	parser
		.set_language(&elixir().get_ts_language())
		.expect("Elixir grammar should load");
	parser.parse(source, None).expect("Elixir source should parse")
}

/// Pre-order, iterative collection of every `call` and `binary_operator`.
fn call_nodes(root: Node) -> Vec<Node> {
	let mut found = Vec::new();
	let mut cursor = root.walk();
	loop {
		if matches!(cursor.node().kind(), "call" | "binary_operator") {
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

#[test]
fn modules_and_defs_are_items() {
	let src = "defmodule Foo do\n\tdef bar do\n\t\t:ok\n\tend\n\n\tdefp baz, do: :ok\nend\n";
	let items = outline("a.ex", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("module", "Foo"), ("function", "bar"), ("function", "baz")]
	);
}

#[test]
fn ordinary_calls_are_not_items() {
	assert!(outline("a.exs", "Enum.map([1, 2], & &1)\n").unwrap().is_empty());
}

#[test]
fn defimpl_names_the_protocol_and_target() {
	let src = "defimpl Protocol, for: Integer do\n\tdef value(x), do: x\nend\n";
	let items = outline("a.ex", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("implementation", "Protocol for Integer"), ("function", "value")]
	);
}

#[test]
fn registers_and_reports_elixir_extensions() {
	let language = elixir();
	assert_eq!(language.name(), "elixir");
	assert_eq!(language.get_symbol_kinds(), vec!["call"]);
}

#[test]
fn extracts_imports_exports_calls_owners_and_protocol_relations() {
	let language = elixir();
	let tree = parse(SOURCE);
	let mut imports = Vec::new();
	let mut exports = Vec::new();
	let mut calls: Vec<CallTarget> = Vec::new();
	let mut declarations = Vec::new();
	let mut relations = Vec::new();

	for node in call_nodes(tree.root_node()) {
		let (node_imports, node_exports) = language.extract_imports_exports(node, SOURCE);
		imports.extend(node_imports);
		exports.extend(node_exports);
		calls.extend(language.extract_function_calls(node, SOURCE));
		if let Some(name) = language.extract_declaration_name(node, SOURCE) {
			declarations.push((name, language.extract_symbol_owner(node, SOURCE)));
		}
		for (kind, target) in language.extract_type_relations(node, SOURCE) {
			relations.push((kind, target, language.extract_type_relation_source(node, SOURCE)));
		}
	}

	imports.sort();
	imports.dedup();
	assert_eq!(
		imports,
		vec![
			"Logger",
			"MyApp.Repo",
			"MyApp.Telemetry",
			"MyApp.User",
			"MyApp.Validation"
		]
	);
	assert!(exports.contains(&"MyApp.Accounts".to_string()));
	assert!(exports.contains(&"fetch_user".to_string()));
	assert!(!exports.contains(&"validate".to_string()));
	assert!(calls.contains(&CallTarget {
		name: "get".to_string(),
		qualifier: Some("Repo".to_string())
	}));
	assert!(calls.contains(&CallTarget {
		name: "call".to_string(),
		qualifier: Some("MyApp::Validation".to_string())
	}));
	assert!(!calls.iter().any(|call| call.name == "fetch_user" || call.name == "def"));
	assert!(declarations.contains(&("fetch_user".to_string(), Some("MyApp.Accounts".to_string()))));
	assert_eq!(
		relations,
		vec![(
			TypeRelationKind::Implements,
			"MyApp.Renderable".to_string(),
			Some("MyApp.User".to_string())
		)]
	);
}

#[test]
fn exunit_tests_are_owned_function_like_declarations() {
	let source = "defmodule MyApp.AccountsTest do\n  use ExUnit.Case\n  alias MyApp.Accounts\n\n  test \"fetches an account\" do\n    assert Accounts.fetch_user(42)\n  end\nend\n";
	let language = elixir();
	let tree = parse(source);
	let declaration = call_nodes(tree.root_node())
		.into_iter()
		.find(|node| language.extract_declaration_name(*node, source).as_deref() == Some("fetches an account"))
		.map(|node| {
			(
				language.extract_declaration_kind(node, source),
				language.extract_symbol_owner(node, source),
			)
		});
	assert_eq!(
		declaration,
		Some((Some("function"), Some("MyApp.AccountsTest".to_string())))
	);
}

#[test]
fn resolves_mix_module_paths() {
	let language = elixir();
	let files: Vec<String> = [
		"lib/my_app/accounts.ex",
		"lib/my_app/repo.ex",
		"lib/my_app/user.ex",
		"test/support/my_app/user.exs",
	]
	.iter()
	.map(|file| file.to_string())
	.collect();
	let files = FileRegistry::new(&files);

	assert_eq!(
		language.resolve_import("MyApp.Repo", "lib/my_app/accounts.ex", &files),
		Some("lib/my_app/repo.ex".to_string())
	);
	assert_eq!(
		language.resolve_import("MyApp.User", "lib/my_app/accounts.ex", &files),
		Some("lib/my_app/user.ex".to_string())
	);
	assert_eq!(
		language.resolve_import("External.Package", "lib/my_app/accounts.ex", &files),
		None
	);
}

#[test]
fn a_module_path_must_match_whole_directory_components() {
	let files = FileRegistry::new(&["lib/notmy_app/user.ex".to_string(), "lib/my_app/user.ex".to_string()]);
	let language = elixir();
	assert_eq!(
		language.resolve_import("MyApp.User", "lib/x.ex", &files),
		Some("lib/my_app/user.ex".to_string())
	);
	let files = FileRegistry::new(&["lib/notmy_app/user.ex".to_string()]);
	assert_eq!(language.resolve_import("MyApp.User", "lib/x.ex", &files), None);
}

#[test]
fn module_names_convert_to_mix_paths_and_grouped_aliases_expand() {
	assert_eq!(
		super::module_to_file_path("MyApp.HTTPClient").as_deref(),
		Some("my_app/http_client")
	);
	assert_eq!(
		super::module_to_file_path("Elixir.OAuth2.Client").as_deref(),
		Some("o_auth2/client")
	);
	assert_eq!(super::module_to_file_path("dynamic(module)"), None);
	assert_eq!(
		super::expand_module_aliases("MyApp.{Accounts, Repo}".to_string()),
		vec!["MyApp.Accounts", "MyApp.Repo"]
	);
}

#[test]
fn builds_elixir_live_graph_with_import_call_and_protocol_edges() {
	let file = |path: &str, contents: &str| SourceFile {
		path: path.to_string(),
		language: "elixir",
		contents: contents.to_string(),
	};
	let graph = build_graph(vec![
		file(
			"lib/fixture/accounts.ex",
			"defmodule Fixture.Accounts do\n  alias Fixture.Repo\n  def fetch(id), do: Repo.get(User, id)\nend\n",
		),
		file(
			"lib/fixture/repo.ex",
			"defmodule Fixture.Repo do\n  def get(schema, id), do: {schema, id}\nend\n",
		),
		file(
			"lib/fixture/renderable.ex",
			"defprotocol Fixture.Renderable do\n  def render(value)\nend\ndefimpl Fixture.Renderable, for: Fixture.User do\n  def render(user), do: user.email\nend\n",
		),
		file(
			"lib/fixture/accounts_test.exs",
			"defmodule Fixture.AccountsTest do\n  alias Fixture.Accounts\n  test \"fetches an account\" do\n    Accounts.fetch(42)\n  end\nend\n",
		),
	]);

	let accounts = "lib/fixture/accounts.ex";
	let repo = "lib/fixture/repo.ex";
	let renderable_protocol = "lib/fixture/renderable.ex::Fixture.Renderable";
	let accounts_fetch = "lib/fixture/accounts.ex::Fixture.Accounts::fetch";
	let repo_get = "lib/fixture/repo.ex::Fixture.Repo::get";
	let impl_renderable = "lib/fixture/renderable.ex::Fixture.Renderable for Fixture.User";
	let test_fetch = "lib/fixture/accounts_test.exs::Fixture.AccountsTest::fetches an account";

	let has = |source: &str, target: &str, relation: RelationType| {
		graph
			.relationships
			.iter()
			.any(|edge| edge.source == source && edge.target == target && edge.relation == relation)
	};

	assert!(graph.nodes.contains_key(accounts));
	assert!(graph.nodes.contains_key(accounts_fetch));
	assert!(graph.nodes.contains_key(test_fetch));
	assert!(has(accounts, repo, RelationType::Imports));
	assert!(has(accounts_fetch, repo_get, RelationType::Calls));
	assert!(has(impl_renderable, renderable_protocol, RelationType::Implements));
	assert!(has(test_fetch, accounts_fetch, RelationType::Calls));
}
