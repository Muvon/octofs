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

//! Go grammar: functions, methods and type specs.

use super::Language;

pub(crate) struct Go {}

impl Language for Go {
	fn name(&self) -> &'static str {
		"go"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_go::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `type_spec` instead of `type_declaration`: the name lives two levels
		// deep (type_declaration > type_spec > identifier), so per-spec nodes
		// are the only way the default name scan resolves them — and grouped
		// declarations (`type ( A ...; B ... )`) yield one item per spec.
		// const/var/import declarations declare no single named item.
		vec!["function_declaration", "method_declaration", "type_spec"]
	}
}

#[cfg(test)]
#[path = "go_tests.rs"]
mod go_tests;
