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

//! Rust grammar: items, types, traits, modules, constants and macros.

use super::{extract_symbol_by_kinds, simple_type_name, Language};
use tree_sitter::Node;

pub(crate) struct Rust {}

impl Language for Rust {
	fn name(&self) -> &'static str {
		"rust"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// impl_item is included: the impl header line is an item, so methods of
		// different impls are distinguishable in an outline.
		vec![
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"impl_item",
			"mod_item",
			"const_item",
			"macro_definition",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			// `impl Trait for Type` names both sides; the type alone would
			// collide with the inherent impl of the same type.
			let type_name = node
				.child_by_field_name("type")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name)?;
			let trait_name = node
				.child_by_field_name("trait")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name);
			return Some(match trait_name {
				Some(trait_name) => format!("{trait_name} for {type_name}"),
				None => type_name,
			});
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}
}

#[cfg(test)]
#[path = "rust_tests.rs"]
mod rust_tests;
