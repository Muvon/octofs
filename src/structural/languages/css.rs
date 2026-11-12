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
/// selector). Iterative: a pathological parse can nest selectors arbitrarily
/// deep, and the walk must not recurse.
fn first_selector(node: Node, contents: &str) -> Option<String> {
	let mut stack = vec![node];
	while let Some(current) = stack.pop() {
		match current.kind() {
			"class_selector" | "id_selector" | "tag_name" | "universal_selector" => {
				if let Ok(text) = current.utf8_text(contents.as_bytes()) {
					let selector = text.trim();
					if !selector.is_empty() {
						return Some(selector.to_string());
					}
				}
				// Empty selector text: skip, like the sibling walk did.
				continue;
			}
			"pseudo_class_selector" | "pseudo_element_selector" => {
				// Pseudo names are pushed before the decorated selector.
				for pseudo_child in current.children(&mut current.walk()) {
					if matches!(pseudo_child.kind(), "class_name" | "identifier") {
						if let Ok(name) = pseudo_child.utf8_text(contents.as_bytes()) {
							return Some(format!(":{}", name.trim()));
						}
					}
				}
				// No name child: descend into the decorated selector.
			}
			_ => {}
		}
		// Children are pushed reversed so the LIFO stack pops them in sibling
		// order, each subtree fully before the next — matching the recursive
		// original.
		let children: Vec<Node> = current.children(&mut current.walk()).collect();
		stack.extend(children.into_iter().rev());
	}
	None
}

#[cfg(test)]
#[path = "css_tests.rs"]
mod css_tests;
