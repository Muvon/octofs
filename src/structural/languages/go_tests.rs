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

//! Go outline extraction tests.

use crate::structural::outline;

#[test]
fn functions_methods_and_type_specs_are_items() {
	let src = "package main\n\nfunc main() {}\n\nfunc (s *T) Method() {}\n\ntype Widget struct {\n\tX int\n}\n";
	let items = outline("a.go", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("function", "main"), ("method", "Method"), ("type", "Widget")]
	);
}

#[test]
fn grouped_type_declarations_yield_one_item_per_spec() {
	let src = "package p\n\ntype (\n\tA struct{ X int }\n\tB int\n)\n";
	let items = outline("a.go", src).unwrap();
	let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
	assert_eq!(names, vec!["A", "B"]);
}
