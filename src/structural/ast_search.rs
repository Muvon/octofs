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

//! AST-pattern search over the tree-sitter grammars, powered by ast-grep-core.
//! Pure content-in API like the rest of `structural`: a path label picks the
//! language, the pattern is an ast-grep pattern (`fn $NAME($$$ARGS)`), and the
//! result is each matched node's line span. Matching is iterative in tree
//! depth — ast-grep's traversal is an explicit stack machine and its matcher
//! is non-recursive — so deeply nested files cannot overflow the stack.

use ast_grep_core::language::Language as AstGrepLanguage;
use ast_grep_core::matcher::{PatternBuilder, PatternNode};
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc};
use ast_grep_core::{Pattern, PatternError};
use std::borrow::Cow;

use super::languages::{self, PatternGrammar};

/// Rewrite `$` metavariables to `expando` before parsing, mirroring
/// ast-grep's own language configs: languages whose identifiers cannot start
/// with `$` would otherwise parse `$NAME` into error nodes that match
/// nothing. `$`s that are not metavariable-shaped — lowercase names, `$`
/// inside literals — are left alone. With `expando == '$'` this is the
/// identity rewrite.
fn expand_pattern(expando: char, query: &str) -> Cow<'_, str> {
	let mut ret = Vec::with_capacity(query.len());
	let mut dollar_count = 0;
	for c in query.chars() {
		if c == '$' {
			dollar_count += 1;
			continue;
		}
		let need_replace = matches!(c, 'A'..='Z' | '_') // $A or $$A or $$$A
			|| dollar_count == 3; // anonymous multiple
		let sigil = if need_replace { expando } else { '$' };
		ret.extend(std::iter::repeat_n(sigil, dollar_count));
		dollar_count = 0;
		ret.push(c);
	}
	// trailing anonymous multiple
	let sigil = if dollar_count == 3 { expando } else { '$' };
	ret.extend(std::iter::repeat_n(sigil, dollar_count));
	Cow::Owned(ret.into_iter().collect())
}

/// Bridge a language's pattern grammar to ast-grep's language traits. The
/// expando char replaces metavariable `$`s at parse time for languages whose
/// identifiers may not start with `$`; where `$` is a valid identifier
/// character the rewrite is a no-op.
impl AstGrepLanguage for PatternGrammar {
	fn expando_char(&self) -> char {
		self.expando
	}

	fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
		expand_pattern(self.expando, query)
	}

	fn kind_to_id(&self, kind: &str) -> u16 {
		self.grammar.id_for_node_kind(kind, true)
	}

	fn field_to_id(&self, field: &str) -> Option<u16> {
		self.grammar.field_id_for_name(field).map(|f| f.get())
	}

	fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
		builder.build(|src| StrDoc::try_new(src, self.clone()))
	}
}

impl LanguageExt for PatternGrammar {
	fn get_ts_language(&self) -> tree_sitter::Language {
		self.grammar.clone()
	}
}

fn grammar_named(language: &str) -> Result<PatternGrammar, String> {
	languages::pattern_grammar_named(language)
		.ok_or_else(|| format!("No grammar implementation for language '{language}'."))
}

/// One match: the 1-based line the matched node starts at, and how many
/// source lines the node spans (>= 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstMatch {
	pub line: usize,
	pub line_count: usize,
}

/// What a pattern parses to — the signals a zero-match diagnostic names.
#[derive(Debug)]
pub struct PatternInfo {
	/// Tree-sitter kind of the pattern's root node, e.g. `function_item`.
	pub root_kind: Option<String>,
	/// The pattern parsed into an error node: it is not valid standalone code
	/// in the language.
	pub has_error: bool,
	/// Named metavariables (`$X`, `$$$X`), sorted.
	pub defined_vars: Vec<String>,
}

/// The language an AST-pattern search uses for a path label. None for files
/// without a grammar and for the languages that offer no pattern search,
/// where a syntax-tree pattern buys nothing over the outline, symbol and
/// content modes.
pub fn ast_language(path_label: &str) -> Option<&'static str> {
	languages::pattern_grammar_for_extension(&super::path_extension(path_label)).map(|grammar| grammar.name)
}

/// The error a single-file AST search on an unsupported file reports. Mirrors
/// the symbol mode's css/json guidance so both modes steer the caller the
/// same way.
pub fn unsupported_message(language: &str) -> String {
	format!(
		"AST pattern search is not supported for {language} files — use flags: \"s\" for an outline, `symbol` for navigation or `content` for text"
	)
}

/// Search one source for every node matching the ast-grep pattern. The error
/// is the pattern's parse error for the language; callers wrap it with the
/// language name. A file that holds code of other languages (Svelte
/// scripts) is searched block by block with each block's own grammar, and
/// hits are offset back to file coordinates.
pub fn search_source(language: &str, source: &str, pattern: &str) -> Result<Vec<AstMatch>, String> {
	let grammar = grammar_named(language)?;
	let blocks = languages::get_language(language).and_then(|owner| owner.script_blocks(source));
	match blocks {
		Some(blocks) => search_script_blocks(blocks, pattern),
		None => search_with_grammar(grammar, source, pattern),
	}
}

fn search_script_blocks(blocks: Vec<languages::ScriptBlock>, pattern: &str) -> Result<Vec<AstMatch>, String> {
	let mut matches = Vec::new();
	for (language, script, start_row, _) in blocks {
		let script_matches = search_source(language, &script, pattern)?;
		// The script text is a contiguous slice of the file, so its line k is
		// the file's line start_row + k.
		matches.extend(script_matches.into_iter().map(|m| AstMatch {
			line: m.line + start_row,
			..m
		}));
	}
	Ok(matches)
}

/// Pattern parse errors quote the pre-processed pattern source, which carries
/// the expando char instead of `$`. Swap that exact substring back for the
/// user's pattern so every diagnostic echoes what was typed — a blanket char
/// replace would corrupt user patterns that legitimately contain the char.
fn demangle_pattern_error(grammar: &PatternGrammar, pattern: &str, error: PatternError) -> String {
	let message = error.to_string();
	let expanded = expand_pattern(grammar.expando, pattern);
	if expanded.as_ref() != pattern {
		return message.replacen(expanded.as_ref(), pattern, 1);
	}
	message
}

fn search_with_grammar(lang: PatternGrammar, source: &str, pattern_str: &str) -> Result<Vec<AstMatch>, String> {
	let grep = lang.ast_grep(source);
	let pattern =
		Pattern::try_new(pattern_str, lang.clone()).map_err(|e| demangle_pattern_error(&lang, pattern_str, e))?;
	Ok(grep
		.root()
		.find_all(&pattern)
		.map(|m| {
			let start = m.start_pos().line() + 1;
			let end = m.end_pos().line() + 1;
			AstMatch {
				line: start,
				line_count: end - start + 1,
			}
		})
		.collect())
}

/// Inspect what a pattern parses to: the root node kind, the parse-error
/// flag and the named metavariables. Cheap — no corpus needed.
pub fn pattern_info(pattern: &str, language: &str) -> Result<PatternInfo, String> {
	pattern_info_with_grammar(grammar_named(language)?, pattern)
}

fn pattern_info_with_grammar(lang: PatternGrammar, pattern_str: &str) -> Result<PatternInfo, String> {
	let pattern =
		Pattern::try_new(pattern_str, lang.clone()).map_err(|e| demangle_pattern_error(&lang, pattern_str, e))?;
	let has_error = pattern.has_error();
	let root_kind = match &pattern.node {
		PatternNode::Terminal { kind_id, .. } | PatternNode::Internal { kind_id, .. } => {
			lang.get_ts_language().node_kind_for_id(*kind_id).map(|s| s.to_string())
		}
		PatternNode::MetaVar { .. } => None,
	};
	let mut defined_vars: Vec<String> = pattern.defined_vars().into_iter().map(str::to_string).collect();
	defined_vars.sort();
	Ok(PatternInfo {
		root_kind,
		has_error,
		defined_vars,
	})
}

/// Short diagnostic for a zero-match search: the signals the caller needs to
/// retry — parsed kind, parse-error flag, metavariables — plus the two usual
/// causes and the plain-text fallback.
pub fn miss_diagnostic(pattern: &str, language: &str) -> String {
	let mut out = format!("No AST matches for pattern {pattern:?} in {language}.\n");
	match pattern_info(pattern, language) {
		Ok(info) => {
			let kind = info.root_kind.as_deref().unwrap_or("(bare metavariable)");
			let vars = if info.defined_vars.is_empty() {
				"none".to_string()
			} else {
				info.defined_vars
					.iter()
					.map(|v| format!("${v}"))
					.collect::<Vec<_>>()
					.join(",")
			};
			out.push_str(&format!(
				"Parsed: kind={kind}, parse_error={}, metavars={vars}\n",
				info.has_error
			));
			if info.has_error {
				out.push_str("Hint: the pattern does not parse as a complete node of this language — wrap the fragment in a valid context.\n");
			}
			out.push_str(
				"Hint: `$NAME` matches one node and `$$$NAME` a sequence (e.g. `fn $F($$$ARGS)`); `$_` matches any single node.\n",
			);
			out.push_str("Try a plain `content` search for literal text.");
		}
		Err(e) => {
			out.push_str(&format!("The pattern does not parse as {language}: {e}\n"));
			out.push_str("Hint: the pattern must be syntactically valid code in the target language. Try a plain `content` search for literal text.");
		}
	}
	out
}

#[cfg(test)]
#[path = "ast_search_tests.rs"]
mod ast_search_tests;
