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

//! Python outline extraction tests.

use crate::structural::outline;

#[test]
fn classes_and_nested_methods_are_items() {
	let src = "import os\n\n\nclass Foo:\n\tdef bar(self):\n\t\tpass\n\n\ndef baz():\n\tpass\n";
	let items = outline("a.py", src).unwrap();
	let summary: Vec<(&str, &str, usize)> = items
		.iter()
		.map(|i| (i.kind.as_str(), i.name.as_str(), i.start_line))
		.collect();
	assert_eq!(
		summary,
		vec![("class", "Foo", 4), ("function", "bar", 5), ("function", "baz", 9)]
	);
}
