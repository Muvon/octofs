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

//! AST-pattern search mode of `view` (`ast: true` + `content`): matches an
//! ast-grep pattern against the syntax tree of a file, or of every supported
//! file under a directory. Lookup is pure (`crate::structural::ast_search`);
//! this module owns the I/O, the walk and the rendering, mirroring the
//! symbol mode. Every source line renders as an ordinary `N:hh|content` line
//! so the ids feed `batch_edit` unchanged. AST reads never register in the
//! delta/view cache.

use super::directory::{
	apply_pattern, build_pattern_matcher, build_walker, collect_file_paths, collect_remote_files, join_bounded,
	read_capped, remote_child,
};
use super::file_ops::MAX_VIEW_FILE_BYTES;
use super::remote::{io_is_dir, io_metadata, io_read, io_read_capped, IoMetadata, PathSource};
use super::search::{SearchOutput, ViewFlags};
use super::structure::{join_blocks_budgeted, TRUNCATION_NOTICE_RESERVE};
use crate::mcp::McpToolCall;
use crate::structural::ast_search::{ast_language, miss_diagnostic, search_source, unsupported_message, AstMatch};
use crate::structural::{language_for_extension, path_extension};
use crate::utils::line_hash::line_id_at;
use crate::utils::truncation::MAX_RESULT_BYTES;
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Lines one match may render before its span is cut with an elision marker.
/// A whole-file pattern (`$A`) can match nodes spanning thousands of lines;
/// the marker keeps one match from eating the result budget.
const MAX_MATCH_LINES: usize = 30;

/// Render the AST-pattern results for a file, or for every supported file
/// under a directory. Parameter and flag exclusivity (`ast` requires
/// `content`, no `regex`/`symbol`/`start`/`end`, no `s`/`v`/`o` flags) is
/// validated upstream in `view_existing_path`.
/// The search-wide parameters every AST mode carries: the pattern and the
/// rendering knobs. Grouped so the directory helpers stay under the argument
/// limit.
struct AstQuery {
	pattern: String,
	flags: ViewFlags,
	context: usize,
}

pub async fn view_ast(call: &McpToolCall, path: &str, source: &PathSource, pattern: &str) -> Result<String> {
	let flags = ViewFlags::parse(call.parameters.get("flags").and_then(|v| v.as_str()), true, false)?;
	let context = call.parameters.get("context").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
	let query = AstQuery {
		pattern: pattern.to_string(),
		flags,
		context,
	};
	if io_is_dir(source).await? {
		return ast_directory(call, path, source, &query).await;
	}
	ast_file(path, source, &query).await
}

async fn ast_file(path: &str, source: &PathSource, query: &AstQuery) -> Result<String> {
	let pattern = query.pattern.as_str();
	let flags = query.flags;
	let context = query.context;
	let language = match ast_language(path) {
		Some(language) => language,
		None => bail!("{}", unsupported_language_message(path)),
	};
	// Same caps as a whole-file view: refuse oversized files instead of
	// parsing megabytes of syntax.
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
	// Lossy decode like the symbol mode: a mostly-binary file yields no
	// matches rather than an encoding error.
	let content = String::from_utf8_lossy(&bytes).into_owned();
	let label = path.to_string();
	let language = language.to_string();
	// Parsing is CPU-bound: keep it off the async reactor; the content stays
	// here for rendering, so the closure parses a clone.
	let parse_content = content.clone();
	let search_language = language.clone();
	let search_pattern = pattern.to_string();
	let body = tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
		let matches = search_source(&search_language, &parse_content, &search_pattern)
			.map_err(|e| format!("The pattern does not parse as {search_language}: {e}"))?;
		Ok(render_ast_body(&label, &parse_content, &matches, context, flags))
	})
	.await
	.map_err(|e| anyhow!("Failed to search AST patterns: {e}"))?
	.map_err(|e| anyhow!("{e}"))?;

	let Some(body) = body else {
		return Ok(miss_diagnostic(pattern, &language));
	};
	// Count and files-only results are one row that names the file, like the
	// content search; line results stand alone.
	Ok(if flags.output.is_compact() {
		flags.output.label(path, &body)
	} else {
		body
	})
}

async fn ast_directory(call: &McpToolCall, path: &str, source: &PathSource, query: &AstQuery) -> Result<String> {
	let pattern = query.pattern.clone();
	let flags = query.flags;
	let context = query.context;
	let glob = call
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
		return ast_directory_remote(path, source, query, glob.as_deref(), max_depth, include_hidden).await;
	}

	let abs_dir = if Path::new(path).is_absolute() {
		PathBuf::from(path)
	} else {
		call.workdir.join(path)
	};
	let abs_dir_str = abs_dir.to_string_lossy().to_string();
	let working_dir = call.workdir.clone();
	let label = path.to_string();

	// The walk, read, parse and render are all CPU/IO-bound blocking work —
	// one spawn_blocking, files in parallel like the tree content search.
	let output = tokio::task::spawn_blocking(move || -> Result<String, String> {
		let files = match ast_files_under(&abs_dir_str, &working_dir, glob.as_deref(), max_depth, include_hidden)? {
			Some(files) => files,
			None => return Ok(format!("No files matched pattern \"{}\".", glob.unwrap_or_default())),
		};

		// A pattern that fails to parse for one language must not fail the
		// whole walk: files of that language are skipped and the error is
		// reported once per language, not once per file.
		let parse_errors: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
		let mut blocks: Vec<(usize, String)> = files
			.par_iter()
			.enumerate()
			.filter_map(|(i, rel_path)| {
				let full_path = working_dir.join(rel_path);
				// Files over the view cap are not loaded — same OOM bound as
				// the content search; they are skipped silently like files
				// with no grammar or no matches.
				let meta = std::fs::metadata(&full_path).ok()?;
				if meta.len() > MAX_VIEW_FILE_BYTES {
					return None;
				}
				let bytes = read_capped(&full_path)?;
				let content = String::from_utf8_lossy(&bytes);
				let language = ast_language(rel_path)?;
				let matches = match search_source(language, &content, &pattern) {
					Ok(matches) => matches,
					Err(e) => {
						if let Ok(mut errors) = parse_errors.lock() {
							errors.entry(language.to_string()).or_insert(e);
						}
						return None;
					}
				};
				let body = render_ast_body(rel_path, &content, &matches, context, flags)?;
				Some((i, flags.output.label(rel_path, &body)))
			})
			.collect();
		blocks.sort_by_key(|(i, _)| *i);
		let blocks: Vec<String> = blocks.into_iter().map(|(_, block)| block).collect();
		let parse_errors = parse_errors.into_inner().ok();
		Ok(join_ast_blocks(
			&label,
			blocks,
			&pattern,
			files.first().and_then(|file| ast_language(file)),
			parse_errors.as_ref(),
			flags,
		))
	})
	.await;

	match output {
		Ok(Ok(s)) => Ok(s),
		Ok(Err(e)) => bail!("{e}"),
		Err(join_err) => bail!("Failed to search AST patterns: {join_err}"),
	}
}

async fn ast_directory_remote(
	path: &str,
	source: &PathSource,
	query: &AstQuery,
	glob: Option<&str>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<String> {
	let pattern = query.pattern.as_str();
	let flags = query.flags;
	let context = query.context;
	let files = match ast_remote_files(source, glob, max_depth, include_hidden).await? {
		Some(files) => files,
		None => return Ok(format!("No files matched pattern \"{}\".", glob.unwrap_or_default())),
	};

	// Read concurrently like the remote symbol search (oversized files are
	// not downloaded); parse and render afterwards, off the reactor.
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
	let pattern = pattern.to_string();
	let output = tokio::task::spawn_blocking(move || {
		let mut parse_errors: BTreeMap<String, String> = BTreeMap::new();
		let blocks: Vec<String> = reads
			.into_iter()
			.filter_map(|(_, rel_path, content)| {
				let content = content?;
				let language = ast_language(&rel_path)?;
				let matches = match search_source(language, &content, &pattern) {
					Ok(matches) => matches,
					Err(e) => {
						parse_errors.entry(language.to_string()).or_insert(e);
						return None;
					}
				};
				let body = render_ast_body(&rel_path, &content, &matches, context, flags)?;
				Some(flags.output.label(&rel_path, &body))
			})
			.collect();
		join_ast_blocks(
			&label,
			blocks,
			&pattern,
			files.first().and_then(|(file, _)| ast_language(file)),
			Some(&parse_errors),
			flags,
		)
	})
	.await
	.map_err(|e| anyhow!("Failed to search AST patterns: {e}"))?;
	Ok(output)
}

/// The error for a single-file search on a file AST patterns cannot cover:
/// css/json/markdown get the guided message, everything else the plain
/// no-grammar one.
fn unsupported_language_message(path: &str) -> String {
	let extension = path_extension(path);
	match language_for_extension(&extension) {
		Some(language) => unsupported_message(language),
		None => crate::structural::unsupported_message(&extension),
	}
}

/// AST-searchable files under a local directory, glob-filtered. `None` when
/// the glob matched no file at all — callers report that distinctly from
/// "matched, but nothing supported".
fn ast_files_under(
	abs_dir_str: &str,
	working_dir: &Path,
	glob: Option<&str>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<Option<Vec<String>>, String> {
	let pattern_matcher = glob.map(build_pattern_matcher).transpose()?;
	let mut builder = build_walker(abs_dir_str, max_depth, include_hidden);
	let mut files = collect_file_paths(&mut builder, working_dir);
	if let Some(matcher) = pattern_matcher {
		apply_pattern(&mut files, &matcher);
		if files.is_empty() {
			return Ok(None);
		}
	}
	files.retain(|file| ast_language(file).is_some());
	Ok(Some(files))
}

/// AST-searchable files under a remote directory, glob-filtered. `None` when
/// the glob matched no file at all.
async fn ast_remote_files(
	source: &PathSource,
	glob: Option<&str>,
	max_depth: Option<usize>,
	include_hidden: bool,
) -> Result<Option<Vec<(String, IoMetadata)>>> {
	let pattern_matcher = glob
		.map(build_pattern_matcher)
		.transpose()
		.map_err(|e| anyhow!("{e}"))?;
	let mut files = collect_remote_files(source, max_depth, include_hidden).await?;
	if let Some(matcher) = pattern_matcher {
		let mut names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
		apply_pattern(&mut names, &matcher);
		if names.is_empty() {
			return Ok(None);
		}
		let keep: HashSet<String> = names.into_iter().collect();
		files.retain(|(name, _)| keep.contains(name));
	}
	files.retain(|(name, _)| ast_language(name).is_some());
	Ok(Some(files))
}

/// One file's matches in the shape `flags` asks for, or None when the file
/// has nothing to report. Files-only output has an empty body: the path is
/// the whole result. Only-matching output cannot reach here (`o` is rejected
/// with `ast` upstream) and renders like whole lines.
fn render_ast_body(
	path_label: &str,
	contents: &str,
	matches: &[AstMatch],
	context: usize,
	flags: ViewFlags,
) -> Option<String> {
	if matches.is_empty() {
		return None;
	}
	// Nested nodes can match with identical spans; one rendered span per
	// distinct (line, line_count) keeps counts and output honest.
	let mut matches: Vec<AstMatch> = matches.to_vec();
	matches.sort_by_key(|m| (m.line, m.line_count));
	matches.dedup_by_key(|m| (m.line, m.line_count));
	match flags.output {
		SearchOutput::Count => Some(matches.len().to_string()),
		SearchOutput::FilesOnly => Some(String::new()),
		SearchOutput::Lines | SearchOutput::OnlyMatching => {
			let (mut body, omitted) = render_match_lines(path_label, contents, &matches, context, MAX_RESULT_BYTES);
			if omitted > 0 {
				body.push_str(&format!(
					"\n[ast results truncated: {omitted} matches omitted — narrow the pattern or count them with `flags: \"c\"`]"
				));
			}
			Some(body)
		}
	}
}

/// Render matches as `N:hh|content` lines: each match its line span (spans
/// longer than [`MAX_MATCH_LINES`] cut with an elision marker plus their last
/// line), expanded by `context`, headed by the enclosing-symbol breadcrumb
/// when it changes. A line is never rendered twice: a previous match's span
/// or context may already have shown it. Returns the body and how many
/// matches the budget dropped.
fn render_match_lines(
	path_label: &str,
	contents: &str,
	matches: &[AstMatch],
	context: usize,
	budget: usize,
) -> (String, usize) {
	let lines: Vec<&str> = contents.lines().collect();
	// Breadcrumbs are decoration: a file that cannot be parsed renders
	// without them rather than failing the search.
	let breadcrumbs = crate::structural::symbols::line_breadcrumbs(path_label, contents);
	let mut out: Vec<String> = Vec::new();
	let mut used = 0usize;
	let mut rendered = 0usize;
	let mut shown: HashSet<usize> = HashSet::new();
	let mut last_group: Option<&str> = None;
	for m in matches {
		let mut block: Vec<String> = Vec::new();
		let mut has_header = false;
		if let Some(per_line) = &breadcrumbs {
			let group = per_line.get(m.line - 1).and_then(|b| b.as_deref());
			if let Some(breadcrumb) = group {
				if last_group != Some(breadcrumb) {
					block.push(format!("[{breadcrumb}]"));
					has_header = true;
				}
			}
			last_group = group;
		}
		let span_end = m.line + m.line_count - 1;
		let capped_end = (m.line + MAX_MATCH_LINES - 1).min(span_end);
		for n in m.line.saturating_sub(context).max(1)..=capped_end {
			if shown.insert(n) {
				if let Some(line) = lines.get(n - 1) {
					block.push(format!("{}|{}", line_id_at(&lines, n), line));
				}
			}
		}
		if capped_end < span_end {
			block.push(format!("… {} lines omitted …", span_end - capped_end));
			let n = span_end.min(lines.len());
			if shown.insert(n) {
				if let Some(line) = lines.get(n - 1) {
					block.push(format!("{}|{}", line_id_at(&lines, n), line));
				}
			}
		}
		for n in (span_end + 1)..=(span_end + context) {
			if n > lines.len() {
				break;
			}
			if shown.insert(n) {
				if let Some(line) = lines.get(n - 1) {
					block.push(format!("{}|{}", line_id_at(&lines, n), line));
				}
			}
		}
		if block.len() <= usize::from(has_header) {
			// Every line already appeared in a previous match's span or
			// context; the match itself was shown, so it counts as rendered.
			rendered += 1;
			continue;
		}
		let block = block.join("\n");
		if used + 1 + block.len() > budget.saturating_sub(TRUNCATION_NOTICE_RESERVE) {
			break;
		}
		used += 1 + block.len();
		out.push(block);
		rendered += 1;
	}
	(out.join("\n"), matches.len() - rendered)
}

/// Join per-file blocks for a directory search: compact modes one row per
/// file, line modes the `path:` block style under the result budget. Zero
/// matches yield the miss diagnostic (plus per-language parse errors when
/// the pattern failed some languages); parse errors are appended as notes
/// even when other files matched, once per language.
fn join_ast_blocks(
	label: &str,
	blocks: Vec<String>,
	pattern: &str,
	first_language: Option<&str>,
	parse_errors: Option<&BTreeMap<String, String>>,
	flags: ViewFlags,
) -> String {
	let empty_errors = BTreeMap::new();
	let parse_errors = parse_errors.unwrap_or(&empty_errors);
	if blocks.is_empty() {
		let mut out = match first_language {
			Some(language) => miss_diagnostic(pattern, language),
			None => format!("No AST-searchable files found under {label}."),
		};
		for (language, error) in parse_errors {
			out.push_str(&format!(
				"\n\n[skipped {language} files: the pattern does not parse as {language}: {error}]"
			));
		}
		return out;
	}
	let mut out = if flags.output.is_compact() {
		blocks.join("\n")
	} else {
		let (joined, omitted) = join_blocks_budgeted(&blocks, MAX_RESULT_BYTES);
		if joined.is_empty() {
			format!("[ast results truncated: {omitted} files omitted — narrow `path`, `pattern` or `max_depth`]")
		} else if omitted > 0 {
			format!("{joined}\n\n[ast results truncated: {omitted} files omitted — narrow `path`, `pattern` or `max_depth`]")
		} else {
			joined
		}
	};
	// Notes ride after the budgeted result; each is short, and a pattern
	// failing more languages than fit is cut rather than blowing the limit.
	for (language, error) in parse_errors {
		let note = format!("\n\n[skipped {language} files: the pattern does not parse as {language}: {error}]");
		if out.len() + note.len() > MAX_RESULT_BYTES {
			break;
		}
		out.push_str(&note);
	}
	out
}

#[cfg(test)]
#[path = "ast_tests.rs"]
mod ast_tests;
