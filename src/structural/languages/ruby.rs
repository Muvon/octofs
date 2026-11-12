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

//! Ruby grammar: methods, singleton methods, classes and modules.

use super::{extract_symbol_by_kinds, Language};
use tree_sitter::Node;

pub(crate) struct Ruby {}

impl Language for Ruby {
	fn name(&self) -> &'static str {
		"ruby"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_ruby::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `call` is dropped: a call's method-name child matches the default
		// name scan, which would turn every call site into a bogus item.
		vec!["method", "singleton_method", "class", "module"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if matches!(node.kind(), "class" | "module") {
			// Names are `constant` nodes; qualified names (`Foo::Bar`) are
			// `scope_resolution` nodes whose LAST `constant` is the defined one.
			let mut cursor = node.walk();
			for child in node.children(&mut cursor) {
				let name_node = match child.kind() {
					"constant" => Some(child),
					"scope_resolution" => {
						let mut inner = child.walk();
						child.children(&mut inner).filter(|c| c.kind() == "constant").last()
					}
					_ => None,
				};
				if let Some(name_node) = name_node {
					return name_node.utf8_text(contents.as_bytes()).ok().map(String::from);
				}
			}
			return None;
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}
}

#[cfg(test)]
#[path = "ruby_tests.rs"]
mod ruby_tests;
