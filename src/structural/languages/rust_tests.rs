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

//! Rust outline extraction tests.

use crate::structural::outline;

#[test]
fn functions_structs_enums_traits_and_consts_are_items() {
	let src = "use std::fmt;\n\nconst N: usize = 1;\n\nstruct Point {\n\tx: f64,\n}\n\nenum Kind {\n\tA,\n}\n\ntrait Show {\n\tfn show(&self) {}\n}\n\nfn main() {\n\tprintln!(\"hi\");\n}\n";
	let items = outline("a.rs", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("constant", "N"),
			("struct", "Point"),
			("enum", "Kind"),
			("trait", "Show"),
			("function", "show"),
			("function", "main"),
		]
	);
}

#[test]
fn impl_headers_and_methods_are_items() {
	let src = "impl Point {\n\tfn norm(&self) -> f64 {\n\t\t0.0\n\t}\n}\n";
	let items = outline("a.rs", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	// The impl header is an item too: methods of different impls are
	// otherwise indistinguishable in an outline.
	assert_eq!(summary, vec![("impl", "Point"), ("function", "norm")]);
}
