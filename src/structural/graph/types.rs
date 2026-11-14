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

//! The code graph's data model: nodes (files, symbols, external imports),
//! typed relationships between them, and the files that could not be parsed.

use std::collections::{BTreeMap, HashMap};

/// Node kind of a source file.
pub const FILE_KIND: &str = "source_file";

/// Node kind of an import that resolves to no file of the repository.
pub const EXTERNAL_KIND: &str = "external";

/// One vertex of the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeNode {
	/// Unique id: the workdir-relative path for a file, `path::name` (or
	/// `path::owner::name`) for a symbol, `external:<import>` for an external
	/// import.
	pub id: String,
	pub name: String,
	/// `source_file`, `external`, or a coarse symbol kind ("function",
	/// "class", "struct", "trait", "interface", "enum", "module", "macro",
	/// "const", "type", "symbol").
	pub kind: String,
	/// Workdir-relative path with forward slashes; empty for external nodes.
	pub path: String,
	/// Nearest syntactic type/module that owns a symbol.
	pub owner: Option<String>,
	/// Language of the file; empty for external nodes.
	pub language: String,
	/// 1-based first line; 0 for external nodes.
	pub start_line: u32,
	/// 1-based last line; 0 for external nodes.
	pub end_line: u32,
}

/// What a relationship says about its two endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RelationType {
	/// A file declares a symbol.
	Contains,
	/// A file imports a file or an external module.
	Imports,
	/// A function calls a function.
	Calls,
	/// A type inherits from another type.
	Extends,
	/// A type implements an interface or trait.
	Implements,
}

impl RelationType {
	pub fn as_str(self) -> &'static str {
		match self {
			RelationType::Contains => "contains",
			RelationType::Imports => "imports",
			RelationType::Calls => "calls",
			RelationType::Extends => "extends",
			RelationType::Implements => "implements",
		}
	}
}

/// One directed edge of the graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeRelationship {
	/// Id of the node the edge starts at.
	pub source: String,
	/// Id of the node the edge points to.
	pub target: String,
	pub relation: RelationType,
	/// How sure the resolver is that `target` is what the source refers to:
	/// 1.0 for facts read straight from the syntax, lower for name matches.
	pub confidence: f32,
	/// 1-based line of the call or type clause in the source file, when the
	/// relation has one.
	pub line: Option<u32>,
}

/// A file that was handed in but contributed nothing to the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
	pub path: String,
	pub reason: String,
}

/// Nodes keyed by id, relationships sorted by (source, target, relation) with
/// duplicates removed, and the files that failed to parse.
#[derive(Debug, Clone, Default)]
pub struct CodeGraph {
	pub nodes: BTreeMap<String, CodeNode>,
	pub relationships: Vec<CodeRelationship>,
	pub skipped: Vec<SkippedFile>,
	/// File -> canonical file of its package, for languages whose imports
	/// name a package of several files (Go, Java wildcards, Swift modules):
	/// one `Imports` edge per importer targets the canonical file, and this
	/// map lets every other file of the package answer for it. Files that
	/// are their own canonical file are absent.
	pub package_of: HashMap<String, String>,
}
