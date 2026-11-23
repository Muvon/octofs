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

//! The `symbol` tool: code navigation that starts from a symbol name.
//! `definition` and `references` are text-level lookups in a file or under a
//! directory; lookup is pure (`crate::structural::symbols`) and this module
//! owns the I/O, the walk (shared with the outline mode) and the rendering.
//! Every source line renders as an ordinary `N:hh content` line so the ids
//! feed `batch_edit` unchanged. Symbol reads never register in the delta/view
//! cache. The graph relations (`callers`, `callees`, ...) are validated here
//! and answered by `super::graph`.

use super::code_index::{identifier_runs, CodeIndexes, IndexUnavailable};
use super::directory::{join_bounded, missing_path_hint, read_capped, remote_child};
use super::file_ops::MAX_VIEW_FILE_BYTES;
use super::graph::{execute_graph, GraphRelation, GraphRequest, MAX_GRAPH_DEPTH};
use super::remote::{io_exists, io_is_dir, io_metadata, io_read, io_read_capped, resolve_path_source, PathSource};
use super::search::{SearchOutput, ViewFlags};
use super::structure::{join_blocks_budgeted, supported_files_under, supported_remote_files};
use crate::mcp::McpToolCall;
use crate::structural::symbols::{find_definitions, find_references, NameMatcher, SymbolHit};
use crate::utils::line_hash::format_line;
use crate::utils::truncation::{format_megabytes, MAX_RESULT_BYTES};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// An exact-name definition of at most this many lines renders in full;
/// longer definitions and wildcard matches render a bounded preview.
const SMALL_DEFINITION_LINES: usize = 32;
/// Preview lines shown before the omitted-lines marker.
const PREVIEW_LINES: usize = 4;

/// Parameters that only shape a definition or references search; a graph
/// relation rejects them. One list so the check and the message stay together.
const TEXT_SEARCH_PARAMS: [&str; 5] = ["flags", "context", "pattern", "max_depth", "include_hidden"];

/// The `definition` relation: what the tool does when `relation` is omitted.
const DEFINITION_RELATION: &str = "definition";
/// The `references` relation.
const REFERENCES_RELATION: &str = "references";

/// A trimmed string parameter; `None` when absent, null or blank (the default
/// a client fills in for an unused field).
fn optional_text<'a>(call: &'a McpToolCall, key: &str) -> Result<Option<&'a str>> {
	match call.parameters.get(key) {
		None | Some(Value::Null) => Ok(None),
		Some(Value::String(text)) => Ok(Some(text.trim()).filter(|text| !text.is_empty())),
		Some(_) => bail!("`{key}` must be a string — drop it or pass a value."),
	}
}

/// Whether the caller passed a real value. Clients that send every schema
/// field fill the unused ones with defaults (null, "", 0, false): those count
/// as absent.
fn is_set(call: &McpToolCall, key: &str) -> bool {
	match call.parameters.get(key) {
		None | Some(Value::Null) => false,
		Some(Value::String(text)) => !text.is_empty(),
		Some(Value::Number(number)) => number.as_f64() != Some(0.0),
		Some(Value::Bool(flag)) => *flag,
		Some(_) => true,
	}
}

/// The `flags` of a `symbol` call: `c` (count) or `l` (files only), each
/// optionally behind a leading `-`; the lines output is the default. `r` is
/// not a flag here: references are a relation.
fn symbol_output(call: &McpToolCall) -> Result<SearchOutput> {
	let mut output = SearchOutput::Lines;
	for flag in call
		.parameters
		.get("flags")
		.and_then(|v| v.as_str())
		.unwrap_or_default()
		.chars()
	{
		let requested = match flag {
			'-' => continue,
			'c' => SearchOutput::Count,
			'l' => SearchOutput::FilesOnly,
			'r' => bail!("Flag r does not exist on `symbol`: pass relation \"references\" instead."),
			other => bail!("Unknown flag {other:?} for `symbol`: the flags are c (count) and l (files only)."),
		};
		if output != SearchOutput::Lines && output != requested {
			bail!("Flags c and l are alternatives for `symbol`: pass one of them.");
		}
		output = requested;
	}
	Ok(output)
}

fn optional_depth(call: &McpToolCall) -> Result<Option<usize>> {
	match call.parameters.get("depth") {
		None | Some(Value::Null) => Ok(None),
		Some(value) => match value.as_u64() {
			Some(0) => Ok(None),
			Some(depth) => Ok(Some(depth as usize)),
			_ => bail!("`depth` must be an integer from 1 to {MAX_GRAPH_DEPTH}."),
		},
	}
}

/// Entry point of the `symbol` tool: validate the parameter combination for
/// the relation, then answer it from a text-level lookup or the code graph.
pub async fn execute_symbol(call: &McpToolCall) -> Result<String> {
	let relation = optional_text(call, "relation")?.unwrap_or(DEFINITION_RELATION);
	let name = optional_text(call, "name")?;
	let to = optional_text(call, "to")?;
	let path = optional_text(call, "path")?;
	let depth = optional_depth(call)?;

	if relation == DEFINITION_RELATION || relation == REFERENCES_RELATION {
		let Some(name) = name else {
			bail!("relation \"{relation}\" needs `name`: the symbol to look up (`*` wildcards allowed, e.g. \"handle_*\").");
		};
		if to.is_some() {
			bail!("`to` is only valid with relation \"path\" — drop it.");
		}
		if depth.is_some() {
			bail!("`depth` applies to graph relations (callers, callees, imports, imported_by, supertypes, subtypes, path) — drop it.");
		}
		let output = symbol_output(call)?;
		let path = path.unwrap_or(".");
		let source = resolve_path_source(path, &call.workdir);
		if !io_exists(&source).await? {
			let nearby = match &source {
				PathSource::Local(missing) => missing_path_hint(missing.clone(), call.workdir.clone()).await,
				PathSource::Remote { .. } => String::new(),
			};
			bail!("Path not found: {path} (resolved to {}){nearby}", source.display());
		}
		let result = view_symbols(call, path, &source, name, relation == REFERENCES_RELATION, output).await?;
		// Count and files-only rows are not budgeted per file, so a huge tree
		// could still overflow the transport.
		if result.len() > MAX_RESULT_BYTES {
			bail!(
				"Result is {}, too large to return in one tool result (limit {}). Narrow `path`, `pattern` or `name`.",
				format_megabytes(result.len()),
				format_megabytes(MAX_RESULT_BYTES)
			);
		}
		return Ok(result);
	}

	let Some(graph_relation) = GraphRelation::parse(relation) else {
		bail!(
			"Unknown relation {relation:?}: the relations are definition, references, callers, callees, imports, imported_by, supertypes, subtypes and path."
		);
	};
	for key in TEXT_SEARCH_PARAMS {
		if is_set(call, key) {
			bail!("`{key}` applies to relations definition and references only — drop it for \"{relation}\".");
		}
	}
	if matches!(graph_relation, GraphRelation::Imports | GraphRelation::ImportedBy) {
		if name.is_some() {
			bail!("relation \"{relation}\" works on a file: pass its `path` and drop `name`.");
		}
		if path.is_none() {
			bail!("relation \"{relation}\" needs `path`: the file whose imports to follow.");
		}
	} else if name.is_none() {
		bail!("relation \"{relation}\" needs `name`: the symbol to start from.");
	}
	if graph_relation == GraphRelation::Path {
		if to.is_none() {
			bail!("relation \"path\" needs `to`: the symbol to reach from `name`.");
		}
	} else if to.is_some() {
		bail!("`to` is only valid with relation \"path\" — drop it.");
	}
	for (key, value) in [("name", name), ("to", to)] {
		if value.is_some_and(|symbol| symbol.contains('*')) {
			bail!("`{key}` cannot contain `*`: wildcards work with relations definition and references only.");
		}
	}

	execute_graph(
		call,
		&GraphRequest {
			relation: graph_relation,
			name,
			to,
			path,
			depth,
		},
	)
	.await
}

/// Render the symbol results for a file, or for every supported file under a
/// directory. `references` selects references over definitions; `output` is
/// the validated `c`/`l` flag.
pub async fn view_symbols(
	call: &McpToolCall,
	path: &str,
	source: &PathSource,
	symbol: &str,
	references: bool,
	output: SearchOutput,
) -> Result<String> {
	let flags = ViewFlags {
		output,
		references,
		..ViewFlags::default()
	};
	let matcher = NameMatcher::new(symbol).map_err(|e| anyhow!("{e}"))?;
	let context = call.parameters.get("context").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
	if io_is_dir(source).await? {
		return symbol_directory(call, path, source, symbol, &matcher, flags, context).await;
	}
	symbol_file(path, source, symbol, &matcher, flags, context).await
}

async fn symbol_file(
	path: &str,
	source: &PathSource,
	symbol: &str,
	matcher: &NameMatcher,
	flags: ViewFlags,
	context: usize,
) -> Result<String> {
	// Same caps as a whole-file view: refuse oversized files instead of
	// parsing megabytes of declarations.
	let metadata = io_metadata(source)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {e}"))?;
	if metadata.size > MAX_VIEW_FILE_BYTES {
		bail!("File is too large (>5MB)");
	}
	let bytes = io_read_capped(source, MAX_VIEW_FILE_BYTES)
		.await
		.map_err(|e| anyhow!("Permission denied. Cannot read file: {e}"))?
		.ok_or_else(|| anyhow!("File is too large (>5MB)"))?;
	// Lossy decode like the outline: a mostly-binary file yields no hits
	// rather than an encoding error.
	let content = String::from_utf8_lossy(&bytes).into_owned();
	let label = path.to_string();
	let matcher = matcher.clone();
	// Parsing is CPU-bound: keep it off the async reactor; the content stays
	// here for rendering, so the closure parses a clone.
	let parse_content = content.clone();
	let body = tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
		let hits = symbol_hits(&label, &parse_content, &matcher, flags)?;
		Ok(render_hits_body(&parse_content, &hits, &matcher, flags, context))
	})
	.await
	.map_err(|e| anyhow!("Failed to search symbols: {e}"))?
	.map_err(|e| anyhow!("{e}"))?;

	let Some(body) = body else {
		return Ok(no_symbol_matches(flags, symbol, &format!("in {path}")));
	};
	// Count and files-only results are one row that names the file, like the
	// content search; line results stand alone.
	Ok(if flags.output.is_compact() {
		flags.output.label(path, &body)
	} else {
		body
	})
}

/// Workdir-relative files that can hold the exact name `symbol`, from the
/// code index, so the directory walk parses those only. `None` when the
/// index cannot narrow the lookup: a wildcard, hidden files wanted, a name
/// with no identifier text, a workdir that does not fit the index, or a
/// directory the index does not list the way the walker does (outside the
/// workdir once symlinks and `..` are resolved, hidden, or ignored so that
/// the index holds nothing of it).
async fn indexed_candidates(
	workdir: &Path,
	directory: &Path,
	matcher: &NameMatcher,
	symbol: &str,
	include_hidden: bool,
) -> Result<Option<HashSet<String>>> {
	if matcher.is_wildcard() || include_hidden || identifier_runs(symbol).is_empty() {
		return Ok(None);
	}
	let root = tokio::fs::canonicalize(workdir)
		.await
		.map_err(|e| anyhow!("Cannot resolve the workdir {}: {e}", workdir.display()))?;
	let Ok(canonical_directory) = tokio::fs::canonicalize(directory).await else {
		return Ok(None);
	};
	let Ok(relative) = canonical_directory.strip_prefix(&root) else {
		return Ok(None);
	};
	if relative
		.components()
		.any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
	{
		return Ok(None);
	}
	let relative = relative.to_string_lossy().replace('\\', "/");
	let index = CodeIndexes::global().index(&root);
	let found = async {
		index.refresh().await?;
		if !index.has_files_under(&relative) {
			return Ok(None);
		}
		index.files_spelling_verified(&[symbol]).await.map(Some)
	}
	.await;
	match found {
		Ok(Some(ids)) => Ok(Some(ids.iter().filter_map(|id| index.path_of(*id)).collect())),
		Ok(None) => Ok(None),
		Err(error) if error.is::<IndexUnavailable>() => Ok(None),
		Err(error) => Err(error),
	}
}

async fn symbol_directory(
	call: &McpToolCall,
	path: &str,
	source: &PathSource,
	symbol: &str,
	matcher: &NameMatcher,
	flags: ViewFlags,
	context: usize,
) -> Result<String> {
	let pattern = call
		.parameters
		.get("pattern")
		.and_then(|v| v.as_str())
		.map(str::to_string);
	let max_depth = call
		.parameters
		.get("max_depth")
		.and_then(|v| v.as_u64())
		.map(|n| n as usize);
	let include_hidden = call
		.parameters
		.get("include_hidden")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);

	if source.is_remote() {
		return symbol_directory_remote(call, path, source, symbol, matcher, flags, context).await;
	}

	let abs_dir = if Path::new(path).is_absolute() {
		PathBuf::from(path)
	} else {
		call.workdir.join(path)
	};
	let abs_dir_str = abs_dir.to_string_lossy().to_string();
	let working_dir = call.workdir.clone();
	let label = path.to_string();
	let matcher = matcher.clone();
	let candidates = indexed_candidates(&call.workdir, &abs_dir, &matcher, symbol, include_hidden).await?;
	let symbol = symbol.to_string();
	// The walk, read, parse and render are all CPU/IO-bound blocking work —
	// one spawn_blocking, files in parallel like the tree content search.
	let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
		let files = match supported_files_under(
			&abs_dir_str,
			&working_dir,
			pattern.as_deref(),
			max_depth,
			include_hidden,
		)? {
			Some(files) => files,
			None => return Ok(format!("No files matched pattern \"{}\".", pattern.unwrap_or_default())),
		};

		let mut blocks: Vec<(usize, String)> = files
			.par_iter()
			.enumerate()
			.filter_map(|(i, rel_path)| {
				// A file of a language the index leaves out (Markdown, JSON) is
				// never in the narrowed list: it is searched like any other.
				if candidates
					.as_ref()
					.is_some_and(|spelled| super::graph::is_indexed_language(rel_path) && !spelled.contains(rel_path))
				{
					return None;
				}
				let full_path = working_dir.join(rel_path);
				// Files over the view cap are not loaded — same OOM bound as
				// the content search; they are skipped silently like files
				// with no grammar or no hits.
				let meta = std::fs::metadata(&full_path).ok()?;
				if meta.len() > MAX_VIEW_FILE_BYTES {
					return None;
				}
				let bytes = read_capped(&full_path)?;
				let content = String::from_utf8_lossy(&bytes);
				let body = render_symbol_body(rel_path, &content, &matcher, flags, context)?;
				Some((i, flags.output.label(rel_path, &body)))
			})
			.collect();
		blocks.sort_by_key(|(i, _)| *i);
		let blocks: Vec<String> = blocks.into_iter().map(|(_, block)| block).collect();
		Ok(join_symbol_blocks(&label, blocks, flags, &symbol))
	})
	.await;

	match output {
		Ok(Ok(s)) => Ok(s),
		Ok(Err(e)) => bail!("{e}"),
		Err(join_err) => bail!("Failed to search symbols: {join_err}"),
	}
}

async fn symbol_directory_remote(
	call: &McpToolCall,
	path: &str,
	source: &PathSource,
	symbol: &str,
	matcher: &NameMatcher,
	flags: ViewFlags,
	context: usize,
) -> Result<String> {
	let pattern = call
		.parameters
		.get("pattern")
		.and_then(|v| v.as_str())
		.map(str::to_string);
	let max_depth = call
		.parameters
		.get("max_depth")
		.and_then(|v| v.as_u64())
		.map(|n| n as usize);
	let include_hidden = call
		.parameters
		.get("include_hidden")
		.and_then(|v| v.as_bool())
		.unwrap_or(false);
	let files = match supported_remote_files(source, pattern.as_deref(), max_depth, include_hidden).await? {
		Some(files) => files,
		None => return Ok(format!("No files matched pattern \"{}\".", pattern.unwrap_or_default())),
	};

	// Read concurrently like the remote outline (oversized files are not
	// downloaded); parse and render afterwards, off the reactor.
	let reads: Vec<_> = files
		.iter()
		.enumerate()
		.filter(|(_, (_, meta))| meta.size <= MAX_VIEW_FILE_BYTES)
		.map(|(i, (rel_path, _))| {
			let file_source = remote_child(source, rel_path);
			let rel_path = rel_path.clone();
			async move {
				let content = io_read(&file_source)
					.await
					.ok()
					.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
				(i, rel_path, content)
			}
		})
		.collect();
	let mut reads = join_bounded(reads).await?;
	reads.sort_by_key(|(i, _, _)| *i);

	let label = path.to_string();
	let matcher = matcher.clone();
	let symbol = symbol.to_string();
	let output = tokio::task::spawn_blocking(move || {
		let blocks: Vec<String> = reads
			.into_iter()
			.filter_map(|(_, rel_path, content)| {
				let content = content?;
				let body = render_symbol_body(&rel_path, &content, &matcher, flags, context)?;
				Some(flags.output.label(&rel_path, &body))
			})
			.collect();
		join_symbol_blocks(&label, blocks, flags, &symbol)
	})
	.await
	.map_err(|e| anyhow!("Failed to search symbols: {e}"))?;
	Ok(output)
}

/// Compute the hits for one file. The error is the unsupported-extension
/// message: a single file surfaces it, the directory walk has already
/// filtered such files out.
fn symbol_hits(
	path_label: &str,
	contents: &str,
	matcher: &NameMatcher,
	flags: ViewFlags,
) -> Result<Vec<SymbolHit>, String> {
	if flags.references {
		find_references(path_label, contents, matcher)
	} else {
		find_definitions(path_label, contents, matcher)
	}
}

/// One file's symbol hits in the shape `flags` asks for, or None when the
/// file has nothing to report. Files-only output has an empty body: the path
/// is the whole result. A lookup error skips the file — the walk already
/// filtered unsupported files, so only a grammar hiccup can land here.
fn render_symbol_body(
	path_label: &str,
	contents: &str,
	matcher: &NameMatcher,
	flags: ViewFlags,
	context: usize,
) -> Option<String> {
	let hits = symbol_hits(path_label, contents, matcher, flags).ok()?;
	render_hits_body(contents, &hits, matcher, flags, context)
}

fn render_hits_body(
	contents: &str,
	hits: &[SymbolHit],
	matcher: &NameMatcher,
	flags: ViewFlags,
	context: usize,
) -> Option<String> {
	if hits.is_empty() {
		return None;
	}
	match flags.output {
		SearchOutput::Count => Some(hits.len().to_string()),
		SearchOutput::FilesOnly => Some(String::new()),
		SearchOutput::Lines | SearchOutput::OnlyMatching => {
			let (mut body, omitted) = if flags.references {
				render_reference_hits(contents, hits, context, MAX_RESULT_BYTES)
			} else {
				render_definition_hits(contents, hits, !matcher.is_wildcard(), MAX_RESULT_BYTES)
			};
			if omitted > 0 {
				body.push_str(&format!(
					"\n[symbol results truncated: {omitted} hits omitted — narrow `name` or count them with `flags: \"c\"`]"
				));
			}
			Some(body)
		}
	}
}

/// Render definition hits as `N:hh content` lines, each definition headed by
/// its breadcrumb when nested. `complete` (exact name) definitions of at most
/// [`SMALL_DEFINITION_LINES`] lines render in full; longer ones and wildcard
/// matches render a preview plus the last line. Returns the body and how many
/// hits the budget dropped.
fn render_definition_hits(contents: &str, hits: &[SymbolHit], complete: bool, budget: usize) -> (String, usize) {
	let lines: Vec<&str> = contents.lines().collect();
	let mut out: Vec<String> = Vec::new();
	let mut used = 0usize;
	let mut rendered = 0usize;
	for hit in hits {
		let mut block = Vec::new();
		if let Some(breadcrumb) = &hit.breadcrumb {
			block.push(format!("[{breadcrumb}]"));
		}
		let start = hit.line;
		let end = hit.line + hit.line_count.saturating_sub(1);
		let full = complete && hit.line_count <= SMALL_DEFINITION_LINES;
		let preview_end = if full {
			end
		} else {
			(start + PREVIEW_LINES - 1).min(end)
		};
		for n in start..=preview_end {
			// A broken parse can name a line past the file's end; render what
			// exists rather than failing.
			if let Some(line) = lines.get(n - 1) {
				block.push(format_line(n, line));
			}
		}
		if preview_end < end {
			if preview_end + 1 < end {
				block.push(format!("… {} lines omitted …", end - preview_end - 1));
			}
			if let Some(line) = lines.get(end - 1) {
				block.push(format_line(end, line));
			}
		}
		let block = block.join("\n");
		if used + 1 + block.len() > budget.saturating_sub(super::structure::TRUNCATION_NOTICE_RESERVE) {
			break;
		}
		used += 1 + block.len();
		out.push(block);
		rendered += 1;
	}
	(out.join("\n"), hits.len() - rendered)
}

/// Render reference hits grouped by enclosing symbol: a `[breadcrumb]` header
/// per group (tagged `[def]` when the group holds the definition site), then
/// the hit lines — one per hit, expanded by `context` and merged. A line is
/// never rendered twice: a previous group's context may already have shown it.
fn render_reference_hits(contents: &str, hits: &[SymbolHit], context: usize, budget: usize) -> (String, usize) {
	let lines: Vec<&str> = contents.lines().collect();
	// Hits arrive in document order, so grouping consecutive hits by
	// breadcrumb groups every hit exactly once.
	let mut groups: Vec<(Option<&str>, Vec<&SymbolHit>)> = Vec::new();
	for hit in hits {
		match groups.last_mut() {
			Some((breadcrumb, group)) if *breadcrumb == hit.breadcrumb.as_deref() => group.push(hit),
			_ => groups.push((hit.breadcrumb.as_deref(), vec![hit])),
		}
	}

	let mut out: Vec<String> = Vec::new();
	let mut used = 0usize;
	let mut rendered = 0usize;
	let mut shown: HashSet<usize> = HashSet::new();
	for (breadcrumb, group) in &groups {
		let mut block: Vec<String> = Vec::new();
		let header = match breadcrumb {
			Some(bc) if group.iter().any(|h| h.is_definition) => format!("[{bc}] [def]"),
			Some(bc) => format!("[{bc}]"),
			None if group.iter().any(|h| h.is_definition) => "[def]".to_string(),
			None => String::new(),
		};
		let has_header = !header.is_empty();
		if has_header {
			block.push(header);
		}
		let mut ranges: Vec<(usize, usize)> = Vec::new();
		for hit in group {
			let start = hit.line.saturating_sub(context).max(1);
			let end = (hit.line + context).min(lines.len());
			match ranges.last_mut() {
				Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
				_ => ranges.push((start, end)),
			}
		}
		for (start, end) in ranges {
			for n in start..=end {
				if !shown.insert(n) {
					continue;
				}
				if let Some(line) = lines.get(n - 1) {
					block.push(format_line(n, line));
				}
			}
		}
		if block.len() <= usize::from(has_header) {
			// Every line already appeared in a previous group's context; the
			// hits themselves were shown, so they count as rendered.
			rendered += group.len();
			continue;
		}
		let block = block.join("\n");
		if used + 1 + block.len() > budget.saturating_sub(super::structure::TRUNCATION_NOTICE_RESERVE) {
			break;
		}
		used += 1 + block.len();
		out.push(block);
		rendered += group.len();
	}
	(out.join("\n"), hits.len() - rendered)
}

/// Join per-file blocks for a directory search: compact modes one row per
/// file, line modes the `path:` block style under the result budget.
fn join_symbol_blocks(label: &str, blocks: Vec<String>, flags: ViewFlags, symbol: &str) -> String {
	if blocks.is_empty() {
		return no_symbol_matches(flags, symbol, &format!("under {label}"));
	}
	if flags.output.is_compact() {
		return blocks.join("\n");
	}
	let (mut out, omitted) = join_blocks_budgeted(&blocks, MAX_RESULT_BYTES);
	if out.is_empty() {
		return format!(
			"[symbol results truncated: {omitted} files omitted — narrow `path`, `pattern` or `max_depth`]"
		);
	}
	if omitted > 0 {
		out.push_str(&format!(
			"\n\n[symbol results truncated: {omitted} files omitted — narrow `path`, `pattern` or `max_depth`]"
		));
	}
	out
}

fn no_symbol_matches(flags: ViewFlags, symbol: &str, scope: &str) -> String {
	let kind = if flags.references { "references" } else { "definitions" };
	format!(
		"No {kind} found for symbol \"{symbol}\" {scope}.\nTry a `*` wildcard (e.g. \"{symbol}*\") or a `view` `content` search for plain text."
	)
}

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod symbols_tests;
