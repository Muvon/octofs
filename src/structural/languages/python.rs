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

//! Python grammar: functions and classes (methods are nested functions),
//! plus the graph methods (imports, calls, base classes, module resolution).

use super::javascript::{join_path, parent_dir};
use super::{parse_call_target, simple_type_name, CallTarget, Language, TypeRelationKind};
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::symbols::ImportBinding;
use tree_sitter::Node;

/// Marks an import path that is only a candidate: `from pkg import sub` may
/// name the module `pkg.sub` or an object of `pkg`.
const SUBMODULE_CANDIDATE: &str = "~";
pub(crate) struct Python {}

impl Language for Python {
	fn name(&self) -> &'static str {
		"python"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_python::LANGUAGE.into()
	}

	// `import a.b`, `from a import b`; the names of a parenthesised group sit
	// on lines of their own, which the default name-list rule accepts.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&["import", "from"]
	}

	// A package's `__init__.py` passes on what it imports; any other file
	// only with a wildcard import.
	fn line_may_reexport(&self, line: &str, name: &str, path: &str) -> bool {
		let text = line.trim_start();
		text.starts_with("from")
			&& self.line_may_import(line, name)
			&& (path.rsplit('/').next() == Some("__init__.py") || text.contains("import *"))
	}

	// `class A(Base):`; further bases may sit on lines of their own.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["class"]
	}

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// The class container is included (chunking drops it as too large);
		// imports are dropped — they declare nothing an outline needs.
		vec!["function_definition", "class_definition"]
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		self.get_outline_kinds()
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			// `import module` / `import module as alias`
			"import_statement" => {
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					imports.extend(parse_python_import_statement_full_path(import_text));
				}
			}
			// `from module import item1, item2` / `from module import *`
			"import_from_statement" => {
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					imports.extend(parse_python_from_import_statement_full_path(import_text));
				}
			}
			"function_definition" | "class_definition" => {
				// Everything at module level is exported by default. A decorated
				// definition's immediate parent is `decorated_definition`, not
				// `module`, so walk up through that wrapper too.
				let is_module_level = node.parent().is_some_and(|parent| {
					parent.kind() == "module"
						|| (parent.kind() == "decorated_definition"
							&& parent
								.parent()
								.is_some_and(|grandparent| grandparent.kind() == "module"))
				});
				if is_module_level {
					let mut cursor = node.walk();
					let name_node = node.children(&mut cursor).find(|child| child.kind() == "identifier");
					if let Some(name_node) = name_node {
						if let Ok(name) = name_node.utf8_text(contents.as_bytes()) {
							exports.push(name.to_string());
						}
					}
				}
			}
			_ => {}
		}

		(imports, exports)
	}

	fn is_directory_index(&self, path: &str) -> bool {
		path.rsplit('/').next() == Some("__init__.py")
	}

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		let Ok(text) = node.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		if imports.is_empty() {
			return Vec::new();
		}
		let text = text.trim();
		let mut bindings = Vec::new();
		if let Some(modules) = text.strip_prefix("import ") {
			// Only `import a.b as c` binds a name of its own.
			for item in modules.split(',') {
				let mut parts = item.split_whitespace();
				let Some(path) = parts.next() else { continue };
				if parts.next() != Some("as") {
					continue;
				}
				if let Some(alias) = parts.next() {
					bindings.push(ImportBinding {
						local_name: alias.to_string(),
						imported_name: None,
						import_path: path.to_string(),
					});
				}
			}
		} else if let Some((module, names)) = text.strip_prefix("from ").and_then(|rest| rest.split_once(" import ")) {
			for item in names.trim_matches(['(', ')']).split(',') {
				let mut parts = item.split_whitespace();
				let Some(imported) = parts.next() else {
					continue;
				};
				let local = if parts.next() == Some("as") {
					parts.next().unwrap_or(imported)
				} else {
					imported
				};
				bindings.push(ImportBinding {
					local_name: local.to_string(),
					imported_name: Some(imported.to_string()),
					import_path: module.trim().to_string(),
				});
			}
		}
		bindings
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		if node.kind() != "call" {
			return Vec::new();
		}
		node.child_by_field_name("function")
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(parse_call_target)
			.into_iter()
			.collect()
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		// Python has no `implements`; the bases in `class Foo(Bar, Baz):` are
		// all Extends. They sit in `argument_list` (or `superclasses`,
		// depending on the grammar version).
		let mut relations = Vec::new();
		if node.kind() != "class_definition" {
			return relations;
		}
		let mut cursor = node.walk();
		for child in node.children(&mut cursor) {
			if !matches!(child.kind(), "argument_list" | "superclasses") {
				continue;
			}
			let mut base_cursor = child.walk();
			for base in child.children(&mut base_cursor) {
				if matches!(base.kind(), "identifier" | "attribute" | "subscript" | "call") {
					if let Ok(text) = base.utf8_text(contents.as_bytes()) {
						if let Some(name) = simple_type_name(text) {
							relations.push((TypeRelationKind::Extends, name));
						}
					}
				}
			}
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		let import_path = import_path.strip_prefix(SUBMODULE_CANDIDATE).unwrap_or(import_path);
		let resolved = if import_path.starts_with('.') {
			resolve_relative_import(import_path, source_file, all_files)
		} else {
			resolve_absolute_import(import_path, source_file, all_files).map(|(found, _)| found)
		};
		// A module that names itself (`import pkg` inside `pkg/__init__.py`)
		// is no dependency.
		resolved.filter(|found| found != source_file)
	}

	fn import_is_guess(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> bool {
		let import_path = import_path.strip_prefix(SUBMODULE_CANDIDATE).unwrap_or(import_path);
		!import_path.starts_with('.')
			&& resolve_absolute_import(import_path, source_file, all_files).is_some_and(|(_, guess)| guess)
	}

	fn import_is_optional(&self, import_path: &str) -> bool {
		import_path.starts_with(SUBMODULE_CANDIDATE)
	}
}

/// Module names of `import a, b as c`.
fn parse_python_import_statement_full_path(import_text: &str) -> Vec<String> {
	let Some(rest) = import_text.trim().strip_prefix("import ") else {
		return Vec::new();
	};
	rest.split(',')
		.map(|item| {
			let item = item.trim();
			item.find(" as ").map_or(item, |as_pos| &item[..as_pos])
		})
		.filter(|module_name| !module_name.is_empty())
		.map(str::to_string)
		.collect()
}

/// Module paths of `from module import item`, leading dots included. Each
/// imported name may be a submodule of that package, so one candidate path
/// per name follows (`from pkg import sub` gives `pkg` and `~pkg.sub`; `from .
/// import a, b` gives `.a` and `.b`, the bare dots stand for `*` imports). A
/// candidate (marked with `~`) that names no file is dropped, not an external
/// module: `from typing import List` imports `typing`.
fn parse_python_from_import_statement_full_path(import_text: &str) -> Vec<String> {
	let cleaned = import_text.trim();
	let Some(rest) = cleaned.strip_prefix("from ") else {
		return Vec::new();
	};
	let Some((module_part, names_part)) = rest.split_once(" import ") else {
		return Vec::new();
	};
	let module_part = module_part.trim();
	if module_part.is_empty() {
		return Vec::new();
	}
	let names: Vec<&str> = names_part
		.trim()
		.trim_matches(|character: char| matches!(character, '(' | ')'))
		.split(',')
		.filter_map(|item| item.split_whitespace().next())
		.filter(|name| {
			name.chars()
				.all(|character| character.is_alphanumeric() || character == '_')
		})
		.collect();
	if !module_part.chars().all(|character| character == '.') {
		let mut paths = vec![module_part.to_string()];
		paths.extend(
			names
				.iter()
				.map(|name| format!("{SUBMODULE_CANDIDATE}{module_part}.{name}")),
		);
		return paths;
	}
	if names.is_empty() {
		return vec![module_part.to_string()];
	}
	names.iter().map(|name| format!("{module_part}{name}")).collect()
}

/// Parent of a directory, `None` above the root — `Path::parent` semantics,
/// where the root of a relative path is the empty string.
fn parent_of_dir(directory: &str) -> Option<&str> {
	if directory.is_empty() {
		None
	} else {
		Some(parent_dir(directory))
	}
}

/// Resolve `.module` or `..module`: one leading dot is the importing file's
/// own directory, each further dot goes one directory up.
fn resolve_relative_import(import_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let module_name = import_path.trim_start_matches('.');
	let dots = import_path.len() - module_name.len();

	let mut target_dir = parent_dir(source_file);
	for _ in 1..dots {
		target_dir = parent_of_dir(target_dir)?;
	}

	find_python_module(module_name, target_dir, registry).or_else(|| {
		// `from . import name` lists names that may live in the package's
		// `__init__.py` rather than in a submodule of their own.
		if module_name.contains('.') {
			return None;
		}
		registry.find_exact_file(&join_path(target_dir, "__init__.py"))
	})
}

/// Resolve `pkg.module`: the importing file's directory first, then each
/// directory above it up to the project root. The registry holds
/// workdir-relative paths, so the root is the empty directory. The flag says
/// the file was found only in a directory between the two: a standard-library
/// name (`types`) can collide with a local file up there, so that is a guess.
fn resolve_absolute_import(import_path: &str, source_file: &str, registry: &FileRegistry) -> Option<(String, bool)> {
	// The import root is the project root or any directory above the file:
	// `src/` layouts and nested packages put it anywhere on the way up.
	let own_directory = parent_dir(source_file);
	let mut directory = own_directory;
	loop {
		if let Some(found) = find_python_module(import_path, directory, registry) {
			let intermediate = directory != own_directory && !directory.is_empty();
			return Some((found, intermediate));
		}
		directory = parent_of_dir(directory)?;
	}
}

/// Find `a.b.c` below `base_dir` as `a/b/c.py` or `a/b/c/__init__.py`.
fn find_python_module(module_name: &str, base_dir: &str, registry: &FileRegistry) -> Option<String> {
	// `from . import *` names the package itself.
	if module_name.is_empty() {
		return registry.find_exact_file(&join_path(base_dir, "__init__.py"));
	}
	let mut parts: Vec<&str> = module_name.split('.').collect();
	let last = parts.pop()?;
	let mut current_path = base_dir.to_string();
	for part in parts {
		current_path = join_path(&current_path, part);
	}

	if let Some(found) = registry.find_exact_file(&join_path(&current_path, &format!("{last}.py"))) {
		return Some(found);
	}
	registry.find_exact_file(&join_path(&join_path(&current_path, last), "__init__.py"))
}

#[cfg(test)]
#[path = "python_tests.rs"]
mod python_tests;
