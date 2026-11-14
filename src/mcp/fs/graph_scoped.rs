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

//! Query-scoped graphs for the relations that start at one name or one file
//! (`callers`, `callees`, `supertypes`, `subtypes`, `imports`, `imported_by`).
//!
//! An edge that involves the name `X` can only come from a file whose text
//! spells `X`: the call site, the definition, the import line and the
//! `extends` clause all do. So a query parses just the files that mention the
//! names it follows, level by level, instead of every file of the repository.
//! Import resolution still looks paths up in the list of ALL repository
//! files, so the subset graph resolves exactly like the full one and renders
//! the same answer. A query that would need too many files falls back to the
//! cached whole-repository graph. One deliberate difference: a callee name
//! spelled in more than `MAX_FILES_PER_FOLLOWED_NAME` files is not searched for
//! repository-wide, so a `(guess)` edge to it can be missing; calls that
//! resolve through imports or the calling file still appear.

use super::directory::read_capped;
use super::graph::{
	oversize_reason, read_source, start_nodes, walk_supported, GraphRelation, GraphSnapshot, MAX_EXPANDED_DEFINITIONS,
	MAX_REPORTED_HOPS, UNREADABLE_REASON,
};
use crate::structural::graph::types::{EXTERNAL_KIND, FILE_KIND};
use crate::structural::graph::{
	assemble_repository_graph, declaration_graph, neighbors, parse_files, CodeGraph, Direction, FileSpec, GraphIndex,
	ParsedSource, RelationType, SkippedFile,
};
use aho_corasick::{AhoCorasick, MatchKind};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

/// Files a scoped query parses before it gives up and uses the whole-repository
/// graph: parsing this many costs about a second on a multi-core machine.
const MAX_SCOPED_FILES: usize = 3_000;

/// A callee name spelled in more files than this is not searched for across
/// the repository (`get`, `push`: they appear nearly everywhere). Such a call
/// still resolves through the imports of the calling file and the file itself.
const MAX_FILES_PER_FOLLOWED_NAME: usize = 200;

/// Files a common name's definition scan parses to count the definitions.
const MAX_DEFINITION_SCAN_FILES: usize = 12_000;

/// What a scoped build produced.
pub(super) enum Outcome {
	Scoped(Box<GraphSnapshot>),
	/// The query touches too many files to scope; use the full graph.
	TooBroad,
}

type WalkedPath = (String, &'static str);

struct Scoped<'a> {
	root: &'a Path,
	files: &'a [WalkedPath],
	all_paths: Vec<String>,
	parsed: Vec<ParsedSource>,
	parsed_paths: HashSet<String>,
	skipped: Vec<SkippedFile>,
	unread: BTreeMap<String, SkippedFile>,
}

/// Identifier characters: a match that touches one is part of a longer word.
fn is_identifier_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// Which of the automaton's names `bytes` spell as a whole identifier.
fn mentioned_names(automaton: &AhoCorasick, bytes: &[u8], name_count: usize) -> Vec<usize> {
	let mut seen = vec![false; name_count];
	for found in automaton.find_overlapping_iter(bytes) {
		let before = found.start() == 0 || !is_identifier_byte(bytes[found.start() - 1]);
		let after = found.end() == bytes.len() || !is_identifier_byte(bytes[found.end()]);
		if before && after {
			seen[found.pattern().as_usize()] = true;
		}
	}
	seen.into_iter()
		.enumerate()
		.filter_map(|(index, hit)| hit.then_some(index))
		.collect()
}

impl<'a> Scoped<'a> {
	fn new(root: &'a Path, files: &'a [WalkedPath]) -> Self {
		Self {
			root,
			files,
			all_paths: files.iter().map(|(path, _)| path.clone()).collect(),
			parsed: Vec::new(),
			parsed_paths: HashSet::new(),
			skipped: Vec::new(),
			unread: BTreeMap::new(),
		}
	}

	/// Unparsed files whose text spells any of `names` as a whole identifier,
	/// each with the indexes (into `names` in order) of the names it spells.
	fn candidates(&mut self, names: &[&String]) -> Result<Vec<(&'a WalkedPath, Vec<usize>)>> {
		if names.is_empty() {
			return Ok(Vec::new());
		}
		let automaton = AhoCorasick::builder()
			.match_kind(MatchKind::Standard)
			.build(names.iter().map(|name| name.as_bytes()))
			.map_err(|e| anyhow!("Cannot search for the names: {e}"))?;
		let root = self.root;
		let parsed_paths = &self.parsed_paths;
		let outcomes: Vec<(&WalkedPath, Option<Vec<usize>>)> = self
			.files
			.par_iter()
			.filter(|(path, _)| !parsed_paths.contains(path))
			.map(|file| {
				let outcome =
					read_capped(&root.join(&file.0)).map(|bytes| mentioned_names(&automaton, &bytes, names.len()));
				(file, outcome)
			})
			.collect();
		let mut found = Vec::new();
		for (file, outcome) in outcomes {
			match outcome {
				Some(hits) if hits.is_empty() => {}
				Some(hits) => found.push((file, hits)),
				// Over the size cap or unreadable: it could not be parsed
				// either, so name it like the full graph does.
				None => self.note_unreadable(&file.0),
			}
		}
		Ok(found)
	}

	fn note_unreadable(&mut self, path: &str) {
		let oversize =
			std::fs::metadata(self.root.join(path)).is_ok_and(|m| m.len() > super::file_ops::MAX_VIEW_FILE_BYTES);
		let reason = if oversize {
			oversize_reason()
		} else {
			UNREADABLE_REASON.to_string()
		};
		self.unread.entry(path.to_string()).or_insert_with(|| SkippedFile {
			path: path.to_string(),
			reason,
		});
	}

	fn parse(&mut self, files: &[&WalkedPath]) {
		let specs: Vec<FileSpec> = files
			.iter()
			.map(|(path, language)| FileSpec {
				path: path.clone(),
				language,
			})
			.collect();
		let root = self.root;
		let (sources, skipped) = parse_files(specs, &|path: &str| read_source(root, path));
		for source in sources {
			self.parsed_paths.insert(source.data().path.clone());
			self.parsed.push(source);
		}
		for file in &skipped {
			self.parsed_paths.insert(file.path.clone());
		}
		self.skipped.extend(skipped);
	}

	/// Parse the files that mention `names`. `false` when that is more than
	/// `limit` files: nothing is parsed then.
	fn add_mentioning(&mut self, names: &BTreeSet<String>, limit: usize) -> Result<bool> {
		let found: Vec<&WalkedPath> = self
			.candidates(&names.iter().collect::<Vec<_>>())?
			.into_iter()
			.map(|(file, _)| file)
			.collect();
		if self.parsed_paths.len() + found.len() > limit {
			return Ok(false);
		}
		self.parse(&found);
		Ok(true)
	}

	/// Like `add_mentioning`, but a name spelled in more than
	/// `MAX_FILES_PER_FOLLOWED_NAME` files is not followed (`get`, `push`):
	/// its files would be most of the repository. Returns the names left out,
	/// or `None` when the rest still needs more than `limit` files.
	fn add_mentioning_uncommon(&mut self, names: &BTreeSet<String>, limit: usize) -> Result<Option<Vec<String>>> {
		let names: Vec<&String> = names.iter().collect();
		let found = self.candidates(&names)?;
		let mut files_per_name = vec![0usize; names.len()];
		for (_, hits) in &found {
			for hit in hits {
				files_per_name[*hit] += 1;
			}
		}
		let is_common = |index: usize| files_per_name[index] > MAX_FILES_PER_FOLLOWED_NAME;
		let to_parse: Vec<&WalkedPath> = found
			.iter()
			.filter(|(_, hits)| hits.iter().any(|hit| !is_common(*hit)))
			.map(|(file, _)| *file)
			.collect();
		if self.parsed_paths.len() + to_parse.len() > limit {
			return Ok(None);
		}
		self.parse(&to_parse);
		Ok(Some(
			names
				.iter()
				.enumerate()
				.filter(|(index, _)| is_common(*index))
				.map(|(_, name)| (*name).clone())
				.collect(),
		))
	}

	/// Parse exactly these files, when they are supported source files not yet
	/// parsed.
	fn add_files(&mut self, paths: &[String]) {
		let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
		let found: Vec<&WalkedPath> = self
			.files
			.iter()
			.filter(|(path, _)| wanted.contains(path.as_str()) && !self.parsed_paths.contains(path))
			.collect();
		self.parse(&found);
	}

	fn graph(&self) -> CodeGraph {
		assemble_repository_graph(&self.parsed, &self.all_paths, self.skipped.clone())
	}

	fn snapshot(&self, graph: CodeGraph) -> GraphSnapshot {
		let index = GraphIndex::new(&graph);
		GraphSnapshot {
			graph,
			index,
			unread: self.unread.values().cloned().collect(),
			left_out: 0,
			scoped: true,
		}
	}

	fn source_of(&self, path: &str) -> Option<&ParsedSource> {
		self.parsed.iter().find(|source| source.data().path == path)
	}

	/// Names the bodies of `node_ids` mention: callees for call edges, parents
	/// for type edges.
	fn body_names(&self, graph: &CodeGraph, relations: &[RelationType], node_ids: &[String]) -> BTreeSet<String> {
		let mut names = BTreeSet::new();
		for node in node_ids.iter().filter_map(|id| graph.nodes.get(id)) {
			if node.kind == FILE_KIND || node.kind == EXTERNAL_KIND {
				continue;
			}
			let Some(source) = self.source_of(&node.path) else {
				continue;
			};
			let in_body = |line: u32| line + 1 >= node.start_line && line < node.end_line;
			if relations.contains(&RelationType::Calls) {
				for (_, callee) in source.data().calls.iter().filter(|(line, _)| in_body(*line)) {
					names.insert(callee.name.clone());
					// A call through an import alias spells the alias; the
					// definition spells the imported name.
					names.extend(
						source
							.data()
							.import_bindings
							.iter()
							.filter(|binding| binding.local_name == callee.name)
							.filter_map(|binding| binding.imported_name.clone()),
					);
				}
			} else {
				names.extend(
					source
						.data()
						.type_relations
						.iter()
						.filter(|relation| in_body(relation.line))
						.map(|relation| relation.target_name.clone()),
				);
			}
		}
		names
	}
}

/// The files the files of `node_ids` import: where a call to a name too
/// common to search for is still found.
fn imported_files_of(graph: &CodeGraph, node_ids: &[String]) -> Vec<String> {
	let paths: HashSet<&str> = node_ids
		.iter()
		.filter_map(|id| graph.nodes.get(id))
		.map(|node| node.path.as_str())
		.collect();
	graph
		.relationships
		.iter()
		.filter(|relationship| {
			relationship.relation == RelationType::Imports && paths.contains(relationship.source.as_str())
		})
		.map(|relationship| relationship.target.clone())
		.collect()
}

fn simple_name(name: &str) -> &str {
	name.rsplit("::").next().unwrap_or(name)
}

fn node_names(graph: &CodeGraph, ids: &[String]) -> BTreeSet<String> {
	ids.iter()
		.filter_map(|id| graph.nodes.get(id))
		.filter(|node| node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
		.map(|node| node.name.clone())
		.collect()
}

/// Extensions whose imports name a package directory, not a file.
const PACKAGE_EXTENSIONS: [&str; 3] = ["go", "java", "swift"];

/// File stems an import replaces by their directory (`./components` for
/// `components/index.ts`).
const DIRECTORY_STEMS: [&str; 5] = ["index", "mod", "__init__", "lib", "main"];

/// What importers of a file spell: its name without extensions, and for a
/// package file or a directory index also its directory. Any other file's
/// directory (`common`, `src`) is spelled by thousands of unrelated files and
/// would make the query as wide as the repository.
fn file_tokens(path: &str) -> BTreeSet<String> {
	let mut tokens = BTreeSet::new();
	let mut parts = path.rsplit('/');
	let mut names_directory = false;
	if let Some(file_name) = parts.next() {
		let mut pieces = file_name.split('.');
		let stem = pieces.next().unwrap_or(file_name);
		if !stem.is_empty() {
			tokens.insert(stem.to_string());
		}
		names_directory =
			DIRECTORY_STEMS.contains(&stem) || pieces.next_back().is_some_and(|ext| PACKAGE_EXTENSIONS.contains(&ext));
	}
	if let Some(directory) = parts.next().filter(|_| names_directory) {
		if !directory.is_empty() {
			tokens.insert(directory.to_string());
		}
	}
	tokens
}

fn last_level_ids(found: &crate::structural::graph::traverse::Neighborhood) -> Vec<String> {
	found
		.levels
		.last()
		.map(|level| level.iter().map(|hop| hop.node_id.clone()).collect())
		.unwrap_or_default()
}

/// Build the graph a query needs. `Err` is a user-facing message.
pub(super) fn build(
	root: &Path,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
) -> Result<Outcome> {
	let files = walk_supported(root)?;
	let mut query = Scoped::new(root, &files);
	if relation.works_on_files() {
		let file = scope
			.filter(|path| !path.is_empty())
			.ok_or_else(|| anyhow!("relation \"{}\" needs `path`: the file to follow.", relation.noun()))?;
		if !query.files.iter().any(|(path, _)| path == file) {
			bail!(
				"`path` {file} is not a source file in the code graph: imports and imported_by take one supported source file that is not gitignored, hidden or over the size limit."
			);
		}
		return if relation == GraphRelation::Imports {
			imports(&mut query, file, depth)
		} else {
			imported_by(&mut query, file, depth)
		};
	}
	let name = name.ok_or_else(|| anyhow!("relation \"{}\" needs `name`.", relation.noun()))?;
	symbol_query(&mut query, relation, name, scope, depth)
}

fn imports(query: &mut Scoped, file: &str, depth: usize) -> Result<Outcome> {
	query.add_files(&[file.to_string()]);
	let mut graph = query.graph();
	for level in 1..=depth {
		let index = GraphIndex::new(&graph);
		let found = neighbors(
			&graph,
			&index,
			&[file.to_string()],
			&[RelationType::Imports],
			Direction::Outgoing,
			level,
			MAX_REPORTED_HOPS,
		);
		let frontier = last_level_ids(&found);
		if level == depth || frontier.is_empty() {
			break;
		}
		query.add_files(&frontier);
		graph = query.graph();
	}
	Ok(Outcome::Scoped(Box::new(query.snapshot(graph))))
}

fn imported_by(query: &mut Scoped, file: &str, depth: usize) -> Result<Outcome> {
	// `from . import name` spells no module name: the files of a package
	// directory are the importers of its `__init__.py`.
	if file.rsplit('/').next() == Some("__init__.py") {
		let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
		let siblings: Vec<String> = query
			.files
			.iter()
			.filter(|(path, _)| directory.is_empty() || path.starts_with(&format!("{directory}/")))
			.map(|(path, _)| path.clone())
			.collect();
		if siblings.len() > MAX_SCOPED_FILES {
			return Ok(Outcome::TooBroad);
		}
		query.add_files(&siblings);
	}
	let mut tokens = file_tokens(file);
	let mut graph = CodeGraph::default();
	let mut start = file.to_string();
	for level in 1..=depth {
		if !query.add_mentioning(&tokens, MAX_SCOPED_FILES)? {
			return Ok(Outcome::TooBroad);
		}
		graph = query.graph();
		if level == 1 {
			start = graph.package_of.get(file).cloned().unwrap_or_else(|| file.to_string());
		}
		let index = GraphIndex::new(&graph);
		let found = neighbors(
			&graph,
			&index,
			&[start.clone()],
			&[RelationType::Imports],
			Direction::Incoming,
			level,
			MAX_REPORTED_HOPS,
		);
		let frontier = last_level_ids(&found);
		if level == depth || frontier.is_empty() {
			break;
		}
		tokens = frontier.iter().flat_map(|id| file_tokens(id)).collect();
	}
	Ok(Outcome::Scoped(Box::new(query.snapshot(graph))))
}

fn symbol_query(
	query: &mut Scoped,
	relation: GraphRelation,
	name: &str,
	scope: Option<&str>,
	depth: usize,
) -> Result<Outcome> {
	let Some((relations, direction)) = relation.traversal() else {
		bail!("relation \"{}\" is not answered by a scoped query.", relation.noun());
	};
	let simple = simple_name(name).to_string();
	let seed: BTreeSet<String> = BTreeSet::from([simple.clone()]);

	if !query.add_mentioning(&seed, MAX_SCOPED_FILES)? {
		return definitions_scan(query, &seed, name, scope);
	}
	let mut graph = query.graph();
	let starts = start_nodes(&graph, name, scope);
	if starts.len() > MAX_EXPANDED_DEFINITIONS {
		// Listed, not followed: no relations needed.
		return Ok(Outcome::Scoped(Box::new(query.snapshot(graph))));
	}
	let mut frontier = starts.clone();
	for level in 1..=depth {
		if frontier.is_empty() {
			break;
		}
		let names = if direction == Direction::Outgoing {
			query.body_names(&graph, relations, &frontier)
		} else if level > 1 {
			node_names(&graph, &frontier)
		} else {
			BTreeSet::new()
		};
		if !names.is_empty() {
			if direction == Direction::Outgoing {
				let Some(left_out) = query.add_mentioning_uncommon(&names, MAX_SCOPED_FILES)? else {
					return Ok(Outcome::TooBroad);
				};
				if !left_out.is_empty() {
					graph = query.graph();
					query.add_files(&imported_files_of(&graph, &frontier));
				}
			} else if !query.add_mentioning(&names, MAX_SCOPED_FILES)? {
				return Ok(Outcome::TooBroad);
			}
			graph = query.graph();
		}
		let index = GraphIndex::new(&graph);
		let found = neighbors(&graph, &index, &starts, relations, direction, level, MAX_REPORTED_HOPS);
		frontier = last_level_ids(&found);
	}
	Ok(Outcome::Scoped(Box::new(query.snapshot(graph))))
}

/// A name spelled in too many files to follow: count its definitions from a
/// declarations-only parse. Many definitions get listed; a handful means the
/// name is a common call target and needs the full graph.
fn definitions_scan(query: &mut Scoped, seed: &BTreeSet<String>, name: &str, scope: Option<&str>) -> Result<Outcome> {
	if !query.add_mentioning(seed, MAX_DEFINITION_SCAN_FILES)? {
		return Ok(Outcome::TooBroad);
	}
	let graph = declaration_graph(&query.parsed);
	if start_nodes(&graph, name, scope).len() > MAX_EXPANDED_DEFINITIONS {
		return Ok(Outcome::Scoped(Box::new(query.snapshot(graph))));
	}
	Ok(Outcome::TooBroad)
}

#[cfg(test)]
#[path = "graph_scoped_tests.rs"]
mod graph_scoped_tests;
