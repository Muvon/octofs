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
//! (`callers`, `callees`, `supertypes`, `subtypes`, `imports`, `imported_by`)
//! or connect two names (`path`),
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
//!   language;
//! * `path`: both ends at once, one level at a time. The forward side
//!   follows what the bodies of its frontier call (as `callees`), the
//!   backward side what spells the names of its frontier (as `callers`);
//!   each round takes the side whose next level needs fewer unparsed files,
//!   and the search stops when the two sides meet, when `depth`
//!   levels are used up, when a side has nothing left or when the budget ends.

use super::code_index::{CodeIndex, CodeIndexes, FileId, LineKind, ParseLimit, SpellingSet, PARSE_CHUNK};
use super::graph::{
	in_scope, oversize_reason, path_endpoints, start_nodes, GraphRelation, GraphSnapshot, MAX_EXPANDED_DEFINITIONS,
	MAX_REPORTED_HOPS, PATH_RELATIONS,
};
use crate::structural;
use crate::structural::graph::registry::FileRegistry;
use crate::structural::graph::types::{EXTERNAL_KIND, FILE_KIND};
use crate::structural::graph::{
	assemble_filtered_graph_in, declaration_graph, find_paths, import_edge_graph, neighbors, reexport_sources,
	reexported_as, resolved_imports, CodeGraph, Direction, GraphIndex, ParsedSource, RelationFilter, RelationType,
	SkippedFile,
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

/// Files a name is followed through when a file only re-exports it.
const REEXPORT_HOPS: usize = 3;

/// Files parsed per round while no definition of the name has been found.
const DEFINITION_SEARCH_CHUNK: usize = 100;

/// What a scoped query produced.
pub(super) enum Answer {
	Graph {
		snapshot: Box<GraphSnapshot>,
		/// Closing lines about what the cap or the clock left out: the answer
		/// is partial and is not kept.
		notices: Vec<String>,
		/// Closing lines about what the answer did not look at that do not make
		/// it partial: repeating the query would not change them.
		info: Vec<String>,
	},
	/// The query is not traced at all; the text says why and how to narrow it.
	Message(String),
}

/// The two ends of a `path` search and how far each side went.
struct PathProgress {
	from: String,
	to: String,
	forward: usize,
	backward: usize,
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
	/// Set by a `path` search: its two ends and the levels explored from each.
	path_progress: Option<PathProgress>,
	/// Notes that do not make the answer partial.
	info: Vec<String>,
}

impl Query {
	async fn new(index: Arc<CodeIndex>, budget: Option<Duration>) -> Result<Self> {
		let registry = index.registry().await?;
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
			path_progress: None,
			info: Vec::new(),
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
		let mut batch = self.index.parse_many(new, limit).await?;
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

	/// Records that a step could not finish: the search is partial.
	fn note_cut(&mut self, label: &str) {
		if self.cut_at.is_none() {
			self.cut_at = Some(label.to_string());
		}
	}

	/// The files whose lines mention `token` the way `kind` asks, read under
	/// this query's clock. Files not read before it ran out are candidates
	/// the answer did not examine: it is partial, and repeating goes on.
	async fn marked_lines(&mut self, token: &str, kind: LineKind) -> Result<Arc<Vec<FileId>>> {
		let scan = self.index.marked_lines(token, kind, self.deadline).await?;
		if !scan.unscanned.is_empty() {
			self.considered.extend(scan.unscanned.iter().copied());
			self.note_cut(token);
		}
		Ok(scan.files)
	}

	/// The files that spell `name`, read under this query's clock. Files not
	/// read before it ran out are candidates the answer did not examine: it
	/// is partial, and repeating goes on.
	async fn spelling(&mut self, name: &str) -> Result<Arc<SpellingSet>> {
		let set = self.index.spelling(name, self.deadline).await?;
		if !set.unread.is_empty() {
			self.considered.extend(set.unread.iter().copied());
			self.note_cut(name);
		}
		Ok(set)
	}

	/// `marked_lines` among the files of `spelling`, with the same accounting.
	async fn marked_among(
		&mut self,
		name: &str,
		spelling: &SpellingSet,
		token: &str,
		kind: LineKind,
	) -> Result<Arc<Vec<FileId>>> {
		// Files of a partly read spelling are candidates too, so a result
		// cached now stays a superset once the rest is read.
		let candidates: Vec<FileId> = spelling.files.iter().chain(&spelling.unread).copied().collect();
		let scan = self
			.index
			.marked_among(name, &candidates, token, kind, self.deadline)
			.await?;
		if !scan.unscanned.is_empty() {
			self.considered.extend(scan.unscanned.iter().copied());
			self.note_cut(token);
		}
		Ok(scan.files)
	}

	/// A definition search that left candidate files unexamined. Without a
	/// definition yet the answer is partial (repeating goes on); with one, the
	/// answer stands but says how many files were not looked at.
	fn note_unsearched(&mut self, name: &str, found: &Definitions) {
		if found.unexamined.is_empty() {
			return;
		}
		if found.starts.is_empty() {
			self.considered.extend(found.unexamined.iter().copied());
			self.note_cut(name);
		} else {
			self.info.push(format!(
				"[{} more file(s) spell `{name}` without looking like declarations and were not searched for further definitions; give `path` (a file or directory) to search them]",
				found.unexamined.len()
			));
		}
	}

	/// What is left when a call stopped at the cap or the clock. Counted over
	/// the whole query (every name and level followed), so repeating it shows
	/// how far the answer has come.
	fn progress_notice(&self) -> Option<String> {
		let label = self.cut_at.as_ref()?;
		let examined = self.considered.iter().filter(|id| self.have.contains(id)).count();
		let candidates = self.considered.len();
		Some(match &self.path_progress {
			Some(path) => format!(
				"[the search from `{}` to `{}` went {} level(s) forward and {} back over {candidates} candidate files, {examined} examined; repeat the query to examine the rest (parsed files are kept), or narrow it with `path` or `Owner::name`]",
				path.from, path.to, path.forward, path.backward
			),
			None => format!(
				"[`{label}` and what it leads to span {candidates} candidate files, {examined} examined; repeat the query to examine the rest (parsed files are kept), or narrow it with `path` or `Owner::name`]"
			),
		})
	}

	/// What the next answer reads of the calls and type relations grows with
	/// every level: the names (callers, subtypes) or the bodies (callees,
	/// supertypes) of the definitions reached so far.
	/// Whether it asked for something new.
	fn widen_filter(&mut self, graph: &CodeGraph, direction: Direction, frontier: &[String]) -> bool {
		let mut widened = false;
		for node in frontier.iter().filter_map(|id| graph.nodes.get(id)) {
			if node.kind == FILE_KIND || node.kind == EXTERNAL_KIND {
				continue;
			}
			if direction == Direction::Outgoing {
				widened |= self.filter.declared_types.insert(node.name.clone());
				let bodies = self.filter.bodies.entry(node.path.clone()).or_default();
				let body = (node.start_line, node.end_line);
				if !bodies.contains(&body) {
					bodies.push(body);
					widened = true;
				}
			} else {
				widened |= self.filter.names.insert(node.name.clone());
			}
		}
		widened
	}

	fn graph(&self) -> CodeGraph {
		assemble_filtered_graph_in(&self.sources, self.registry.clone(), self.skipped.clone(), &self.filter)
	}

	fn finish(&self, graph: CodeGraph) -> Answer {
		self.index.account_memos(self.have.iter().copied());
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
			}),
			notices: self.progress_notice().into_iter().collect(),
			info: self.info.clone(),
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
			}
			if relations.contains(&RelationType::Extends) || relations.contains(&RelationType::Implements) {
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

	/// The names `frontier` follows (callees or parent types), each with the
	/// files that may define it: the files the frontier files import, and
	/// repository-wide only when few files spell it. `bounded`: stop looking
	/// names up once the budget is spent; the flag says whether every name was.
	async fn followed_candidates(
		&mut self,
		graph: &CodeGraph,
		relations: &[RelationType],
		frontier: &[String],
		bounded: bool,
	) -> Result<(Vec<(String, Vec<FileId>)>, bool)> {
		let names = self.body_names(graph, relations, frontier);
		let from_paths: HashSet<&str> = frontier
			.iter()
			.filter_map(|id| graph.nodes.get(id))
			.map(|node| node.path.as_str())
			.collect();
		let mut imported: HashSet<FileId> = imported_paths(graph, &from_paths)
			.iter()
			.filter_map(|path| self.index.file_id(path))
			.collect();
		// A package sees its own names without importing them.
		for path in &from_paths {
			imported.extend(self.package_mates(path).await?);
		}
		let mut followed = Vec::new();
		let mut looked_up = 0usize;
		for name in names {
			// Looking a name up reads the files that may spell it. Past the
			// budget a step stops there and goes on in the next call, which
			// finds what was read already cached.
			if bounded && looked_up > 0 && self.is_capped() {
				return Ok((followed, false));
			}
			if !self.index.spelling_known(&name) {
				looked_up += 1;
			}
			let candidates = self.index.files_spelling(&[&name]).await?;
			let wanted: Vec<FileId> = if candidates.len() <= MAX_FILES_PER_FOLLOWED_NAME {
				declaring_first(&*self.spelling(&name).await?)
			} else {
				let near: Vec<FileId> = candidates.into_iter().filter(|id| imported.contains(id)).collect();
				if near.is_empty() {
					continue;
				}
				self.index.verify_spelling(near, &[&name]).await?
			};
			followed.push((name, wanted));
		}
		Ok((followed, true))
	}

	async fn add_followed(&mut self, graph: &CodeGraph, relations: &[RelationType], frontier: &[String]) -> Result<()> {
		let (followed, _) = self.followed_candidates(graph, relations, frontier, false).await?;
		for (name, wanted) in followed {
			self.ensure_followed(&name, &wanted).await?;
		}
		Ok(())
	}

	/// Examine the candidates of a followed `name`, then the files they take
	/// the name from without declaring it: a name imported through a barrel
	/// file (one that only passes names on) is declared further on.
	async fn ensure_followed(&mut self, name: &str, wanted: &[FileId]) -> Result<()> {
		self.ensure(wanted, name).await?;
		let mut pending: Vec<FileId> = wanted.to_vec();
		let mut seen: HashSet<FileId> = pending.iter().copied().collect();
		for _ in 0..REEXPORT_HOPS {
			let mut next = Vec::new();
			for id in &pending {
				let Some(source) = self.parsed_source(*id) else {
					continue;
				};
				for path in reexport_sources(&self.registry, &source, name) {
					if let Some(target) = self.index.file_id(&path).filter(|target| seen.insert(*target)) {
						next.push(target);
					}
				}
			}
			if next.is_empty() {
				break;
			}
			self.ensure(&next, name).await?;
			pending = next;
		}
		Ok(())
	}

	/// Candidates for the callers (and, for type edges, the subtypes) of the
	/// definitions in `frontier`.
	async fn add_dependents(
		&mut self,
		graph: &CodeGraph,
		relations: &[RelationType],
		frontier: &[String],
	) -> Result<()> {
		let targets: BTreeSet<(String, String)> = frontier
			.iter()
			.filter_map(|id| graph.nodes.get(id))
			.filter(|node| node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
			.map(|node| (node.name.clone(), node.path.clone()))
			.collect();
		for (name, definition_path) in targets {
			let mut candidates = Vec::new();
			if relations.contains(&RelationType::Calls) {
				candidates = self.call_candidates(graph, &name, &definition_path).await?;
			}
			if relations.contains(&RelationType::Extends) || relations.contains(&RelationType::Implements) {
				candidates.extend(self.marked_lines(&name, LineKind::Inheritance).await?.iter());
			}
			self.ensure(&candidates, &name).await?;
		}
		Ok(())
	}

	/// How many of `files` no query has examined yet, each counted once. A
	/// file an earlier call of a repeated query parsed is free.
	fn unparsed<'a>(&self, files: impl Iterator<Item = &'a FileId>) -> usize {
		let wanted: Vec<FileId> = files
			.filter(|id| !self.have.contains(id))
			.collect::<HashSet<_>>()
			.into_iter()
			.copied()
			.collect();
		self.index.split_examined(&wanted).0.len()
	}

	/// Every file that spells one of the names of `frontier`: the files a
	/// backward step from it would look at.
	async fn spelling_files(&self, graph: &CodeGraph, frontier: &[String]) -> Result<HashSet<FileId>> {
		let names: BTreeSet<&str> = frontier
			.iter()
			.filter_map(|id| graph.nodes.get(id))
			.filter(|node| node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
			.map(|node| node.name.as_str())
			.collect();
		let mut files = HashSet::new();
		for name in names {
			files.extend(self.index.files_spelling(&[name]).await?);
		}
		Ok(files)
	}

	/// Follow the chains the parsed files already hold, in both directions,
	/// before anything new is read: widening the filter makes the next graph
	/// show the calls and type relations of the files parsed so far one more
	/// level out. Rebuilds `graph` while that reveals more.
	fn take_parsed_hops(&mut self, graph: &mut CodeGraph, starts: &[String], ends: &[String], depth: usize) {
		for _ in 0..depth {
			let mut widened = false;
			for level in 0..depth {
				let forward = level_frontier(graph, starts, Direction::Outgoing, level);
				let backward = level_frontier(graph, ends, Direction::Incoming, level);
				if forward.is_empty() && backward.is_empty() {
					break;
				}
				widened |= self.widen_filter(graph, Direction::Outgoing, &forward);
				widened |= self.widen_filter(graph, Direction::Incoming, &backward);
			}
			if !widened {
				return;
			}
			*graph = self.graph();
		}
	}

	/// The files that can call a definition, in the order they are parsed:
	/// its own file, the files that re-export it, the files importing it or a
	/// re-exporter, then (for a name with one definition) every other file
	/// spelling the name in path order.
	///
	/// A re-exporter of a file is a file whose line re-exports from it (the
	/// language says what such a line looks like) and whose parse confirms the
	/// import edge; the name it passes on may be an alias, which the importers
	/// spell. The chain is followed `REEXPORT_HOPS` times.
	async fn call_candidates(&mut self, graph: &CodeGraph, name: &str, definition_path: &str) -> Result<Vec<FileId>> {
		let set = self.spelling(name).await?;
		let definitions = graph
			.nodes
			.values()
			.filter(|node| node.name == name && node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
			.count();
		// Only the files that spell the followed name are read for an import
		// line: a header imported by thousands of files is not searched for
		// importers.
		let mut importing: Vec<FileId> = Vec::new();
		let mut reexporters: Vec<FileId> = Vec::new();
		let mut level: Vec<(String, String)> = vec![(definition_path.to_string(), name.to_string())];
		let mut visited: HashSet<(String, String)> = level.iter().cloned().collect();
		for _ in 0..=REEXPORT_HOPS {
			let mut next = Vec::new();
			for (path, followed) in &level {
				let followed_set = self.spelling(followed).await?;
				let mut candidates: Vec<FileId> = Vec::new();
				for token in file_tokens(self, path) {
					importing.extend(
						self.marked_among(followed, &followed_set, &token, LineKind::Import)
							.await?
							.iter(),
					);
					// A module spelled everywhere is not searched for re-exports.
					if self.index.files_spelling(&[token.as_str()]).await?.len() <= MAX_FILES_SPELLING_TO_TRACE {
						candidates.extend(self.marked_lines(&token, LineKind::ReExport).await?.iter());
					}
				}
				let candidates: Vec<FileId> = candidates
					.into_iter()
					.collect::<BTreeSet<_>>()
					.into_iter()
					.filter(|id| self.index.path_of(*id).is_some_and(|other| other != *path))
					.collect();
				self.ensure(&candidates, name).await?;
				for id in candidates {
					let (Some(source), Some(other)) = (self.parsed_source(id), self.index.path_of(id)) else {
						continue;
					};
					for alias in reexported_as(&self.registry, &source, path, followed) {
						if visited.insert((other.clone(), alias.clone())) {
							reexporters.push(id);
							// The callers spell the alias, so the call filter keeps it too.
							self.filter.names.insert(alias.clone());
							next.push((other.clone(), alias));
						}
					}
				}
			}
			if next.is_empty() {
				break;
			}
			level = next;
		}
		let mut ordered: Vec<FileId> = self.index.file_id(definition_path).into_iter().collect();
		ordered.extend(self.sorted_by_path(reexporters));
		ordered.extend(self.sorted_by_path(importing));
		ordered.extend(self.package_mates(definition_path).await?);
		if definitions <= 1 {
			ordered.extend(set.files.iter());
		}
		// The definition file and the importers come up again in the rest of
		// the list; each file is one candidate.
		let mut seen = HashSet::new();
		ordered.retain(|id| seen.insert(*id));
		Ok(ordered)
	}

	/// The other files that may be in the package of the file `path`: the
	/// callers of a name its package declares need no import. Where the
	/// package is the directory they are its files; where files declare their
	/// package, the files that spell the last segment of its name (a
	/// superset the parse confirms).
	async fn package_mates(&self, path: &str) -> Result<Vec<FileId>> {
		let own = self.index.file_id(path);
		let mates = match structural::package_scope(path) {
			structural::PackageScope::NoPackage => return Ok(Vec::new()),
			structural::PackageScope::Directory => self
				.index
				.files_in_directory(path.rsplit_once('/').map_or("", |(directory, _)| directory)),
			structural::PackageScope::Declared => {
				let package = own
					.and_then(|id| self.parsed_source(id))
					.and_then(|source| source.data().declared_package.clone());
				let segment = package
					.as_deref()
					.and_then(|name| {
						name.rsplit(|character: char| !character.is_alphanumeric() && character != '_')
							.next()
					})
					.filter(|segment| !segment.is_empty());
				match segment {
					Some(segment) => self.index.files_spelling(&[segment]).await?,
					None => return Ok(Vec::new()),
				}
			}
		};
		Ok(self.sorted_by_path(mates.into_iter().filter(|id| Some(*id) != own).collect()))
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

/// What importers of a file spell, without the names of directories that
/// thousands of unrelated files spell: deduplicated, in order.
fn file_tokens(query: &Query, path: &str) -> BTreeSet<String> {
	let source = query.index.file_id(path).and_then(|id| query.parsed_source(id));
	let declared: Vec<&str> = source.as_ref().map_or_else(Vec::new, |source| {
		source
			.data()
			.symbols
			.iter()
			.filter(|symbol| symbol.owner.is_none())
			.map(|symbol| symbol.name.as_str())
			.collect()
	});
	importer_tokens(path, &declared)
}

/// The names importers of the file spell, deduplicated, in order.
fn importer_tokens(path: &str, declared: &[&str]) -> BTreeSet<String> {
	structural::importer_names(path, declared).into_iter().collect()
}

fn last_level_ids(found: &crate::structural::graph::traverse::Neighborhood) -> Vec<String> {
	found
		.levels
		.last()
		.map(|level| level.iter().map(|hop| hop.node_id.clone()).collect())
		.unwrap_or_default()
}

fn too_common_message(name: &str, files: usize) -> String {
	format!(
		"`{name}` is spelled in about {files} files: too common to trace repository-wide. Give `path` (the file or directory of the definition) or `Owner::name`."
	)
}

/// The index of `root`, refreshed.
pub(super) async fn open(root: &Path) -> Result<Arc<CodeIndex>> {
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
	to: Option<&str>,
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
	if relation == GraphRelation::Path {
		let to = to.ok_or_else(|| anyhow!("relation \"path\" needs `to`."))?;
		return path_query(&mut query, name, to, scope, depth).await;
	}
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
	if !structural::is_directory_index(file) {
		return Vec::new();
	}
	let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
	query.index.files_near(directory)
}

/// Importers found without building a graph: each candidate's imports come
/// from the memo on its parsed data, so the cost is linear in the candidates
/// examined by this call and nothing examined earlier is resolved again.
async fn imported_by(query: &mut Query, file: &str, depth: usize) -> Result<Answer> {
	// The names a file declares are among what its importers spell.
	query.ensure_paths(&[file.to_string()], file).await?;
	let mut tokens = file_tokens(query, file);
	// The files whose importers the current level looks for.
	let mut targets: HashSet<String> = HashSet::from([file.to_string()]);
	let mut seen: HashSet<String> = HashSet::from([file.to_string()]);
	let mut importers: Vec<Arc<ParsedSource>> = Vec::new();
	let mut edges: Vec<(String, String)> = Vec::new();
	let nameless = nameless_importers(query, file);
	for level in 1..=depth {
		let mut candidates: Vec<FileId> = Vec::new();
		let mut listed: HashSet<FileId> = HashSet::new();
		for (position, token) in tokens.iter().enumerate() {
			// One token is read in full, so the clock is checked between them.
			if position > 0 && query.is_capped() {
				query.note_cut(token);
				break;
			}
			// A token spelled in thousands of files is still read: the stem of
			// a module (`lifecycle`) is how its importers name it. The scan
			// runs under the query's clock and a repeat goes on.
			for id in query.marked_lines(token, LineKind::Import).await?.iter() {
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
		query.ensure(&candidates, &label).await?;
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
		if level == depth || frontier.is_empty() {
			break;
		}
		tokens = frontier.iter().flat_map(|path| file_tokens(query, path)).collect();
		targets = frontier.into_iter().collect();
	}
	Ok(query.finish(import_edge_graph(&importers, &edges)))
}

/// What a search for definitions found: the declaration graph of every file
/// examined, the matching nodes and the candidate files not looked at.
struct Definitions {
	graph: CodeGraph,
	starts: Vec<String>,
	searched_all: bool,
	unexamined: Vec<FileId>,
}

/// Look for the definitions of `name`: the files that look like declarations
/// first (those under `scope` when given) and, while none holds one, the rest
/// in rounds. The first round takes at most `first_round_limit` files.
async fn find_definitions(
	query: &mut Query,
	name: &str,
	scope: Option<&str>,
	first_round_limit: usize,
) -> Result<Definitions> {
	let set = query.spelling(name).await?;
	let mut ordered = declaring_first(&set);
	if let Some(scope) = scope {
		ordered.retain(|id| {
			query
				.index
				.path_of(*id)
				.is_some_and(|path| in_scope(&path, Some(scope)))
		});
	}
	// The files that look like declarations are examined first and, when
	// they hold no definition, the rest in rounds.
	let mut round = match scope {
		Some(_) => ordered.len(),
		None if set.declaring.is_empty() => DEFINITION_SEARCH_CHUNK,
		None => set.declaring.len(),
	}
	.min(first_round_limit);
	let mut examined = 0;
	loop {
		examined = (examined + round).min(ordered.len());
		query.ensure(&ordered[..examined], name).await?;
		round = DEFINITION_SEARCH_CHUNK;
		// Declarations are all the search for a definition needs.
		let graph = declaration_graph(&query.sources);
		let starts = start_nodes(&graph, name, scope);
		let remaining = ordered.len() - examined;
		// A found definition ends the search: the files not examined are
		// counted in the answer, not parsed for another definition.
		let enough = !starts.is_empty();
		if enough || remaining == 0 || query.is_capped() {
			return Ok(Definitions {
				graph,
				starts,
				searched_all: remaining == 0,
				unexamined: ordered[examined..].to_vec(),
			});
		}
	}
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
	let filter_hits = query.index.files_spelling(&[name]).await?.len();
	if !narrowed && filter_hits > MAX_FILES_SPELLING_TO_TRACE {
		return Ok(Answer::Message(too_common_message(name, filter_hits)));
	}
	let found = find_definitions(query, name, scope, usize::MAX).await?;
	query.note_unsearched(name, &found);
	let Definitions { mut graph, starts, .. } = found;
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

/// The nodes first reached `level` steps from `ids`: the ends themselves at
/// level 0, nothing once the walk ran out before that level.
fn level_frontier(graph: &CodeGraph, ids: &[String], direction: Direction, level: usize) -> Vec<String> {
	if level == 0 {
		return ids.to_vec();
	}
	let index = GraphIndex::new(graph);
	let found = neighbors(graph, &index, ids, &PATH_RELATIONS, direction, level, MAX_REPORTED_HOPS);
	if found.levels.len() < level {
		return Vec::new();
	}
	last_level_ids(&found)
}

/// A chain from `name` to `to`, searched from both ends one level at a time.
/// Forward steps examine the files that define what the frontier's bodies
/// call; backward steps examine the files that spell the frontier's names. Each
/// round takes the side that needs fewer files, and the search stops when
/// the two sides meet, when `depth` levels are used up, when a side has
/// nothing left to explore or when the budget ends.
async fn path_query(query: &mut Query, name: &str, to: &str, scope: Option<&str>, depth: usize) -> Result<Answer> {
	let scope = scope.filter(|path| !path.is_empty());
	let narrowed = scope.is_some() || name.contains("::");
	let filter_hits = query.index.files_spelling(&[name]).await?.len();
	if !narrowed && filter_hits > MAX_FILES_SPELLING_TO_TRACE {
		return Ok(Answer::Message(too_common_message(name, filter_hits)));
	}
	let from = find_definitions(query, name, scope, usize::MAX).await?;
	query.note_unsearched(name, &from);
	if from.starts.is_empty() {
		return Ok(query.finish(from.graph));
	}
	// A common `to` has too many definitions to look all of them up: the
	// first round is one chunk, and a definition the forward side reaches is
	// an end like any other.
	let end = find_definitions(query, to, None, DEFINITION_SEARCH_CHUNK).await?;
	if end.starts.is_empty() && end.searched_all {
		return Ok(query.finish(end.graph));
	}
	// No definition of `to` yet and files left unexamined: if no chain is
	// found either, the answer is partial, never a final "no definition".
	let end_unexamined = if end.starts.is_empty() {
		end.unexamined.clone()
	} else {
		Vec::new()
	};

	let mut graph = query.graph();
	// Levels explored from each end by steps that looked at every file they
	// needed: with `depth` of them in all, no chain is left unsearched.
	let (mut forward, mut backward) = (0usize, 0usize);
	let mut parsing_steps = 0usize;
	loop {
		let (starts, _) = path_endpoints(&graph, name, scope);
		let (ends, _) = path_endpoints(&graph, to, None);
		query.take_parsed_hops(&mut graph, &starts, &ends, depth);
		// The search walks the whole graph: off the async reactor.
		let (searched, found) = tokio::task::spawn_blocking({
			let (starts, ends) = (starts.clone(), ends.clone());
			move || {
				let index = GraphIndex::new(&graph);
				let found = find_paths(&graph, &index, &starts, &ends, &PATH_RELATIONS, depth);
				(graph, found)
			}
		})
		.await
		.map_err(|error| anyhow!("The path search failed: {error}"))?;
		graph = searched;
		// The two sides met, or every level is used.
		if !found.paths.is_empty() || forward + backward >= depth {
			break;
		}
		let forward_frontier = level_frontier(&graph, &starts, Direction::Outgoing, forward);
		let backward_frontier =
			(!ends.is_empty()).then(|| level_frontier(&graph, &ends, Direction::Incoming, backward));
		if forward_frontier.is_empty() || backward_frontier.as_ref().is_some_and(Vec::is_empty) {
			// One side reaches nothing new: no further chain can appear.
			break;
		}
		let (followed, complete) = query
			.followed_candidates(&graph, &PATH_RELATIONS, &forward_frontier, true)
			.await?;
		let forward_files: HashSet<FileId> = followed.iter().flat_map(|(_, files)| files.iter().copied()).collect();
		let forward_cost = query.unparsed(forward_files.iter());
		let backward_files = match &backward_frontier {
			Some(frontier) => Some(query.spelling_files(&graph, frontier).await?),
			None => None,
		};
		let backward_cost = backward_files.as_ref().map(|files| query.unparsed(files.iter()));
		let (backward_is_cheaper, cost) = match backward_cost {
			Some(backward_cost) if backward_cost < forward_cost => (true, backward_cost),
			_ => (false, forward_cost),
		};
		if !complete && !backward_is_cheaper {
			// Looking up what the forward step needs ran out of time: parse
			// what was found (in the first parsing step of a call only) and
			// go on in the next call, which finds the lookups cached.
			if parsing_steps == 0 {
				for (label, files) in &followed {
					query.ensure_followed(label, files).await?;
				}
			}
			query.considered.extend(forward_files);
			query.note_cut(name);
			break;
		}
		// A step over files that are all parsed already is free whatever the
		// clock says. One that parses stops at the budget, except the first of
		// a call: a repeated query always gets further.
		if cost > 0 {
			if parsing_steps > 0 && query.is_capped() {
				query.considered.extend(if backward_is_cheaper {
					backward_files.unwrap_or_default()
				} else {
					forward_files
				});
				query.note_cut(name);
				break;
			}
			parsing_steps += 1;
		}
		if let (true, Some(frontier)) = (backward_is_cheaper, &backward_frontier) {
			query.add_dependents(&graph, &PATH_RELATIONS, frontier).await?;
			query.widen_filter(&graph, Direction::Incoming, frontier);
			backward += 1;
		} else {
			for (label, files) in &followed {
				query.ensure_followed(label, files).await?;
			}
			query.widen_filter(&graph, Direction::Outgoing, &forward_frontier);
			forward += 1;
		}
		graph = query.graph();
	}
	if !end_unexamined.is_empty() && path_endpoints(&graph, to, None).0.is_empty() {
		query.considered.extend(end_unexamined);
		query.note_cut(to);
	}
	query.path_progress = Some(PathProgress {
		from: name.to_string(),
		to: to.to_string(),
		forward,
		backward,
	});
	Ok(query.finish(graph))
}

#[cfg(test)]
#[path = "graph_scoped_tests.rs"]
mod graph_scoped_tests;
