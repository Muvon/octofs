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
//! (`callers`, `callees`, `supertypes`, `subtypes`, `imports`, `imported_by`),
//! built on the code index: the index says which files spell a name, and only
//! the files a rule picks are parsed, level by level.
//!
//! No query parses more than `MAX_PARSED_FILES_PER_QUERY` files; files the
//! index already parsed are free. The candidates are ranked, the best are
//! parsed first, and when the cap cuts the list the answer comes from what
//! was examined and a notice says so. Parsed files are kept, so repeating the
//! query examines the next ones. A ranking can only cost a repeat, never
//! lose a result. There is no fallback to a bigger graph.
//!
//! Rules, by relation:
//! * definitions of `name`: the files that look like declaring it first (the
//!   files under `path` when given), then the other files spelling it;
//!   `Owner::name` needs both names in a file. A name spelled in more than
//!   `MAX_FILES_SPELLING_TO_TRACE` files with no narrowing is not traced;
//! * `callers`: the files spelling the name, those importing the definition
//!   first; a name with several definitions only looks in the definition
//!   files and their importers;
//! * `callees` / `supertypes`: the definition file, then for each followed
//!   name the files the definition file imports that spell it, and
//!   repository-wide only a name spelled in at most
//!   `MAX_FILES_PER_FOLLOWED_NAME` files (`get`, `push` are never searched);
//! * `subtypes`: the files with a line naming the type that may be an
//!   inheritance line of their language;
//! * `imported_by`: the files with a line naming the file (its stem, or its
//!   directory for package files) that may be an import line of their
//!   language.

use super::code_index::{CodeIndex, CodeIndexes, FileId, LineKind, ParseLimit, SpellingSet, PARSE_CHUNK};
use super::graph::{
	oversize_reason, start_nodes, GraphRelation, GraphSnapshot, MAX_EXPANDED_DEFINITIONS, MAX_REPORTED_HOPS,
};
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::types::{EXTERNAL_KIND, FILE_KIND};
use crate::structural::graph::{
	assemble_filtered_graph_in, declaration_graph, import_edge_graph, neighbors, resolved_imports, CodeGraph,
	Direction, GraphIndex, ParsedSource, RelationFilter, RelationType, SkippedFile,
};
use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Files one query may really parse. Cached files do not count.
pub(super) const MAX_PARSED_FILES_PER_QUERY: usize = 600;

/// Time a query may spend after the index refresh. When it runs out, parsing
/// stops and the answer comes from what was examined, with the notice that
/// repeating the query goes on. The file cap is the second limit.
pub(super) const QUERY_TIME_BUDGET_MS: u64 = 800;

/// A name spelled in more files than this, with no `path` or `Owner::name`,
/// is not traced at all: the reply is the file count.
pub(super) const MAX_FILES_SPELLING_TO_TRACE: usize = 3000;

/// A callee name spelled in more files than this is not searched for across
/// the repository (`get`, `push`: they appear nearly everywhere). Such a call
/// still resolves through the imports of the calling file and the file itself.
const MAX_FILES_PER_FOLLOWED_NAME: usize = 200;

/// Files parsed per round while no definition of the name has been found.
const DEFINITION_SEARCH_CHUNK: usize = 100;

/// File stems an import replaces by their directory (`./components` for
/// `components/index.ts`).
const DIRECTORY_STEMS: [&str; 5] = ["index", "mod", "__init__", "lib", "main"];

/// Extensions whose imports name a package directory, not a file.
const PACKAGE_EXTENSIONS: [&str; 3] = ["go", "java", "swift"];

/// What a scoped query produced.
pub(super) enum Answer {
	Graph {
		snapshot: Box<GraphSnapshot>,
		/// Closing lines about what the cap left out.
		notices: Vec<String>,
	},
	/// The query is not traced at all; the text says why and how to narrow it.
	Message(String),
}

struct Query {
	index: Arc<CodeIndex>,
	registry: Arc<FileRegistry>,
	sources: Vec<Arc<ParsedSource>>,
	by_path: HashMap<String, usize>,
	have: HashSet<FileId>,
	skipped: Vec<SkippedFile>,
	fresh: usize,
	/// No fresh parse starts after it; `None` is unlimited.
	deadline: Option<Instant>,
	/// What of the call and type relations the answer needs resolved.
	filter: RelationFilter,
	/// Every file a rule asked to examine, over all names and levels.
	considered: HashSet<FileId>,
	/// The name whose files the cap or the clock cut first.
	cut_at: Option<String>,
}

impl Query {
	async fn new(index: Arc<CodeIndex>, budget: Option<Duration>) -> Result<Self> {
		let span_started = std::time::Instant::now(); // SPANTMP
		let registry = index.registry().await?;
		eprintln!("SPAN registry {:?}", span_started.elapsed()); // SPANTMP
		Ok(Self {
			registry,
			index,
			sources: Vec::new(),
			by_path: HashMap::new(),
			have: HashSet::new(),
			skipped: Vec::new(),
			fresh: 0,
			deadline: budget.map(|budget| Instant::now() + budget),
			filter: RelationFilter::default(),
			considered: HashSet::new(),
			cut_at: None,
		})
	}

	fn is_capped(&self) -> bool {
		self.fresh >= MAX_PARSED_FILES_PER_QUERY || self.deadline.is_some_and(|deadline| Instant::now() >= deadline)
	}

	/// Parse `ids` in the given order, within what is left of the per-query
	/// cap and clock. `label` names the cut in the progress notice.
	async fn ensure(&mut self, ids: &[FileId], label: &str) -> Result<()> {
		self.considered.extend(ids.iter().copied());
		let mut wanted: Vec<FileId> = Vec::new();
		let mut listed = HashSet::new();
		for id in ids {
			if listed.insert(*id) && !self.have.contains(id) {
				wanted.push(*id);
			}
		}
		if wanted.is_empty() {
			return Ok(());
		}
		// Files examined by an earlier call come after the new ones and do not
		// count against the cap: when the memory ceiling evicted their data,
		// parsing it again must not keep a repeated query from getting further.
		let (new, seen_before) = self.index.split_examined(&wanted);
		let limit = ParseLimit {
			max_fresh: MAX_PARSED_FILES_PER_QUERY.saturating_sub(self.fresh),
			// A query always parses one chunk, so repeating one that ran out
			// of time gets further.
			min_fresh: PARSE_CHUNK.saturating_sub(self.fresh),
			deadline: self.deadline,
		};
		let t = std::time::Instant::now(); // SPANTMP
		let mut batch = self.index.parse_many(new, limit).await?;
		eprintln!("SPAN ensure_parse {:?} (fresh {})", t.elapsed(), batch.fresh); // SPANTMP
		self.fresh += batch.fresh;
		let again = ParseLimit {
			max_fresh: usize::MAX,
			min_fresh: 0,
			deadline: self.deadline,
		};
		let reparsed = self.index.parse_many(seen_before, again).await?;
		batch.left_over += reparsed.left_over;
		batch.parsed.extend(reparsed.parsed);
		batch.skipped.extend(reparsed.skipped);
		self.index.mark_examined(batch.parsed.iter().map(|(id, _)| *id));
		for (id, source) in batch.parsed {
			if self.have.insert(id) {
				self.by_path.insert(source.data().path.clone(), self.sources.len());
				self.sources.push(source);
			}
		}
		for skipped in batch.skipped {
			if !self.skipped.iter().any(|known| known.path == skipped.path) {
				self.skipped.push(skipped);
			}
		}
		if batch.left_over > 0 && self.cut_at.is_none() {
			self.cut_at = Some(label.to_string());
		}
		Ok(())
	}

	async fn ensure_paths(&mut self, paths: &[String], label: &str) -> Result<()> {
		let ids: Vec<FileId> = paths.iter().filter_map(|path| self.index.file_id(path)).collect();
		self.ensure(&ids, label).await
	}

	/// What is left when a call stopped at the cap or the clock. Counted over
	/// the whole query (every name and level followed), so repeating it shows
	/// how far the answer has come.
	fn progress_notice(&self) -> Option<String> {
		let label = self.cut_at.as_ref()?;
		let examined = self.considered.iter().filter(|id| self.have.contains(id)).count();
		Some(format!(
			"[`{label}` and what it leads to span {} candidate files, {examined} examined; repeat the query to examine the rest (parsed files are kept), or narrow it with `path` or `Owner::name`]",
			self.considered.len()
		))
	}

	/// What the next answer reads of the calls and type relations grows with
	/// every level: the names (callers, subtypes) or the bodies (callees,
	/// supertypes) of the definitions reached so far.
	fn widen_filter(&mut self, graph: &CodeGraph, direction: Direction, frontier: &[String]) {
		for node in frontier.iter().filter_map(|id| graph.nodes.get(id)) {
			if node.kind == FILE_KIND || node.kind == EXTERNAL_KIND {
				continue;
			}
			if direction == Direction::Outgoing {
				self.filter.declared_types.insert(node.name.clone());
				self.filter
					.bodies
					.entry(node.path.clone())
					.or_default()
					.push((node.start_line, node.end_line));
			} else {
				self.filter.names.insert(node.name.clone());
			}
		}
	}

	fn graph(&self) -> CodeGraph {
		let t = std::time::Instant::now(); // SPANTMP
		let g = assemble_filtered_graph_in(&self.sources, self.registry.clone(), self.skipped.clone(), &self.filter);
		eprintln!("SPAN graph_total {:?} ({} sources)", t.elapsed(), self.sources.len()); // SPANTMP
		g
	}

	fn finish(&self, graph: CodeGraph) -> Answer {
		let span_started = std::time::Instant::now(); // SPANTMP
		self.index.account_memos(self.have.iter().copied());
		eprintln!("SPAN account_memos {:?}", span_started.elapsed()); // SPANTMP
		let index = GraphIndex::new(&graph);
		Answer::Graph {
			snapshot: Box::new(GraphSnapshot {
				graph,
				index,
				unread: self
					.index
					.oversized_paths()
					.into_iter()
					.map(|path| SkippedFile {
						path,
						reason: oversize_reason(),
					})
					.collect(),
				left_out: 0,
				scoped: true,
			}),
			notices: self.progress_notice().into_iter().collect(),
		}
	}

	fn sorted_by_path(&self, mut ids: Vec<FileId>) -> Vec<FileId> {
		ids.sort_by_cached_key(|id| self.index.path_of(*id).unwrap_or_default());
		ids
	}

	fn parsed_source(&self, id: FileId) -> Option<Arc<ParsedSource>> {
		let path = self.index.path_of(id)?;
		self.by_path.get(path.as_str()).map(|slot| self.sources[*slot].clone())
	}

	/// Names the bodies of `node_ids` mention: callees for call edges, parents
	/// for type edges.
	fn body_names(&self, graph: &CodeGraph, relations: &[RelationType], node_ids: &[String]) -> BTreeSet<String> {
		let mut names = BTreeSet::new();
		for node in node_ids.iter().filter_map(|id| graph.nodes.get(id)) {
			if node.kind == FILE_KIND || node.kind == EXTERNAL_KIND {
				continue;
			}
			let Some(source) = self.by_path.get(&node.path).map(|index| &self.sources[*index]) else {
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

	/// The names `frontier` follows (callees or parent types): each is looked
	/// for in the files the frontier files import, and repository-wide only
	/// when few files spell it.
	async fn add_followed(&mut self, graph: &CodeGraph, relations: &[RelationType], frontier: &[String]) -> Result<()> {
		let names = self.body_names(graph, relations, frontier);
		let from_paths: HashSet<&str> = frontier
			.iter()
			.filter_map(|id| graph.nodes.get(id))
			.map(|node| node.path.as_str())
			.collect();
		let imported: HashSet<FileId> = imported_paths(graph, &from_paths)
			.iter()
			.filter_map(|path| self.index.file_id(path))
			.collect();
		for name in names {
			let candidates = self.index.files_spelling(&[&name]).await?;
			let wanted: Vec<FileId> = if candidates.len() <= MAX_FILES_PER_FOLLOWED_NAME {
				declaring_first(&*self.index.spelling(&name).await?)
			} else {
				let near: Vec<FileId> = candidates.into_iter().filter(|id| imported.contains(id)).collect();
				if near.is_empty() {
					continue;
				}
				self.index.verify_spelling(near, &[&name]).await?
			};
			self.ensure(&wanted, &name).await?;
		}
		Ok(())
	}

	/// Candidates for the callers (or, for type edges, the subtypes) of the
	/// definitions in `frontier`.
	async fn add_dependents(
		&mut self,
		graph: &CodeGraph,
		relations: &[RelationType],
		frontier: &[String],
	) -> Result<()> {
		let is_calls = relations.contains(&RelationType::Calls);
		let targets: BTreeSet<(String, String)> = frontier
			.iter()
			.filter_map(|id| graph.nodes.get(id))
			.filter(|node| node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
			.map(|node| (node.name.clone(), node.path.clone()))
			.collect();
		for (name, definition_path) in targets {
			let candidates = if is_calls {
				self.call_candidates(graph, &name, &definition_path).await?
			} else {
				self.index.marked_lines(&name, LineKind::Inheritance).await?.to_vec()
			};
			self.ensure(&candidates, &name).await?;
		}
		Ok(())
	}

	/// The files that can call a definition, in the order they are parsed:
	/// its own file, the files importing it, then (for a name with one
	/// definition) every other file spelling the name in path order.
	async fn call_candidates(&mut self, graph: &CodeGraph, name: &str, definition_path: &str) -> Result<Vec<FileId>> {
		let set = self.index.spelling(name).await?;
		let definitions = graph
			.nodes
			.values()
			.filter(|node| node.name == name && node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
			.count();
		// Only the files that spell the name are read for an import line: a
		// header imported by thousands of files is not searched for importers.
		let mut importing: HashSet<FileId> = HashSet::new();
		for token in file_tokens(definition_path) {
			let span_started = std::time::Instant::now(); // SPANTMP
			importing.extend(
				self.index
					.marked_among(name, &set.files, &token, LineKind::Import)
					.await?
					.iter(),
			);
			eprintln!(
				"SPAN marked_among {:?} ({} spelling)",
				span_started.elapsed(),
				set.files.len()
			); // SPANTMP
		}
		let mut ordered: Vec<FileId> = self.index.file_id(definition_path).into_iter().collect();
		ordered.extend(set.files.iter().filter(|id| importing.contains(id)));
		if definitions <= 1 {
			ordered.extend(set.files.iter());
		}
		// The definition file and the importers come up again in the rest of
		// the list; each file is one candidate.
		let mut seen = HashSet::new();
		ordered.retain(|id| seen.insert(*id));
		Ok(ordered)
	}
}

/// The files of `set` that look like declarations first, then the others.
fn declaring_first(set: &SpellingSet) -> Vec<FileId> {
	let declaring: HashSet<FileId> = set.declaring.iter().copied().collect();
	let mut ordered = set.declaring.clone();
	ordered.extend(set.files.iter().filter(|id| !declaring.contains(id)));
	ordered
}

/// The files the files `from` import: each import's target and, for a
/// package import, every member of the package.
fn imported_paths(graph: &CodeGraph, from: &HashSet<&str>) -> Vec<String> {
	let targets: HashSet<&str> = graph
		.relationships
		.iter()
		.filter(|edge| edge.relation == RelationType::Imports && from.contains(edge.source.as_str()))
		.map(|edge| edge.target.as_str())
		.collect();
	let mut paths: Vec<String> = targets.iter().map(|path| path.to_string()).collect();
	paths.extend(
		graph
			.package_of
			.iter()
			.filter(|(_, canonical)| targets.contains(canonical.as_str()))
			.map(|(member, _)| member.clone()),
	);
	paths
}

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

fn in_scope(path: &str, scope: &str) -> bool {
	path == scope || path.strip_prefix(scope).is_some_and(|rest| rest.starts_with('/'))
}

fn too_common_message(name: &str, files: usize) -> String {
	format!(
		"`{name}` is spelled in about {files} files: too common to trace repository-wide. Give `path` (the file or directory of the definition) or `Owner::name`."
	)
}

/// The index of `root`, refreshed.
pub(super) async fn open(root: &Path) -> Result<Arc<CodeIndex>> {
	eprintln!(
		"SPAN baseline malloc in use {} MB",
		super::code_index::spantmp_malloc_in_use_mb()
	); // SPANTMP
	let index = CodeIndexes::global().index(root);
	index.refresh().await?;
	// The token filters of a cold process are built here, before the query's
	// clock starts: they are one-time work no deadline can cut short.
	index.ensure_ready().await?;
	Ok(index)
}

/// Answer one query on the code index of a workdir.
pub(super) async fn build(
	index: Arc<CodeIndex>,
	relation: GraphRelation,
	name: Option<&str>,
	scope: Option<&str>,
	depth: usize,
	budget: Option<Duration>,
) -> Result<Answer> {
	let mut query = Query::new(index, budget).await?;
	if relation.works_on_files() {
		let file = scope
			.filter(|path| !path.is_empty())
			.ok_or_else(|| anyhow!("relation \"{}\" needs `path`: the file to follow.", relation.noun()))?;
		if query.index.file_id(file).is_none() {
			bail!(
				"`path` {file} is not a source file in the code graph: imports and imported_by take one supported source file that is not gitignored, hidden or over the size limit."
			);
		}
		return if relation == GraphRelation::Imports {
			imports(&mut query, file, depth).await
		} else {
			imported_by(&mut query, file, depth).await
		};
	}
	let name = name.ok_or_else(|| anyhow!("relation \"{}\" needs `name`.", relation.noun()))?;
	symbol_query(&mut query, relation, name, scope, depth).await
}

async fn imports(query: &mut Query, file: &str, depth: usize) -> Result<Answer> {
	query.ensure_paths(&[file.to_string()], file).await?;
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
		query.ensure_paths(&frontier, file).await?;
		graph = query.graph();
	}
	Ok(query.finish(graph))
}

/// Files of the directory of an index-like file and one level below: the
/// importers of a `from . import name` that spells no module name.
fn nameless_importers(query: &Query, file: &str) -> Vec<FileId> {
	let stem = file
		.rsplit('/')
		.next()
		.and_then(|name| name.split('.').next())
		.unwrap_or("");
	if !DIRECTORY_STEMS.contains(&stem) {
		return Vec::new();
	}
	let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
	query.index.files_near(directory)
}

/// Importers found without building a graph: each candidate's imports come
/// from the memo on its parsed data, so the cost is linear in the candidates
/// examined by this call and nothing examined earlier is resolved again.
async fn imported_by(query: &mut Query, file: &str, depth: usize) -> Result<Answer> {
	let mut tokens = file_tokens(file);
	// The files whose importers the current level looks for.
	let mut targets: HashSet<String> = HashSet::from([file.to_string()]);
	let mut seen: HashSet<String> = HashSet::from([file.to_string()]);
	let mut importers: Vec<Arc<ParsedSource>> = Vec::new();
	let mut edges: Vec<(String, String)> = Vec::new();
	let nameless = nameless_importers(query, file);
	for level in 1..=depth {
		let mut candidates: Vec<FileId> = Vec::new();
		let mut listed: HashSet<FileId> = HashSet::new();
		for token in &tokens {
			for id in query.index.marked_lines(token, LineKind::Import).await?.iter() {
				if listed.insert(*id) {
					candidates.push(*id);
				}
			}
		}
		if level == 1 {
			let nameless_ids: Vec<FileId> = nameless.iter().copied().filter(|id| listed.insert(*id)).collect();
			candidates.extend(query.sorted_by_path(nameless_ids));
		}
		let label = tokens.iter().next().cloned().unwrap_or_default();
		eprintln!("SPAN imported_by_candidates ready {}", candidates.len()); // SPANTMP
		query.ensure(&candidates, &label).await?;
		let span_started = std::time::Instant::now(); // SPANTMP
		let mut frontier: Vec<String> = Vec::new();
		for id in &candidates {
			let Some(source) = query.parsed_source(*id) else {
				continue;
			};
			let path = &source.data().path;
			let mut imports_a_target = false;
			for resolved in resolved_imports(&query.registry, &source) {
				if let Some(target) = resolved.iter().find(|member| targets.contains(*member)) {
					edges.push((path.clone(), target.clone()));
					imports_a_target = true;
				}
			}
			if imports_a_target && seen.insert(path.clone()) {
				frontier.push(path.clone());
				importers.push(source.clone());
			}
		}
		eprintln!(
			"SPAN imported_by_resolve {:?} ({} candidates, {} have)",
			span_started.elapsed(),
			candidates.len(),
			query.have.len()
		); // SPANTMP
		if level == depth || frontier.is_empty() {
			break;
		}
		tokens = frontier.iter().flat_map(|path| file_tokens(path)).collect();
		targets = frontier.into_iter().collect();
	}
	Ok(query.finish(import_edge_graph(&importers, &edges)))
}

async fn symbol_query(
	query: &mut Query,
	relation: GraphRelation,
	name: &str,
	scope: Option<&str>,
	depth: usize,
) -> Result<Answer> {
	let Some((relations, direction)) = relation.traversal() else {
		bail!("relation \"{}\" is not answered by a scoped query.", relation.noun());
	};
	let scope = scope.filter(|path| !path.is_empty());
	let narrowed = scope.is_some() || name.contains("::");
	// Counted from the filters alone: nothing is read to say "too common".
	let span_started = std::time::Instant::now(); // SPANTMP
	let filter_hits = query.index.files_spelling(&[name]).await?.len();
	eprintln!("SPAN files_spelling {:?} ({filter_hits})", span_started.elapsed()); // SPANTMP
	if !narrowed && filter_hits > MAX_FILES_SPELLING_TO_TRACE {
		return Ok(Answer::Message(too_common_message(name, filter_hits)));
	}
	let t = std::time::Instant::now(); // SPANTMP
	let set = query.index.spelling(name).await?;
	eprintln!(
		"SPAN spelling {:?} ({} files, {} declaring)",
		t.elapsed(),
		set.files.len(),
		set.declaring.len()
	); // SPANTMP
	let mut ordered = declaring_first(&set);
	if let Some(scope) = scope {
		ordered.retain(|id| query.index.path_of(*id).is_some_and(|path| in_scope(&path, scope)));
	}
	// The files that look like declarations are examined first and, when
	// they hold no definition, the rest in rounds.
	let mut round = match scope {
		Some(_) => ordered.len(),
		None if set.declaring.is_empty() => DEFINITION_SEARCH_CHUNK,
		None => set.declaring.len(),
	};
	let mut examined = 0;
	let mut graph;
	let mut starts;
	loop {
		examined = (examined + round).min(ordered.len());
		query.ensure(&ordered[..examined], name).await?;
		round = DEFINITION_SEARCH_CHUNK;
		// Declarations are all the search for a definition needs.
		graph = declaration_graph(&query.sources);
		starts = start_nodes(&graph, name, scope);
		if !starts.is_empty() || examined >= ordered.len() || query.is_capped() {
			break;
		}
	}
	if starts.len() > MAX_EXPANDED_DEFINITIONS {
		// Listed, not followed: no relations needed.
		return Ok(query.finish(graph));
	}
	if direction == Direction::Outgoing {
		// Following callees reads the imports of the definition files.
		graph = query.graph();
	}
	let mut frontier = starts.clone();
	for level in 1..=depth {
		if frontier.is_empty() {
			break;
		}
		if direction == Direction::Outgoing {
			query.add_followed(&graph, relations, &frontier).await?;
		} else {
			query.add_dependents(&graph, relations, &frontier).await?;
		}
		query.widen_filter(&graph, direction, &frontier);
		graph = query.graph();
		let index = GraphIndex::new(&graph);
		let found = neighbors(&graph, &index, &starts, relations, direction, level, MAX_REPORTED_HOPS);
		frontier = last_level_ids(&found);
	}
	Ok(query.finish(graph))
}

#[cfg(test)]
#[path = "graph_scoped_tests.rs"]
mod graph_scoped_tests;
