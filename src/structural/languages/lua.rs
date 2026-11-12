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

//! Lua grammar: named function declarations.

use super::{extract_call_target, Language};
use tree_sitter::Node;

pub(crate) struct Lua {}

impl Language for Lua {
	fn name(&self) -> &'static str {
		"lua"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_lua::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `function_definition` is anonymous in this grammar; named local and
		// table methods are represented by `function_declaration`.
		vec!["function_declaration"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// The `name` field holds a possibly dotted path (`M.foo`); the item is
		// named by its last segment.
		let name = node.child_by_field_name("name")?;
		let text = name.utf8_text(contents.as_bytes()).ok()?;
		extract_call_target(text)
	}
}

#[cfg(test)]
#[path = "lua_tests.rs"]
mod lua_tests;
