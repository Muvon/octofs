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

use super::C_STYLE_COMMENTS;
use super::{
	files_in_dir_matching, parse_call_target, path_ends_with_components, path_ends_with_name, simple_type_name,
	CallTarget, Language, TypeRelationKind,
};
use super::{CalleeSyntax, PackageScope, SymbolKind};
use super::{PatternGrammar, IDENTIFIER_EXPANDO};
use crate::structural::graph::registry::{normalize_separators, resolve_relative_path, FileRegistry};
use crate::structural::graph::symbols::ImportBinding;
use tree_sitter::Node;

/// What an embedded field line may hold besides its name: a pointer star, a
/// package qualifier and a struct tag.
const GO_EMBEDDED_FIELD_EXTRA: &str = "*.`\":;";

pub(crate) struct Go {}

// Go takes a function's address with `&` and dereferences with `*`.
const CALLEE_SYNTAX: CalleeSyntax = CalleeSyntax {
	prefix_operators: &['&', '*'],
	..CalleeSyntax::DOTTED
};

impl Language for Go {
	fn name(&self) -> &'static str {
		"go"
	}

	fn extensions(&self) -> &'static [&'static str] {
		&["go"]
	}

	fn container_kinds(&self) -> &'static [(&'static str, &'static str)] {
		&[
			("function_declaration", "func"),
			("method_declaration", "method"),
			("type_declaration", "type"),
		]
	}

	fn definition_kinds(&self) -> &'static [&'static str] {
		&[
			"function_declaration",
			"method_declaration",
			"type_spec",
			"const_spec",
			"var_spec",
		]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_go::LANGUAGE.into()
	}

	fn pattern_grammar(&self) -> Option<PatternGrammar> {
		Some(PatternGrammar {
			name: self.name(),
			grammar: self.get_ts_language(),
			expando: IDENTIFIER_EXPANDO,
		})
	}

	// The path is a string literal, so even in a `import (` block (no `import`
	// keyword on the line) the name is the last segment before the closing
	// quote; a URL or other string that merely holds the name does not match.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		path_ends_with_name(line, name, "\"`")
	}

	// An embedded field is a bare type name (`Logger`, `*Logger`, `pkg.Logger`)
	// that may be the only token on its line, optionally with a tag.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["struct", "interface"]
	}

	fn inheritance_name_extras(&self) -> &'static str {
		GO_EMBEDDED_FIELD_EXTRA
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		C_STYLE_COMMENTS
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		&["type", "const", "var"]
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		&["func"]
	}

	fn symbol_kind(&self, node: Node, _contents: &str) -> SymbolKind {
		match node.kind() {
			"function_declaration" | "method_declaration" => SymbolKind::Function,
			"type_spec" => SymbolKind::Type,
			_ => SymbolKind::Other,
		}
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

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		let Ok(text) = node.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		if imports.is_empty() {
			return Vec::new();
		}
		// `alias "path"` lines; a blank or dot alias binds no name.
		text.lines()
			.filter_map(|line| {
				let parts: Vec<_> = line
					.trim()
					.trim_start_matches("import")
					.trim_matches(['(', ')'])
					.split_whitespace()
					.collect();
				(parts.len() == 2 && !matches!(parts[0], "_" | ".")).then(|| ImportBinding {
					local_name: parts[0].to_string(),
					imported_name: None,
					import_path: parts[1].trim_matches('"').to_string(),
				})
			})
			.collect()
	}

	fn module_root_file(&self) -> Option<&'static str> {
		Some(GO_MODULE_FILE)
	}

	fn package_scope(&self) -> PackageScope {
		PackageScope::Directory
	}

	fn import_names_directory(&self) -> bool {
		true
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "call_expression" {
			return Vec::new();
		}
		node.child_by_field_name("function")
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(|text| parse_call_target(text, &CALLEE_SYNTAX))
			.into_iter()
			.collect()
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() != "method_declaration" {
			return None;
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
		// Go has no `extends`/`implements`; embedding (`type Foo struct { Bar }`,
		// `type Foo interface { Bar }`) is the idiomatic equivalent and the only
		// edge reported. Interface satisfaction is structural, so it is not.
		let embedded = match node.kind() {
			"field_declaration" if node.child_by_field_name("name").is_none() => node.child_by_field_name("type"),
			// An interface element that is one plain type (`Logger`, `io.Reader`):
			// a union or a `~T` constraint embeds nothing by name.
			"type_elem" if node.named_child_count() == 1 => node.named_child(0),
			_ => None,
		};
		embedded
			.filter(|type_node| matches!(type_node.kind(), "type_identifier" | "qualified_type" | "pointer_type"))
			.and_then(|type_node| type_node.utf8_text(contents.as_bytes()).ok())
			.and_then(simple_type_name)
			.map(|name| (TypeRelationKind::Extends, name))
			.into_iter()
			.collect()
	}

	// The type that embeds, not the embedded one: the field line names the
	// embedded type, which would make `Foo` extend itself.
	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		let mut current = node.parent();
		while let Some(parent) = current {
			if parent.kind() == "type_spec" {
				return parent
					.child_by_field_name("name")
					.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
					.map(str::to_string);
			}
			current = parent.parent();
		}
		None
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

/// Directory of the Go standard library inside the Go source tree.
const GO_STANDARD_LIBRARY_ROOT: &str = "src";

/// The manifest that roots a Go module: imports of the module's own
/// packages are written relative to its directory.
const GO_MODULE_FILE: &str = "go.mod";

/// Match the import path against directory paths component by component,
/// longest suffix of the import first: `example.com/app/internal/store`
/// prefers `internal/store` over a bare `store` directory. A directory must
/// end with the suffix on a component boundary.
///
/// Only a module-qualified path (its first segment has a dot) is matched by
/// suffix. A path without one is the project's own module when a `go.mod`
/// directory holds the rest of it (`module myapp` makes `myapp/internal/log`
/// the directory `internal/log` next to the `go.mod`); otherwise it is a
/// standard-library package (`log`, `net/http`) and names the directory
/// `src/<path>` of the Go source tree or `<path>` at the root, never a
/// project directory such as `internal/log`.
fn resolve_package_import(import_path: &str, registry: &FileRegistry) -> Vec<String> {
	let normalized_import = normalize_separators(import_path);
	let segments: Vec<&str> = normalized_import
		.split('/')
		.filter(|segment| !segment.is_empty())
		.collect();
	let Some(last_segment) = segments.last() else {
		return Vec::new();
	};
	if !segments[0].contains('.') {
		let path = segments.join("/");
		if segments.len() > 1 {
			let rest = segments[1..].join("/");
			let mut modules: Vec<&String> = registry.files_named(GO_MODULE_FILE).iter().collect();
			modules.sort();
			for go_mod in modules {
				let directory = go_mod.rsplit_once('/').map_or("", |(directory, _)| directory);
				let candidate = if directory.is_empty() {
					rest.clone()
				} else {
					format!("{directory}/{rest}")
				};
				let files = package_files(registry, &candidate);
				if !files.is_empty() {
					return files;
				}
			}
		}
		let in_source_tree = package_files(registry, &format!("{GO_STANDARD_LIBRARY_ROOT}/{path}"));
		if !in_source_tree.is_empty() {
			return in_source_tree;
		}
		return package_files(registry, &path);
	}
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
