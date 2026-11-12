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

//! Ruby outline extraction tests.

use crate::structural::outline;

#[test]
fn classes_modules_and_methods_are_items() {
	let src = "module Outer\n\tclass Foo\n\t\tdef bar\n\t\tend\n\tend\n\n\tdef self.baz\n\tend\nend\n";
	let items = outline("a.rb", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("module", "Outer"),
			("class", "Foo"),
			("method", "bar"),
			("method", "baz")
		]
	);
}

#[test]
fn qualified_module_names_are_named_by_their_last_segment() {
	let src = "module Outer::Inner\nend\n";
	let items = outline("a.rb", src).unwrap();
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].name, "Inner");
}
