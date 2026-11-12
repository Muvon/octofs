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

//! Swift grammar: functions, classes, protocols, initializers and type
//! aliases.

use super::{extract_symbol_by_kinds, simple_type_name, Language};
use tree_sitter::Node;

pub(crate) struct Swift {}

impl Language for Swift {
	fn name(&self) -> &'static str {
		"swift"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_swift::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `class_declaration` covers class / struct / enum / extension / actor
		// in this grammar; property/subscript/import declarations are dropped.
		vec![
			"function_declaration",
			"class_declaration",
			"protocol_declaration",
			"init_declaration",
			"typealias_declaration",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "init_declaration" {
			return Some("init".to_string());
		}
		for field in ["name", "type"] {
			if let Some(name) = node.child_by_field_name(field) {
				if let Ok(text) = name.utf8_text(contents.as_bytes()) {
					if let Some(simple) = simple_type_name(text) {
						return Some(simple);
					}
				}
			}
		}
		extract_symbol_by_kinds(node, contents, &["simple_identifier", "type_identifier", "user_type"])
	}
}

#[cfg(test)]
#[path = "swift_tests.rs"]
mod swift_tests;
