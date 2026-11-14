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

use super::{
	extract_call_target, parse_call_target, path_ends_with_components, CallTarget, Language, TypeRelationKind,
};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
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

/// Special forms and control-flow macros that parse as `call` nodes but are
/// not calls to a user function.
const NON_CALL_MACROS: &[&str] = &[
	"after",
	"alias",
	"case",
	"catch",
	"cond",
	"else",
	"for",
	"if",
	"import",
	"quote",
	"raise",
	"receive",
	"require",
	"rescue",
	"reraise",
	"super",
	"throw",
	"try",
	"unless",
	"unquote",
	"unquote_splicing",
	"use",
	"with",
];

/// Macros whose first argument names an import.
const IMPORT_MACROS: &[&str] = &["alias", "import", "require", "use"];

const ELIXIR_EXTENSIONS: [&str; 2] = ["ex", "exs"];

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

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Every declaration is a `call`; `extract_declaration_name` filters the
		// ones that are not definitions.
		vec!["call"]
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		extract_symbol_owner(node, contents)
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let Some(macro_name) = call_macro_name(node, contents) else {
			return (Vec::new(), Vec::new());
		};

		let imports = if IMPORT_MACROS.contains(&macro_name.as_str()) {
			first_argument(node)
				.and_then(|argument| node_text(argument, contents))
				.map(expand_module_aliases)
				.unwrap_or_default()
		} else {
			Vec::new()
		};

		let exports =
			if matches!(macro_name.as_str(), "defmodule" | "defprotocol") || is_public_definition_macro(&macro_name) {
				self.extract_declaration_name(node, contents).into_iter().collect()
			} else {
				Vec::new()
			};

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() == "binary_operator" {
			// Only the right side of a pipe is a callee: `x |> run()`.
			let operator = node
				.child_by_field_name("operator")
				.and_then(|operator| node_text(operator, contents));
			if operator.as_deref() == Some("|>") {
				if let Some(right) = node.child_by_field_name("right") {
					if matches!(right.kind(), "identifier" | "dot") {
						return node_text(right, contents)
							.and_then(|target| parse_call_target(&target))
							.into_iter()
							.collect();
					}
				}
			}
			return Vec::new();
		}

		if node.kind() != "call" || is_definition_head_call(node, contents) {
			return Vec::new();
		}
		let Some(target) = node.child_by_field_name("target") else {
			return Vec::new();
		};
		let Some(target_text) = node_text(target, contents) else {
			return Vec::new();
		};
		if target.kind() == "identifier"
			&& (is_definition_macro(&target_text) || NON_CALL_MACROS.contains(&target_text.as_str()))
		{
			return Vec::new();
		}
		parse_call_target(&target_text).into_iter().collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		if call_macro_name(node, contents).as_deref() != Some("defimpl") {
			return Vec::new();
		}
		first_argument(node)
			.and_then(|argument| node_text(argument, contents))
			.map(normalize_module_name)
			.map(|protocol| vec![(TypeRelationKind::Implements, protocol)])
			.unwrap_or_default()
	}

	// The implementing type, not the `defimpl` name: octocode relates by type.
	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		implementation_type(node, contents)
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			let path = resolve_relative_path(source_file, import_path);
			return all_files.find_file_with_extensions(&path, &ELIXIR_EXTENSIONS);
		}

		let module_path = module_to_file_path(import_path)?;
		let last_segment = module_path.rsplit('/').next()?;
		let mut matches: Vec<&String> = Vec::new();
		for extension in ELIXIR_EXTENSIONS {
			let relative_path = format!("{module_path}.{extension}");
			let file_name = format!("{last_segment}.{extension}");
			matches.extend(
				all_files
					.files_named(&file_name)
					.iter()
					.filter(|file| path_ends_with_components(file, &relative_path)),
			);
		}
		// Path order first, so equal candidates resolve the same way every
		// run; then the Mix convention (`lib/` copy), then the shortest path.
		matches.sort();
		let lib_path = format!("lib/{module_path}.ex");
		matches
			.into_iter()
			.min_by_key(|file| (!path_ends_with_components(file, &lib_path), file.len()))
			.cloned()
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
	// Iterative: a `def` head can nest arbitrarily deep (operator chains,
	// nested calls), and the walk must not recurse.
	let mut stack = vec![node];
	while let Some(current) = stack.pop() {
		match current.kind() {
			"identifier" | "operator_identifier" => return node_text(current, contents),
			"call" => {
				// A call whose target is not a plain name has no head name —
				// the recursive original returned None without scanning.
				if let Some(name) = current
					.child_by_field_name("target")
					.and_then(|target| node_text(target, contents))
					.and_then(|target| extract_call_target(&target))
				{
					return Some(name);
				}
			}
			"binary_operator" => {
				// Only the left side names a `def` head (`a when b` → a).
				if let Some(left) = current.child_by_field_name("left") {
					stack.push(left);
				}
				continue;
			}
			_ => {
				// Children are pushed reversed so the LIFO stack pops them in
				// sibling order, each subtree fully before the next — matching
				// the recursive original.
				let children: Vec<Node> = current.children(&mut current.walk()).collect();
				stack.extend(children.into_iter().rev());
			}
		}
	}
	None
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
	// Iterative: `for:` can sit arbitrarily deep in nested arguments.
	let mut stack = vec![node];
	while let Some(current) = stack.pop() {
		if current.kind() == "pair" {
			let key = current
				.child_by_field_name("key")
				.and_then(|child| node_text(child, contents));
			if key.as_deref().is_some_and(|key| key.trim_end_matches(':') == "for") {
				return current
					.child_by_field_name("value")
					.and_then(|child| node_text(child, contents))
					.map(normalize_module_name);
			}
		}
		let children: Vec<Node> = current.children(&mut current.walk()).collect();
		stack.extend(children.into_iter().rev());
	}
	None
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

fn is_public_definition_macro(name: &str) -> bool {
	matches!(
		name,
		"def" | "defdelegate" | "defguard" | "defmacro" | "defn" | "defcallback" | "defmacrocallback"
	)
}

/// `alias MyApp.{Repo, User}` becomes one import per member.
fn expand_module_aliases(text: String) -> Vec<String> {
	let normalized = normalize_module_name(text);
	if let Some((prefix, tail)) = normalized.split_once(".{") {
		if let Some(names) = tail.strip_suffix('}') {
			return names
				.split(',')
				.map(str::trim)
				.filter(|name| !name.is_empty())
				.map(|name| format!("{prefix}.{name}"))
				.collect();
		}
	}
	(!normalized.is_empty()).then_some(normalized).into_iter().collect()
}

/// Whether `node` sits inside the head (first argument) of a `def`-family
/// call: `fetch(id)` in `def fetch(id)` is a head, not a call.
fn is_definition_head_call(node: Node, contents: &str) -> bool {
	let start = node.start_byte();
	let end = node.end_byte();
	let mut ancestor = node.parent();
	while let Some(candidate) = ancestor {
		if candidate.kind() == "call" {
			if call_macro_name(candidate, contents)
				.is_some_and(|name| FUNCTION_DEFINITION_MACROS.contains(&name.as_str()))
			{
				return first_argument(candidate)
					.is_some_and(|head| start >= head.start_byte() && end <= head.end_byte());
			}
			return false;
		}
		ancestor = candidate.parent();
	}
	false
}

/// `MyApp.HTTPClient` -> `my_app/http_client`, the Mix file layout.
fn module_to_file_path(module: &str) -> Option<String> {
	let module = module.trim().trim_start_matches(':').trim_start_matches("Elixir.");
	if module.is_empty()
		|| !module
			.chars()
			.all(|character| character.is_alphanumeric() || matches!(character, '.' | '_'))
	{
		return None;
	}
	Some(module.split('.').map(camel_to_snake).collect::<Vec<_>>().join("/"))
}

fn camel_to_snake(segment: &str) -> String {
	let characters: Vec<char> = segment.chars().collect();
	let mut output = String::with_capacity(segment.len() + 4);
	for (index, character) in characters.iter().copied().enumerate() {
		let previous = index.checked_sub(1).and_then(|i| characters.get(i)).copied();
		let next = characters.get(index + 1).copied();
		if character.is_uppercase()
			&& index > 0
			&& (previous.is_some_and(|previous| previous.is_lowercase() || previous.is_numeric())
				|| (previous.is_some_and(char::is_uppercase) && next.is_some_and(char::is_lowercase)))
		{
			output.push('_');
		}
		output.extend(character.to_lowercase());
	}
	output
}

#[cfg(test)]
#[path = "elixir_tests.rs"]
mod elixir_tests;
