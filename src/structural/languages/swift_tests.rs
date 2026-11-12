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

//! Swift outline extraction tests.

use crate::structural::outline;

#[test]
fn classes_methods_and_inits_are_items() {
	let src = "class Foo {\n\tfunc bar() {}\n\tinit() {}\n}\n\nstruct Point {\n\tvar x = 0.0\n}\n";
	let items = outline("a.swift", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("class", "Foo"),
			("function", "bar"),
			("constructor", "init"),
			("class", "Point")
		]
	);
}
