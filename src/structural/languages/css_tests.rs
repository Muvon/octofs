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

//! CSS outline extraction tests.

use crate::structural::outline;

#[test]
fn rule_sets_are_named_by_their_first_selector() {
	let src = ".card {\n\tcolor: red;\n}\n\n#main {\n\tmargin: 0;\n}\n";
	let items = outline("a.css", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![(".card", 1), ("#main", 5)]);
}

#[test]
fn keyframes_are_named_by_their_identifier() {
	let src = "@keyframes spin {\n\tfrom {\n\t\trotate: 0;\n\t}\n}\n";
	let items = outline("a.css", src).unwrap();
	let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
	assert!(names.contains(&"spin"), "items: {items:?}");
}
