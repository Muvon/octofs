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

//! Elixir grammar: declarations (`defmodule`, `def`, and so on) are ordinary
//! `call` nodes, so items are classified by their call target instead of by
//! node kind.

use super::{extract_call_target, Language};
use tree_sitter::Node;

const FUNCTION_DEFINITION_MACROS: &[&str] = &[
	"def",
	"defp",
	"defdelegate",
	"defguard",
	"defguardp",
	"defmacro",
	"defmacrop",
	"defn",
	"defnp",
	"defcallback",
	"defmacrocallback",
];

const CONTAINER_DEFINITION_MACROS: &[&str] = &["defmodule", "defprotocol", "defimpl"];
const DATA_DEFINITION_MACROS: &[&str] = &["defstruct", "defexception"];
const TEST_DEFINITION_MACROS: &[&str] = &["test", "property"];

pub(crate) struct Elixir {}

impl Language for Elixir {
	fn name(&self) -> &'static str {
		"elixir"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_elixir::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec!["call"]
	}

	fn is_signature_node(&self, node: Node, contents: &str) -> bool {
		call_macro_name(node, contents).is_some_and(|name| is_definition_macro(&name))
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		let macro_name = call_macro_name(node, contents)?;
		if matches!(macro_name.as_str(), "defmodule" | "defprotocol") {
			return first_argument(node)
				.and_then(|argument| node_text(argument, contents))
				.map(normalize_module_name);
		}
		if macro_name == "defimpl" {
			let protocol = first_argument(node)
				.and_then(|argument| node_text(argument, contents))
				.map(normalize_module_name)?;
			return implementation_type(node, contents)
				.map(|target| format!("{protocol} for {target}"))
				.or(Some(protocol));
		}
		if FUNCTION_DEFINITION_MACROS.contains(&macro_name.as_str()) {
			return first_argument(node).and_then(|argument| definition_head_name(argument, contents));
		}
		if TEST_DEFINITION_MACROS.contains(&macro_name.as_str()) {
			return first_argument(node)
				.and_then(|argument| node_text(argument, contents))
				.map(|name| name.trim().trim_matches(['"', '\'']).to_string())
				.filter(|name| !name.is_empty());
		}
		None
	}

	fn extract_declaration_kind(&self, node: Node, contents: &str) -> Option<&'static str> {
		match call_macro_name(node, contents)?.as_str() {
			"defmodule" => Some("module"),
			"defprotocol" => Some("interface"),
			"defimpl" => Some("implementation"),
			"defstruct" => Some("struct"),
			"defexception" => Some("class"),
			name if TEST_DEFINITION_MACROS.contains(&name) => Some("function"),
			"defmacro" | "defmacrop" | "defmacrocallback" => Some("macro"),
			name if FUNCTION_DEFINITION_MACROS.contains(&name) => Some("function"),
			_ => None,
		}
	}

	fn extract_signature_name(&self, node: Node, contents: &str) -> Option<String> {
		let macro_name = call_macro_name(node, contents)?;
		if DATA_DEFINITION_MACROS.contains(&macro_name.as_str()) {
			// A struct/exception is named by the module it is defined in.
			return extract_symbol_owner(node, contents);
		}
		self.extract_declaration_name(node, contents)
	}
}

fn is_definition_macro(name: &str) -> bool {
	FUNCTION_DEFINITION_MACROS.contains(&name)
		|| CONTAINER_DEFINITION_MACROS.contains(&name)
		|| DATA_DEFINITION_MACROS.contains(&name)
		|| TEST_DEFINITION_MACROS.contains(&name)
}

fn node_text(node: Node, contents: &str) -> Option<String> {
	node.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

fn call_macro_name(node: Node, contents: &str) -> Option<String> {
	if node.kind() != "call" {
		return None;
	}
	let target = node.child_by_field_name("target")?;
	(target.kind() == "identifier")
		.then(|| node_text(target, contents))
		.flatten()
}

fn first_argument(node: Node) -> Option<Node> {
	let mut cursor = node.walk();
	let arguments = node.children(&mut cursor).find(|child| child.kind() == "arguments")?;
	arguments.named_child(0)
}

fn definition_head_name(node: Node, contents: &str) -> Option<String> {
	match node.kind() {
		"identifier" | "operator_identifier" => node_text(node, contents),
		"call" => node
			.child_by_field_name("target")
			.and_then(|target| node_text(target, contents))
			.and_then(|target| extract_call_target(&target)),
		"binary_operator" => node
			.child_by_field_name("left")
			.and_then(|left| definition_head_name(left, contents)),
		_ => {
			let mut cursor = node.walk();
			let found = node
				.children(&mut cursor)
				.find_map(|child| definition_head_name(child, contents));
			found
		}
	}
}

fn normalize_module_name(name: String) -> String {
	name.trim()
		.trim_start_matches(':')
		.trim_start_matches("Elixir.")
		.to_string()
}

fn implementation_type(node: Node, contents: &str) -> Option<String> {
	if call_macro_name(node, contents).as_deref() != Some("defimpl") {
		return None;
	}
	if let Some(text) = node_text(node, contents) {
		if let Some((_, tail)) = text.split_once("for:") {
			let target = tail
				.trim_start()
				.split(|character: char| character.is_whitespace() || matches!(character, ',' | ')' | ']'))
				.find(|part| !part.is_empty())?;
			if !target.starts_with('[') {
				return Some(normalize_module_name(target.to_string()));
			}
		}
	}
	let mut cursor = node.walk();
	for descendant in node.children(&mut cursor) {
		if let Some(found) = find_for_option(descendant, contents) {
			return Some(found);
		}
	}
	None
}

fn find_for_option(node: Node, contents: &str) -> Option<String> {
	if node.kind() == "pair" {
		let key = node
			.child_by_field_name("key")
			.and_then(|child| node_text(child, contents));
		if key.as_deref().is_some_and(|key| key.trim_end_matches(':') == "for") {
			return node
				.child_by_field_name("value")
				.and_then(|child| node_text(child, contents))
				.map(normalize_module_name);
		}
	}
	let mut cursor = node.walk();
	let found = node
		.children(&mut cursor)
		.find_map(|child| find_for_option(child, contents));
	found
}

/// The enclosing `defmodule`/`defprotocol`/`defimpl` of a node, by name.
fn extract_symbol_owner(node: Node, contents: &str) -> Option<String> {
	let current_macro = call_macro_name(node, contents)?;
	if CONTAINER_DEFINITION_MACROS.contains(&current_macro.as_str()) {
		return None;
	}

	let mut parent = node.parent();
	while let Some(candidate) = parent {
		if candidate.kind() == "call" {
			if let Some(macro_name) = call_macro_name(candidate, contents) {
				if CONTAINER_DEFINITION_MACROS.contains(&macro_name.as_str()) {
					if macro_name == "defimpl" {
						return implementation_type(candidate, contents)
							.or_else(|| extract_declaration_name_of(candidate, contents));
					}
					return extract_declaration_name_of(candidate, contents);
				}
			}
		}
		parent = candidate.parent();
	}
	None
}

/// `extract_declaration_name` as a free function, callable on a bare node's
/// language-independent shape (the Elixir declaration-name logic needs no
/// `self`).
fn extract_declaration_name_of(node: Node, contents: &str) -> Option<String> {
	Elixir {}.extract_declaration_name(node, contents)
}

#[cfg(test)]
#[path = "elixir_tests.rs"]
mod elixir_tests;
