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

//! Java outline extraction tests.

use crate::structural::outline;

#[test]
fn classes_methods_and_constructors_are_items() {
	let src = "package p;\n\npublic class Foo {\n\tprivate int x;\n\n\tpublic Foo() {}\n\n\tvoid bar() {}\n}\n";
	let items = outline("a.java", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("class", "Foo"), ("constructor", "Foo"), ("method", "bar")]
	);
}

#[test]
fn a_methods_name_is_not_its_return_type() {
	let src = "class A {\n\tString name() {\n\t\treturn \"\";\n\t}\n}\n";
	let items = outline("a.java", src).unwrap();
	assert_eq!(items.len(), 2);
	assert_eq!(items[1].name, "name");
}
