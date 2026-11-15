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

//! Symbol extraction and edge discovery: one tree-sitter parse per file yields
//! its declarations, imports, call sites and type relations; resolution then
//! turns those names into edges between symbols. No model, no index.

use super::registry::{heap_chunk, FileRegistry};
use super::types::{CodeRelationship, RelationType};
use crate::structural::languages::{
	parse_declarations, CallTarget, Language, PackageScope, SymbolKind, TypeRelationKind, QUALIFIER_SEPARATOR,
	SELF_RECEIVER,
};
use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

/// Finds the language implementation for a language name. A parameter instead
/// of a direct `get_language` call so tests can plug in a language that
/// resolves imports. `Sync` because files are parsed in parallel.
pub(crate) type LanguageLookup<'a> = &'a (dyn Fn(&str) -> Option<Box<dyn Language>> + Sync);

/// What names a package: the package a file declares, or the directory of a
/// file that declares none (its unnamed package). The two never meet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PackageName {
	Declared(String),
	Directory(String),
}

/// The package a file belongs to: its members see each other's top-level
/// names without importing them. A package never spans two languages.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
	language: String,
	name: PackageName,
}

/// Confidence of a fact read straight from the syntax (a symbol calling
/// itself, a file declaring a symbol, a resolved import).
pub(crate) const CONFIDENCE_CERTAIN: f32 = 1.0;
/// A call through an explicit import binding.
const CONFIDENCE_BOUND_IMPORT: f32 = 0.95;
/// `self.run()` / `Service::run()` matched to an owner declared in the same file.
const CONFIDENCE_OWNER_IN_FILE: f32 = 0.95;
/// A name declared exactly once in the source's own file.
const CONFIDENCE_SAME_FILE: f32 = 0.9;
/// `Service::run()` where exactly one project-wide owner declares `run`.
const CONFIDENCE_OWNER_UNIQUE: f32 = 0.9;
/// `module.run()` where `module` names an imported file.
const CONFIDENCE_MODULE_QUALIFIED: f32 = 0.9;
/// A name declared exactly once across the files the source imports.
const CONFIDENCE_IMPORTED_FILE: f32 = 0.85;
/// An owner-qualified name narrowed to a single imported file.
const CONFIDENCE_OWNER_IMPORTED: f32 = 0.85;
/// A name declared exactly once anywhere in the project.
const CONFIDENCE_GLOBAL_UNIQUE: f32 = 0.6;
/// Cap for a call on a receiver of unknown type: only the name matched.
const CONFIDENCE_UNKNOWN_RECEIVER_CAP: f32 = 0.65;
/// Highest confidence of an edge that rests on a name match alone (a
/// project-wide name, an ambiguous name, a call on an unknown receiver): the
/// rendering marks everything at or below it as a guess.
pub(crate) const CONFIDENCE_NAME_ONLY_MAX: f32 = CONFIDENCE_UNKNOWN_RECEIVER_CAP;
/// Several same-file declarations share the name.
const CONFIDENCE_SAME_FILE_AMBIGUOUS: f32 = 0.4;
/// Several imported-file declarations share the name.
const CONFIDENCE_IMPORTED_AMBIGUOUS: f32 = 0.45;
/// Several project-wide declarations share the name.
const CONFIDENCE_GLOBAL_AMBIGUOUS: f32 = 0.4;

/// A declared symbol extracted from one tree-sitter node.
#[derive(Debug, Clone)]
pub struct SymbolDecl {
	pub name: String,
	/// Nearest syntactic type/module that owns this declaration.
	pub owner: Option<String>,
	pub kind: SymbolKind,
	/// A function that builds an instance of its owning type.
	pub is_constructor: bool,
	/// 0-based start line of the declaration node.
	pub start_line: u32,
	/// 0-based end line of the declaration node.
	pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct TypeRelationDecl {
	/// 0-based line of the relation clause.
	pub line: u32,
	pub source_name: Option<String>,
	pub kind: TypeRelationKind,
	pub target_name: String,
}

#[derive(Debug, Clone)]
pub struct ImportBinding {
	/// Local identifier used at call sites.
	pub local_name: String,
	/// Original identifier in the imported file; None means a module namespace.
	pub imported_name: Option<String>,
	/// Import path consumed by the language's resolver.
	pub import_path: String,
}

/// Everything the graph builder needs from ONE tree-sitter parse of a file.
#[derive(Debug, Clone, Default)]
pub struct FileAstData {
	pub imports: Vec<String>,
	pub import_bindings: Vec<ImportBinding>,
	/// The modules the file's calls name inline: one binding per distinct
	/// qualifier, local name the qualifier as written. Not statements, so
	/// they never make an external node and bind no unqualified name.
	pub inline_imports: Vec<ImportBinding>,
	pub exports: Vec<String>,
	/// The name the file's default export declares, when it has one.
	pub default_export: Option<String>,
	/// The package the file declares, for a language with declared packages.
	pub declared_package: Option<String>,
	/// (0-based line, structured callee) pairs.
	pub calls: Vec<(u32, CallTarget)>,
	pub type_relations: Vec<TypeRelationDecl>,
	/// Declared symbols with line ranges, in source order.
	pub symbols: Vec<SymbolDecl>,
}

/// AST-derived relationship input for one source file. Source ranges stay
/// available until edge discovery so calls can be attributed to their owning
/// declaration.
#[derive(Debug, Clone)]
pub struct SymbolFileData {
	pub path: String,
	pub language: String,
	pub imports: Vec<String>,
	pub import_bindings: Vec<ImportBinding>,
	/// The modules the file's calls name inline, as in [`FileAstData`].
	pub inline_imports: Vec<ImportBinding>,
	pub calls: Vec<(u32, CallTarget)>,
	pub type_relations: Vec<TypeRelationDecl>,
	pub symbols: Vec<SymbolDecl>,
	/// The name the file's default export declares (`draw` in
	/// `export default function draw() {}`): a default import calls it under
	/// whatever name the importer chose.
	pub default_export: Option<String>,
	/// The package the file declares, for a language with declared packages.
	pub declared_package: Option<String>,
	/// What this file's imports resolved to, kept for as long as the file is.
	pub memo: ImportMemo,
}

/// The repository files each import string of one file resolved to, valid for
/// one registry. A different registry (files were added or removed) empties
/// it on the next use. Cloning gives an empty memo.
#[derive(Default)]
pub struct ImportMemo(Mutex<MemoState>);

#[derive(Default)]
struct MemoState {
	registry_id: u64,
	resolved: HashMap<String, Arc<Vec<String>>>,
}

/// Per entry: the map slot (key header, pointer, control byte) and the shared
/// vector's block, beyond the text blocks counted separately.
const MEMO_ENTRY_OVERHEAD_BYTES: usize = 33 + 48;

impl ImportMemo {
	fn state(&self) -> std::sync::MutexGuard<'_, MemoState> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn get(&self, registry_id: u64, import: &str) -> Option<Arc<Vec<String>>> {
		let state = self.state();
		if state.registry_id != registry_id {
			return None;
		}
		state.resolved.get(import).cloned()
	}

	fn insert(&self, registry_id: u64, import: &str, resolved: Arc<Vec<String>>) {
		let mut state = self.state();
		if state.registry_id != registry_id {
			state.registry_id = registry_id;
			state.resolved.clear();
		}
		state.resolved.insert(import.to_string(), resolved);
	}

	/// Memory held, for the index's accounting.
	pub fn bytes(&self) -> usize {
		self.state()
			.resolved
			.iter()
			.map(|(import, paths)| {
				MEMO_ENTRY_OVERHEAD_BYTES
					+ heap_chunk(import.capacity())
					+ heap_chunk(paths.capacity() * std::mem::size_of::<String>())
					+ paths.iter().map(|path| heap_chunk(path.capacity())).sum::<usize>()
			})
			.sum()
	}
}

impl Clone for ImportMemo {
	fn clone(&self) -> Self {
		Self::default()
	}
}

impl std::fmt::Debug for ImportMemo {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(formatter, "ImportMemo({} imports)", self.state().resolved.len())
	}
}

impl SymbolFileData {
	pub fn from_ast(path: String, language: String, mut data: FileAstData) -> Self {
		// Kept for as long as the file stays cached: no growth slack.
		data.imports.shrink_to_fit();
		data.import_bindings.shrink_to_fit();
		data.inline_imports.shrink_to_fit();
		data.calls.shrink_to_fit();
		data.type_relations.shrink_to_fit();
		data.symbols.shrink_to_fit();
		Self {
			path,
			language,
			imports: data.imports,
			import_bindings: data.import_bindings,
			inline_imports: data.inline_imports,
			calls: data.calls,
			type_relations: data.type_relations,
			symbols: data.symbols,
			default_export: data.default_export,
			declared_package: data.declared_package,
			memo: ImportMemo::default(),
		}
	}
}

/// The id of a declaration before any collision suffix: owned methods use
/// `{path}::{owner}::{name}` so `Service::run` and `Worker::run` stay
/// distinguishable.
fn declaration_base_id(path: &str, symbol: &SymbolDecl) -> String {
	if let Some(owner) = &symbol.owner {
		format!("{}::{}::{}", path, owner, symbol.name)
	} else {
		format!("{}::{}", path, symbol.name)
	}
}

/// The type a constructor builds when the declaration is not named after it
/// (a Swift `init`); a constructor named after its type is found by that name
/// already.
fn constructed_type(symbol: &SymbolDecl) -> Option<&str> {
	symbol
		.owner
		.as_deref()
		.filter(|owner| symbol.is_constructor && *owner != symbol.name)
}

/// Generate stable, human-readable symbol ids for one file. Only true
/// collisions (such as overloads on the same owner) receive a source-line
/// suffix on top of [`declaration_base_id`].
pub fn symbol_node_ids(path: &str, symbols: &[SymbolDecl]) -> Vec<String> {
	let mut counts: HashMap<(Option<&str>, &str), usize> = HashMap::new();
	for symbol in symbols {
		*counts
			.entry((symbol.owner.as_deref(), symbol.name.as_str()))
			.or_default() += 1;
	}

	symbols
		.iter()
		.map(|symbol| {
			let base = declaration_base_id(path, symbol);
			let collisions = counts
				.get(&(symbol.owner.as_deref(), symbol.name.as_str()))
				.copied()
				.unwrap_or(0);
			if collisions > 1 {
				format!("{}@{}", base, symbol.start_line + 1)
			} else {
				base
			}
		})
		.collect()
}

/// Indices into `symbols`, sorted by `start_line` (ties broken by original
/// index so backward scans in `find_owning_symbol` reproduce declaration
/// order), optionally restricted to functions.
fn symbol_start_order(symbols: &[SymbolDecl], functions_only: bool) -> Vec<usize> {
	let mut order: Vec<usize> = (0..symbols.len())
		.filter(|&index| !functions_only || symbols[index].kind == SymbolKind::Function)
		.collect();
	order.sort_by_key(|&index| (symbols[index].start_line, index));
	order
}

/// Find the innermost symbol (smallest `end_line - start_line` span) whose
/// range contains `line`, using an `order` produced by `symbol_start_order`.
/// Ties break toward the earlier-declared symbol (smaller original index).
fn find_owning_symbol(symbols: &[SymbolDecl], order: &[usize], line: u32) -> Option<usize> {
	// Everything at or after `pos` has start_line > line and can't contain it.
	let pos = order.partition_point(|&index| symbols[index].start_line <= line);
	let mut best: Option<(u32, usize)> = None;
	for &index in order[..pos].iter().rev() {
		let symbol = &symbols[index];
		if line <= symbol.end_line {
			let span = symbol.end_line.saturating_sub(symbol.start_line);
			let better = match best {
				Some((best_span, best_index)) => span < best_span || (span == best_span && index < best_index),
				None => true,
			};
			if better {
				best = Some((span, index));
			}
		}
		// Scanning backward, start_line only shrinks, so the smallest span any
		// earlier candidate could still achieve is `line - start_line`, which
		// only grows. Once it exceeds the best span, no earlier one can win.
		if let Some((best_span, _)) = best {
			if line.saturating_sub(symbol.start_line) > best_span {
				break;
			}
		}
	}
	best.map(|(_, index)| index)
}

/// The syntax tree of `contents`, and the text it was parsed from (see
/// `parse_declarations`).
fn parse_tree<'a>(contents: &'a str, language: &dyn Language) -> Result<(tree_sitter::Tree, Cow<'a, str>)> {
	let mut parser = tree_sitter::Parser::new();
	parser
		.set_language(&language.get_ts_language())
		.with_context(|| format!("tree-sitter grammar mismatch for {}", language.name()))?;
	parse_declarations(language, &mut parser, contents).ok_or_else(|| anyhow!("tree-sitter produced no syntax tree"))
}

/// Single-pass AST extraction: imports, exports, call sites, type relations
/// and symbol declarations from ONE parse of `contents`, including the code
/// regions the language embeds in other languages (a script block in a markup file).
pub(crate) fn extract_symbols_from_contents(
	contents: &str,
	language: &dyn Language,
	lookup: LanguageLookup,
) -> Result<FileAstData> {
	let (tree, declarations) = parse_tree(contents, language)?;
	let contents = declarations.as_ref();
	let root = tree.root_node();
	let mut data = FileAstData::default();
	walk_ast(root, contents, language, &language.get_symbol_kinds(), &mut data);

	for embedded in language.extract_embedded_sources(root, contents) {
		// A block that cannot be parsed costs only itself: the host file's own
		// symbols are already collected and still useful.
		let Some(embedded_language) = lookup(embedded.language) else {
			continue;
		};
		let Ok((embedded_tree, embedded_contents)) = parse_tree(&embedded.contents, embedded_language.as_ref()) else {
			continue;
		};
		let mut embedded_data = FileAstData::default();
		walk_ast(
			embedded_tree.root_node(),
			&embedded_contents,
			embedded_language.as_ref(),
			&embedded_language.get_symbol_kinds(),
			&mut embedded_data,
		);
		for (line, _) in &mut embedded_data.calls {
			*line += embedded.start_line;
		}
		for relation in &mut embedded_data.type_relations {
			relation.line += embedded.start_line;
		}
		for symbol in &mut embedded_data.symbols {
			symbol.start_line += embedded.start_line;
			symbol.end_line += embedded.start_line;
		}
		data.imports.extend(embedded_data.imports);
		data.import_bindings.extend(embedded_data.import_bindings);
		data.inline_imports.extend(embedded_data.inline_imports);
		data.exports.extend(embedded_data.exports);
		data.default_export = data.default_export.take().or(embedded_data.default_export);
		data.declared_package = data.declared_package.take().or(embedded_data.declared_package);
		data.calls.extend(embedded_data.calls);
		data.type_relations.extend(embedded_data.type_relations);
		data.symbols.extend(embedded_data.symbols);
	}
	Ok(data)
}

/// Pre-order walk over the syntax tree collecting every extractor's output per
/// node. Iterative on a cursor: deep nesting must not overflow the stack, and
/// the cursor avoids the quadratic `child(i)` lookups on wide nodes.
fn walk_ast(
	root: tree_sitter::Node,
	contents: &str,
	language: &dyn Language,
	symbol_kinds: &[&str],
	data: &mut FileAstData,
) {
	let mut cursor = root.walk();
	loop {
		// Keywords and punctuation declare nothing and call nothing: skipping
		// them halves the per-node extractor calls.
		if cursor.node().is_named() {
			visit_node(cursor.node(), contents, language, symbol_kinds, data);
		}
		if cursor.goto_first_child() {
			continue;
		}
		while !cursor.goto_next_sibling() {
			if !cursor.goto_parent() {
				return;
			}
		}
	}
}

fn visit_node(
	node: tree_sitter::Node,
	contents: &str,
	language: &dyn Language,
	symbol_kinds: &[&str],
	data: &mut FileAstData,
) {
	let (imports, exports) = language.extract_imports_exports(node, contents);
	if !imports.is_empty() {
		data.import_bindings
			.extend(language.extract_import_bindings(node, contents, &imports));
	}
	data.imports.extend(imports);
	data.exports.extend(exports);
	if let Some(name) = language.default_export_name(node, contents) {
		data.default_export = Some(name);
	}
	if data.declared_package.is_none() {
		data.declared_package = language.declared_package(node, contents);
	}

	let line = node.start_position().row as u32;
	for callee in language.extract_function_calls(node, contents) {
		if let Some(binding) = language.inline_import(node, contents, &callee) {
			if !data
				.inline_imports
				.iter()
				.any(|known| known.local_name == binding.local_name)
			{
				data.inline_imports.push(binding);
			}
		}
		data.calls.push((line, callee));
	}
	let type_relations = language.extract_type_relations(node, contents);
	if !type_relations.is_empty() {
		let source_name = language.extract_type_relation_source(node, contents);
		// The declaration's own line: its node starts at the first annotation,
		// decorator or modifier, which can sit lines above the name.
		let relation_line = ["name", "type"]
			.iter()
			.find_map(|field| node.child_by_field_name(field))
			.map_or(line, |name| name.start_position().row as u32);
		for (kind, target_name) in type_relations {
			data.type_relations.push(TypeRelationDecl {
				line: relation_line,
				source_name: source_name.clone(),
				kind,
				target_name,
			});
		}
	}

	if symbol_kinds.contains(&node.kind()) {
		if let Some(name) = language.extract_declaration_name(node, contents) {
			let name = name.trim().to_string();
			if !name.is_empty() {
				data.symbols.push(SymbolDecl {
					kind: language.symbol_kind(node, contents),
					is_constructor: language.is_constructor(node, contents),
					name,
					owner: language.extract_symbol_owner(node, contents),
					start_line: line,
					end_line: node.end_position().row as u32,
				});
			}
		}
	}
}

/// A resolved reference: the target symbol id and how sure the match is.
type Resolved<'a> = (&'a str, f32);

/// Files a lookup follows from one that only re-exports a name (a barrel
/// file that passes on what other files declare) before it gives up.
const MAX_REEXPORT_HOPS: usize = 3;

/// File path -> (symbol name -> symbol node ids).
type SymbolsByFile = HashMap<String, HashMap<String, Vec<String>>>;

/// A name a file takes in: its name in the other file and the files the
/// import resolved to.
type TakenName = (String, Arc<Vec<String>>);

/// The names one file takes in from other files, so a lookup that lands on it
/// can go on to the file that declares the name.
#[derive(Default)]
struct ReExports {
	/// Local name -> what it is taken from.
	named: HashMap<String, Vec<TakenName>>,
	/// Files a wildcard import (`from .x import *`, `export * from './x'`) names.
	wildcard: Vec<Arc<Vec<String>>>,
}

/// What a wildcard import binds instead of a name.
pub(crate) const WILDCARD: &str = "*";

/// Symbol lookup structures shared by symbol-edge discovery.
struct SymbolIndex {
	/// Symbol name -> symbol node ids declaring it (project-wide).
	by_name: HashMap<String, Vec<String>>,
	/// Owning file path -> (symbol name -> symbol node ids).
	by_file: HashMap<String, HashMap<String, Vec<String>>>,
	/// Owning type/module + symbol name -> symbol node ids.
	by_owner: HashMap<(String, String), Vec<String>>,
	/// File + owning type/module + symbol name -> symbol node ids.
	by_file_owner: HashMap<(String, String, String), Vec<String>>,
	/// File path -> symbol node ids in `file.symbols` order, computed once so
	/// callers iterating the same files never recompute `symbol_node_ids`.
	by_file_symbol_ids: HashMap<String, Vec<String>>,
	/// Owning file path -> (symbol name -> ids of the symbols without an
	/// owning type: free functions and top-level declarations).
	free_by_file: HashMap<String, HashMap<String, Vec<String>>>,
	/// Same shape for the symbols that have an owning type (methods).
	method_by_file: HashMap<String, HashMap<String, Vec<String>>>,
	/// Symbol name -> the files declaring it, so a lookup visits those and not
	/// every file the caller imports.
	files_by_name: HashMap<String, Vec<String>>,
	/// File path -> what it brings in from other files, for the files that
	/// import names or everything of another file.
	reexports: HashMap<String, ReExports>,
	/// File path -> the name its default export declares.
	default_exports: HashMap<String, String>,
	/// File path -> the package it belongs to, for the files of a language
	/// with packages.
	packages: HashMap<String, Package>,
	/// Ids of declarations that share owner and name with another one in the
	/// file (overloads, the two sides of a conditional compilation) -> the id
	/// they would have without the collision suffix. Declarations with a
	/// common base are one declaration to a call.
	redeclared: HashMap<String, String>,
	/// Ids of constructors.
	constructors: HashSet<String>,
}

impl SymbolIndex {
	fn build(files: &[&SymbolFileData]) -> Self {
		let mut index = SymbolIndex {
			by_name: HashMap::new(),
			by_file: HashMap::new(),
			by_owner: HashMap::new(),
			by_file_owner: HashMap::new(),
			by_file_symbol_ids: HashMap::new(),
			free_by_file: HashMap::new(),
			method_by_file: HashMap::new(),
			files_by_name: HashMap::new(),
			reexports: HashMap::new(),
			default_exports: HashMap::new(),
			packages: HashMap::new(),
			redeclared: HashMap::new(),
			constructors: HashSet::new(),
		};
		for file in files {
			if let Some(name) = &file.default_export {
				index.default_exports.insert(file.path.clone(), name.clone());
			}
			let ids = symbol_node_ids(&file.path, &file.symbols);
			for (symbol, id) in file.symbols.iter().zip(ids.iter()) {
				index.by_name.entry(symbol.name.clone()).or_default().push(id.clone());
				let base = declaration_base_id(&file.path, symbol);
				if *id != base {
					index.redeclared.insert(id.clone(), base);
				}
				if symbol.is_constructor {
					index.constructors.insert(id.clone());
				}
				// A call that names a type builds it through its constructor,
				// whatever the language calls that declaration (`init`): the
				// type's name reaches the constructor as well.
				if let Some(type_name) = constructed_type(symbol) {
					index.by_name.entry(type_name.to_string()).or_default().push(id.clone());
					index
						.by_file
						.entry(file.path.clone())
						.or_default()
						.entry(type_name.to_string())
						.or_default()
						.push(id.clone());
				}
				index
					.by_file
					.entry(file.path.clone())
					.or_default()
					.entry(symbol.name.clone())
					.or_default()
					.push(id.clone());
				let by_kind = if symbol.owner.is_none() {
					&mut index.free_by_file
				} else {
					&mut index.method_by_file
				};
				by_kind
					.entry(file.path.clone())
					.or_default()
					.entry(symbol.name.clone())
					.or_default()
					.push(id.clone());
				if let Some(owner) = &symbol.owner {
					index
						.by_owner
						.entry((owner.clone(), symbol.name.clone()))
						.or_default()
						.push(id.clone());
					index
						.by_file_owner
						.entry((file.path.clone(), owner.clone(), symbol.name.clone()))
						.or_default()
						.push(id.clone());
				}
			}
			index.by_file_symbol_ids.insert(file.path.clone(), ids);
			let mut names: Vec<&str> = file
				.symbols
				.iter()
				.flat_map(|symbol| std::iter::once(symbol.name.as_str()).chain(constructed_type(symbol)))
				.collect();
			names.sort_unstable();
			names.dedup();
			for name in names {
				index
					.files_by_name
					.entry(name.to_string())
					.or_default()
					.push(file.path.clone());
			}
		}
		index
	}

	/// The declaration an id belongs to: declarations that collide on owner
	/// and name share one.
	fn declaration_of<'a>(&'a self, id: &'a str) -> &'a str {
		self.redeclared.get(id).map_or(id, String::as_str)
	}

	/// Whether `ids` are one type and its constructors: a call that names the
	/// type has the type and every constructor as targets, and all of them are
	/// the call's facts, not an ambiguity.
	fn is_type_with_constructors<S: AsRef<str>>(&self, ids: &[S]) -> bool {
		let types = ids.iter().filter(|id| !self.constructors.contains(id.as_ref())).count();
		types == 1 && ids.len() > 1
	}

	/// What each file takes in from other files, for [`Self::declared_or_reexported`].
	fn set_reexports(&mut self, files: &[&SymbolFileData], resolver: &mut ImportResolver) {
		for file in files {
			if !resolver.has_language(&file.language) {
				continue;
			}
			let mut reexports = ReExports::default();
			for binding in &file.import_bindings {
				let Some(imported) = binding.imported_name.as_deref() else {
					continue;
				};
				let resolved = resolver.resolve_for(file, &binding.import_path);
				if resolved.is_empty() {
					continue;
				}
				if binding.local_name == WILDCARD {
					reexports.wildcard.push(resolved);
				} else {
					reexports
						.named
						.entry(binding.local_name.clone())
						.or_default()
						.push((imported.to_string(), resolved));
				}
			}
			if !reexports.named.is_empty() || !reexports.wildcard.is_empty() {
				self.reexports.insert(file.path.clone(), reexports);
			}
		}
	}

	/// The symbols `name` is in `path`, looked up in `declarations` (every
	/// symbol of a file, or only its free functions): its own declarations and
	/// what the files it takes the name from declare (a barrel file that only
	/// passes names on, or one that declares a variant of its own).
	fn declared_or_reexported<'a>(
		&'a self,
		declarations: &'a SymbolsByFile,
		path: &str,
		name: &str,
	) -> Vec<&'a String> {
		let mut found = Vec::new();
		let mut seen = HashSet::new();
		self.collect_declared(declarations, path, name, MAX_REEXPORT_HOPS, &mut seen, &mut found);
		found
	}

	fn collect_declared<'a>(
		&'a self,
		declarations: &'a SymbolsByFile,
		path: &str,
		name: &str,
		hops: usize,
		seen: &mut HashSet<(String, String)>,
		found: &mut Vec<&'a String>,
	) {
		if !seen.insert((path.to_string(), name.to_string())) {
			return;
		}
		// A file may declare a name and also re-export it: the two sit under
		// opposite `cfg` attributes (a stub next to `pub(crate) use real::f`).
		// Both are candidates.
		if let Some(ids) = declarations.get(path).and_then(|symbols| symbols.get(name)) {
			found.extend(ids);
		}
		let Some(reexports) = self.reexports.get(path).filter(|_| hops > 0) else {
			return;
		};
		for (imported, files) in reexports.named.get(name).into_iter().flatten() {
			for file in files.iter() {
				self.collect_declared(declarations, file, imported, hops - 1, seen, found);
			}
		}
		for files in &reexports.wildcard {
			for file in files.iter() {
				self.collect_declared(declarations, file, name, hops - 1, seen, found);
			}
		}
	}

	/// The imported files that declare `name`: only the declaring files are
	/// visited, not every imported file (a package import names hundreds).
	fn imported_declaring<'a>(&'a self, imported: &'a ImportedFiles, name: &str) -> impl Iterator<Item = &'a str> + 'a {
		self.files_by_name
			.get(name)
			.into_iter()
			.flatten()
			.map(String::as_str)
			.filter(move |file| imported.set.contains(*file))
	}

	/// The declarations of `name` in the other files of the package of
	/// `source_path`: a package sees its own names without an import.
	fn same_package_targets(&self, source_path: &str, name: &str) -> Vec<&String> {
		let Some(package) = self.packages.get(source_path) else {
			return Vec::new();
		};
		self.files_by_name
			.get(name)
			.into_iter()
			.flatten()
			.filter(|file| file.as_str() != source_path && self.packages.get(file.as_str()) == Some(package))
			.filter_map(|file| self.by_file.get(file.as_str())?.get(name))
			.flatten()
			.collect()
	}

	/// Records the package of every file whose language has packages.
	fn set_packages(&mut self, files: &[&SymbolFileData], resolver: &ImportResolver) {
		for file in files {
			if let Some(package) = resolver.package_of(file) {
				self.packages.insert(file.path.clone(), package);
			}
		}
	}

	/// Resolve a referenced name to symbol node ids, scoped: same file first,
	/// then files the source imports, then a unique global declaration.
	fn resolve(
		&self,
		source_path: &str,
		target_name: &str,
		imported: &ImportedFiles,
		allow_ambiguous: bool,
	) -> Vec<Resolved<'_>> {
		// 1. Same file: a direct AST fact.
		if let Some(targets) = self.by_file.get(source_path).and_then(|names| names.get(target_name)) {
			if targets.len() == 1 || self.is_type_with_constructors(targets) {
				return targets.iter().map(|id| (id.as_str(), CONFIDENCE_SAME_FILE)).collect();
			}
			// The same declaration twice (overloads, the two sides of a
			// conditional compilation: one owner and name, so the ids carry `@line`) is one
			// direct fact: a call reaches each. Different owners keep the
			// ambiguity rule.
			let first = self.declaration_of(&targets[0]);
			if targets.iter().all(|id| self.declaration_of(id) == first) {
				return targets
					.iter()
					.map(|id| (id.as_str(), CONFIDENCE_SAME_FILE_AMBIGUOUS))
					.collect();
			}
			if allow_ambiguous {
				return targets
					.iter()
					.map(|id| (id.as_str(), CONFIDENCE_SAME_FILE_AMBIGUOUS))
					.collect();
			}
			return Vec::new();
		}

		// 1b. Same package: the files of one package see each other's names
		// without an import.
		let mut package_targets = self.same_package_targets(source_path, target_name);
		package_targets.sort_unstable();
		package_targets.dedup();
		if package_targets.len() == 1 || self.is_type_with_constructors(&package_targets) {
			return package_targets
				.into_iter()
				.map(|id| (id.as_str(), CONFIDENCE_IMPORTED_FILE))
				.collect();
		}

		// 2. Files the source imports declare the target. Collect across every
		// import before deciding: returning the first match makes import order
		// silently choose between two equally plausible declarations.
		let mut imported_targets = Vec::new();
		for declaring in self.imported_declaring(imported, target_name) {
			if let Some(targets) = self.by_file.get(declaring).and_then(|names| names.get(target_name)) {
				imported_targets.extend(targets);
			}
		}
		if imported_targets.is_empty() {
			// An imported file that only re-exports the name leads to its declaration.
			for path in &imported.list {
				let takes_the_name = self.reexports.get(path).is_some_and(|reexports| {
					reexports.named.contains_key(target_name) || !reexports.wildcard.is_empty()
				});
				if takes_the_name {
					imported_targets.extend(self.declared_or_reexported(&self.by_file, path, target_name));
				}
			}
		}
		imported_targets.sort_unstable();
		imported_targets.dedup();
		if imported_targets.len() == 1 || self.is_type_with_constructors(&imported_targets) {
			return imported_targets
				.into_iter()
				.map(|id| (id.as_str(), CONFIDENCE_IMPORTED_FILE))
				.collect();
		}
		if !imported_targets.is_empty() {
			if allow_ambiguous {
				return imported_targets
					.into_iter()
					.map(|id| (id.as_str(), CONFIDENCE_IMPORTED_AMBIGUOUS))
					.collect();
			}
			return Vec::new();
		}

		// 3. Unique global declaration.
		match self.by_name.get(target_name) {
			Some(ids) if ids.len() == 1 || self.is_type_with_constructors(ids) => {
				ids.iter().map(|id| (id.as_str(), CONFIDENCE_GLOBAL_UNIQUE)).collect()
			}
			// Multiple candidates: only type relations (extends/implements)
			// record them. Call sites hit ubiquitous method names (`new`,
			// `get`) and would balloon the edge set with noise.
			Some(ids) if allow_ambiguous => ids
				.iter()
				.map(|id| (id.as_str(), CONFIDENCE_GLOBAL_AMBIGUOUS))
				.collect(),
			_ => Vec::new(),
		}
	}

	/// The symbols the import bindings of the file give the callee: a binding
	/// that names it, or that names its module or type, looked up in
	/// `declarations`. Sorted, without duplicates.
	fn bound_targets<'a>(
		&'a self,
		target: &CallTarget,
		bindings: &[(ImportBinding, String)],
		declarations: &'a SymbolsByFile,
	) -> Vec<&'a String> {
		let mut bound_targets = Vec::new();
		for (binding, path) in bindings {
			if target.qualifier.is_none() && binding.local_name == target.name {
				let imported_name = binding.imported_name.as_deref().unwrap_or(&target.name);
				let mut declared = self.declared_or_reexported(declarations, path, imported_name);
				// A default import takes whatever name the importer chose; the
				// file's default export is the declaration it names.
				if declared.is_empty() && binding.imported_name.is_none() {
					if let Some(default_name) = self.default_exports.get(path) {
						declared = self.declared_or_reexported(declarations, path, default_name);
					}
				}
				bound_targets.extend(declared);
			}
			if target.qualifier.as_deref() == Some(binding.local_name.as_str()) {
				if let Some(owner) = &binding.imported_name {
					if let Some(ids) = self
						.by_file_owner
						.get(&(path.clone(), owner.clone(), target.name.clone()))
					{
						bound_targets.extend(ids);
					}
				} else {
					bound_targets.extend(self.declared_or_reexported(declarations, path, &target.name));
				}
			}
		}
		bound_targets.sort_unstable();
		bound_targets.dedup();
		bound_targets
	}

	fn resolve_call(
		&self,
		source_path: &str,
		source_owner: Option<&str>,
		target: &CallTarget,
		bindings: &[(ImportBinding, String)],
		imported: &ImportedFiles,
	) -> Vec<Resolved<'_>> {
		let bound_targets = self.bound_targets(target, bindings, &self.by_file);
		if bound_targets.len() == 1 || self.is_type_with_constructors(&bound_targets) {
			return bound_targets
				.into_iter()
				.map(|id| (id.as_str(), CONFIDENCE_BOUND_IMPORT))
				.collect();
		}
		if bound_targets.len() > 1 {
			// A bare or module-qualified call names a free function: a method
			// that shares its name (`Spawner::spawn_blocking` next to
			// `spawn_blocking`) is reached through a receiver. Several free
			// functions are alternatives the import can bind to (builds under
			// opposite `cfg`): the caller is listed for each.
			return self
				.bound_targets(target, bindings, &self.free_by_file)
				.into_iter()
				.map(|id| (id.as_str(), CONFIDENCE_BOUND_IMPORT))
				.collect();
		}

		let Some(qualifier) = target.qualifier.as_deref() else {
			return self.resolve(source_path, &target.name, imported, false);
		};
		let qualifier_leaf = qualifier.rsplit(QUALIFIER_SEPARATOR).next().unwrap_or(qualifier);

		if qualifier == SELF_RECEIVER {
			if let Some(owner) = source_owner {
				if let Some(ids) =
					self.by_file_owner
						.get(&(source_path.to_string(), owner.to_string(), target.name.clone()))
				{
					if ids.len() == 1 {
						return vec![(ids[0].as_str(), CONFIDENCE_OWNER_IN_FILE)];
					}
				}
			}
			return Vec::new();
		}

		// Static/type-qualified call (`Service::run`, `Service.run`).
		if let Some(ids) =
			self.by_file_owner
				.get(&(source_path.to_string(), qualifier_leaf.to_string(), target.name.clone()))
		{
			if ids.len() == 1 {
				return vec![(ids[0].as_str(), CONFIDENCE_OWNER_IN_FILE)];
			}
		}
		if let Some(ids) = self.by_owner.get(&(qualifier_leaf.to_string(), target.name.clone())) {
			if ids.len() == 1 {
				return vec![(ids[0].as_str(), CONFIDENCE_OWNER_UNIQUE)];
			}
			let imported: Vec<_> = ids
				.iter()
				.filter(|id| imported.list.iter().any(|path| id.starts_with(path.as_str())))
				.collect();
			if imported.len() == 1 {
				return vec![(imported[0].as_str(), CONFIDENCE_OWNER_IMPORTED)];
			}
		}

		// Module-qualified call: constrain lookup to the matching imported file.
		let named_files = imported.by_qualifier.get(qualifier_leaf);
		for imported_file in named_files.into_iter().flatten() {
			if let Some(ids) = self
				.by_file
				.get(imported_file)
				.and_then(|symbols| symbols.get(&target.name))
			{
				if ids.len() == 1 {
					return vec![(ids[0].as_str(), CONFIDENCE_MODULE_QUALIFIED)];
				}
			}
		}
		// A call qualified by an import's name reaches a free function of the
		// imported files (`http.ListenAndServe`), never a method that happens
		// to share the name (`Server.ListenAndServe`).
		if named_files.is_some() {
			let free_targets: Vec<&String> = self
				.imported_declaring(imported, &target.name)
				.filter_map(|imported| self.free_by_file.get(imported)?.get(&target.name))
				.flatten()
				.collect();
			if let [only] = free_targets.as_slice() {
				return vec![(only.as_str(), CONFIDENCE_MODULE_QUALIFIED)];
			}
		}
		// Unknown instance receiver: retain only a uniquely resolvable scoped
		// target and cap its confidence; never fan out ubiquitous method names.
		let scoped = self.resolve(source_path, &target.name, imported, false);
		if !scoped.is_empty() {
			return scoped
				.into_iter()
				.map(|(id, confidence)| (id, confidence.min(CONFIDENCE_UNKNOWN_RECEIVER_CAP)))
				.collect();
		}
		// A free function and a method sharing the name (`ListenAndServe` and
		// `Server.ListenAndServe`) made the name ambiguous; a call on a receiver
		// is the method.
		let method_in = |file: &str| self.method_by_file.get(file).and_then(|names| names.get(&target.name));
		let methods: Vec<&String> = match method_in(source_path) {
			Some(here) => here.iter().collect(),
			None => self
				.imported_declaring(imported, &target.name)
				.filter_map(method_in)
				.flatten()
				.collect(),
		};
		match methods.as_slice() {
			[only] => vec![(only.as_str(), CONFIDENCE_UNKNOWN_RECEIVER_CAP)],
			_ => Vec::new(),
		}
	}
}

/// File name without directory or final extension (`src/utils.ext` -> `utils`).
fn file_stem(path: &str) -> &str {
	let file_name = path.rsplit('/').next().unwrap_or(path);
	match file_name.rsplit_once('.') {
		Some((stem, _)) if !stem.is_empty() => stem,
		_ => file_name,
	}
}

/// Resolves import strings to repository files, once per (source file,
/// import string) for the whole build: the import-edge pass, the call pass and
/// the binding pass all ask for the same imports, and a resolver can cost a
/// directory scan.
pub(crate) struct ImportResolver<'a> {
	registry: Arc<FileRegistry>,
	lookup: LanguageLookup<'a>,
	cache: HashMap<(String, String), Arc<Vec<String>>>,
}

impl<'a> ImportResolver<'a> {
	/// A resolver over the given paths, for tests that build a registry per call.
	#[cfg(test)]
	pub(crate) fn new(paths: &[String], lookup: LanguageLookup<'a>) -> Self {
		Self::with_registry(Arc::new(FileRegistry::new(paths)), lookup)
	}

	/// A resolver over a registry built once and shared by many builds.
	pub(crate) fn with_registry(registry: Arc<FileRegistry>, lookup: LanguageLookup<'a>) -> Self {
		Self {
			registry,
			lookup,
			cache: HashMap::new(),
		}
	}

	pub(crate) fn has_language(&self, language_name: &str) -> bool {
		(self.lookup)(language_name).is_some()
	}

	fn import_name_separators(&self, language_name: &str) -> &'static [char] {
		(self.lookup)(language_name).map_or(&[] as &[char], |language| language.import_name_separators())
	}

	/// The package `file` belongs to, with the language in it: files of two
	/// languages never share one.
	fn package_of(&self, file: &SymbolFileData) -> Option<Package> {
		let directory = || {
			PackageName::Directory(
				file.path
					.rsplit_once('/')
					.map_or("", |(directory, _)| directory)
					.to_string(),
			)
		};
		let name = match (self.lookup)(&file.language)?.package_scope() {
			PackageScope::NoPackage => return None,
			PackageScope::Directory => directory(),
			PackageScope::Declared => file
				.declared_package
				.clone()
				.map_or_else(directory, PackageName::Declared),
		};
		Some(Package {
			language: file.language.clone(),
			name,
		})
	}

	/// The repository files `import` names from `source_file`: one for most
	/// languages, a whole package for imports that name one,
	/// none for an external module or a language without an implementation.
	pub(crate) fn resolve(&mut self, language_name: &str, source_file: &str, import: &str) -> Arc<Vec<String>> {
		let key = (source_file.to_string(), import.to_string());
		if let Some(resolved) = self.cache.get(&key) {
			return resolved.clone();
		}
		let resolved = Arc::new(
			(self.lookup)(language_name)
				.map(|language| language.resolve_import_files(import, source_file, &self.registry))
				.unwrap_or_default(),
		);
		self.cache.insert(key, resolved.clone());
		resolved
	}

	/// [`Self::resolve`] for an import of `file`, memoized on the file: later
	/// builds over the same registry do not resolve it again.
	pub(crate) fn resolve_for(&mut self, file: &SymbolFileData, import: &str) -> Arc<Vec<String>> {
		let registry_id = self.registry.id();
		if let Some(resolved) = file.memo.get(registry_id, import) {
			return resolved;
		}
		let resolved = self.resolve(&file.language, &file.path, import);
		file.memo.insert(registry_id, import, resolved.clone());
		resolved
	}

	/// Whether `import` of `file` is only a candidate that is dropped when it
	/// names no file.
	pub(crate) fn is_optional(&self, file: &SymbolFileData, import: &str) -> bool {
		(self.lookup)(&file.language).is_some_and(|language| language.import_is_optional(import))
	}

	/// Whether `import` of `file` resolves by a guess rather than a stated path.
	pub(crate) fn is_guess(&self, file: &SymbolFileData, import: &str) -> bool {
		(self.lookup)(&file.language)
			.is_some_and(|language| language.import_is_guess(import, &file.path, &self.registry))
	}
}

/// The part of a build's call and type relations an answer reads. A query
/// follows a few names (callers, subtypes) or the bodies of a few definitions
/// (callees, supertypes); resolving every call of every examined file would
/// cost far more than the answer needs.
#[derive(Default)]
pub struct RelationFilter {
	/// Callee and parent-type names to resolve, wherever they are written.
	pub names: HashSet<String>,
	/// File path -> `(first, last)` 1-based lines of the definitions whose
	/// bodies are resolved in full.
	pub bodies: HashMap<String, Vec<(u32, u32)>>,
	/// Types whose parent clauses are resolved wherever they are written.
	pub declared_types: HashSet<String>,
}

impl RelationFilter {
	fn in_body(&self, path: &str, line: u32) -> bool {
		// `line` is 0-based, node lines are 1-based.
		self.bodies
			.get(path)
			.is_some_and(|ranges| ranges.iter().any(|(first, last)| line + 1 >= *first && line < *last))
	}

	/// Whether the file has anything the filter asks for.
	fn touches(&self, file: &SymbolFileData) -> bool {
		self.bodies.contains_key(&file.path)
			|| file
				.calls
				.iter()
				.any(|(line, callee)| self.keeps_call(file, *line, callee))
			|| file
				.type_relations
				.iter()
				.any(|relation| self.keeps_type_relation(file, relation))
	}

	fn keeps_call(&self, file: &SymbolFileData, line: u32, callee: &CallTarget) -> bool {
		self.names.contains(&callee.name)
			|| self.in_body(&file.path, line)
			// A call through an import alias spells the alias, and a default
			// import names nothing: both can reach a definition of a wanted name.
			|| file.import_bindings.iter().any(|binding| {
				binding.local_name == callee.name
					&& binding.imported_name.as_ref().is_none_or(|imported| self.names.contains(imported))
			})
	}

	fn keeps_type_relation(&self, file: &SymbolFileData, relation: &TypeRelationDecl) -> bool {
		self.names.contains(&relation.target_name)
			|| self.in_body(&file.path, relation.line)
			// `impl Shape for Square` sits outside the body of `Square`.
			|| relation.source_name.as_ref().is_some_and(|source| self.declared_types.contains(source))
	}
}

/// Discover symbol-to-symbol edges (calls / extends / implements) for the
/// given files. Pure tree-sitter output plus name resolution. A call outside
/// any function (a script body, module scope) is attributed to its file node.
pub(crate) fn discover_symbol_relationships(
	files: &[&SymbolFileData],
	resolver: &mut ImportResolver,
	filter: Option<&RelationFilter>,
) -> Vec<CodeRelationship> {
	let mut index = SymbolIndex::build(files);
	index.set_reexports(files, resolver);
	index.set_packages(files, resolver);

	// Import resolution memoizes through `&mut resolver`, so it runs first and
	// serially (cheap: one lookup per distinct import); the per-call matching
	// that dominates the cost then runs on every file in parallel.
	let resolutions: Vec<Option<FileResolution>> = files
		.iter()
		.map(|source_file| {
			if !resolver.has_language(&source_file.language)
				|| filter.is_some_and(|filter| !filter.touches(source_file))
			{
				return None;
			}
			let mut resolved_imports: Vec<(String, String)> = Vec::new();
			for import in &source_file.imports {
				for path in resolver.resolve_for(source_file, import).iter() {
					resolved_imports.push((import.clone(), path.clone()));
				}
			}
			let mut resolved_bindings: Vec<(ImportBinding, String)> = Vec::new();
			for binding in source_file.import_bindings.iter().chain(&source_file.inline_imports) {
				for path in resolver.resolve_for(source_file, &binding.import_path).iter() {
					resolved_bindings.push((binding.clone(), path.clone()));
				}
			}
			Some(FileResolution {
				imported: ImportedFiles::new(
					&resolved_imports,
					resolver.import_name_separators(&source_file.language),
				),
				resolved_bindings,
			})
		})
		.collect();

	let mut relationships: Vec<CodeRelationship> = files
		.par_iter()
		.zip(resolutions.par_iter())
		.flat_map_iter(|(source_file, resolution)| match resolution {
			Some(resolution) => discover_file_relationships(source_file, resolution, &index, filter),
			None => Vec::new(),
		})
		.collect();

	sort_and_dedup(&mut relationships);
	relationships
}

/// Imports of one file with the repository files they resolved to.
struct FileResolution {
	resolved_bindings: Vec<(ImportBinding, String)>,
	imported: ImportedFiles,
}

/// The files one file imports, in the shapes a call lookup needs.
struct ImportedFiles {
	/// Sorted, without duplicates.
	list: Vec<String>,
	set: HashSet<String>,
	/// What a qualifier of a call can name an imported file by (the last
	/// part of the import string, or the file's stem) -> the files, in import
	/// order.
	by_qualifier: HashMap<String, Vec<String>>,
}

impl ImportedFiles {
	fn new(resolved_imports: &[(String, String)], name_separators: &[char]) -> Self {
		let mut list: Vec<String> = resolved_imports.iter().map(|(_, path)| path.clone()).collect();
		list.sort_unstable();
		list.dedup();
		let mut by_qualifier: HashMap<String, Vec<String>> = HashMap::new();
		for (raw_import, imported_file) in resolved_imports {
			let leaf = import_leaf(raw_import, name_separators);
			by_qualifier
				.entry(leaf.to_string())
				.or_default()
				.push(imported_file.clone());
			let stem = file_stem(imported_file);
			if stem != leaf {
				by_qualifier
					.entry(stem.to_string())
					.or_default()
					.push(imported_file.clone());
			}
		}
		Self {
			set: list.iter().cloned().collect(),
			list,
			by_qualifier,
		}
	}
}

/// The last segment of an import, split at the separators its language writes.
fn import_leaf<'a>(raw_import: &'a str, name_separators: &[char]) -> &'a str {
	raw_import
		.rsplit(name_separators)
		.next()
		.unwrap_or(raw_import)
		.trim_matches(|character: char| !character.is_alphanumeric() && character != '_')
}

/// Call and type-relation edges of one file.
fn discover_file_relationships(
	source_file: &SymbolFileData,
	resolution: &FileResolution,
	index: &SymbolIndex,
	filter: Option<&RelationFilter>,
) -> Vec<CodeRelationship> {
	let FileResolution {
		resolved_bindings,
		imported,
	} = resolution;
	let mut relationships = Vec::new();
	// Built from the same `files`, so the entry always exists.
	let Some(symbol_ids) = index.by_file_symbol_ids.get(&source_file.path) else {
		return relationships;
	};

	// Symbol indices sorted by start_line so `owning_symbol` can
	// binary-search and scan backward for the innermost containing
	// declaration instead of scanning every symbol per call site.
	let symbols_by_start = symbol_start_order(&source_file.symbols, false);
	let functions_by_start = symbol_start_order(&source_file.symbols, true);

	let owning_symbol = |line: u32, functions_only: bool| {
		let order = if functions_only {
			&functions_by_start
		} else {
			&symbols_by_start
		};
		find_owning_symbol(&source_file.symbols, order, line)
			.map(|position| (symbol_ids[position].as_str(), &source_file.symbols[position]))
	};

	for (line, callee) in &source_file.calls {
		if filter.is_some_and(|filter| !filter.keeps_call(source_file, *line, callee)) {
			continue;
		}
		let (source_id, source_symbol) = match owning_symbol(*line, true) {
			Some((id, symbol)) => (id, Some(symbol)),
			None => (source_file.path.as_str(), None),
		};
		let call_line = Some(*line + 1);
		if let Some(symbol) = source_symbol {
			if callee.qualifier.is_none() && callee.name == symbol.name {
				relationships.push(CodeRelationship {
					source: source_id.to_string(),
					target: source_id.to_string(),
					relation: RelationType::Calls,
					confidence: CONFIDENCE_CERTAIN,
					line: call_line,
				});
				continue;
			}
		}
		for (target, confidence) in index.resolve_call(
			&source_file.path,
			source_symbol.and_then(|symbol| symbol.owner.as_deref()),
			callee,
			resolved_bindings,
			imported,
		) {
			relationships.push(CodeRelationship {
				source: source_id.to_string(),
				target: target.to_string(),
				relation: RelationType::Calls,
				confidence,
				line: call_line,
			});
		}
	}

	for relation in &source_file.type_relations {
		if filter.is_some_and(|filter| !filter.keeps_type_relation(source_file, relation)) {
			continue;
		}
		// The declared type wins over whatever symbol encloses the clause:
		// `impl Trait for Foo` inside `mod x {}`, or in another file than
		// `struct Foo`, still belongs to Foo.
		let declared = relation.source_name.as_deref().and_then(|name| {
			index
				.resolve(&source_file.path, name, imported, false)
				.into_iter()
				.next()
				.map(|(id, _)| id)
		});
		let Some(source_id) = declared.or_else(|| owning_symbol(relation.line, false).map(|(id, _)| id)) else {
			continue;
		};
		let relation_type = match relation.kind {
			TypeRelationKind::Extends => RelationType::Extends,
			TypeRelationKind::Implements => RelationType::Implements,
		};
		for (target, confidence) in index.resolve(&source_file.path, &relation.target_name, imported, true) {
			// A parent that resolves to the type itself is a same-named
			// outside type (`impl std::error::Error for Error`).
			if target == source_id {
				continue;
			}
			relationships.push(CodeRelationship {
				source: source_id.to_string(),
				target: target.to_string(),
				relation: relation_type,
				confidence,
				line: Some(relation.line + 1),
			});
		}
	}
	relationships
}

/// Order relationships by (source, target, relation) and keep one edge per
/// triple: the most confident, then the earliest line.
pub(crate) fn sort_and_dedup(relationships: &mut Vec<CodeRelationship>) {
	relationships.sort_by(|a, b| {
		(&a.source, &a.target, a.relation)
			.cmp(&(&b.source, &b.target, b.relation))
			.then_with(|| b.confidence.total_cmp(&a.confidence))
			.then_with(|| a.line.cmp(&b.line))
	});
	relationships.dedup_by(|later, earlier| {
		later.source == earlier.source && later.target == earlier.target && later.relation == earlier.relation
	});
}

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod symbols_tests;
