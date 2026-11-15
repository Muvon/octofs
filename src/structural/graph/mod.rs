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

//! Live code graph: calls, imports, and extends/implements relationships
//! between symbols and files, built straight from source with tree-sitter.
//! No index, no embeddings, no model, no database.
//!
//! The graph is pure: callers list the files (workdir-relative, forward-slash
//! paths) and supply a reader; [`parse_files`] reads, parses and
//! drops each file's text inside one parallel pass, so only the extracted
//! symbols stay in memory. Edges are resolved by name in a fixed order (same
//! file, then imported files, then a unique project-wide match), and each
//! carries a confidence that says how direct the match was. Imports that point
//! at no file of the set become `external` nodes instead of vanishing. Files
//! that fail to read or parse are listed in [`CodeGraph::skipped`].
//!
//! Which symbols, calls and imports a language yields is decided by the
//! graph methods of the `Language` trait; a language that implements none of
//! them contributes only its file node.

pub mod registry;
pub mod symbols;
pub mod traverse;
pub mod types;

pub use traverse::{find_paths, neighbors, Direction, GraphIndex, Hop, MAX_PATHS};
pub use types::{CodeGraph, CodeNode, CodeRelationship, RelationType, SkippedFile};

use crate::structural::languages::get_language;
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use registry::FileRegistry;
use std::sync::Arc;
pub use symbols::RelationFilter;
use symbols::{
	discover_symbol_relationships, extract_symbols_from_contents, sort_and_dedup, symbol_node_ids, FileAstData,
	ImportResolver, LanguageLookup, SymbolDecl, SymbolFileData, CONFIDENCE_CERTAIN, CONFIDENCE_NAME_ONLY_MAX, WILDCARD,
};
use types::{EXTERNAL_KIND, FILE_KIND};

/// Prefix of the id of an external-import node, so it cannot collide with a
/// file path or a `path::name` symbol id.
const EXTERNAL_ID_PREFIX: &str = "external:";

/// One source file to put in the graph. Its text is read by the caller's
/// reader during the parallel parse.
pub struct FileSpec {
	/// Workdir-relative path with forward slashes; becomes the file node id.
	pub path: String,
	/// Language name as `Language::name` spells it.
	pub language: &'static str,
}

/// One source file with its text already in memory.
#[cfg(test)]
pub struct SourceFile {
	/// Workdir-relative path with forward slashes; becomes the file node id.
	pub path: String,
	/// Language name as `Language::name` spells it.
	pub language: &'static str,
	pub contents: String,
}

struct LoadedFile {
	path: String,
	language: &'static str,
	contents: Result<String>,
}

/// One file read and parsed. Kept after the parse so a build can be
/// assembled again over a larger set of files without parsing them twice.
pub struct ParsedSource {
	data: SymbolFileData,
	line_count: u32,
}

impl AsRef<ParsedSource> for ParsedSource {
	fn as_ref(&self) -> &ParsedSource {
		self
	}
}

impl ParsedSource {
	pub fn data(&self) -> &SymbolFileData {
		&self.data
	}
}

/// Parse `files`: each is read, parsed and its text released inside one
/// parallel pass. A file `read` fails on, or that does not parse, comes back
/// in the skipped list instead.
pub fn parse_files(
	files: Vec<FileSpec>,
	read: &(dyn Fn(&str) -> Result<String> + Sync),
) -> (Vec<ParsedSource>, Vec<SkippedFile>) {
	parse_stage(
		files,
		&|file: FileSpec| {
			let contents = read(&file.path);
			LoadedFile {
				path: file.path,
				language: file.language,
				contents,
			}
		},
		&get_language,
	)
}

/// Build the graph of in-memory `files`.
#[cfg(test)]
pub fn build_graph(files: Vec<SourceFile>) -> CodeGraph {
	build_graph_with(files, &get_language)
}

/// `build_graph` with the language lookup injected, so tests can supply a
/// language that resolves imports.
#[cfg(test)]
pub(crate) fn build_graph_with(files: Vec<SourceFile>, lookup: LanguageLookup) -> CodeGraph {
	let (sources, skipped) = parse_stage(
		files,
		&|file: SourceFile| LoadedFile {
			path: file.path,
			language: file.language,
			contents: Ok(file.contents),
		},
		lookup,
	);
	let paths: Vec<String> = sources.iter().map(|source| source.data.path.clone()).collect();
	assemble_graph(&sources, &paths, skipped, lookup)
}

fn parse_stage<T: Send>(
	items: Vec<T>,
	load: &(dyn Fn(T) -> LoadedFile + Sync),
	lookup: LanguageLookup,
) -> (Vec<ParsedSource>, Vec<SkippedFile>) {
	let parsed: Vec<Result<ParsedSource, SkippedFile>> = items
		.into_par_iter()
		.map(|item| parse_loaded(load(item), lookup))
		.collect();
	let mut sources = Vec::with_capacity(parsed.len());
	let mut skipped = Vec::new();
	for outcome in parsed {
		match outcome {
			Ok(source) => sources.push(source),
			Err(file) => skipped.push(file),
		}
	}
	(sources, skipped)
}

fn parse_loaded(file: LoadedFile, lookup: LanguageLookup) -> Result<ParsedSource, SkippedFile> {
	let skip = |error: anyhow::Error, path: &str| SkippedFile {
		path: path.to_string(),
		reason: format!("{error:#}"),
	};
	let contents = match file.contents {
		Ok(contents) => contents,
		Err(error) => return Err(skip(error, &file.path)),
	};
	let line_count = u32::try_from(contents.lines().count()).unwrap_or(u32::MAX).max(1);
	match parse_contents(file.language, &contents, lookup) {
		Ok(ast) => Ok(ParsedSource {
			data: SymbolFileData::from_ast(file.path, file.language.to_string(), ast),
			line_count,
		}),
		Err(error) => Err(skip(error, &file.path)),
	}
}

fn parse_contents(language_name: &str, contents: &str, lookup: LanguageLookup) -> Result<FileAstData> {
	let language = lookup(language_name).ok_or_else(|| anyhow!("no language implementation for '{language_name}'"))?;
	extract_symbols_from_contents(contents, language.as_ref(), lookup)
}

/// The graph over `sources` over a registry of every repository path that the
/// caller built once and keeps, with call and type relations resolved
/// only as far as `filter` asks: what a scoped query reads.
pub fn assemble_filtered_graph_in<S: AsRef<ParsedSource>>(
	sources: &[S],
	registry: Arc<FileRegistry>,
	skipped: Vec<SkippedFile>,
	filter: &RelationFilter,
) -> CodeGraph {
	assemble_with_resolver(
		sources,
		skipped,
		ImportResolver::with_registry(registry, &get_language),
		Some(filter),
	)
}

/// The graph over `sources`, as a test oracle: import resolution looks paths
/// up in `all_paths`, which may name more files than were parsed. An import of
/// an unparsed file points at a stub file node. The result does not depend on
/// the order of `sources`.
#[cfg(test)]
pub(crate) fn assemble_graph<S: AsRef<ParsedSource>>(
	sources: &[S],
	all_paths: &[String],
	skipped: Vec<SkippedFile>,
	lookup: LanguageLookup,
) -> CodeGraph {
	assemble_with_resolver(sources, skipped, ImportResolver::new(all_paths, lookup), None)
}

fn assemble_with_resolver<S: AsRef<ParsedSource>>(
	sources: &[S],
	skipped: Vec<SkippedFile>,
	mut resolver: ImportResolver,
	filter: Option<&RelationFilter>,
) -> CodeGraph {
	let mut ordered: Vec<&ParsedSource> = sources.iter().map(AsRef::as_ref).collect();
	ordered.sort_by(|a, b| a.data.path.cmp(&b.data.path));

	let mut graph = CodeGraph {
		skipped,
		..CodeGraph::default()
	};
	graph.skipped.sort_by(|a, b| a.path.cmp(&b.path));
	for source in &ordered {
		let data = &source.data;
		add_file_nodes(&mut graph, &data.path, &data.language, source.line_count, &data.symbols);
	}

	// One resolver for the whole build: every import is resolved once, however
	// many passes ask for it.
	let files: Vec<&SymbolFileData> = ordered.iter().map(|source| &source.data).collect();
	add_import_edges(&mut graph, &files, &mut resolver);
	graph
		.relationships
		.extend(discover_symbol_relationships(&files, &mut resolver, filter));
	sort_and_dedup(&mut graph.relationships);
	add_stub_file_nodes(&mut graph);
	graph
}

/// File nodes without symbols, for the files an import points at that were
/// not parsed.
fn add_stub_file_nodes(graph: &mut CodeGraph) {
	let missing: Vec<String> = graph
		.relationships
		.iter()
		.filter(|edge| edge.relation == RelationType::Imports && !graph.nodes.contains_key(&edge.target))
		.map(|edge| edge.target.clone())
		.collect();
	for path in missing {
		let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
		graph.nodes.entry(path.clone()).or_insert_with(|| CodeNode {
			id: path.clone(),
			name: file_name,
			kind: FILE_KIND.to_string(),
			path,
			owner: None,
			language: String::new(),
			start_line: 1,
			end_line: 1,
		});
	}
}

/// Nodes and `Contains` edges only, with no import or call resolution: enough
/// to look definitions up cheaply.
pub fn declaration_graph<S: AsRef<ParsedSource>>(sources: &[S]) -> CodeGraph {
	let mut graph = CodeGraph::default();
	for source in sources.iter().map(AsRef::as_ref) {
		let data = &source.data;
		add_file_nodes(&mut graph, &data.path, &data.language, source.line_count, &data.symbols);
	}
	graph
}

/// What each import of `source` resolves to, as lists of repository files.
/// The answers are kept on the file, so asking again over the same registry
/// resolves nothing. Imports of external modules give empty lists; an import
/// that names the file's own package is left out, as in the full graph.
pub fn resolved_imports(registry: &Arc<FileRegistry>, source: &ParsedSource) -> Vec<Arc<Vec<String>>> {
	let mut resolver = ImportResolver::with_registry(registry.clone(), &get_language);
	let data = &source.data;
	data.imports
		.iter()
		.map(|import| import.trim())
		.filter(|import| !import.is_empty())
		.map(|import| resolver.resolve_for(data, import))
		.filter(|resolved| !resolved.iter().any(|member| member == &data.path))
		.collect()
}

/// The repository files `source` takes `name` in from without declaring it
/// (the named import of `name` and every wildcard import): where a barrel
/// file (a file that only passes names on) sends a lookup next.
pub fn reexport_sources(registry: &Arc<FileRegistry>, source: &ParsedSource, name: &str) -> Vec<String> {
	let data = &source.data;
	if data.symbols.iter().any(|symbol| symbol.name == name) {
		return Vec::new();
	}
	let mut resolver = ImportResolver::with_registry(registry.clone(), &get_language);
	let mut files = Vec::new();
	for binding in &data.import_bindings {
		let takes_the_name =
			binding.imported_name.is_some() && (binding.local_name == name || binding.local_name == WILDCARD);
		if takes_the_name {
			files.extend(resolver.resolve_for(data, &binding.import_path).iter().cloned());
		}
	}
	files
}

/// The names under which `source` passes on `name` of the file `from`: the
/// local name of a named import of it (`export { a as b } from './a'` gives
/// `b`) and `name` itself for a wildcard import. Empty when `source` takes
/// nothing of that name from `from`.
pub fn reexported_as(registry: &Arc<FileRegistry>, source: &ParsedSource, from: &str, name: &str) -> Vec<String> {
	let data = &source.data;
	let mut resolver = ImportResolver::with_registry(registry.clone(), &get_language);
	let mut names = Vec::new();
	for binding in &data.import_bindings {
		let Some(imported) = binding.imported_name.as_deref() else {
			continue;
		};
		let wildcard = binding.local_name == WILDCARD;
		if !wildcard && imported != name {
			continue;
		}
		if resolver
			.resolve_for(data, &binding.import_path)
			.iter()
			.any(|path| path == from)
		{
			names.push(if wildcard {
				name.to_string()
			} else {
				binding.local_name.clone()
			});
		}
	}
	names
}

/// [`declaration_graph`] of `sources` plus the given `(importer, imported)`
/// edges. A file that was not parsed gets a stub node.
pub fn import_edge_graph<S: AsRef<ParsedSource>>(sources: &[S], edges: &[(String, String)]) -> CodeGraph {
	let mut graph = declaration_graph(sources);
	for (importer, imported) in edges {
		graph.relationships.push(CodeRelationship {
			source: importer.clone(),
			target: imported.clone(),
			relation: RelationType::Imports,
			confidence: CONFIDENCE_CERTAIN,
			line: None,
		});
	}
	sort_and_dedup(&mut graph.relationships);
	add_stub_file_nodes(&mut graph);
	graph
}

/// The file's node, one node per declared symbol, and a `Contains` edge from
/// the file to each symbol. Node lines are 1-based.
fn add_file_nodes(graph: &mut CodeGraph, path: &str, language: &str, line_count: u32, symbols: &[SymbolDecl]) {
	let file_name = path.rsplit('/').next().unwrap_or(path);
	graph.nodes.insert(
		path.to_string(),
		CodeNode {
			id: path.to_string(),
			name: file_name.to_string(),
			kind: FILE_KIND.to_string(),
			path: path.to_string(),
			owner: None,
			language: language.to_string(),
			start_line: 1,
			end_line: line_count,
		},
	);

	let symbol_ids = symbol_node_ids(path, symbols);
	for (symbol, symbol_id) in symbols.iter().zip(symbol_ids) {
		graph.nodes.insert(
			symbol_id.clone(),
			CodeNode {
				id: symbol_id.clone(),
				name: symbol.name.clone(),
				kind: symbol.kind.as_str().to_string(),
				path: path.to_string(),
				owner: symbol.owner.clone(),
				language: language.to_string(),
				start_line: symbol.start_line + 1,
				end_line: symbol.end_line + 1,
			},
		);
		graph.relationships.push(CodeRelationship {
			source: path.to_string(),
			target: symbol_id,
			relation: RelationType::Contains,
			confidence: CONFIDENCE_CERTAIN,
			line: None,
		});
	}
}

/// An `Imports` edge per import: to the repository file it resolves to, or to
/// an `external` node named by the import string when it resolves to none. An
/// import that names a package of several files gets one edge, to the
/// package's canonical (alphabetically first) file, and the other files are
/// recorded in `package_of`. An import that resolves to the importing file
/// itself adds no edge.
fn add_import_edges(graph: &mut CodeGraph, files: &[&SymbolFileData], resolver: &mut ImportResolver) {
	for file in files {
		for import in &file.imports {
			let import = import.trim();
			if import.is_empty() {
				continue;
			}
			let resolved = resolver.resolve_for(file, import);
			let target = match resolved.first() {
				None if resolver.is_optional(file, import) => continue,
				None => {
					let id = format!("{EXTERNAL_ID_PREFIX}{import}");
					graph.nodes.entry(id.clone()).or_insert_with(|| CodeNode {
						id: id.clone(),
						name: import.to_string(),
						kind: EXTERNAL_KIND.to_string(),
						path: String::new(),
						owner: None,
						language: String::new(),
						start_line: 0,
						end_line: 0,
					});
					id
				}
				Some(canonical) => {
					for member in resolved.iter().skip(1) {
						graph.package_of.insert(member.clone(), canonical.clone());
					}
					if resolved.iter().any(|member| member == &file.path) {
						continue;
					}
					canonical.clone()
				}
			};
			// A guessed target (found by a name match, not by the path the
			// import states) is
			// no more certain than a name-only call edge.
			let confidence = if !resolved.is_empty() && resolver.is_guess(file, import) {
				CONFIDENCE_NAME_ONLY_MAX
			} else {
				CONFIDENCE_CERTAIN
			};
			graph.relationships.push(CodeRelationship {
				source: file.path.clone(),
				target,
				relation: RelationType::Imports,
				confidence,
				line: None,
			});
		}
	}
}

#[cfg(test)]
#[path = "test_language_tests.rs"]
mod test_language_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
