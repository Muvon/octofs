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

use super::{
	files_in_dir_matching, find_graph_symbol_owner, parse_call_target, path_ends_with_components, simple_type_name,
	CallTarget, Language, TypeRelationKind,
};
use crate::structural::graph::registry::{normalize_separators, resolve_relative_path, FileRegistry};
use tree_sitter::Node;

pub(crate) struct Go {}

impl Language for Go {
	fn name(&self) -> &'static str {
		"go"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_go::LANGUAGE.into()
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Same kinds as the outline: `type_spec` rather than
		// `type_declaration` so grouped declarations yield one symbol per spec.
		self.get_outline_kinds()
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// `type_spec` instead of `type_declaration`: the name lives two levels
		// deep (type_declaration > type_spec > identifier), so per-spec nodes
		// are the only way the default name scan resolves them — and grouped
		// declarations (`type ( A ...; B ... )`) yield one item per spec.
		// const/var/import declarations declare no single named item.
		vec!["function_declaration", "method_declaration", "type_spec"]
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"import_declaration" => {
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					imports.extend(parse_go_import_statement(import_text));
				}
			}
			"function_declaration"
			| "method_declaration"
			| "type_declaration"
			| "const_declaration"
			| "var_declaration" => {
				// Exported Go names start with an uppercase letter. Func and
				// method names are direct children, but a type/var/const name
				// sits one level down inside its spec node, so both levels are
				// scanned.
				exports.extend(exported_name(node, contents));
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					if matches!(child.kind(), "type_spec" | "var_spec" | "const_spec") {
						exports.extend(exported_name(child, contents));
					}
				}
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "call_expression" {
			return Vec::new();
		}
		node.child_by_field_name("function")
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(parse_call_target)
			.into_iter()
			.collect()
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() != "method_declaration" {
			return find_graph_symbol_owner(node, contents);
		}
		// The receiver type (`func (s *Server) Handle()`) owns the method.
		let receiver = node.child_by_field_name("receiver")?;
		let mut cursor = receiver.walk();
		for parameter in receiver.children(&mut cursor) {
			if let Some(receiver_type) = parameter.child_by_field_name("type") {
				if let Ok(text) = receiver_type.utf8_text(contents.as_bytes()) {
					return simple_type_name(text);
				}
			}
		}
		None
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// Go has no `extends`/`implements`; struct embedding
		// (`type Foo struct { Bar }`) is the idiomatic equivalent and the only
		// edge reported. Interface satisfaction is structural, so it is not.
		let mut relations = Vec::new();
		if node.kind() == "field_declaration" && node.child_by_field_name("name").is_none() {
			if let Some(name) = node
				.child_by_field_name("type")
				.and_then(|type_node| type_node.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name)
			{
				relations.push((TypeRelationKind::Extends, name));
			}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		self.resolve_import_files(import_path, source_file, all_files)
			.into_iter()
			.next()
	}

	/// A Go import names a package: every file of its directory.
	fn resolve_import_files(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Vec<String> {
		if import_path.starts_with("./") || import_path.starts_with("../") {
			resolve_relative_import(import_path, source_file, all_files)
		} else {
			resolve_package_import(import_path, all_files)
		}
	}
}

/// Name of the first identifier-like direct child when it is exported (starts
/// with an uppercase letter).
fn exported_name(node: Node, contents: &str) -> Option<String> {
	let mut cursor = node.walk();
	let name_node = node
		.children(&mut cursor)
		.find(|child| matches!(child.kind(), "identifier" | "field_identifier" | "type_identifier"))?;
	let name = name_node.utf8_text(contents.as_bytes()).ok()?;
	name.chars()
		.next()
		.is_some_and(char::is_uppercase)
		.then(|| name.to_string())
}

/// Import paths of one `import` declaration, quotes and aliases stripped:
/// `import "fmt"`, `import alias "pkg/path"` and the grouped `import ( ... )`
/// form. The full path is returned because the resolver matches it against
/// directory suffixes.
fn parse_go_import_statement(import_text: &str) -> Vec<String> {
	let cleaned = import_text.trim();

	if let Some(rest) = cleaned.strip_prefix("import ") {
		if !cleaned.contains('(') {
			return import_path_of_line(rest).into_iter().collect();
		}
	}

	if let (Some(start), Some(end)) = (cleaned.find('('), cleaned.rfind(')')) {
		if let Some(block) = cleaned.get(start + 1..end) {
			return block.lines().filter_map(import_path_of_line).collect();
		}
	}

	Vec::new()
}

/// The path of one import line: `"pkg"` or `alias "pkg"`, with a trailing
/// `// comment` ignored.
fn import_path_of_line(line: &str) -> Option<String> {
	let line = line.split("//").next().unwrap_or(line).trim();
	let parts: Vec<&str> = line.split_whitespace().collect();
	let raw_path = match parts.as_slice() {
		[path] | [_, path] => path.trim_matches('"'),
		_ => return None,
	};
	(!raw_path.is_empty()).then(|| raw_path.to_string())
}

/// The files of a package directory: every non-test `.go` file, or the test
/// files when only tests exist.
fn package_files(registry: &FileRegistry, directory: &str) -> Vec<String> {
	let sources = files_in_dir_matching(registry, directory, |file| {
		file.ends_with(".go") && !file.ends_with("_test.go")
	});
	if sources.is_empty() {
		return files_in_dir_matching(registry, directory, |file| file.ends_with(".go"));
	}
	sources
}

/// A `./` or `../` import names a directory: the files of that package.
fn resolve_relative_import(import_path: &str, source_file: &str, registry: &FileRegistry) -> Vec<String> {
	package_files(registry, &resolve_relative_path(source_file, import_path))
}

/// Match the import path against directory paths component by component,
/// longest suffix of the import first: `example.com/app/internal/store`
/// prefers `internal/store` over a bare `store` directory. A directory must
/// end with the suffix on a component boundary, so `os` never matches
/// `repos` and a standard-library import with no such directory stays
/// external.
fn resolve_package_import(import_path: &str, registry: &FileRegistry) -> Vec<String> {
	let normalized_import = normalize_separators(import_path);
	let segments: Vec<&str> = normalized_import
		.split('/')
		.filter(|segment| !segment.is_empty())
		.collect();
	let Some(last_segment) = segments.last() else {
		return Vec::new();
	};
	for start in 0..segments.len() {
		let suffix = segments[start..].join("/");
		let mut directories: Vec<&String> = registry
			.dirs_named(last_segment)
			.iter()
			.filter(|directory| path_ends_with_components(directory, &suffix))
			.collect();
		directories.sort();
		if let Some(found) = directories
			.into_iter()
			.map(|directory| package_files(registry, directory))
			.find(|files| !files.is_empty())
		{
			return found;
		}
	}
	Vec::new()
}

#[cfg(test)]
#[path = "go_tests.rs"]
mod go_tests;
