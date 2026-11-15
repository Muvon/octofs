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

//! Language registry for structural outline extraction: one implementation
//! per tree-sitter grammar, exposing the node kinds that become outline items
//! and how to name them.

pub(crate) mod bash;
pub(crate) mod cpp;
pub(crate) mod css;
pub(crate) mod elixir;
pub(crate) mod go;
pub(crate) mod java;
pub(crate) mod javascript;
pub(crate) mod json;
pub(crate) mod lua;
pub(crate) mod php;
pub(crate) mod python;
pub(crate) mod ruby;
pub(crate) mod rust;
pub(crate) mod svelte;
pub(crate) mod swift;
pub(crate) mod typescript;
pub(crate) mod use_alias;

use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::symbols::ImportBinding;
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::Node;

/// Kind of type-level relationship a language may report from an AST node,
/// emitted as an extends or implements edge between symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRelationKind {
	/// Inheritance: subclass extends superclass; trait/interface inherits from another.
	Extends,
	/// Interface or trait implementation: a concrete type satisfies an interface/trait.
	Implements,
}

/// A callable reference extracted from a language AST.
///
/// `name` is always the terminal callable (`run` in `service.run()`), while
/// `qualifier` preserves the receiver/module/type (`service`). Keeping both
/// avoids turning every segment of a qualified expression into a bogus call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTarget {
	pub name: String,
	pub qualifier: Option<String>,
}

/// A code region embedded in another supported language (for example a
/// JavaScript/TypeScript `<script>` inside Svelte).
#[derive(Debug, Clone)]
pub struct EmbeddedSource {
	pub language: &'static str,
	pub contents: String,
	/// 0-based line of the file where `contents` begins; added to every line
	/// the embedded block reports.
	pub start_line: u32,
}

/// Where a declaration's body opens, deciding which lines of an outline
/// item are its signature.
#[derive(Clone, Copy)]
pub(crate) enum BodyOpen {
	/// The first line containing `{`.
	Brace,
	/// The first line whose trimmed text ends with this.
	LineEnding(&'static str),
	/// The first line only.
	FirstLine,
}

/// How a language groups its files into packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageScope {
	/// A file belongs to no package of its own.
	NoPackage,
	/// The files of one directory are one package.
	Directory,
	/// The files that declare the same package name are one package; the
	/// files that declare none are one package per directory.
	Declared,
}

/// Comment starts of the languages that write `//` and `/* */`.
pub(crate) const C_STYLE_COMMENTS: &[&str] = &["//", "/*"];

/// Comment starts of the languages that write `#`.
pub(crate) const HASH_COMMENTS: &[&str] = &["#"];

/// The keyword that declares a function in the languages that write `function`.
pub(crate) const FUNCTION_KEYWORD: &[&str] = &["function"];

/// One block of another language inside a file: (language, text, 0-based row
/// and column the text starts at).
pub(crate) type ScriptBlock = (&'static str, String, usize, usize);

/// Metavariable stand-in for the languages whose identifiers may not start
/// with `$`: a pattern's `$NAME` is rewritten to it before parsing, as
/// ast-grep's own language configs do.
pub(crate) const IDENTIFIER_EXPANDO: char = 'µ';

/// The stand-in ast-grep's configs use for C and C++.
pub(crate) const C_EXPANDO: char = '𐀀';

/// The stand-in for the languages where `$` is a valid identifier character,
/// which makes the rewrite a no-op.
pub(crate) const DOLLAR_EXPANDO: char = '$';

/// A grammar AST patterns are parsed and matched with.
#[derive(Clone)]
pub(crate) struct PatternGrammar {
	/// The language name callers and messages use for it.
	pub name: &'static str,
	pub grammar: tree_sitter::Language,
	/// What a pattern's `$` metavariable sigil becomes before parsing.
	pub expando: char,
}

/// What a graph symbol is, in the words the graph output shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
	/// A function, method or constructor: something a call can reach.
	Function,
	Class,
	Struct,
	Trait,
	Interface,
	Enum,
	/// A module or namespace.
	Module,
	Macro,
	Const,
	/// A type alias.
	Type,
	/// An implementation of a protocol or interface for a type.
	Implementation,
	/// A declaration the graph has no word for.
	Other,
}

impl SymbolKind {
	pub fn as_str(self) -> &'static str {
		match self {
			SymbolKind::Function => "function",
			SymbolKind::Class => "class",
			SymbolKind::Struct => "struct",
			SymbolKind::Trait => "trait",
			SymbolKind::Interface => "interface",
			SymbolKind::Enum => "enum",
			SymbolKind::Module => "module",
			SymbolKind::Macro => "macro",
			SymbolKind::Const => "const",
			SymbolKind::Type => "type",
			SymbolKind::Implementation => "implementation",
			SymbolKind::Other => "symbol",
		}
	}
}

/// A tree-sitter grammar plus the outline decisions for it.
pub(crate) trait Language: Send + Sync {
	/// Name of the language.
	fn name(&self) -> &'static str;

	/// The lowercase file extensions of the language, without the dot.
	fn extensions(&self) -> &'static [&'static str];

	/// The tree-sitter grammar to parse with.
	fn get_ts_language(&self) -> tree_sitter::Language;

	/// Byte ranges to blank out of the text before its declarations are read:
	/// the tokens of a wrapper around real declarations (Rust's
	/// `cfg_rt! { ... }` blocks, whose items the grammar leaves opaque), found
	/// in the tree already parsed. Blanking keeps every line and column where
	/// it was, so what a parse of the result finds maps straight back to the
	/// file. Empty when the file has none.
	fn declaration_blanks(&self, tree: &tree_sitter::Tree, contents: &str) -> Vec<(usize, usize)> {
		let _ = (tree, contents);
		Vec::new()
	}

	/// Node kinds that become outline items. Defaults to the chunking kinds;
	/// languages override to expose container declarations (classes,
	/// interfaces, enums) that chunking deliberately drops, and to drop
	/// non-declarations (imports, calls, statements) that declare nothing.
	fn get_outline_kinds(&self) -> Vec<&'static str>;

	/// Whether a listed node becomes an item. Macro-oriented grammars such as
	/// Elixir classify their `call` nodes by call target instead of kind.
	fn is_signature_node(&self, node: Node, contents: &str) -> bool {
		let _ = (node, contents);
		true
	}

	/// Declared name of the item at this node, or None when the node has no
	/// single declarable name (such nodes are skipped). The default covers the
	/// dominant tree-sitter convention: a direct child whose kind is or
	/// contains "identifier", "name", or "type_identifier".
	fn extract_declaration_name(&self, node: Node, contents: &str) -> Option<String> {
		extract_symbol_by_kinds(node, contents, &["identifier", "name", "type_identifier"])
	}

	/// Name shown by the outline. Usually identical to the declaration name,
	/// but declaration forms that do not create a standalone symbol may still
	/// expose a useful name.
	fn extract_signature_name(&self, node: Node, contents: &str) -> Option<String> {
		self.extract_declaration_name(node, contents)
	}

	/// Coarse declaration kind when it cannot be inferred from the grammar's
	/// node kind. Callers fall back to their normal node-kind mapping.
	fn extract_declaration_kind(&self, node: Node, contents: &str) -> Option<&'static str> {
		let _ = (node, contents);
		None
	}

	/// What the symbol declared at this node is. Languages list their own
	/// grammar's node kinds here; a node it does not list is `Other`.
	fn symbol_kind(&self, node: Node, contents: &str) -> SymbolKind {
		let _ = (node, contents);
		SymbolKind::Other
	}

	/// Whether the function declared at this node builds an instance of the
	/// type that owns it (a Java constructor, a Swift `init`), so a call that
	/// names the type reaches it.
	fn is_constructor(&self, node: Node, contents: &str) -> bool {
		let _ = (node, contents);
		false
	}

	/// (node kind, short display label) pairs of the nodes that enclose a
	/// symbol and show in its breadcrumb. Empty for a language whose
	/// declarations are not told apart by node kind.
	fn container_kinds(&self) -> &'static [(&'static str, &'static str)] {
		&[]
	}

	/// Node kinds that define a named symbol for symbol navigation.
	fn definition_kinds(&self) -> &'static [&'static str] {
		&[]
	}

	/// Whether the identifier `node` (with the text `text`) is the defining
	/// name of a declaration: by default the `name` field of a definition.
	fn is_definition_name(&self, node: Node, text: &str, contents: &str) -> bool {
		let _ = (text, contents);
		node.parent().is_some_and(|parent| {
			self.definition_kinds().contains(&parent.kind())
				&& parent
					.child_by_field_name("name")
					.is_some_and(|name| name.id() == node.id())
		})
	}

	/// Whether symbol navigation (definitions, references) has anything to
	/// find in the language.
	fn has_navigable_symbols(&self) -> bool {
		true
	}

	/// The code blocks of other languages a file holds, when the file is a
	/// container of them (a Svelte component's scripts); each is searched
	/// with its own language and its hits mapped back to the file.
	fn script_blocks(&self, contents: &str) -> Option<Vec<ScriptBlock>> {
		let _ = contents;
		None
	}

	/// The grammar AST patterns are parsed with, or None when the language
	/// offers no pattern search (a syntax-tree pattern buys nothing over its
	/// outline, symbol and content modes).
	fn pattern_grammar(&self) -> Option<PatternGrammar> {
		None
	}

	/// The pattern grammar for a file with this extension, for a language
	/// whose extensions are not all parsed by one grammar.
	fn pattern_grammar_for_extension(&self, extension: &str) -> Option<PatternGrammar> {
		let _ = extension;
		self.pattern_grammar()
	}

	/// Node kinds that become symbol-level graph nodes. Empty by default, so a
	/// language contributes no symbols until it opts in; kinds listed here must
	/// declare something (not imports, calls or statements).
	fn get_symbol_kinds(&self) -> Vec<&'static str> {
		Vec::new()
	}

	/// Import and export names declared at this node, as `(imports, exports)`.
	/// The caller walks the tree; this inspects one node.
	fn extract_imports_exports(&self, node: Node, contents: &str) -> (Vec<String>, Vec<String>) {
		let _ = (node, contents);
		(Vec::new(), Vec::new())
	}

	/// Callees invoked at this node, empty unless the node is a call. The
	/// caller walks the tree and tracks the line.
	fn extract_function_calls(&self, node: Node, contents: &str) -> Vec<CallTarget> {
		let _ = (node, contents);
		Vec::new()
	}

	/// Owning type/module for a declared symbol, when it is syntactically
	/// known: [`enclosing_owner`] over the language's container kinds, or the
	/// receiver written on the declaration itself (Go, Lua). None for a
	/// language without owned declarations.
	fn extract_symbol_owner(&self, node: Node, contents: &str) -> Option<String> {
		let _ = (node, contents);
		None
	}

	/// Code regions embedded in another language that should join the graph.
	fn extract_embedded_sources(&self, root: Node, contents: &str) -> Vec<EmbeddedSource> {
		let _ = (root, contents);
		Vec::new()
	}

	/// Extends/implements relations declared at this node as `(kind, target
	/// name)` pairs: `class Foo extends Bar implements Baz` yields
	/// `[(Extends, "Bar"), (Implements, "Baz")]`. The caller walks the tree.
	fn extract_type_relations(&self, node: Node, contents: &str) -> Vec<(TypeRelationKind, String)> {
		let _ = (node, contents);
		Vec::new()
	}

	/// Name of the declaration that owns relations emitted for this node. Most
	/// languages name it on the same node; languages with separate
	/// implementation blocks override this.
	fn extract_type_relation_source(&self, node: Node, contents: &str) -> Option<String> {
		self.extract_declaration_name(node, contents)
	}

	/// Resolve an import string to a path in `all_files`, or None when it does
	/// not point at a file of the repository (an external dependency).
	fn resolve_import(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Option<String> {
		let _ = (import_path, source_file, all_files);
		None
	}

	/// Substrings of which at least one appears on the same line as the imported
	/// module/file name in every import form this language's extractor understands
	/// that the line holds a keyword for. Empty means "no cheap filter": every line
	/// naming the module is a candidate.
	fn import_line_markers(&self) -> &'static [&'static str] {
		&[]
	}

	/// Same, for the line that names a parent type (extends / implements / embeds /
	/// mixes in) in every form `extract_type_relations` understands.
	fn inheritance_line_markers(&self) -> &'static [&'static str] {
		&[]
	}

	/// Whether `line` may be the line of an import of the module `name` (a file
	/// stem or directory name). Sound, not exact: it never rejects a line the
	/// extractor reads an import from. The default accepts a line with a marker
	/// and a line that holds nothing but names, which is how the continuation
	/// lines of a grouped import look. Languages whose imports end in a quoted
	/// path override it.
	fn line_may_import(&self, line: &str, name: &str) -> bool {
		let markers = self.import_line_markers();
		contains_identifier(line, name)
			&& (markers.is_empty()
				|| markers.iter().any(|marker| marker_in_line(line, marker))
				|| name_list_line(line, name, "", self.comment_starts()))
	}

	/// Whether `line` of the file `path` may re-export what the module `name`
	/// declares (`export * from './name'`, `from .name import *`), so the
	/// files importing this one reach the declarations through it. Sound, not
	/// exact, like `line_may_import`; a language without re-exports says no.
	fn line_may_reexport(&self, _line: &str, _name: &str, _path: &str) -> bool {
		false
	}

	/// Whether `line` may name the parent type `name` of a declaration, with
	/// the same soundness rule as `line_may_import`.
	fn line_may_inherit(&self, line: &str, name: &str) -> bool {
		inheritance_line(
			self.inheritance_line_markers(),
			line,
			name,
			self.inheritance_name_extras(),
			self.comment_starts(),
		)
	}

	/// Every repository file an import names; empty for an external
	/// dependency. Languages whose imports name a package or directory rather
	/// than one file (Go, Java wildcards, Swift modules) override this with
	/// all the files of that package; everything else names at most one file.
	fn resolve_import_files(&self, import_path: &str, source_file: &str, all_files: &FileRegistry) -> Vec<String> {
		self.resolve_import(import_path, source_file, all_files)
			.into_iter()
			.collect()
	}

	/// Whether the import is only a candidate: when it names no file it is
	/// dropped instead of becoming an external dependency.
	fn import_is_optional(&self, _import_path: &str) -> bool {
		false
	}

	/// Whether the file an import resolves to was found by a guess (a name
	/// match, not a path the import states): its edge is then not certain.
	fn import_is_guess(&self, _import_path: &str, _source_file: &str, _all_files: &FileRegistry) -> bool {
		false
	}

	/// The names a statement binds to the files it imports: `imports` are
	/// what [`Self::extract_imports_exports`] returned for the same node.
	/// Languages whose imports bind no names of their own leave it empty.
	fn extract_import_bindings(&self, _node: Node, _contents: &str, _imports: &[String]) -> Vec<ImportBinding> {
		Vec::new()
	}

	/// The import a call makes inline, when its callee is written with the
	/// path of the module or type that owns it (`crate::runtime::spawn(f)`,
	/// `\App\Models\User::find()`) instead of through an import statement:
	/// the binding is the one the statement for that path would give, so the
	/// call resolves through the import resolver and the re-exports it
	/// follows. `node` is the call node `callee` was read from, for a
	/// language whose call target does not keep the whole path. None when the
	/// qualifier names no such path: a receiver, a type written without its
	/// module, the enclosing type itself.
	fn inline_import(&self, node: Node, contents: &str, callee: &CallTarget) -> Option<ImportBinding> {
		let _ = (node, contents, callee);
		None
	}

	/// The name a node declares as the file's default export (`draw` in
	/// `export default function draw() {}`): a default import names it
	/// whatever the importer calls it.
	fn default_export_name(&self, _node: Node, _contents: &str) -> Option<String> {
		None
	}

	/// The file name that marks the root of a module or package of this
	/// language, when imports are written relative to it. Only where the
	/// file sits is read, never its contents.
	fn module_root_file(&self) -> Option<&'static str> {
		None
	}

	/// How this language groups files into packages whose members see each
	/// other's top-level names without importing them.
	fn package_scope(&self) -> PackageScope {
		PackageScope::NoPackage
	}

	/// The package a node declares for its file, for a language whose
	/// [`Self::package_scope`] is [`PackageScope::Declared`].
	fn declared_package(&self, _node: Node, _contents: &str) -> Option<String> {
		None
	}

	/// Whether the file at `path` stands for its directory: importing the
	/// directory names this file (`index.ts`, `__init__.py`, `mod.rs`).
	fn is_directory_index(&self, _path: &str) -> bool {
		false
	}

	/// Whether an import can name the directory that holds a file, so
	/// importers spell the directory's name as well as the file's.
	fn import_names_directory(&self) -> bool {
		false
	}

	/// The names importers of the file at `path` write: its stem and, where
	/// an import can name the directory or the file stands for it, the
	/// directory's name. Any other directory is spelled by unrelated files
	/// and is left out. `declared` are the names the file declares at its top
	/// level, for a language whose import names are declared, not derived
	/// from the path; empty when the file has not been parsed.
	fn importer_names(&self, path: &str, _declared: &[&str]) -> Vec<String> {
		stem_and_directory(path, self.is_directory_index(path) || self.import_names_directory())
	}

	/// Where a declaration's body opens, deciding which lines of an outline
	/// item are its signature.
	fn body_open(&self) -> BodyOpen {
		BodyOpen::Brace
	}

	/// What begins a comment that runs to the end of a line or is closed
	/// later (`//`, `/*`, `#`, `--`). A line is read up to the first one.
	fn comment_starts(&self) -> &'static [&'static str] {
		&[]
	}

	/// Characters other than identifier characters, spaces and brackets that
	/// the line of a parent-type list may hold: bounds (`+`, `&`), generic
	/// brackets, `:`.
	fn inheritance_name_extras(&self) -> &'static str {
		""
	}

	/// Whether files of this language can add symbols to the code graph: it
	/// declares symbols itself, or embeds code of a language that does.
	fn has_graph_symbols(&self) -> bool {
		!self.get_symbol_kinds().is_empty()
	}

	/// What separates the segments of the module an import names (`::` in
	/// Rust, `.` in Python and Java, `/` in a path). A call qualified by the
	/// last segment names the imported file.
	fn import_name_separators(&self) -> &'static [char] {
		&['/']
	}

	/// Words after which a name is being declared (`fn`, `class`, `const`).
	/// Used to rank files by reading raw text, so a miss only costs a repeat.
	fn declaration_keywords(&self) -> &'static [&'static str] {
		&[]
	}

	/// The declaration keywords whose name is followed by a parameter list,
	/// possibly after a receiver in parentheses (`func (s *Server) Name(`).
	fn callable_keywords(&self) -> &'static [&'static str] {
		&[]
	}
}

fn is_identifier_char(character: char) -> bool {
	character.is_alphanumeric() || character == '_' || character == '$'
}

/// `name` occurs in `line` as a whole identifier.
pub(crate) fn contains_identifier(line: &str, name: &str) -> bool {
	!name.is_empty()
		&& line.match_indices(name).any(|(start, _)| {
			line[..start]
				.chars()
				.next_back()
				.is_none_or(|before| !is_identifier_char(before))
				&& line[start + name.len()..]
					.chars()
					.next()
					.is_none_or(|after| !is_identifier_char(after))
		})
}

/// A keyword marker must stand alone (`use` is not in `user`); a symbol
/// marker (`::`, `\`) is a plain substring.
fn marker_in_line(line: &str, marker: &str) -> bool {
	let keyword = marker
		.chars()
		.next()
		.zip(marker.chars().last())
		.is_some_and(|(first, last)| is_identifier_char(first) && is_identifier_char(last));
	if keyword {
		contains_identifier(line, marker)
	} else {
		line.contains(marker)
	}
}

/// A parent-type line: a marker keyword, or names only (`extra` widens what
/// counts as a name list for the language). No markers keeps every line that
/// names the type.
pub(crate) fn inheritance_line(markers: &[&str], line: &str, name: &str, extra: &str, comment_starts: &[&str]) -> bool {
	contains_identifier(line, name)
		&& (markers.is_empty()
			|| markers.iter().any(|marker| marker_in_line(line, marker))
			|| name_list_line(line, name, extra, comment_starts))
}

/// The line without a trailing comment that starts with one of `comment_starts`.
fn without_comment<'a>(line: &'a str, comment_starts: &[&str]) -> &'a str {
	let cut = comment_starts.iter().filter_map(|start| line.find(start)).min();
	cut.map_or(line, |index| &line[..index])
}

/// A line that holds `name` among names only: identifiers, spaces, commas and
/// brackets (plus `extra`), such as the `b,` of a parenthesised import group.
/// A name used in code (`query = x`, `query.filter()`) has other characters.
fn name_list_line(line: &str, name: &str, extra: &str, comment_starts: &[&str]) -> bool {
	let code = without_comment(line, comment_starts);
	contains_identifier(code, name)
		&& code.chars().all(|character| {
			is_identifier_char(character)
				|| character.is_whitespace()
				|| ",(){}".contains(character)
				|| extra.contains(character)
		})
}

/// `name` is the last segment of a path written in `line`: whole identifier,
/// then any file extensions, then one of `closers` (or the end of the line
/// when `closers` holds a newline). A URL string that merely contains the
/// name does not qualify.
pub(crate) fn path_ends_with_name(line: &str, name: &str, closers: &str) -> bool {
	if name.is_empty() {
		return false;
	}
	line.match_indices(name).any(|(start, _)| {
		if line[..start].chars().next_back().is_some_and(is_identifier_char) {
			return false;
		}
		let mut rest = &line[start + name.len()..];
		while let Some(after_dot) = rest.strip_prefix('.') {
			let extension_len = after_dot
				.find(|character: char| !is_identifier_char(character))
				.unwrap_or(after_dot.len());
			if extension_len == 0 {
				break;
			}
			rest = &after_dot[extension_len..];
		}
		match rest.chars().next() {
			None => closers.contains('\n'),
			Some(next) => closers.contains(next),
		}
	})
}

type LanguageConstructor = fn() -> Box<dyn Language>;

/// Every language, by the name `get_language` takes.
const LANGUAGES: [(&str, LanguageConstructor); 17] = [
	("rust", || Box::new(rust::Rust {})),
	("javascript", || Box::new(javascript::JavaScript {})),
	("typescript", || Box::new(typescript::TypeScript {})),
	("tsx", || Box::new(typescript::Tsx {})),
	("python", || Box::new(python::Python {})),
	("go", || Box::new(go::Go {})),
	("java", || Box::new(java::Java {})),
	("cpp", || Box::new(cpp::Cpp {})),
	("php", || Box::new(php::Php {})),
	("bash", || Box::new(bash::Bash {})),
	("ruby", || Box::new(ruby::Ruby {})),
	("lua", || Box::new(lua::Lua {})),
	("json", || Box::new(json::Json {})),
	("swift", || Box::new(swift::Swift {})),
	("css", || Box::new(css::Css {})),
	("elixir", || Box::new(elixir::Elixir {})),
	("svelte", || Box::new(svelte::Svelte {})),
];

/// Parses `contents` with the grammar `parser` was set to. When the language
/// finds wrappers around real declarations in the tree (see
/// `Language::declaration_blanks`), the tree is that of the text with the
/// wrappers blanked out, derived from the first tree by an incremental parse
/// (the blanked text has the same byte length, so nothing moves); the text is
/// returned with it. Files without wrappers cost one parse and one tree walk.
pub(crate) fn parse_declarations<'a>(
	language: &dyn Language,
	parser: &mut tree_sitter::Parser,
	contents: &'a str,
) -> Option<(tree_sitter::Tree, Cow<'a, str>)> {
	let mut tree = parser.parse(contents, None)?;
	let mut blanks = language.declaration_blanks(&tree, contents);
	if blanks.is_empty() {
		return Some((tree, Cow::Borrowed(contents)));
	}
	blanks.sort_unstable();
	let mut bytes = contents.as_bytes().to_vec();
	for (start, end) in &blanks {
		// Newlines stay, so every line and column is unchanged.
		for byte in &mut bytes[*start..*end] {
			if !matches!(*byte, b'\n' | b'\r') {
				*byte = b' ';
			}
		}
	}
	let Ok(blanked) = String::from_utf8(bytes) else {
		return Some((tree, Cow::Borrowed(contents)));
	};
	let offsets: Vec<usize> = blanks.iter().flat_map(|(start, end)| [*start, *end]).collect();
	let points = points_at(contents, &offsets);
	for (index, (start, end)) in blanks.iter().enumerate() {
		tree.edit(&tree_sitter::InputEdit {
			start_byte: *start,
			old_end_byte: *end,
			new_end_byte: *end,
			start_position: points[2 * index],
			old_end_position: points[2 * index + 1],
			new_end_position: points[2 * index + 1],
		});
	}
	let reparsed = parser.parse(&blanked, Some(&tree))?;
	Some((reparsed, Cow::Owned(blanked)))
}

/// The row and column (in bytes) of each offset, in one pass over the text.
fn points_at(text: &str, offsets: &[usize]) -> Vec<tree_sitter::Point> {
	let mut sorted: Vec<(usize, usize)> = offsets.iter().copied().zip(0..).collect();
	sorted.sort_unstable();
	let mut points = vec![tree_sitter::Point::new(0, 0); offsets.len()];
	let (mut row, mut line_start) = (0, 0);
	for (offset, slot) in sorted {
		while let Some(newline) = text.as_bytes()[line_start..offset]
			.iter()
			.position(|byte| *byte == b'\n')
		{
			row += 1;
			line_start += newline + 1;
		}
		points[slot] = tree_sitter::Point::new(row, offset - line_start);
	}
	points
}

/// Gets a language implementation by its name.
pub(crate) fn get_language(name: &str) -> Option<Box<dyn Language>> {
	LANGUAGES
		.iter()
		.find(|(language_name, _)| *language_name == name)
		.map(|(_, make)| make())
}

/// The name of the language that owns the file extension (lowercase, without
/// the dot), when one does.
pub(crate) fn language_for_extension(extension: &str) -> Option<&'static str> {
	LANGUAGES
		.iter()
		.find(|(_, make)| make().extensions().contains(&extension))
		.map(|(name, _)| *name)
}

/// The pattern grammar a file with this extension is searched with, when its
/// language offers one.
pub(crate) fn pattern_grammar_for_extension(extension: &str) -> Option<PatternGrammar> {
	LANGUAGES
		.iter()
		.map(|(_, make)| make())
		.find(|language| language.extensions().contains(&extension))?
		.pattern_grammar_for_extension(extension)
}

/// The pattern grammar called `name`: a language's own, or one it picks for
/// some of its extensions.
pub(crate) fn pattern_grammar_named(name: &str) -> Option<PatternGrammar> {
	LANGUAGES.iter().map(|(_, make)| make()).find_map(|language| {
		language
			.pattern_grammar()
			.into_iter()
			.chain(
				language
					.extensions()
					.iter()
					.filter_map(|extension| language.pattern_grammar_for_extension(extension)),
			)
			.find(|grammar| grammar.name == name)
	})
}

/// The stem of the file at `path` and, with `with_directory`, the name of
/// its directory.
pub(crate) fn stem_and_directory(path: &str, with_directory: bool) -> Vec<String> {
	let mut parts = path.rsplit('/');
	let file_name = parts.next().unwrap_or(path);
	let stem = file_name.split('.').next().unwrap_or(file_name);
	let mut names: Vec<String> = Vec::new();
	if !stem.is_empty() {
		names.push(stem.to_string());
	}
	if with_directory {
		names.extend(
			parts
				.next()
				.filter(|directory| !directory.is_empty())
				.map(str::to_string),
		);
	}
	names
}

/// The file names that mark a module root in any language, collected once.
pub(crate) fn module_root_files() -> &'static [&'static str] {
	static FILES: OnceLock<Vec<&'static str>> = OnceLock::new();
	FILES.get_or_init(|| {
		LANGUAGES
			.iter()
			.filter_map(|(_, make)| make().module_root_file())
			.collect()
	})
}

/// The names of the languages with a graph presence: their files can add
/// symbols to the code graph. Markdown has a grammar for the outline modes but
/// no language here, and JSON declares nothing, so handing their files over
/// would list each as skipped or cost a parse for nothing.
pub(crate) fn graph_languages() -> &'static HashSet<&'static str> {
	static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
	NAMES.get_or_init(|| {
		LANGUAGES
			.iter()
			.filter(|(_, make)| make().has_graph_symbols())
			.map(|(name, _)| *name)
			.collect()
	})
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

/// Extract a symbol from a node by finding a child matching any of multiple
/// kinds. A kind also matches when the child's kind merely contains it, which
/// is how "type_identifier" and "property_identifier" hit "identifier".
pub(crate) fn extract_symbol_by_kinds(node: Node, contents: &str, target_kinds: &[&str]) -> Option<String> {
	for child in node.children(&mut node.walk()) {
		// Anonymous tokens (keywords such as TS's `namespace`) are never
		// declaration names, and their kinds can contain a target kind
		// ("namespace" contains "name").
		if !child.is_named() {
			continue;
		}
		if target_kinds.contains(&child.kind()) || target_kinds.iter().any(|k| child.kind().contains(k)) {
			if let Ok(text) = child.utf8_text(contents.as_bytes()) {
				return Some(text.to_string());
			}
		}
	}
	None
}

/// The simple (unqualified, non-generic) type name from a possibly qualified
/// or generic type expression: `std::collections::HashMap<K, V>` → `HashMap`,
/// `com.example.Foo` → `Foo`, `App\Models\User` → `User`, `Store[T]` → `Store`.
pub(crate) fn simple_type_name(text: &str) -> Option<String> {
	// Cut at the first bracket of any kind: trimming only the ends leaves the
	// opening bracket behind (`Store[T]` → `Store[T`).
	let stripped = text.split(['<', '[', '(']).next().unwrap_or(text);
	let after_colons = stripped.rsplit("::").next().unwrap_or(stripped);
	let after_backslash = after_colons.rsplit('\\').next().unwrap_or(after_colons);
	let after_dots = after_backslash.rsplit('.').next().unwrap_or(after_backslash);
	let trimmed = after_dots
		.trim()
		.trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
	if trimmed.is_empty() {
		None
	} else {
		Some(trimmed.to_string())
	}
}

/// Whether `path` ends with `suffix` on a path-component boundary:
/// `a/b/c.go` ends with `b/c.go` and `c.go`, but `abc.go` does not end with
/// `c.go`.
pub(crate) fn path_ends_with_components(path: &str, suffix: &str) -> bool {
	path.strip_suffix(suffix)
		.is_some_and(|head| head.is_empty() || head.ends_with('/'))
}

/// Every file directly in `directory` that `accept`s, in alphabetical order.
pub(crate) fn files_in_dir_matching(
	registry: &FileRegistry,
	directory: &str,
	accept: impl Fn(&str) -> bool,
) -> Vec<String> {
	let mut files: Vec<String> = registry
		.files_in_dir(directory)
		.iter()
		.filter(|file| accept(file.as_str()))
		.cloned()
		.collect();
	files.sort();
	files
}

/// The qualifier of a member call whose receiver is an expression, not a
/// name (`getRepo().save()`): nothing is known about the receiver, so the
/// call resolves by name only, at guess confidence.
pub(crate) const UNKNOWN_RECEIVER: &str = "?";

/// The receiver of a member call that names the enclosing type itself
/// (`self.run()`, `this.run()`). Not an identifier, so no real receiver can
/// be written the same way.
pub(crate) const SELF_RECEIVER: &str = ".";

/// What joins the segments of a qualifier in a [`CallTarget`]: `a.b.c()` and
/// `a::b::c()` both have the qualifier `a::b`.
pub(crate) const QUALIFIER_SEPARATOR: &str = "::";

/// How a language writes the callee of a call, so [`parse_call_target`] can
/// split `receiver.name` without knowing any one language.
pub(crate) struct CalleeSyntax {
	/// What joins a receiver and a member name, longest first when one is a
	/// prefix of another (`::` before `:`).
	pub member_separators: &'static [&'static str],
	/// Operators written in front of a callee that are not part of it
	/// (address-of, dereference).
	pub prefix_operators: &'static [char],
	/// Characters besides letters, digits and `_` that an identifier of the
	/// language may hold (`$` and `#` in JavaScript, `@` in Ruby).
	pub identifier_extras: &'static [char],
	/// The character in front of a variable's name, not part of the name.
	pub variable_sigil: Option<char>,
	/// Words that name the enclosing type as receiver.
	pub self_receivers: &'static [&'static str],
	/// Whether `<...>` after a name holds type arguments, not a comparison.
	pub angle_generics: bool,
}

impl CalleeSyntax {
	/// Receiver and name joined by `.`, nothing else.
	pub(crate) const DOTTED: CalleeSyntax = CalleeSyntax {
		member_separators: &["."],
		prefix_operators: &[],
		identifier_extras: &[],
		variable_sigil: None,
		self_receivers: &[],
		angle_generics: false,
	};
}

/// Parse a textual callee expression into its terminal name and qualifier,
/// as the language writes it (`syntax`): `foo`, `module.foo`, `Type::method`,
/// `ptr->method`, optional chaining. Generic arguments on the callable are
/// removed before splitting. Returns None when the callee is not a plain
/// identifier path: the qualifier segments must be identifier-like too, so
/// expressions like `1 + 2` never name an item.
pub(crate) fn parse_call_target(text: &str, syntax: &CalleeSyntax) -> Option<CallTarget> {
	let mut trimmed = text.trim().trim_start_matches(syntax.prefix_operators);
	if trimmed.is_empty() {
		return None;
	}

	let without_generics;
	if syntax.angle_generics {
		let mut stripped = String::with_capacity(trimmed.len());
		let mut generic_depth = 0u32;
		for character in trimmed.chars() {
			match character {
				'<' => generic_depth += 1,
				'>' if generic_depth > 0 => generic_depth -= 1,
				_ if generic_depth == 0 => stripped.push(character),
				_ => {}
			}
		}
		without_generics = stripped;
		trimmed = without_generics.trim();
	}
	// Called for every call of every file: the text is copied only when a
	// separator other than the plain `.` is in it.
	let mut normalized = Cow::Borrowed(trimmed);
	for separator in syntax.member_separators.iter().filter(|separator| **separator != ".") {
		if normalized.contains(separator) {
			normalized = Cow::Owned(normalized.replace(separator, "."));
		}
	}
	let segments: Vec<&str> = normalized
		.split('.')
		.map(str::trim)
		.filter(|segment| !segment.is_empty())
		.collect();
	let (name, qualifier_segments) = segments.split_last()?;
	let name = name.trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
	if name.is_empty()
		|| !name
			.chars()
			.all(|character| character.is_alphanumeric() || character == '_')
	{
		return None;
	}
	if qualifier_segments.iter().any(|segment| {
		!segment.chars().all(|character: char| {
			character.is_alphanumeric() || character == '_' || syntax.identifier_extras.contains(&character)
		})
	}) {
		// A member call on an expression (`getRepo().save()`, `"x".len()`)
		// is still a call of `save`, on a receiver nothing is known about.
		// Only an expression can end right before the `.`: a path such as
		// `./run-tests.sh` or a file name names no item.
		let before_name = normalized
			.trim_end()
			.strip_suffix(name)
			.and_then(|rest| rest.strip_suffix('.'));
		let is_member_call = before_name.is_some_and(|receiver| receiver.ends_with([')', ']', '}', '"', '\'']))
			&& name
				.chars()
				.next()
				.is_some_and(|first| first.is_alphabetic() || first == '_');
		return is_member_call.then(|| CallTarget {
			name: name.to_string(),
			qualifier: Some(UNKNOWN_RECEIVER.to_string()),
		});
	}
	let names: Vec<&str> = qualifier_segments
		.iter()
		.map(|&segment| {
			syntax
				.variable_sigil
				.and_then(|sigil| segment.strip_prefix(sigil))
				.unwrap_or(segment)
		})
		.collect();
	let qualifier = match names.as_slice() {
		[] => None,
		[only] if syntax.self_receivers.contains(only) => Some(SELF_RECEIVER.to_string()),
		_ => Some(names.join(QUALIFIER_SEPARATOR)),
	};
	Some(CallTarget {
		name: name.to_string(),
		qualifier,
	})
}

/// The simple (unqualified) name of a textual callee expression: the terminal
/// segment of [`parse_call_target`], for callers that do not need the
/// qualifier.
pub(crate) fn extract_call_target(text: &str, syntax: &CalleeSyntax) -> Option<String> {
	parse_call_target(text, syntax).map(|target| target.name)
}

/// Name of the nearest enclosing declaration whose node kind is one of
/// `owner_kinds` (the language's classes, interfaces, impl blocks...). Any
/// other enclosing node is stepped over.
pub(crate) fn enclosing_owner(node: Node, contents: &str, owner_kinds: &[&str]) -> Option<String> {
	let mut current = node.parent();
	while let Some(parent) = current {
		if owner_kinds.contains(&parent.kind()) {
			// `type` first: it names the receiver of `impl Trait for Foo`.
			for field in ["type", "name"] {
				if let Some(name_node) = parent.child_by_field_name(field) {
					if let Ok(text) = name_node.utf8_text(contents.as_bytes()) {
						if let Some(name) = simple_type_name(text) {
							return Some(name);
						}
					}
				}
			}
			return extract_symbol_by_kinds(parent, contents, &["identifier", "name", "type_identifier", "constant"])
				.and_then(|name| simple_type_name(&name));
		}
		current = parent.parent();
	}
	None
}
