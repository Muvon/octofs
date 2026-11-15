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

//! CSS grammar: rule sets and at-rules, named by their selectors, plus the
//! graph facts (`@import` edges between stylesheets).

use super::{path_ends_with_name, Language};
use crate::structural::graph::registry::{resolve_relative_path, FileRegistry};
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

	// `@import "x"`, `@import 'x'`, `@import url("x")` and `@import url(x)`: the
	// name ends the path.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		path_ends_with_name(line, name, "\"')")
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

	fn comment_starts(&self) -> &'static [&'static str] {
		&["/*"]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Same as the outline kinds minus `import_statement`: an import
		// declares nothing, and a media-query identifier on it would otherwise
		// become a bogus symbol.
		vec!["rule_set", "at_rule", "keyframes_statement", "media_statement"]
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		if node.kind() == "import_statement" {
			if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
				if let Some(url) = parse_css_import(import_text) {
					imports.push(url);
				}
			}
		}
		// CSS has no exports.
		(imports, Vec::new())
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			let relative_path = resolve_relative_path(source_file, import_path);
			if let Some(found) = all_files.find_normalized(&relative_path) {
				return Some(found.to_string());
			}
			// `@import "../shared/reset"` may omit the extension.
			return all_files.find_file_with_extensions(strip_extension(&relative_path), &CSS_EXTENSIONS);
		}

		// A bare filename lives next to the importing stylesheet first.
		let source_dir = source_file
			.rsplit_once(['/', '\\'])
			.map_or("", |(directory, _)| directory);
		let sibling = if source_dir.is_empty() {
			import_path.to_string()
		} else {
			format!("{source_dir}/{import_path}")
		};
		if let Some(found) = all_files.find_normalized(&sibling) {
			return Some(found.to_string());
		}
		all_files.find_exact_file(import_path)
	}
}

const CSS_EXTENSIONS: [&str; 3] = ["css", "scss", "sass"];

/// `@import "file.css"`, `@import 'file.css'`, `@import url("file.css")` or
/// the unquoted `@import url(file.css)`. The quote scan is not anchored to
/// the start, so it also covers the quoted `url(...)` forms.
fn parse_css_import(import_text: &str) -> Option<String> {
	for quote in ['"', '\''] {
		if let Some(start) = import_text.find(quote) {
			let rest = &import_text[start + 1..];
			if let Some(end) = rest.find(quote) {
				return Some(rest[..end].to_string());
			}
		}
	}
	let url_start = import_text.find("url(")?;
	let url_content = &import_text[url_start + "url(".len()..];
	let close = url_content.find(')')?;
	let unquoted = url_content[..close].trim();
	(!unquoted.is_empty()).then(|| unquoted.to_string())
}

/// The path without the extension of its last component (`a/b.scss` -> `a/b`).
fn strip_extension(path: &str) -> &str {
	let name_start = path.rfind('/').map_or(0, |index| index + 1);
	match path[name_start..].rfind('.') {
		Some(dot) if dot > 0 => &path[..name_start + dot],
		_ => path,
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
