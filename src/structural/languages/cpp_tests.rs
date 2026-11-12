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

//! C++ outline extraction tests.

use crate::structural::outline;

#[test]
fn functions_structs_and_namespaces_are_items() {
	let src = "namespace ns {\nint foo() {\n\treturn 0;\n}\nstruct Widget {\n\tint x;\n};\n}\n";
	let items = outline("a.cpp", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("namespace", "ns"), ("function", "foo"), ("struct", "Widget")]
	);
}

#[test]
fn qualified_member_definitions_are_named_by_their_method() {
	let src = "void Widget::resize() {\n}\n";
	let items = outline("a.cpp", src).unwrap();
	assert_eq!(items.len(), 1);
	assert_eq!(items[0].name, "resize");
}

#[test]
fn bodyless_type_references_are_not_items() {
	// `struct stat st;` is a type reference, not a definition.
	let src = "struct stat st;\n";
	assert!(outline("a.cpp", src).unwrap().is_empty());
}
