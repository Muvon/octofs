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

//! Bash outline extraction tests.

use crate::structural::outline;

#[test]
fn named_functions_are_items() {
	let src = "foo() {\n\techo hi\n}\n\nbar() {\n\techo hi\n}\n";
	let items = outline("a.sh", src).unwrap();
	let summary: Vec<(&str, usize)> = items.iter().map(|i| (i.name.as_str(), i.start_line)).collect();
	assert_eq!(summary, vec![("foo", 1), ("bar", 5)]);
}

#[test]
fn plain_commands_are_not_items() {
	assert!(outline("a.sh", "echo hi\nsource ./env.sh\n").unwrap().is_empty());
}
