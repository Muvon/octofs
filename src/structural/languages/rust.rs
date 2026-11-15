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

use super::use_alias::aliased_use_bindings;
use super::C_STYLE_COMMENTS;
use super::{enclosing_owner, CalleeSyntax, SymbolKind};
use super::{
	extract_symbol_by_kinds, parse_call_target, simple_type_name, stem_and_directory, CallTarget, Language,
	TypeRelationKind, UNKNOWN_RECEIVER,
};
use super::{PatternGrammar, IDENTIFIER_EXPANDO};
use crate::structural::graph::registry::{normalize_separators, resolve_relative_path, FileRegistry};
use crate::structural::graph::symbols::ImportBinding;
use tree_sitter::Node;

/// File names whose child modules sit next to them rather than in a directory
/// named after the file.
const MODULE_ROOT_FILES: [&str; 3] = ["mod.rs", "lib.rs", "main.rs"];

// `?` (try) can sit before the `.` of a method call.
const CALLEE_SYNTAX: CalleeSyntax = CalleeSyntax {
	member_separators: &["?.", "::", "."],
	prefix_operators: &['&', '*'],
	self_receivers: &["self", "Self"],
	angle_generics: true,
	..CalleeSyntax::DOTTED
};

pub(crate) struct Rust {}

impl Language for Rust {
	fn name(&self) -> &'static str {
		"rust"
	}

	fn extensions(&self) -> &'static [&'static str] {
		&["rs"]
	}

	fn container_kinds(&self) -> &'static [(&'static str, &'static str)] {
		&[
			("function_item", "fn"),
			("impl_item", "impl"),
			("trait_item", "trait"),
			("struct_item", "struct"),
			("enum_item", "enum"),
			("mod_item", "mod"),
		]
	}

	fn definition_kinds(&self) -> &'static [&'static str] {
		&[
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"impl_item",
			"mod_item",
			"const_item",
			"static_item",
			"type_item",
			"union_item",
			"macro_definition",
		]
	}

	fn get_ts_language(&self) -> tree_sitter::Language {
		tree_sitter_rust::LANGUAGE.into()
	}

	fn pattern_grammar(&self) -> Option<PatternGrammar> {
		Some(PatternGrammar {
			name: self.name(),
			grammar: self.get_ts_language(),
			expando: IDENTIFIER_EXPANDO,
		})
	}

	// `use a::b;`, `mod b;`, `extern crate b;`; the names of a nested group
	// sit on lines of their own, which the default name-list rule accepts.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&["use", "mod", "extern", "::"]
	}

	// `impl Trait for Type`, `trait A: B` (bounds continue on later lines).
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&["impl", "trait"]
	}

	fn comment_starts(&self) -> &'static [&'static str] {
		C_STYLE_COMMENTS
	}

	// Trait bounds (`T: A + B`), lifetimes, `?Sized`, generic arguments.
	fn inheritance_name_extras(&self) -> &'static str {
		"+&<>:;'?"
	}

	fn import_name_separators(&self) -> &'static [char] {
		&[':']
	}

	fn declaration_keywords(&self) -> &'static [&'static str] {
		&[
			"struct", "enum", "trait", "type", "const", "static", "let", "mod", "union", "impl",
		]
	}

	fn callable_keywords(&self) -> &'static [&'static str] {
		&["fn"]
	}

	fn symbol_kind(&self, node: Node, _contents: &str) -> SymbolKind {
		match node.kind() {
			"function_item" => SymbolKind::Function,
			"struct_item" => SymbolKind::Struct,
			"enum_item" => SymbolKind::Enum,
			"trait_item" => SymbolKind::Trait,
			"mod_item" => SymbolKind::Module,
			"const_item" => SymbolKind::Const,
			"macro_definition" => SymbolKind::Macro,
			_ => SymbolKind::Other,
		}
	}

	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		enclosing_owner(node, contents, &["impl_item", "trait_item"])
	}

	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		// impl_item is left out: it declares no symbol of its own, its methods
		// do (owned by the impl's type).
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

	fn get_outline_kinds(&self) -> Vec<&'static str> {
		// impl_item is included: the impl header line is an item, so methods of
		// different impls are distinguishable in an outline.
		vec![
			"function_item",
			"struct_item",
			"enum_item",
			"trait_item",
			"impl_item",
			"mod_item",
			"const_item",
			"macro_definition",
		]
	}

	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			// `impl Trait for Type` names both sides; the type alone would
			// collide with the inherent impl of the same type.
			let type_name = node
				.child_by_field_name("type")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name)?;
			let trait_name = node
				.child_by_field_name("trait")
				.and_then(|child| child.utf8_text(contents.as_bytes()).ok())
				.and_then(simple_type_name);
			return Some(match trait_name {
				Some(trait_name) => format!("{trait_name} for {type_name}"),
				None => type_name,
			});
		}
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}

	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let mut imports = Vec::new();
		let mut exports = Vec::new();

		match node.kind() {
			"use_declaration" => {
				if let Ok(use_text) = node.utf8_text(contents.as_bytes()) {
					let nesting = inline_module_depth(node);
					imports.extend(
						expand_rust_use_paths(use_text)
							.into_iter()
							.filter_map(|path| lift_super_out_of_inline_modules(path, nesting)),
					);
				}
			}
			"mod_item" => {
				if let Some(name) = declared_name(node, contents) {
					// A mod_item without a body is a file-level declaration
					// (`mod foo;`): it depends on foo.rs or foo/mod.rs, so it is
					// emitted as a self-relative import for resolve_import. A
					// `#[path = "x.rs"]` names the file directly.
					if node.child_by_field_name("body").is_none() {
						match path_attribute(node, contents) {
							Some(path) => imports.push(format!("{PATH_ATTRIBUTE_PREFIX}{path}")),
							None => imports.push(format!("self::{name}")),
						}
					}
					if is_public(node, contents) {
						exports.push(name);
					}
				}
			}
			"function_item" | "struct_item" | "enum_item" | "trait_item" | "const_item" | "macro_definition"
				if is_public(node, contents) =>
			{
				exports.extend(declared_name(node, contents));
			}
			_ => {}
		}

		(imports, exports)
	}

	fn is_directory_index(&self, path: &str) -> bool {
		matches!(path.rsplit('/').next(), Some("mod.rs" | "lib.rs" | "main.rs"))
	}

	// The crate root is what `crate::` names.
	fn importer_names(&self, path: &str, _declared: &[&str]) -> Vec<String> {
		let file_name = path.rsplit('/').next().unwrap_or(path);
		let mut names = stem_and_directory(path, self.is_directory_index(path));
		if matches!(file_name, "lib.rs" | "main.rs") {
			names.push("crate".to_string());
		}
		names
	}

	fn extract_import_bindings(&self, node: Node, contents: &str, imports: &[String]) -> Vec<ImportBinding> {
		if node.kind() != "use_declaration" {
			return Vec::new();
		}
		let Ok(text) = node.utf8_text(contents.as_bytes()) else {
			return Vec::new();
		};
		let mut bindings = aliased_use_bindings(text.trim(), imports);
		// A plain or grouped `use a::b::{C, D}` brings each leaf in under its
		// own name, so a call to `C` is looked up in the files the import names.
		for import in imports {
			let name = import.rsplit(':').next().unwrap_or_default();
			let already_bound = bindings
				.iter()
				.any(|binding| binding.imported_name.as_deref() == Some(name));
			if name.is_empty() || matches!(name, "self" | "super" | "crate") || already_bound {
				continue;
			}
			bindings.push(ImportBinding {
				local_name: name.to_string(),
				imported_name: Some(name.to_string()),
				import_path: import.clone(),
			});
		}
		bindings
	}

	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		let callee = match node.kind() {
			"call_expression" => node.child_by_field_name("function"),
			// The macro name (`vec`, or a path such as `crate::utils::debug_log`)
			// without its `!`.
			"macro_invocation" => node.child_by_field_name("macro"),
			"token_tree" => return calls_in_token_tree(node, contents),
			_ => None,
		};
		callee
			.and_then(|callee| callee.utf8_text(contents.as_bytes()).ok())
			.and_then(|text| parse_call_target(text, &CALLEE_SYNTAX))
			.into_iter()
			.collect()
	}

	// `pub use a::b;` re-exports what the module `name` declares.
	fn line_may_reexport(&self, line: &str, name: &str, _path: &str) -> bool {
		line.trim_start().starts_with("pub") && self.line_may_import(line, name)
	}

	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let mut relations = Vec::new();
		match node.kind() {
			// `impl Trait for Type` makes Type implement Trait; an inherent
			// `impl Type {}` has no `trait` field and emits nothing.
			"impl_item" => {
				if let Some(name) = node
					.child_by_field_name("trait")
					.and_then(|trait_node| trait_node.utf8_text(contents.as_bytes()).ok())
					.filter(|text| !is_standard_library_path(text))
					.and_then(simple_type_name)
				{
					relations.push((TypeRelationKind::Implements, name));
				}
			}
			// `trait A: B + C` makes A extend B and C.
			"trait_item" => {
				if let Some(bounds) = node.child_by_field_name("bounds") {
					let mut cursor = bounds.walk();
					for child in bounds.children(&mut cursor) {
						if matches!(
							child.kind(),
							"type_identifier" | "scoped_type_identifier" | "generic_type"
						) {
							let bound = child
								.utf8_text(contents.as_bytes())
								.ok()
								.filter(|text| !is_standard_library_path(text));
							if let Some(name) = bound.and_then(simple_type_name) {
								relations.push((TypeRelationKind::Extends, name));
							}
						}
					}
				}
			}
			_ => {}
		}
		relations
	}

	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		if node.kind() == "impl_item" {
			let type_node = node.child_by_field_name("type")?;
			return simple_type_name(type_node.utf8_text(contents.as_bytes()).ok()?);
		}
		self.extract_declaration_name(node, contents)
	}

	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		let source_file = normalize_separators(source_file);
		let module_dir = module_directory(&source_file);
		if let Some(path) = import_path.strip_prefix(PATH_ATTRIBUTE_PREFIX) {
			return all_files.find_exact_file(&resolve_relative_path(&source_file, &format!("./{path}")));
		}
		// `use crate::*;`, `use super::*;` and `use self::*;` lose the glob and
		// leave the bare keyword: the module file itself.
		match import_path {
			"crate" => return find_crate_root(&source_file, all_files),
			"super" => return find_module_file(directory_of(&module_dir), all_files),
			"self" => return Some(source_file),
			_ => {}
		}
		if let Some(module_path) = import_path.strip_prefix("crate::") {
			return resolve_crate_import(module_path, &source_file, all_files);
		}
		if let Some(module_path) = import_path.strip_prefix("self::") {
			// `self::x` names a child module of the current module (`mod x;`
			// lands here too), never the source file itself.
			return resolve_relative_import(module_path, &module_dir, all_files);
		}
		if import_path.starts_with("super::") {
			return resolve_super_import(import_path, &module_dir, all_files);
		}
		// A bare path is relative to the current module (`use child::Item`);
		// otherwise it is tried against the crate root, which finds nothing for
		// third-party crates.
		if let Some(found) = resolve_relative_import(import_path, &module_dir, all_files) {
			return Some(found);
		}
		if !import_path.contains("::") {
			return None;
		}
		resolve_crate_import(import_path, &source_file, all_files)
			.or_else(|| resolve_workspace_crate_import(import_path, all_files))
	}

	// A path that only another workspace crate of that directory name can
	// own is matched by name, not by anything the `use` states.
	fn import_is_guess(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> bool {
		let source_file = normalize_separators(source_file);
		let is_bare = import_path.contains("::")
			&& !import_path.starts_with(PATH_ATTRIBUTE_PREFIX)
			&& ["crate::", "self::", "super::"]
				.iter()
				.all(|prefix| !import_path.starts_with(prefix));
		is_bare
			&& resolve_relative_import(import_path, &module_directory(&source_file), all_files).is_none()
			&& resolve_crate_import(import_path, &source_file, all_files).is_none()
			&& resolve_workspace_crate_import(import_path, all_files).is_some()
	}
}

/// How many inline modules (`mod tests { ... }`) enclose the node.
fn inline_module_depth(node: Node) -> usize {
	let mut depth = 0;
	let mut current = node.parent();
	while let Some(parent) = current {
		if parent.kind() == "mod_item" {
			depth += 1;
		}
		current = parent.parent();
	}
	depth
}

/// `super` inside an inline module is the module around it: each level of
/// nesting consumes one `super`, so `use super::*;` in a file's `mod tests`
/// names the file itself (no import) and `use super::sub::Item;` names the
/// file's own child module `sub`.
fn lift_super_out_of_inline_modules(path: String, nesting: usize) -> Option<String> {
	let mut path = path;
	for _ in 0..nesting {
		if path == "super" {
			return None;
		}
		match path.strip_prefix("super::") {
			Some(rest) => path = rest.to_string(),
			None => return Some(path),
		}
	}
	if nesting == 0 || path == "super" || path.starts_with("super::") {
		return Some(path);
	}
	// A bare name after consuming `super` is the file's own item.
	path.contains("::").then(|| format!("self::{path}"))
}

/// A path into the standard library (`std::error::Error`): its trait is
/// never one of the project's, even when the project declares a type with
/// the same final name.
fn is_standard_library_path(text: &str) -> bool {
	["std::", "core::", "alloc::", "::std::", "::core::", "::alloc::"]
		.iter()
		.any(|prefix| text.trim_start().starts_with(prefix))
}

/// Text of the node's `name` field.
fn declared_name(node: Node, contents: &str) -> Option<String> {
	node.child_by_field_name("name")
		.and_then(|name| name.utf8_text(contents.as_bytes()).ok())
		.map(str::to_string)
}

/// Prefix of the import string a `#[path = ".."] mod x;` emits, so
/// `resolve_import` reads the file name instead of searching for a module.
const PATH_ATTRIBUTE_PREFIX: &str = "pathattr::";

/// The `x.rs` of a `#[path = "x.rs"]` attribute directly before the node.
fn path_attribute(node: Node, contents: &str) -> Option<String> {
	let mut previous = node.prev_named_sibling();
	while let Some(sibling) = previous.filter(|sibling| sibling.kind() == "attribute_item") {
		let text = sibling.utf8_text(contents.as_bytes()).ok()?;
		if let Some((_, rest)) = text
			.split_once("path")
			.filter(|(before, _)| before.trim_end_matches(['#', '[', ' ']).is_empty())
		{
			let quoted = rest.split_once('=')?.1.trim().trim_end_matches(']').trim();
			return Some(quoted.trim_matches('"').to_string());
		}
		previous = sibling.prev_named_sibling();
	}
	None
}

/// Calls inside a macro's arguments (`assert_eq!(f(), 1)`, `vec![g()]`): the
/// grammar leaves them as tokens, where a name directly followed by a
/// parenthesised group reads as a call. A name after a `.` is a method call
/// on an unknown receiver.
fn calls_in_token_tree(node: Node, contents: &str) -> Vec<CallTarget> {
	let mut calls = Vec::new();
	let mut cursor = node.walk();
	// Asked of every token tree of a file: the children are walked once,
	// holding the two before the current one, not collected.
	let mut before: Option<Node> = None;
	let mut previous: Option<Node> = None;
	for child in node.children(&mut cursor) {
		if let Some(name_node) = previous.filter(|candidate| candidate.kind() == "identifier") {
			let opens_arguments = child.kind() == "token_tree"
				&& child
					.utf8_text(contents.as_bytes())
					.is_ok_and(|text| text.starts_with('('));
			if let (true, Ok(name)) = (opens_arguments, name_node.utf8_text(contents.as_bytes())) {
				let after_dot = before.is_some_and(|node| node.kind() == ".");
				calls.push(CallTarget {
					name: name.to_string(),
					qualifier: after_dot.then(|| UNKNOWN_RECEIVER.to_string()),
				});
			}
		}
		before = previous;
		previous = Some(child);
	}
	calls
}

/// Whether the item carries a `pub` visibility modifier (`pub`, `pub(crate)`).
fn is_public(node: Node, contents: &str) -> bool {
	let mut cursor = node.walk();
	let modifier = node
		.children(&mut cursor)
		.find(|child| child.kind() == "visibility_modifier");
	modifier
		.and_then(|modifier| modifier.utf8_text(contents.as_bytes()).ok())
		.is_some_and(|text| text.contains("pub"))
}

/// Directory part of a forward-slash path; empty for a bare file name.
fn directory_of(path: &str) -> &str {
	path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn join_path(directory: &str, relative: &str) -> String {
	if directory.is_empty() {
		relative.to_string()
	} else {
		format!("{directory}/{relative}")
	}
}

/// Expand a Rust `use` statement into one fully qualified path per leaf:
/// `use crate::a::{B, c::D};` gives `crate::a::B` and `crate::a::c::D`, a glob
/// (`utils::*`) gives the module itself and an alias (`Arc as StdArc`) is
/// dropped.
fn expand_rust_use_paths(use_text: &str) -> Vec<String> {
	// A re-export carries a visibility prefix (`pub use`, `pub(crate) use`);
	// stripping only "use " would leave it unmatched and drop the import. The
	// prefix only counts when `pub` is a whole token, so `use public::thing;`
	// is left alone.
	let trimmed = use_text.trim();
	let without_visibility = trimmed
		.strip_prefix("pub")
		.filter(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('('))
		.map(|rest| {
			let rest = rest.trim_start();
			match rest.strip_prefix('(') {
				Some(inner) => inner.split_once(')').map_or(rest, |(_, tail)| tail).trim_start(),
				None => rest,
			}
		})
		.unwrap_or(trimmed);
	let cleaned = match without_visibility.strip_prefix("use ") {
		Some(rest) => rest.trim_end_matches(';').trim(),
		None => return Vec::new(),
	};
	let mut results = Vec::new();
	expand_use_tree(cleaned, "", &mut results);
	results
}

/// Expand a use-tree fragment under the accumulated `prefix`. An explicit
/// stack instead of recursion: brace groups can nest as deep as the source
/// likes, and the leaves must keep their source order.
fn expand_use_tree(fragment: &str, prefix: &str, out: &mut Vec<String>) {
	let mut pending: Vec<(&str, String)> = vec![(fragment, prefix.to_string())];
	while let Some((fragment, prefix)) = pending.pop() {
		let fragment = fragment.trim();

		if let Some(brace_start) = fragment.find('{') {
			let before_brace = fragment[..brace_start].trim_end_matches(':');
			let new_prefix = if prefix.is_empty() {
				before_brace.to_string()
			} else {
				format!("{prefix}::{before_brace}")
			};
			let inner = &fragment[brace_start + 1..];
			if let Some(brace_end) = find_matching_brace(inner) {
				// Pushed in reverse so the first item is expanded first.
				for item in split_top_level_commas(&inner[..brace_end]).into_iter().rev() {
					pending.push((item.trim(), new_prefix.clone()));
				}
			}
		} else {
			let without_alias = fragment.find(" as ").map_or(fragment, |position| &fragment[..position]);
			let without_glob = without_alias.trim_end_matches('*').trim_end_matches("::");
			if without_glob.is_empty() {
				continue;
			}
			out.push(if prefix.is_empty() {
				without_glob.to_string()
			} else {
				format!("{prefix}::{without_glob}")
			});
		}
	}
}

/// Index of the `}` that closes the `{` already consumed.
fn find_matching_brace(text: &str) -> Option<usize> {
	let mut depth = 1usize;
	for (index, character) in text.char_indices() {
		match character {
			'{' => depth += 1,
			'}' => {
				depth -= 1;
				if depth == 0 {
					return Some(index);
				}
			}
			_ => {}
		}
	}
	None
}

/// Split on commas that are not inside a nested brace group.
fn split_top_level_commas(text: &str) -> Vec<&str> {
	let mut parts = Vec::new();
	let mut depth = 0usize;
	let mut start = 0;
	for (index, character) in text.char_indices() {
		match character {
			'{' => depth += 1,
			'}' => depth = depth.saturating_sub(1),
			',' if depth == 0 => {
				parts.push(&text[start..index]);
				start = index + 1;
			}
			_ => {}
		}
	}
	parts.push(&text[start..]);
	parts
}

/// The crate root (`lib.rs` or `main.rs`) in the source file's directory or
/// the nearest ancestor that has one.
fn find_crate_root(source_file: &str, registry: &FileRegistry) -> Option<String> {
	let mut directory = directory_of(source_file);
	loop {
		for root_file in ["lib.rs", "main.rs"] {
			if let Some(found) = registry.find_exact_file(&join_path(directory, root_file)) {
				return Some(found);
			}
		}
		if directory.is_empty() {
			return None;
		}
		directory = directory_of(directory);
	}
}

/// `use other_crate::module::Item` names another crate of the same
/// workspace: its root is the `src/lib.rs` of the directory whose name is
/// the crate's (hyphens in a package name become underscores). The manifest
/// is not read, so a package named differently from its directory is not
/// found.
fn resolve_workspace_crate_import(import_path: &str, registry: &FileRegistry) -> Option<String> {
	let (crate_name, module_path) = import_path.split_once("::")?;
	// Asked for every import that no file of the crate answers (the standard
	// library and every third-party crate), so nothing is allocated per root.
	let root = registry
		.files_named("lib.rs")
		.iter()
		.filter(|root| {
			let mut parents = root.rsplit('/').skip(1);
			parents.next() == Some("src")
				&& parents
					.next()
					.is_some_and(|directory| is_directory_of_crate(directory, crate_name))
		})
		.min()?;
	resolve_in_crate(module_path, root.to_string(), registry)
}

/// Whether a package directory is named `crate_name`: hyphens in the
/// directory stand for the underscores of the crate's name.
fn is_directory_of_crate(directory: &str, crate_name: &str) -> bool {
	directory.len() == crate_name.len()
		&& directory
			.chars()
			.zip(crate_name.chars())
			.all(|(written, named)| written == named || (written == '-' && named == '_'))
}

/// Resolve `crate::module::Item`: the deepest module path that names a file,
/// as `module.rs` or `module/mod.rs` under the crate root's directory.
fn resolve_crate_import(module_path: &str, source_file: &str, registry: &FileRegistry) -> Option<String> {
	let crate_root = find_crate_root(source_file, registry)?;
	resolve_in_crate(module_path, crate_root, registry)
}

/// The module path below the root file of one crate.
fn resolve_in_crate(module_path: &str, crate_root: String, registry: &FileRegistry) -> Option<String> {
	let parts: Vec<&str> = module_path.split("::").collect();
	let crate_dir = directory_of(&crate_root);

	for end in (1..=parts.len()).rev() {
		let module = parts[..end].join("/");
		if let Some(found) = registry.find_exact_file(&join_path(crate_dir, &format!("{module}.rs"))) {
			return Some(found);
		}
		if let Some(found) = registry.find_exact_file(&join_path(crate_dir, &format!("{module}/mod.rs"))) {
			return Some(found);
		}
	}

	// A single segment that matched no module file is an item declared at the
	// crate root, e.g. `crate::Url` for a `pub struct Url` in lib.rs. A longer
	// path that resolved nothing is a module we failed to find, and pointing
	// it at the root would invent an edge.
	if parts.len() == 1 {
		return Some(crate_root);
	}
	None
}

/// Directory that holds the child modules of the module `source_file` defines:
/// next to a module root (`mod.rs`, `lib.rs`, `main.rs`), inside a `<name>/`
/// directory for any other file.
fn module_directory(source_file: &str) -> String {
	let (directory, name) = source_file.rsplit_once('/').unwrap_or(("", source_file));
	if MODULE_ROOT_FILES.contains(&name) {
		directory.to_string()
	} else {
		join_path(directory, name.strip_suffix(".rs").unwrap_or(name))
	}
}

/// `super::` (any number of times) then a module path: each `super` moves to
/// the directory of the parent module's children. A lone item name that is no
/// child module (`super::Item`) lives in the parent module's own file.
fn resolve_super_import(import_path: &str, module_dir: &str, registry: &FileRegistry) -> Option<String> {
	let mut rest = import_path;
	let mut parent = module_dir;
	while let Some(after) = rest.strip_prefix("super::") {
		rest = after;
		parent = directory_of(parent);
	}
	resolve_relative_import(rest, parent, registry).or_else(|| {
		if rest.contains("::") {
			return None;
		}
		find_module_file(parent, registry)
	})
}

/// The file that defines the module whose children live in `module_dir`.
fn find_module_file(module_dir: &str, registry: &FileRegistry) -> Option<String> {
	let (parent, name) = module_dir.rsplit_once('/').unwrap_or(("", module_dir));
	[
		join_path(module_dir, "mod.rs"),
		join_path(parent, &format!("{name}.rs")),
		join_path(module_dir, "lib.rs"),
		join_path(module_dir, "main.rs"),
	]
	.iter()
	.find_map(|candidate| registry.find_exact_file(candidate))
}

/// Resolve a module path against `base_dir`, longest prefix first.
fn resolve_relative_import(module_path: &str, base_dir: &str, registry: &FileRegistry) -> Option<String> {
	let parts: Vec<&str> = module_path.split("::").collect();
	for end in (1..=parts.len()).rev() {
		let Some((last, leading)) = parts[..end].split_last() else {
			continue;
		};
		let directory = leading
			.iter()
			.fold(base_dir.to_string(), |directory, part| join_path(&directory, part));
		if let Some(found) = registry.find_exact_file(&join_path(&directory, &format!("{last}.rs"))) {
			return Some(found);
		}
		if let Some(found) = registry.find_exact_file(&join_path(&directory, &format!("{last}/mod.rs"))) {
			return Some(found);
		}
	}
	None
}

#[cfg(test)]
#[path = "rust_tests.rs"]
mod rust_tests;
