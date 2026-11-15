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

use super::{enclosing_owner, CalleeSyntax, PackageScope, SymbolKind};
use super::{
	files_in_dir_matching, parse_call_target, path_ends_with_components, simple_type_name, CallTarget, Language,
	TypeRelationKind,
};
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::symbols::ImportBinding;
use tree_sitter::Node;

const CALLEE_SYNTAX: CalleeSyntax = CalleeSyntax {
	self_receivers: &["this"],
	angle_generics: true,
	..CalleeSyntax::DOTTED
};

pub(crate) struct Java {}

impl Language for Java {
	fn name(&self) -> &'static str {
		"java"
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_java::LANGUAGE.into()
	}

	// A Java import is one statement with the whole qualified name after
	// `import` (also `import static`, `.*`); there is no grouped form.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&["import"]
	}

	// `implements A,` newline `B` leaves a bare name on its own line, which
	// the default name-list rule accepts.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["extends", "implements"]
	}

	fn package_scope(&self) -> PackageScope {
		PackageScope::Declared
	}

	fn declared_package(&self, node: Node, contents: &str) -> Option<String> {
		package_name(node, contents)
	}

	// `import a.*` names the directory of the package.
	fn import_names_directory(&self) -> bool {
		true
	}

	// A single-type or static import binds the last segment: `import a.B`
	// makes `B` mean that class, which wins over a wildcard import of another
	// package with a `B`.
	fn extract_import_bindings(&self, node: Node, _contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		if node.kind() != "import_declaration" {
			return Vec::new();
		}
		imports
			.iter()
			.filter(|path| !path.ends_with(".*"))
			.filter_map(|path| {
				let name = path.rsplit('.').next()?;
				Some(ImportBinding {
					local_name: name.to_string(),
					imported_name: Some(name.to_string()),
					import_path: path.clone(),
				})
			})
			.collect()
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		&["//", "/*"]
	}

	// Generic bounds (`T extends A & B`) and wildcards.
	fn inheritance_name_extras(&self) -> &'static str {
		"<>&?"
	}

	fn import_name_separators(&self) -> &'static [char] {
		&['.']
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		&["class", "interface", "enum", "record"]
	}

	fn symbol_kind(&self, node: Node, _contents: &str) -> SymbolKind {
		match node.kind() {
			"method_declaration" | "constructor_declaration" => SymbolKind::Function,
			"class_declaration" | "record_declaration" => SymbolKind::Class,
			"interface_declaration" | "annotation_type_declaration" => SymbolKind::Interface,
			"enum_declaration" => SymbolKind::Enum,
			_ => SymbolKind::Other,
		}
	}

	fn is_constructor(&self, node: Node, _contents: &str) -> bool {
		node.kind() == "constructor_declaration"
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		enclosing_owner(
			node,
			contents,
			&[
				"class_declaration",
				"interface_declaration",
				"enum_declaration",
				"record_declaration",
			],
		)
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// Same kinds as the outline: field declarations would be named after
		// their type, and lambdas and imports declare no symbol.
		self.get_outline_kinds()
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

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"import_declaration" => {
				if let Ok(import_text) = node.utf8_text(contents.as_bytes()) {
					// Whitespace between `import` and `static` has to go before
					// the `static` prefix can be recognised.
					let after_import = import_text
						.trim()
						.strip_prefix("import")
						.unwrap_or(import_text)
						.trim_start();
					let import_path = after_import
						.strip_prefix("static")
						.map(str::trim_start)
						.unwrap_or(after_import)
						.trim_end_matches(';')
						.trim();
					if !import_path.is_empty() {
						imports.push(import_path.to_string());
					}
				}
			}
			"package_declaration" => {
				exports.extend(package_name(node, contents).map(|name| format!("package:{name}")));
			}
			"class_declaration"
			| "interface_declaration"
			| "enum_declaration"
			| "annotation_type_declaration"
			| "record_declaration" => {
				let type_kind = match node.kind() {
					"class_declaration" => "class",
					"interface_declaration" => "interface",
					"enum_declaration" => "enum",
					"annotation_type_declaration" => "annotation",
					_ => "record",
				};
				if is_public(node, contents) {
					exports.extend(declared_name(node, contents).map(|name| format!("{type_kind}:{name}")));
				}
			}
			"method_declaration" if is_public(node, contents) => {
				exports.extend(declared_name(node, contents).map(|name| format!("method:{name}")));
			}
			_ => {}
		}

		(imports, exports)
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		match node.kind() {
			"method_invocation" => {
				let Some(name) = declared_name(node, contents) else {
					return Vec::new();
				};
				let qualified = node
					.child_by_field_name("object")
					.and_then(|object| object.utf8_text(contents.as_bytes()).ok())
					.and_then(|object| parse_call_target(&format!("{object}.{name}"), &CALLEE_SYNTAX));
				// A receiver that is itself a call or other expression
				// (`getRepo().save()`) has no usable qualifier, but the callee
				// name is still a call.
				qualified
					.or_else(|| parse_call_target(&name, &CALLEE_SYNTAX))
					.into_iter()
					.collect()
			}
			// `new Foo()` calls the constructed type.
			"object_creation_expression" => {
				let mut cursor = node.walk();
				let constructed = node
					.children(&mut cursor)
					.find(|child| matches!(child.kind(), "type_identifier" | "generic_type"));
				constructed
					.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
					.and_then(|text| parse_call_target(text, &CALLEE_SYNTAX))
					.into_iter()
					.collect()
			}
			_ => Vec::new(),
		}
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let mut relations = Vec::new();
		let mut cursor = node.walk();
		for child in node.children(&mut cursor) {
			let kind = match (node.kind(), child.kind()) {
				// `class Foo extends Bar implements I, J`
				("class_declaration", "superclass") => TypeRelationKind::Extends,
				("class_declaration", "super_interfaces") => TypeRelationKind::Implements,
				// `interface A extends B, C`
				("interface_declaration", "extends_interfaces") => TypeRelationKind::Extends,
				// `enum Foo implements Bar` / `record Point(...) implements Shape`
				("enum_declaration" | "record_declaration", "super_interfaces") => TypeRelationKind::Implements,
				_ => continue,
			};
			collect_clause_types(child, contents, kind, &mut relations);
		}
		relations
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		self.resolve_import_files(import_path, source_file, all_files)
			.into_iter()
			.next()
	}

	/// `java.util.*` names a package: every file of its directory. A single
	/// class import names one file.
	fn resolve_import_files(&self, import_path: &str, _source_file: &str, all_files: &FileRegistry) -> Vec<String> {
		if let Some(package) = import_path.strip_suffix(".*") {
			let files = resolve_wildcard_import(package, all_files);
			if !files.is_empty() {
				return files;
			}
			// `import static com.x.Util.*` and `import com.x.Outer.*` name a
			// class, not a package.
			return resolve_class_import(package, all_files).into_iter().collect();
		}
		resolve_class_import(import_path, all_files).into_iter().collect()
	}
}

/// `com.example.MyClass` -> a file `com/example/MyClass.java` that ends with
/// the whole package path, wherever the source root is. A trailing member
/// (`import static com.example.Util.helper`) or nested class
/// (`com.example.Outer.Inner`) is not a file: the path is shortened one
/// segment at a time while a package path remains. A name without a dot has
/// no package path to map.
fn resolve_class_import(import_path: &str, all_files: &FileRegistry) -> Option<String> {
	let mut path = import_path;
	while let Some((parent, _)) = path.rsplit_once('.') {
		let relative_path = format!("{}.java", path.replace('.', "/"));
		let file_name = relative_path.rsplit('/').next()?;
		let found = all_files
			.files_named(file_name)
			.iter()
			.filter(|file| path_ends_with_components(file, &relative_path))
			.min()
			.cloned();
		if found.is_some() {
			return found;
		}
		path = parent;
	}
	None
}

/// The `.java` files of the directory whose path ends with the package path
/// of a wildcard import (`com.example.app` -> `com/example/app`).
fn resolve_wildcard_import(package: &str, registry: &FileRegistry) -> Vec<String> {
	let directory_suffix = package.replace('.', "/");
	let Some(last_segment) = directory_suffix.rsplit('/').next() else {
		return Vec::new();
	};
	let mut directories: Vec<&String> = registry
		.dirs_named(last_segment)
		.iter()
		.filter(|directory| path_ends_with_components(directory, &directory_suffix))
		.collect();
	directories.sort();
	directories
		.into_iter()
		.map(|directory| files_in_dir_matching(registry, directory, |file| file.ends_with(".java")))
		.find(|files| !files.is_empty())
		.unwrap_or_default()
}

/// The qualified name a `package` declaration gives the file.
fn package_name(node: Node, contents: &str) -> Option<String> {
	if node.kind() != "package_declaration" {
		return None;
	}
	let mut cursor = node.walk();
	let package = node
		.children(&mut cursor)
		.find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))?;
	package.utf8_text(contents.as_bytes()).ok().map(str::to_string)
}

/// Text of the node's `name` field.
fn declared_name(node: Node, contents: &str) -> Option<String> {
	node.child_by_field_name("name")
		.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
		.map(str::to_string)
}

/// Whether the declaration's modifiers mention `public`.
fn is_public(node: Node, contents: &str) -> bool {
	let mut cursor = node.walk();
	let modifiers = node.children(&mut cursor).find(|child| child.kind() == "modifiers");
	modifiers
		.and_then(|modifiers| modifiers.utf8_text(contents.as_bytes()).ok())
		.is_some_and(|text| text.contains("public"))
}

/// Type names of a `superclass`, `super_interfaces` or `extends_interfaces`
/// clause, generic arguments and package qualifiers dropped.
fn collect_clause_types(
	clause: Node,
	contents: &str,
	kind: TypeRelationKind,
	out: &mut Vec<(TypeRelationKind, String)>,
) {
	let mut cursor = clause.walk();
	for child in clause.children(&mut cursor) {
		match child.kind() {
			"type_identifier" | "generic_type" | "scoped_type_identifier" => push_type_name(child, contents, kind, out),
			// Some grammar versions wrap the interfaces in a list node.
			"type_list" | "interface_type_list" => {
				let mut list_cursor = child.walk();
				for item in child.children(&mut list_cursor) {
					if matches!(
						item.kind(),
						"type_identifier" | "generic_type" | "scoped_type_identifier"
					) {
						push_type_name(item, contents, kind, out);
					}
				}
			}
			_ => {}
		}
	}
}

fn push_type_name(node: Node, contents: &str, kind: TypeRelationKind, out: &mut Vec<(TypeRelationKind, String)>) {
	if let Some(name) = node.utf8_text(contents.as_bytes()).ok().and_then(simple_type_name) {
		out.push((kind, name));
	}
}

#[cfg(test)]
#[path = "java_tests.rs"]
mod java_tests;
