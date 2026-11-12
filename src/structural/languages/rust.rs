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

use super::Language;

pub(crate) struct Rust {}

impl Language for Rust {
	fn name(&self) -> &'static str {
		"rust"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `impl_item` is deliberately absent: methods inside it are captured
		// individually as `function_item`, and the impl header itself is
		// implied by the methods' indentation.
		vec![
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"mod_item",
			"const_item",
			"macro_definition",
		]
	}
}

#[cfg(test)]
#[path = "rust_tests.rs"]
mod rust_tests;
