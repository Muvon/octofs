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

//! Java grammar: types, records, methods and constructors.

use super::Language;
use tree_sitter::Node;

pub(crate) struct Java {}

impl Language for Java {
	fn name(&self) -> &'static str {
		"java"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_java::LANGUAGE.into()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		vec![
			"class_declaration",
			"interface_declaration",
			"enum_declaration",
			"record_declaration",
			"annotation_type_declaration",
			"method_declaration",
			"constructor_declaration",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		// The default child scan would match a method's RETURN TYPE
		// (type_identifier precedes the name), so use the grammar's `name`
		// field — every outline kind above declares one.
		node.child_by_field_name("name")
			.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
			.map(str::to_string)
	}
}

#[cfg(test)]
#[path = "java_tests.rs"]
mod java_tests;
