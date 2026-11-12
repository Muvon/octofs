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

//! C++ grammar: function definitions and type/namespace containers.

use super::{extract_call_target, extract_symbol_by_kinds, Language};
use tree_sitter::Node;

pub(crate) struct Cpp {}

impl Language for Cpp {
	fn name(&self) -> &'static str {
		"cpp"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_cpp::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The type containers are restored (chunking drops them as too
		// large); plain `declaration` nodes are dropped — they match every
		// local variable declaration and #include directive, which declare
		// nothing an outline needs.
		vec![
			"function_definition",
			"class_specifier",
			"struct_specifier",
			"enum_specifier",
			"namespace_definition",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "function_definition" {
			let declarator = node.child_by_field_name("declarator")?;
			let text = declarator.utf8_text(contents.as_bytes()).ok()?;
			let callable = text.split('(').next().unwrap_or(text);
			let name_part = callable.rsplit("::").next().unwrap_or(callable).trim();
			// Destructors keep their `~` and operators keep their operator
			// token: the generic call-target trim would reduce `~Foo` to `Foo`
			// (colliding with the constructor) and `operator+` to `operator`
			// (colliding with every other operator).
			if name_part.starts_with('~') {
				return Some(name_part.to_string());
			}
			if let Some(rest) = name_part.strip_prefix("operator") {
				if !rest.is_empty() && !rest.starts_with(|character: char| character.is_alphanumeric()) {
					return Some(name_part.to_string());
				}
			}
			return extract_call_target(callable);
		}
		// Bodyless specifiers are type REFERENCES (`struct stat st;`) or forward
		// declarations, not definitions — naming them would mint a bogus item
		// at every usage site.
		if matches!(node.kind(), "class_specifier" | "struct_specifier" | "enum_specifier")
			&& node.child_by_field_name("body").is_none()
		{
			return None;
		}
		node.child_by_field_name("name")
			.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
			.map(str::to_string)
			.or_else(|| {
				extract_symbol_by_kinds(
					node,
					contents,
					&["identifier", "type_identifier", "namespace_identifier"],
				)
			})
	}
}

#[cfg(test)]
#[path = "cpp_tests.rs"]
mod cpp_tests;
