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

//! CSS grammar: rule sets and at-rules, named by their selectors.

use super::Language;
use tree_sitter::Node;

pub(crate) struct Css {}

impl Language for Css {
	fn name(&self) -> &'static str {
		"css"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		// The CSS parser also handles SCSS/SASS syntax.
		tree_sitter_css::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec![
			"rule_set",            // CSS rules like .class { ... }
			"at_rule",             // @media, @keyframes, @import, etc.
			"keyframes_statement", // @keyframes specific
			"media_statement",     // @media specific
			"import_statement",    // @import specific
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "rule_set" {
			// The name is the first selector's text; the default child scan
			// finds nothing because `selectors` matches no name kind.
			for child in node.children(&mut node.walk()) {
				if child.kind() == "selectors" {
					return first_selector(child, contents);
				}
			}
			return None;
		}
		// At-rules: the identifier or keyframes name child (e.g. a keyframe
		// or feature name).
		for child in node.children(&mut node.walk()) {
			if child.kind() == "identifier" || child.kind() == "keyframes_name" {
				if let Ok(name) = child.utf8_text(contents.as_bytes()) {
					let trimmed = name.trim();
					if !trimmed.is_empty() {
						return Some(trimmed.to_string());
					}
				}
			}
		}
		None
	}
}

/// The first selector text of a `selectors` node: the first class, id, tag or
/// universal selector, descending through pseudo selectors (`.card:hover`
/// yields `:hover` first, as pseudo names are pushed before the decorated
/// selector).
fn first_selector(node: Node, contents: &str) -> Option<String> {
	let mut cursor = node.walk();
	if !cursor.goto_first_child() {
		return None;
	}
	loop {
		let child = cursor.node();
		match child.kind() {
			"class_selector" | "id_selector" | "tag_name" | "universal_selector" => {
				if let Ok(text) = child.utf8_text(contents.as_bytes()) {
					let selector = text.trim();
					if !selector.is_empty() {
						return Some(selector.to_string());
					}
				}
			}
			"pseudo_class_selector" | "pseudo_element_selector" => {
				for pseudo_child in child.children(&mut child.walk()) {
					if matches!(pseudo_child.kind(), "class_name" | "identifier") {
						if let Ok(name) = pseudo_child.utf8_text(contents.as_bytes()) {
							return Some(format!(":{}", name.trim()));
						}
					}
				}
				return first_selector(child, contents);
			}
			_ => {
				if let Some(found) = first_selector(child, contents) {
					return Some(found);
				}
			}
		}
		if !cursor.goto_next_sibling() {
			return None;
		}
	}
}

#[cfg(test)]
#[path = "css_tests.rs"]
mod css_tests;
