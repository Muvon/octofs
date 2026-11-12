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

//! Bash grammar: named function definitions.

use super::Language;
use tree_sitter::Node;

pub(crate) struct Bash {}

impl Language for Bash {
	fn name(&self) -> &'static str {
		"bash"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_bash::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// Only named functions are items; `command` nodes (source/. calls)
		// declare nothing.
		vec!["function_definition"]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// Bash function names are `word` nodes exposed via the `name` field;
		// the default identifier/name scan never matches them — and on
		// `command` nodes it would match `command_name`, turning every
		// command into a bogus item.
		if node.kind() == "function_definition" {
			return node
				.child_by_field_name("name")
				.and_then(|n| n.utf8_text(contents.as_bytes()).ok())
				.map(str::to_string);
		}
		None
	}
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod bash_tests;
