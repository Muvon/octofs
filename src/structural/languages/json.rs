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

//! JSON grammar: objects and arrays, named by their key.

use super::Language;
use tree_sitter::Node;

pub(crate) struct Json {}

impl Language for Json {
	fn name(&self) -> &'static str {
		"json"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_json::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec!["object", "array"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// An object is named by its first key (the default child scan finds
		// nothing — `pair` matches no name kind); an array by the key of the
		// pair it is the value of, when there is one.
		if node.kind() == "object" {
			for child in node.children(&mut node.walk()) {
				if child.kind() == "pair" {
					let mut cursor = child.walk();
					if cursor.goto_first_child() && cursor.node().kind() == "string" {
						return cursor
							.node()
							.utf8_text(contents.as_bytes())
							.ok()
							.map(|text| text.trim_matches('"').trim().to_string())
							.filter(|name| !name.is_empty());
					}
				}
			}
			return None;
		}
		let parent = node.parent()?;
		if parent.kind() != "pair" {
			return None;
		}
		let mut cursor = parent.walk();
		if cursor.goto_first_child() && cursor.node().kind() == "string" {
			return cursor
				.node()
				.utf8_text(contents.as_bytes())
				.ok()
				.map(|text| text.trim_matches('"').trim().to_string())
				.filter(|name| !name.is_empty());
		}
		None
	}
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod json_tests;
