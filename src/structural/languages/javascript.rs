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

//! JavaScript grammar: functions, methods, named arrow functions and classes.

use super::{extract_symbol_by_kinds, Language};
use tree_sitter::Node;

pub(crate) struct JavaScript {}

impl Language for JavaScript {
	fn name(&self) -> &'static str {
		"javascript"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_javascript::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The class container is included (chunking drops it as too large);
		// import/export statements are dropped — they declare nothing an
		// outline needs, and anonymous arrow functions have no name.
		vec![
			"function_declaration",
			"method_definition",
			"arrow_function",
			"class_declaration",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "arrow_function" {
			// An arrow function's name is the variable it is assigned to.
			let parent = node.parent()?;
			if parent.kind() != "variable_declarator" {
				return None;
			}
			return parent
				.child_by_field_name("name")
				.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
				.map(str::to_string);
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name"])
	}
}

#[cfg(test)]
#[path = "javascript_tests.rs"]
mod javascript_tests;
