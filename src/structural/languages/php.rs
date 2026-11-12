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

//! PHP grammar: functions, methods and type containers.

use super::Language;

pub(crate) struct Php {}

impl Language for Php {
	fn name(&self) -> &'static str {
		"php"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_php::LANGUAGE_PHP.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The type containers are restored (chunking drops them as too
		// large); namespaces and use statements are dropped — a namespace is
		// one per file and a use declares nothing.
		vec![
			"function_definition",
			"method_declaration",
			"class_declaration",
			"interface_declaration",
			"trait_declaration",
			"enum_declaration",
		]
	}
}

#[cfg(test)]
#[path = "php_tests.rs"]
mod php_tests;
