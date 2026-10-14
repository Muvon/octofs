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

//! A scratch crate for authentic Cargo/libtest output, with no network dependencies.

/// Adds two integers.
///
/// ```
/// assert_eq!(digest_fixture::add(2, 3), 5);
/// ```
pub fn add(a: i32, b: i32) -> i32 {
	a + b
}

#[cfg(feature = "compile_error")]
pub fn broken() -> i32 {
	"not an integer"
}

#[cfg(feature = "warnings")]
fn unused_function() {
	let unused_variable = 1;
}

#[cfg(feature = "warnings")]
pub fn clippy_warning() -> Vec<i32> {
	let mut values = Vec::new();
	values.push(1);
	values.push(2);
	values
}

#[test]
#[ignore = "deliberately ignored fixture"]
fn ignored_fixture() {
	assert_eq!(add(1, 1), 2);
}

#[test]
#[cfg(feature = "failures")]
fn assert_failure() {
	assert_eq!(add(1, 1), 3, "assertion details must survive");
}

#[test]
#[cfg(feature = "failures")]
fn panic_failure() {
	panic!("panic details must survive");
}
