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

//! JSON outline extraction tests.

use crate::structural::outline;

#[test]
fn objects_are_named_by_their_first_key() {
	let src = "{\n\t\"name\": \"x\",\n\t\"nested\": {\n\t\t\"a\": 1\n\t}\n}\n";
	let items = outline("a.json", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![("name", 1), ("a", 3)]);
}

#[test]
fn arrays_are_named_by_the_key_they_are_the_value_of() {
	let src = "{\n\t\"name\": \"x\",\n\t\"items\": [\n\t\t1,\n\t\t2\n\t]\n}\n";
	let items = outline("a.json", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![("name", 1), ("items", 3)]);
}
