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

//! JavaScript outline extraction tests.

use crate::structural::outline;

#[test]
fn functions_methods_classes_and_named_arrows_are_items() {
	let src = "import x from \"y\";\n\nfunction foo() {}\n\nclass Bar {\n\tmethod() {}\n}\n\nconst baz = () => {};\n";
	let items = outline("a.js", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![
			("function", "foo"),
			("class", "Bar"),
			("method", "method"),
			("function", "baz")
		]
	);
}

#[test]
fn imports_exports_and_anonymous_arrows_are_not_items() {
	let src = "import x from \"y\";\nexport const k = 1;\ncall(() => {});\n";
	assert!(outline("a.js", src).unwrap().is_empty());
}
