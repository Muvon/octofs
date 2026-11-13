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

use super::languages;

/// Rewrite `$` metavariables to `expando` before parsing, mirroring
/// ast-grep's own language configs: languages whose identifiers cannot start
/// with `$` (rust, python, go, ruby, swift, php, elixir use `µ`, C and C++
/// `𐀀`) would otherwise parse `$NAME` into error nodes that match nothing.
/// `$`s that are not metavariable-shaped — lowercase names, `$` inside
/// literals — are left alone. With `expando == '$'` this is the identity
/// rewrite.
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

/// Bridge the tree-sitter grammar crates to ast-grep's language traits. The
/// expando char replaces metavariable `$`s at parse time for languages whose
/// identifiers may not start with `$` (ast-grep's configs use `µ` for them,
/// `𐀀` for C/C++); languages where `$` is a valid identifier character keep
/// `$`, making the rewrite a no-op.
macro_rules! define_ast_grep_lang {
	($name:ident, $ts_lang:expr, $expando:expr) => {
		#[derive(Clone, Copy)]
		pub struct $name;

		impl AstGrepLanguage for $name {
			fn expando_char(&self) -> char {
				$expando
			}

			fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
				expand_pattern($expando, query)
			}

			fn kind_to_id(&self, kind: &str) -> u16 {
				let ts_lang: tree_sitter::Language = $ts_lang.into();
				ts_lang.id_for_node_kind(kind, true)
			}

			fn field_to_id(&self, field: &str) -> Option<u16> {
				self.get_ts_language().field_id_for_name(field).map(|f| f.get())
			}

			fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
				builder.build(|src| StrDoc::try_new(src, self.clone()))
			}
		}

		impl LanguageExt for $name {
			fn get_ts_language(&self) -> tree_sitter::Language {
				$ts_lang.into()
			}
		}
	};
}

define_ast_grep_lang!(AstRust, tree_sitter_rust::LANGUAGE, 'µ');
define_ast_grep_lang!(AstJavaScript, tree_sitter_javascript::LANGUAGE, '$');
define_ast_grep_lang!(AstTypeScript, tree_sitter_typescript::LANGUAGE_TYPESCRIPT, '$');
define_ast_grep_lang!(AstPython, tree_sitter_python::LANGUAGE, 'µ');
define_ast_grep_lang!(AstGo, tree_sitter_go::LANGUAGE, 'µ');
define_ast_grep_lang!(AstJava, tree_sitter_java::LANGUAGE, '$');
define_ast_grep_lang!(AstCpp, tree_sitter_cpp::LANGUAGE, '𐀀');
// Plain C gets its own grammar: the C++ grammar parses .c files into
// `declaration` nodes that ellipsis-body patterns never match, while the C
// grammar yields `function_definition` like upstream ast-grep's `--lang c`.
define_ast_grep_lang!(AstC, tree_sitter_c::LANGUAGE, '𐀀');
// PHP patterns carry no `<?php` prologue, so they parse with the tagless
// PHP-only grammar (ast-grep's own choice); a prologue in the file itself
// recovers as an error node that matching simply walks past.
define_ast_grep_lang!(AstPhp, tree_sitter_php::LANGUAGE_PHP_ONLY, 'µ');
define_ast_grep_lang!(AstRuby, tree_sitter_ruby::LANGUAGE, 'µ');
define_ast_grep_lang!(AstSwift, tree_sitter_swift::LANGUAGE, 'µ');
define_ast_grep_lang!(AstLua, tree_sitter_lua::LANGUAGE, '$');
define_ast_grep_lang!(AstBash, tree_sitter_bash::LANGUAGE, '$');
define_ast_grep_lang!(AstElixir, tree_sitter_elixir::LANGUAGE, 'µ');

/// Dispatch a search by language string. Each arm binds the `Ast*` type and
/// monomorphizes the generic helper for it.
macro_rules! dispatch_lang {
	($lang_str:expr, |$lang:ident| $body:expr) => {
		match $lang_str {
			"rust" => {
				#[allow(unused_variables)]
				let $lang = AstRust;
				$body
			}
			"javascript" => {
				#[allow(unused_variables)]
				let $lang = AstJavaScript;
				$body
			}
			"typescript" => {
				#[allow(unused_variables)]
				let $lang = AstTypeScript;
				$body
			}
			"python" => {
				#[allow(unused_variables)]
				let $lang = AstPython;
				$body
			}
			"go" => {
				#[allow(unused_variables)]
				let $lang = AstGo;
				$body
			}
			"java" => {
				#[allow(unused_variables)]
				let $lang = AstJava;
				$body
			}
			"cpp" => {
				#[allow(unused_variables)]
				let $lang = AstCpp;
				$body
			}
			"c" => {
				#[allow(unused_variables)]
				let $lang = AstC;
				$body
			}
			"php" => {
				#[allow(unused_variables)]
				let $lang = AstPhp;
				$body
			}
			"ruby" => {
				#[allow(unused_variables)]
				let $lang = AstRuby;
				$body
			}
			"swift" => {
				#[allow(unused_variables)]
				let $lang = AstSwift;
				$body
			}
			"lua" => {
				#[allow(unused_variables)]
				let $lang = AstLua;
				$body
			}
			"bash" => {
				#[allow(unused_variables)]
				let $lang = AstBash;
				$body
			}
			"elixir" => {
				#[allow(unused_variables)]
				let $lang = AstElixir;
				$body
			}
			_ => Err(format!("No grammar implementation for language '{}'.", $lang_str)),
		}
	};
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
/// without a grammar and for css/json/markdown, where a syntax-tree pattern
/// buys nothing over the outline, symbol and content modes. `.c`/`.h` use the
/// C grammar — upstream ast-grep's own mapping; the C++ grammar mis-parses
/// plain C so ellipsis-body patterns miss. The outline and symbol modes keep
/// `language_for_extension`'s C++ mapping for `.c`/`.h`, which serves them
/// correctly.
pub fn ast_language(path_label: &str) -> Option<&'static str> {
	let extension = super::path_extension(path_label);
	let language = super::language_for_extension(&extension)?;
	let language = match extension.as_str() {
		"c" | "h" => "c",
		_ => language,
	};
	match language {
		"css" | "json" | "markdown" => None,
		language => Some(language),
	}
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
/// language name. Svelte searches each `<script>` block with the JS/TS
/// grammar and offsets hits back to file coordinates.
pub fn search_source(language: &str, source: &str, pattern: &str) -> Result<Vec<AstMatch>, String> {
	if language == "svelte" {
		return search_svelte(source, pattern);
	}
	dispatch_lang!(language, |lang| search_with_lang(lang, source, pattern))
}

fn search_svelte(source: &str, pattern: &str) -> Result<Vec<AstMatch>, String> {
	let mut matches = Vec::new();
	for (language, script, start_row, _) in languages::svelte::script_sources(source) {
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
fn demangle_pattern_error<L: AstGrepLanguage>(lang: &L, pattern: &str, error: PatternError) -> String {
	let message = error.to_string();
	let expanded = expand_pattern(lang.expando_char(), pattern);
	if expanded.as_ref() != pattern {
		return message.replacen(expanded.as_ref(), pattern, 1);
	}
	message
}

fn search_with_lang<L: LanguageExt + AstGrepLanguage + Clone>(
	lang: L,
	source: &str,
	pattern_str: &str,
) -> Result<Vec<AstMatch>, String> {
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
	if language == "svelte" {
		// A svelte pattern is JavaScript/TypeScript code; report it as the
		// language the pattern actually parses as.
		return pattern_info(pattern, "javascript");
	}
	dispatch_lang!(language, |lang| pattern_info_with_lang(lang, pattern))
}

fn pattern_info_with_lang<L: LanguageExt + AstGrepLanguage + Clone>(
	lang: L,
	pattern_str: &str,
) -> Result<PatternInfo, String> {
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
