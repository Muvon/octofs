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

//! Graph relations of the `symbol` tool (`callers`, `callees`, `imports`,
//! `imported_by`, `supertypes`, `subtypes`, `path`). The graph itself is pure
//! (`crate::structural::graph`); this module owns the I/O: it walks the
//! workdir, reads the files, caches the built graphs and renders the answers.
//!
//! The graph always covers the whole session workdir, because resolving a
//! name needs every file that might define it; the `path` parameter only
//! picks which definitions are start nodes. Local workdirs only.

use super::directory::read_capped;
use super::graph_scoped;
use super::remote::{parse_path_source, resolve_path_source, PathSource};
use super::structure::supported_files_under;
use crate::mcp::{request_ctx, McpToolCall};
use crate::structural::graph::symbols::CONFIDENCE_NAME_ONLY_MAX;
use crate::structural::graph::types::{EXTERNAL_KIND, FILE_KIND};
use crate::structural::graph::{
	build_graph_from_reader, find_paths, neighbors, CodeGraph, CodeNode, Direction, FileSpec, GraphIndex, Hop,
	RelationType, SkippedFile, MAX_PATHS,
};
use crate::structural::{language_for_extension, path_extension};
use crate::utils::truncation::CHARS_PER_TOKEN;
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::fs::Metadata;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

/// Deepest hop count a graph query follows. Each level can multiply the
/// result, so the cap bounds both the work and the response.
pub const MAX_GRAPH_DEPTH: usize = 5;

/// Hop count of every relation but `path`, which looks for a connection and
/// so searches as deep as allowed unless told otherwise.
const DEFAULT_GRAPH_DEPTH: usize = 1;

/// Definitions of `name` (and of `to`) a `path` search starts or ends at; a
/// name defined in more places is searched from the first ones only.
const MAX_PATH_ENDPOINTS: usize = 50;

/// Hops one query collects before the walk stops: far more than a response
/// can hold, so the byte budget normally ends the output first.
pub(super) const MAX_REPORTED_HOPS: usize = 5_000;

/// Definitions of one name a query follows one by one; a name with more is
/// listed instead, so one call never fans out into dozens of sections.
pub(super) const MAX_EXPANDED_DEFINITIONS: usize = 8;

/// Definitions listed when a name has too many to follow.
const MAX_LISTED_DEFINITIONS: usize = 40;

/// Results one traversal lists, most certain first; the closing line counts
/// the rest.
pub(super) const MAX_LISTED_RESULTS: usize = 100;

/// What one graph answer may cost the model: the tools' rough token estimate
/// (`CHARS_PER_TOKEN`) turned into bytes.
const MAX_GRAPH_RESPONSE_TOKENS: usize = 8_000;
const MAX_GRAPH_RESPONSE_BYTES: usize = (MAX_GRAPH_RESPONSE_TOKENS as f64 * CHARS_PER_TOKEN) as usize;

/// Skipped files named in the closing note; the rest are counted.
const SKIPPED_PREVIEW: usize = 3;

/// Response bytes kept free for the closing notes, on top of the truncation
/// notice reserve.
const NOTES_RESERVE: usize = 1024;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Largest source file the graph parses. A syntax tree weighs many times the
/// text, and one parse cannot be interrupted, so a larger file would blow the
/// time budget and the memory peak of a query on its own. Larger files are
/// named in a closing note; `definition`, `references` and `view` are not
/// bound by this limit.
pub(super) const MAX_GRAPH_FILE_BYTES: u64 = BYTES_PER_MB;

/// Most source files one graph holds; the rest of a huge tree is left out.
const MAX_GRAPH_FILES: usize = 20_000;

/// Most source bytes one graph reads, summed over its files.
const MAX_GRAPH_SOURCE_BYTES: u64 = 256 * BYTES_PER_MB;

/// Workdirs whose graph stays cached; the least recently used one is dropped.
const MAX_CACHED_WORKDIRS: usize = 4;

/// Per-workdir build locks kept before unused ones are pruned.
const MAX_BUILD_LOCKS: usize = 16;

const PARSE_FAILED_REASON: &str = "failed to parse";
pub(super) const UNREADABLE_REASON: &str = "unreadable";

/// Edge types the `path` relation follows. Imports and `Contains` edges start
/// at files, which no symbol can reach, so they cannot lie on a symbol path.
const PATH_RELATIONS: [RelationType; 3] = [RelationType::Calls, RelationType::Extends, RelationType::Implements];

/// Languages the outline modes parse but that contribute nothing to the graph
/// (Markdown has no graph language, JSON declares no graph symbols): handing
/// them over would list every such file as skipped or cost a parse for nothing.
const NO_GRAPH_LANGUAGES: [&str; 2] = ["markdown", "json"];

const REMOTE_GRAPH_MESSAGE: &str = "Graph relations (callers, callees, imports, imported_by, supertypes, subtypes, path) run on the local machine: an ssh:// path or a remote workdir is not supported. Use relation \"definition\" or \"references\" for remote files.";

/// The relations answered from the code graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRelation {
	Callers,
	Callees,
	Imports,
	ImportedBy,
	Supertypes,
	Subtypes,
	Path,
}

impl GraphRelation {
	pub fn parse(name: &str) -> Option<Self> {
		Some(match name {
			"callers" => Self::Callers,
			"callees" => Self::Callees,
			"imports" => Self::Imports,
			"imported_by" => Self::ImportedBy,
			"supertypes" => Self::Supertypes,
			"subtypes" => Self::Subtypes,
			"path" => Self::Path,
			_ => return None,
		})
	}

	/// What the relation returns, for the empty-result message.
	pub(super) fn noun(self) -> &'static str {
		match self {
			Self::Callers => "callers",
			Self::Callees => "callees",
			Self::Imports => "imports",
			Self::ImportedBy => "importing files",
			Self::Supertypes => "supertypes",
			Self::Subtypes => "subtypes",
			Self::Path => "path",
		}
	}

	/// Edge types and direction a breadth-first walk follows; `None` for
	/// `path`, which searches between two nodes instead.
	pub(super) fn traversal(self) -> Option<(&'static [RelationType], Direction)> {
		const TYPE_EDGES: &[RelationType] = &[RelationType::Extends, RelationType::Implements];
		Some(match self {
			Self::Callers => (&[RelationType::Calls], Direction::Incoming),
			Self::Callees => (&[RelationType::Calls], Direction::Outgoing),
			Self::Supertypes => (TYPE_EDGES, Direction::Outgoing),
			Self::Subtypes => (TYPE_EDGES, Direction::Incoming),
			Self::Imports => (&[RelationType::Imports], Direction::Outgoing),
			Self::ImportedBy => (&[RelationType::Imports], Direction::Incoming),
			Self::Path => return None,
		})
	}

	pub(super) fn works_on_files(self) -> bool {
		matches!(self, Self::Imports | Self::ImportedBy)
	}
}

/// A validated graph query; `symbols.rs` checks the parameter combination.
pub struct GraphRequest<'a> {
	pub relation: GraphRelation,
	pub name: Option<&'a str>,
	pub to: Option<&'a str>,
	pub path: Option<&'a str>,
	pub depth: Option<usize>,
}

/// The built graph, its edge index and the files that never reached it.
pub(super) struct GraphSnapshot {
	pub(super) graph: CodeGraph,
	pub(super) index: GraphIndex,
	/// Files over the per-file size cap, not handed to the graph.
	pub(super) unread: Vec<SkippedFile>,
	/// Files left out because the file-count or total-size cap was reached.
	pub(super) left_out: usize,
	/// Built from the files a query mentions, not from the whole repository.
	pub(super) scoped: bool,
}

/// Inode and change time make a same-size, same-mtime rewrite visible; other
/// platforms have no such fields and rely on size and mtime.
type ChangeToken = (i64, i64, u64);

#[cfg(unix)]
fn change_token(metadata: &Metadata) -> ChangeToken {
	use std::os::unix::fs::MetadataExt;
	(metadata.ctime(), metadata.ctime_nsec(), metadata.ino())
}

#[cfg(not(unix))]
fn change_token(_metadata: &Metadata) -> ChangeToken {
	(0, 0, 0)
}

struct WalkedFile {
	path: String,
	language: &'static str,
	size: u64,
	modified_nanos: Option<u128>,
	change: ChangeToken,
}

struct CachedGraph {
	root: PathBuf,
	stamp: u64,
	snapshot: Arc<GraphSnapshot>,
	last_used: u64,
}

#[derive(Default)]
struct GraphCache {
	entries: Vec<CachedGraph>,
	/// Logical clock for least-recently-used eviction.
	clock: u64,
}

// A few entries: a session works in one repository at a time, but switching
// workdirs back and forth must not rebuild every time. The lock is never held
// across `.await`.
static GRAPH_CACHE: Mutex<GraphCache> = Mutex::new(GraphCache {
	entries: Vec::new(),
	clock: 0,
});

// One async lock per workdir root: concurrent cold calls queue behind the
// first build and then hit its cache entry.
static BUILD_LOCKS: Mutex<Vec<(PathBuf, Arc<tokio::sync::Mutex<()>>)>> = Mutex::new(Vec::new());

/// Graph builds so far, for the single-flight test.
#[cfg(test)]
static GRAPH_BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Answer a graph relation. Local only; the graph scope is `call.workdir`.
pub async fn execute_graph(call: &McpToolCall, request: &GraphRequest<'_>) -> Result<String> {
	if parse_path_source(&call.workdir.to_string_lossy()).is_remote() {
		bail!(REMOTE_GRAPH_MESSAGE);
	}
	let root = tokio::fs::canonicalize(&call.workdir)
		.await
		.with_context(|| format!("Cannot resolve the workdir {}", call.workdir.display()))?;
	let scope = resolve_scope(call, &root, request.path).await?;
	let depth = effective_depth(request);
	let mut cap_notices = Vec::new();
	let mut cache_slot = None;
	let snapshot = if request.relation.traversal().is_some() {
		let index = graph_scoped::open(&root).await?;
		let key = format!("{:?}|{:?}|{scope:?}|{depth}", request.relation, request.name);
		if let Some(text) = index.cached_answer(&key) {
			return Ok(text.to_string());
		}
		cache_slot = Some((index.clone(), key, index.generation()));
		let budget = Some(Duration::from_millis(graph_scoped::QUERY_TIME_BUDGET_MS));
		let built = graph_scoped::build(index, request.relation, request.name, scope.as_deref(), depth, budget).await?;
		match built {
			graph_scoped::Answer::Graph { snapshot, notices } => {
				cap_notices = notices;
				Arc::new(*snapshot)
			}
			graph_scoped::Answer::Message(text) => return Ok(text),
		}
	} else {
		load_graph(&root).await?
	};

	// Traversal and rendering walk the whole graph: off the async reactor.
	let (relation, name, to) = (
		request.relation,
		request.name.map(str::to_string),
		request.to.map(str::to_string),
	);
	let rendered = snapshot.clone();
	let body = tokio::task::spawn_blocking(move || {
		let query = GraphRequest {
			relation,
			name: name.as_deref(),
			to: to.as_deref(),
			path: None,
			depth: None,
		};
		match relation.traversal() {
			Some((relations, direction)) => {
				render_traversal(&rendered, &query, scope.as_deref(), relations, direction, depth)
			}
			None => render_paths(&rendered, &query, scope.as_deref(), depth),
		}
	})
	.await
	.map_err(|e| anyhow!("Failed to query the code graph: {e}"))??;

	let mut result = body;
	let complete = cap_notices.is_empty();
	for note in notes(&snapshot).into_iter().chain(cap_notices) {
		result.push('\n');
		result.push_str(&note);
	}
	// A partial answer is not kept: repeating the query continues it.
	if let Some((index, key, generation)) = cache_slot.filter(|_| complete) {
		index.store_answer(key, result.clone(), generation);
	}
	Ok(result)
}

fn effective_depth(request: &GraphRequest<'_>) -> usize {
	let default = if request.relation == GraphRelation::Path {
		MAX_GRAPH_DEPTH
	} else {
		DEFAULT_GRAPH_DEPTH
	};
	let depth = request.depth.unwrap_or(default);
	if depth > MAX_GRAPH_DEPTH {
		request_ctx::push_hint(&format!(
			"`depth` is capped at {MAX_GRAPH_DEPTH}; used {MAX_GRAPH_DEPTH}."
		));
		return MAX_GRAPH_DEPTH;
	}
	depth
}

/// The `path` parameter as a workdir-relative, forward-slash string (empty
/// for the workdir itself), or `None` when the call gave no `path`.
async fn resolve_scope(call: &McpToolCall, root: &Path, raw: Option<&str>) -> Result<Option<String>> {
	let Some(raw) = raw else {
		return Ok(None);
	};
	let PathSource::Local(local) = resolve_path_source(raw, &call.workdir) else {
		bail!(REMOTE_GRAPH_MESSAGE);
	};
	let canonical = tokio::fs::canonicalize(&local)
		.await
		.map_err(|_| anyhow!("Path not found: {raw} (resolved to {})", local.display()))?;
	let relative = canonical.strip_prefix(root).map_err(|_| {
		anyhow!(
			"`path` {raw} is outside the workdir {}: graph relations resolve names across the whole workdir. Switch workdir with the `workdir` tool, or pass a path inside it.",
			root.display()
		)
	})?;
	let parts: Vec<_> = relative
		.components()
		.map(|part| part.as_os_str().to_string_lossy())
		.collect();
	Ok(Some(parts.join("/")))
}

// ── Graph loading and cache ────────────────────────────────────────────────

fn build_lock(root: &Path) -> Arc<tokio::sync::Mutex<()>> {
	let mut locks = BUILD_LOCKS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
	if let Some((_, lock)) = locks.iter().find(|(path, _)| path == root) {
		return lock.clone();
	}
	if locks.len() >= MAX_BUILD_LOCKS {
		locks.retain(|(_, lock)| Arc::strong_count(lock) > 1);
	}
	let lock = Arc::new(tokio::sync::Mutex::new(()));
	locks.push((root.to_path_buf(), lock.clone()));
	lock
}

/// The graph of every supported file under `root`, rebuilt only when the
/// stamp of the walked files differs from the cached one. Concurrent calls
/// for one root wait for a single build instead of each starting their own.
async fn load_graph(root: &Path) -> Result<Arc<GraphSnapshot>> {
	let lock = build_lock(root);
	let _single_flight = lock.lock().await;
	let root = root.to_path_buf();
	tokio::task::spawn_blocking(move || load_graph_blocking(&root))
		.await
		.map_err(|e| anyhow!("Failed to build the code graph: {e}"))?
}

pub(super) fn load_graph_blocking(root: &Path) -> Result<Arc<GraphSnapshot>> {
	let files = walk_source_files(root)?;
	let stamp = stamp_of(&files);
	if let Some(snapshot) = cached_snapshot(root, stamp) {
		return Ok(snapshot);
	}
	let snapshot = Arc::new(build_snapshot(root, &files));
	store_snapshot(root, stamp, snapshot.clone());
	Ok(snapshot)
}

fn cached_snapshot(root: &Path, stamp: u64) -> Option<Arc<GraphSnapshot>> {
	let mut cache = GRAPH_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
	cache.clock += 1;
	let clock = cache.clock;
	let entry = cache
		.entries
		.iter_mut()
		.find(|entry| entry.stamp == stamp && entry.root == root)?;
	entry.last_used = clock;
	Some(entry.snapshot.clone())
}

fn store_snapshot(root: &Path, stamp: u64, snapshot: Arc<GraphSnapshot>) {
	let mut cache = GRAPH_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
	cache.clock += 1;
	let last_used = cache.clock;
	cache.entries.retain(|entry| entry.root != root);
	cache.entries.push(CachedGraph {
		root: root.to_path_buf(),
		stamp,
		snapshot,
		last_used,
	});
	if cache.entries.len() > MAX_CACHED_WORKDIRS {
		if let Some(oldest) = cache
			.entries
			.iter()
			.enumerate()
			.min_by_key(|(_, entry)| entry.last_used)
			.map(|(position, _)| position)
		{
			cache.entries.remove(oldest);
		}
	}
}

/// Supported graph files under `root`, gitignore-aware, as workdir-relative
/// paths with their language.
pub(super) fn walk_supported(root: &Path) -> Result<Vec<(String, &'static str)>> {
	let paths = supported_files_under(&root.to_string_lossy(), root, None, None, false)
		.map_err(|e| anyhow!("{e}"))?
		.unwrap_or_default();
	Ok(paths
		.into_iter()
		.filter_map(|path| {
			let language =
				language_for_extension(&path_extension(&path)).filter(|l| !NO_GRAPH_LANGUAGES.contains(l))?;
			Some((path, language))
		})
		.collect())
}

/// Supported files under `root` with the metadata the stamp needs. A file
/// that vanishes between listing and stat is dropped.
fn walk_source_files(root: &Path) -> Result<Vec<WalkedFile>> {
	// One stat per file dominates a warm call on a large tree, so they run in
	// parallel; `collect` keeps the walk order the stamp depends on.
	Ok(walk_supported(root)?
		.into_par_iter()
		.filter_map(|(path, language)| {
			let metadata = std::fs::metadata(root.join(&path)).ok()?;
			let modified_nanos = metadata
				.modified()
				.ok()
				.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
				.map(|since_epoch| since_epoch.as_nanos());
			Some(WalkedFile {
				path,
				language,
				size: metadata.len(),
				modified_nanos,
				change: change_token(&metadata),
			})
		})
		.collect())
}

/// Hash over (relative path, size, mtime, ctime, inode) of every walked file:
/// any edit, addition or removal changes it, and no file content is read to
/// find out.
fn stamp_of(files: &[WalkedFile]) -> u64 {
	let mut hasher = DefaultHasher::new();
	for file in files {
		file.path.hash(&mut hasher);
		file.size.hash(&mut hasher);
		file.modified_nanos.hash(&mut hasher);
		file.change.hash(&mut hasher);
	}
	hasher.finish()
}

pub(super) fn oversize_reason() -> String {
	format!("over the {} MB limit", MAX_GRAPH_FILE_BYTES / BYTES_PER_MB)
}

pub(super) fn read_source(root: &Path, path: &str) -> Result<String> {
	read_capped(&root.join(path))
		.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
		.ok_or_else(|| anyhow!(UNREADABLE_REASON))
}

fn build_snapshot(root: &Path, files: &[WalkedFile]) -> GraphSnapshot {
	#[cfg(test)]
	GRAPH_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

	let mut specs = Vec::new();
	let mut unread = Vec::new();
	let mut left_out = 0usize;
	let mut total_bytes = 0u64;
	for file in files {
		// A megabyte-scale file (generated code, test fixtures) costs more to
		// parse, in time and in tree memory, than it can tell.
		if file.size > MAX_GRAPH_FILE_BYTES {
			unread.push(SkippedFile {
				path: file.path.clone(),
				reason: oversize_reason(),
			});
			continue;
		}
		if specs.len() >= MAX_GRAPH_FILES || total_bytes + file.size > MAX_GRAPH_SOURCE_BYTES {
			left_out += 1;
			continue;
		}
		total_bytes += file.size;
		specs.push(FileSpec {
			path: file.path.clone(),
			language: file.language,
		});
	}
	let graph = build_graph_from_reader(specs, &|path: &str| read_source(root, path));
	let index = GraphIndex::new(&graph);
	GraphSnapshot {
		graph,
		index,
		unread,
		left_out,
		scoped: false,
	}
}

// ── Queries ────────────────────────────────────────────────────────────────

fn qualified_name(node: &CodeNode) -> String {
	match &node.owner {
		Some(owner) if !owner.is_empty() => format!("{owner}::{}", node.name),
		_ => node.name.clone(),
	}
}

fn in_scope(path: &str, scope: Option<&str>) -> bool {
	match scope {
		None | Some("") => true,
		Some(scope) => path == scope || path.strip_prefix(scope).is_some_and(|rest| rest.starts_with('/')),
	}
}

/// Ids of the symbol nodes called `name` (or `Owner::name`) inside `scope`.
pub(super) fn start_nodes(graph: &CodeGraph, name: &str, scope: Option<&str>) -> Vec<String> {
	graph
		.nodes
		.values()
		.filter(|node| node.kind != FILE_KIND && node.kind != EXTERNAL_KIND)
		.filter(|node| node.name == name || qualified_name(node) == name)
		.filter(|node| in_scope(&node.path, scope))
		.map(|node| node.id.clone())
		.collect()
}

pub(super) fn render_traversal(
	snapshot: &GraphSnapshot,
	request: &GraphRequest<'_>,
	scope: Option<&str>,
	relations: &[RelationType],
	direction: Direction,
	depth: usize,
) -> Result<String> {
	let graph = &snapshot.graph;
	let relation = request.relation;
	let (starts, target) = if relation.works_on_files() {
		let file = scope
			.filter(|path| !path.is_empty())
			.ok_or_else(|| anyhow!("relation \"{}\" needs `path`: the file to follow.", relation.noun()))?;
		// A scoped build validated the file against the walk already.
		let is_source_file = snapshot.scoped || graph.nodes.get(file).is_some_and(|node| node.kind == FILE_KIND);
		if !is_source_file {
			bail!(
				"`path` {file} is not a source file in the code graph: imports and imported_by take one supported source file that is not gitignored, hidden or over the size limit."
			);
		}
		// Every file of a package answers for its canonical file.
		let start = match graph.package_of.get(file) {
			Some(canonical) if relation == GraphRelation::ImportedBy => canonical.clone(),
			_ => file.to_string(),
		};
		(vec![start], file.to_string())
	} else {
		let name = request
			.name
			.ok_or_else(|| anyhow!("relation \"{}\" needs `name`.", relation.noun()))?;
		let starts = start_nodes(graph, name, scope);
		if starts.is_empty() {
			return Ok(no_definition(name, scope));
		}
		if starts.len() > MAX_EXPANDED_DEFINITIONS {
			return Ok(list_definitions(graph, name, &starts));
		}
		(starts, format!("\"{name}\""))
	};

	let sectioned = starts.len() > 1;
	let mut report = Report::default();
	let mut total_hops = 0usize;
	let mut shown_hops = 0usize;
	let mut capped = false;
	let mut listed_cut = false;
	for start in &starts {
		if listed_cut {
			// The starts left are not walked, so the total is a lower bound.
			capped = true;
			break;
		}
		// A full report or hop budget ends the walk: more starts would only
		// be examined to be thrown away.
		if report.omitted > 0 || total_hops >= MAX_REPORTED_HOPS {
			capped = true;
			break;
		}
		let found = neighbors(
			graph,
			&snapshot.index,
			std::slice::from_ref(start),
			relations,
			direction,
			depth,
			MAX_REPORTED_HOPS - total_hops,
		);
		capped |= found.capped;
		total_hops += found.levels.iter().map(Vec::len).sum::<usize>();
		if sectioned {
			if let Some(node) = graph.nodes.get(start) {
				report.push(node_label(node, " "));
			}
			if found.levels.is_empty() {
				report.push("  (none)".to_string());
			}
		}
		for (index, level) in found.levels.iter().enumerate() {
			if shown_hops >= MAX_LISTED_RESULTS {
				listed_cut = true;
				break;
			}
			report.push(format!("depth {}:", index + 1));
			for row in ranked_rows(graph, level) {
				if shown_hops >= MAX_LISTED_RESULTS {
					listed_cut = true;
					break;
				}
				if report.push(format!("  {row}")) {
					shown_hops += 1;
				}
			}
		}
	}
	if total_hops == 0 {
		return Ok(format!(
			"No {} found for {target} within depth {depth}: relations are resolved by name and imports, not types, so dynamic dispatch and cross-language calls do not appear. Try relation \"references\" for a text-level search.",
			relation.noun()
		));
	}
	let mut text = report.finish();
	if shown_hops < total_hops || capped {
		let at_least = if capped { "at least " } else { "" };
		if listed_cut {
			text.push_str(&format!(
				"\n[+{} more — narrow with `path` or `Owner::name`, or lower `depth`; {at_least}{total_hops} in all, certain ones first then by path]",
				total_hops - shown_hops
			));
		} else {
			text.push_str(&format!(
				"\n[showing {shown_hops} of {at_least}{total_hops} results, certain ones first then by path: narrow with `path` or `Owner::name`, or lower `depth`]"
			));
		}
	}
	Ok(text)
}

/// The hops of one level as rendered rows, certain edges before `(guess)`
/// ones and each group by path and line, so a cut keeps the best results.
fn ranked_rows(graph: &CodeGraph, level: &[Hop]) -> Vec<String> {
	let mut rows: Vec<(bool, &str, u32, String)> = level
		.iter()
		.map(|hop| {
			let node = graph.nodes.get(&hop.node_id);
			let guess = graph
				.relationships
				.get(hop.via)
				.is_some_and(|relationship| relationship.confidence <= CONFIDENCE_NAME_ONLY_MAX);
			(
				guess,
				node.map_or("", |node| node.path.as_str()),
				node.map_or(0, |node| node.start_line),
				hop_label(graph, hop),
			)
		})
		.collect();
	rows.sort();
	rows.into_iter().map(|(_, _, _, label)| label).collect()
}

/// A name with too many definitions to walk each one: the definitions
/// themselves, so the caller can pick one with `Owner::name` or `path`.
fn list_definitions(graph: &CodeGraph, name: &str, starts: &[String]) -> String {
	let mut nodes: Vec<&CodeNode> = starts.iter().filter_map(|id| graph.nodes.get(id)).collect();
	nodes.sort_by(|a, b| (&a.path, a.start_line).cmp(&(&b.path, b.start_line)));
	let mut report = Report::default();
	report.push(format!(
		"`{name}` has {} definitions: too many to follow at once. Pick one with `Owner::name` or narrow with `path`.",
		nodes.len()
	));
	for node in nodes.iter().take(MAX_LISTED_DEFINITIONS) {
		report.push(format!("  {}", node_label(node, "  ")));
	}
	let mut text = report.finish();
	if nodes.len() > MAX_LISTED_DEFINITIONS {
		text.push_str(&format!("\n  [+{} more]", nodes.len() - MAX_LISTED_DEFINITIONS));
	}
	text
}

/// `ids` cut to `MAX_PATH_ENDPOINTS`, with a note saying so.
fn cap_endpoints(mut ids: Vec<String>, name: &str, notes: &mut Vec<String>) -> Vec<String> {
	if ids.len() > MAX_PATH_ENDPOINTS {
		notes.push(format!(
			"[`{name}` has {} definitions: searched with the first {MAX_PATH_ENDPOINTS}; narrow with `path`]",
			ids.len()
		));
		ids.truncate(MAX_PATH_ENDPOINTS);
	}
	ids
}

fn render_paths(
	snapshot: &GraphSnapshot,
	request: &GraphRequest<'_>,
	scope: Option<&str>,
	depth: usize,
) -> Result<String> {
	let graph = &snapshot.graph;
	let (Some(name), Some(to)) = (request.name, request.to) else {
		bail!("relation \"path\" needs `name` and `to`.");
	};
	let starts = start_nodes(graph, name, scope);
	if starts.is_empty() {
		return Ok(no_definition(name, scope));
	}
	let ends = start_nodes(graph, to, None);
	if ends.is_empty() {
		return Ok(no_definition(to, None));
	}
	let mut notes = Vec::new();
	let starts = cap_endpoints(starts, name, &mut notes);
	let ends = cap_endpoints(ends, to, &mut notes);

	// One search for every start and end definition, not one per pair.
	let search = find_paths(graph, &snapshot.index, &starts, &ends, &PATH_RELATIONS, depth);
	let mut text = if search.paths.is_empty() {
		if search.work_cap_hit {
			format!(
				"The path search from \"{name}\" to \"{to}\" stopped at its work limit before finding a path within depth {depth}: the graph is too connected around them. Lower `depth` or narrow `name` with `path`."
			)
		} else {
			format!(
				"No path from \"{name}\" to \"{to}\" within depth {depth} over calls, extends and implements. Raise `depth` (max {MAX_GRAPH_DEPTH}) or try relation \"references\" for a text-level search."
			)
		}
	} else {
		let mut report = Report::default();
		for path in &search.paths {
			report.push(chain_label(graph, path));
		}
		report.finish()
	};
	if search.path_cap_hit {
		notes.push(format!("[showing the first {MAX_PATHS} paths; more exist]"));
	}
	if search.work_cap_hit && !search.paths.is_empty() {
		notes.push("[the search stopped at its work limit: some paths may be missing]".to_string());
	}
	for note in notes {
		text.push('\n');
		text.push_str(&note);
	}
	Ok(text)
}

fn no_definition(name: &str, scope: Option<&str>) -> String {
	let place = match scope {
		Some(scope) if !scope.is_empty() => format!(" under {scope}"),
		_ => String::new(),
	};
	format!(
		"No definition of \"{name}\" found in the code graph{place}. Graph relations match the exact name across the workdir's supported source files; check the spelling or try relation \"references\" for a text-level search."
	)
}

// ── Rendering ──────────────────────────────────────────────────────────────

/// `path:line<sep>kind<sep>Owner::name`; a file node is its path and an
/// external import reads `external: <import string>`.
fn node_label(node: &CodeNode, separator: &str) -> String {
	match node.kind.as_str() {
		EXTERNAL_KIND => format!("external: {}", node.name),
		FILE_KIND => format!("{}{separator}file", node.path),
		kind => format!(
			"{}:{}{separator}{kind}{separator}{}",
			node.path,
			node.start_line,
			qualified_name(node)
		),
	}
}

/// A hop's node. A file reached through a call (code outside any function)
/// shows the call line; `(guess)` marks an edge that rests on a name match
/// alone, every higher confidence tier is left unmarked.
fn hop_label(graph: &CodeGraph, hop: &Hop) -> String {
	let Some(node) = graph.nodes.get(&hop.node_id) else {
		return hop.node_id.clone();
	};
	let relationship = graph.relationships.get(hop.via);
	let call_line = relationship
		.filter(|relationship| relationship.relation == RelationType::Calls && node.kind == FILE_KIND)
		.and_then(|relationship| relationship.line);
	let mut label = match call_line {
		Some(line) => format!("{}:{line}  file", node.path),
		None => node_label(node, "  "),
	};
	if relationship.is_some_and(|relationship| relationship.confidence <= CONFIDENCE_NAME_ONLY_MAX) {
		label.push_str(" (guess)");
	}
	label
}

/// One path as `a -calls-> b -extends-> c`.
fn chain_label(graph: &CodeGraph, path: &[String]) -> String {
	let mut chain = String::new();
	for (index, id) in path.iter().enumerate() {
		if index > 0 {
			let previous = &path[index - 1];
			let relation = graph
				.relationships
				.iter()
				.find(|r| &r.source == previous && &r.target == id)
				.map_or("related", |r| r.relation.as_str());
			chain.push_str(&format!(" -{relation}-> "));
		}
		match graph.nodes.get(id) {
			Some(node) if node.kind == FILE_KIND => chain.push_str(&node.path),
			Some(node) if node.kind == EXTERNAL_KIND => chain.push_str(&node_label(node, " ")),
			Some(node) => chain.push_str(&format!("{}:{} {}", node.path, node.start_line, qualified_name(node))),
			None => chain.push_str(id),
		}
	}
	chain
}

/// Closing lines about what is missing from the graph: skipped files and a cap
/// that cut it.
fn notes(snapshot: &GraphSnapshot) -> Vec<String> {
	let mut lines = Vec::new();
	lines.extend(skipped_note(snapshot));
	if snapshot.left_out > 0 {
		lines.push(format!(
			"[graph limited to {MAX_GRAPH_FILES} files or {} MB of source: {} more file(s) left out]",
			MAX_GRAPH_SOURCE_BYTES / BYTES_PER_MB,
			snapshot.left_out
		));
	}
	lines
}

/// One line naming the files that are not in the graph, or `None`.
fn skipped_note(snapshot: &GraphSnapshot) -> Option<String> {
	let entries: Vec<(&str, &str)> = snapshot
		.unread
		.iter()
		.map(|file| (file.path.as_str(), file.reason.as_str()))
		.chain(snapshot.graph.skipped.iter().map(|file| {
			let reason = if file.reason == UNREADABLE_REASON {
				UNREADABLE_REASON
			} else {
				PARSE_FAILED_REASON
			};
			(file.path.as_str(), reason)
		}))
		.collect();
	if entries.is_empty() {
		return None;
	}
	let shown: Vec<String> = entries
		.iter()
		.take(SKIPPED_PREVIEW)
		.map(|(path, reason)| format!("{path} ({reason})"))
		.collect();
	let mut note = format!("[{} file(s) not in the graph: {}", entries.len(), shown.join(", "));
	if entries.len() > SKIPPED_PREVIEW {
		note.push_str(&format!(", +{} more", entries.len() - SKIPPED_PREVIEW));
	}
	note.push(']');
	Some(note)
}

/// Output lines under the response budget; whole lines past it are counted
/// and replaced by one notice.
#[derive(Default)]
struct Report {
	lines: Vec<String>,
	used: usize,
	omitted: usize,
}

impl Report {
	/// Whether the line fit; a line past the budget is only counted.
	fn push(&mut self, line: String) -> bool {
		let budget = MAX_GRAPH_RESPONSE_BYTES.saturating_sub(NOTES_RESERVE);
		if self.omitted == 0 && self.used + line.len() < budget {
			self.used += line.len() + 1;
			self.lines.push(line);
			true
		} else {
			self.omitted += 1;
			false
		}
	}

	fn finish(self) -> String {
		self.lines.join("\n")
	}
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod graph_tests;
