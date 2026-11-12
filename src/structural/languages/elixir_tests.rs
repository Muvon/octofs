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

//! Elixir outline extraction tests.

use crate::structural::outline;

#[test]
fn modules_and_defs_are_items() {
	let src = "defmodule Foo do\n\tdef bar do\n\t\t:ok\n\tend\n\n\tdefp baz, do: :ok\nend\n";
	let items = outline("a.ex", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("module", "Foo"), ("function", "bar"), ("function", "baz")]
	);
}

#[test]
fn ordinary_calls_are_not_items() {
	assert!(outline("a.exs", "Enum.map([1, 2], & &1)\n").unwrap().is_empty());
}

#[test]
fn defimpl_names_the_protocol_and_target() {
	let src = "defimpl Protocol, for: Integer do\n\tdef value(x), do: x\nend\n";
	let items = outline("a.ex", src).unwrap();
	let summary: Vec<(&str, &str)> = items.iter().map(|i| (i.kind.as_str(), i.name.as_str())).collect();
	assert_eq!(
		summary,
		vec![("implementation", "Protocol for Integer"), ("function", "value")]
	);
}
