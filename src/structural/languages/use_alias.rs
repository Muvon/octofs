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

//! The renames of `use` statements, shared by the languages that write them
//! (Rust, PHP).

use crate::structural::graph::symbols::ImportBinding;

/// The bindings of the renames in a `use` statement (`use a::b as c`,
/// `use A\B as C`, grouped `use a::{b as c, d as e}`): each `x as y` piece
/// binds `y` to what `x` names. `imports` are the full paths the statement
/// expanded to.
pub(crate) fn aliased_use_bindings(statement: &str, imports: &[String]) -> Vec<ImportBinding> {
	let cleaned = statement.trim_start_matches("use ").trim_end_matches(';').trim();
	let mut bindings = Vec::new();
	for piece in cleaned.split([',', '{', '}']) {
		let Some((path, alias)) = piece.rsplit_once(" as ") else {
			continue;
		};
		let path = path.trim();
		let imported = path
			.rsplit([':', '\\', '/'])
			.find(|part| !part.is_empty())
			.unwrap_or(path);
		let import_path = imports
			.iter()
			.find(|candidate| candidate.rsplit([':', '\\', '/']).next() == Some(imported))
			.or(imports.first())
			.cloned()
			.unwrap_or_else(|| path.to_string());
		bindings.push(ImportBinding {
			local_name: alias.trim().to_string(),
			imported_name: Some(imported.to_string()),
			import_path,
		});
	}
	bindings
}
