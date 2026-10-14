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

//! Parser-independent safety invariants, including deliberately broken selections.

use super::*;

fn drop_everything(_: &[&str], _: &[&str]) -> Option<Selection> {
	Some(Selection {
		stdout: Vec::new(),
		stderr: Vec::new(),
	})
}

fn keep_first(stdout: &[&str], _: &[&str]) -> Option<Selection> {
	Some(Selection {
		stdout: (0..stdout.len().min(1)).collect(),
		stderr: Vec::new(),
	})
}

#[test]
fn small_and_unknown_output_is_untouched() {
	let spill = Path::new("/tmp/full-output.log");
	assert!(digest_with("small", "", spill, &[drop_everything]).is_none());
	assert!(digest_with(&"x".repeat(MIN_OUTPUT_BYTES), "", spill, &[drop_everything]).is_none());
	assert!(digest(&"unknown text\n".repeat(1000), "", spill).is_none());
}

#[test]
fn important_lines_and_whole_blocks_cannot_be_dropped() {
	for input in [
		"error[E0308]: mismatch\nordinary diagnostic details\n\nnoise",
		"warning: unused item\n  --> src/lib.rs:10:1\nsource excerpt\n\nnoise",
		"failures:\n\n---- example stdout ----\nfirst paragraph\n\nsecond paragraph\n\nfailures:\n    example\n\ntest result: FAILED",
		"---- example stdout ----\narbitrary details\n\npanic details\ntest result: FAILED",
		"thread 'example' panicked",
		"test important_error_name ... ok",
		"Finished verdict",
	] {
		let lines = lines(input);
		assert!(!valid_selection(&lines, &[]), "{input}");
		let all: Vec<_> = (0..lines.len()).collect();
		assert!(valid_selection(&lines, &all), "{input}");
		for omitted in 0..lines.len() {
			if lines[omitted] == "noise" {
				continue;
			}
			let selected: Vec<_> = all.iter().copied().filter(|&i| i != omitted).collect();
			assert!(!valid_selection(&lines, &selected), "lost block line {omitted}: {input}");
		}
	}
}

#[test]
fn indices_must_be_an_in_order_subsequence() {
	let input = ["first", "second", "third"];
	assert!(!valid_selection(&input, &[1, 0]));
	assert!(!valid_selection(&input, &[0, 0]));
	assert!(!valid_selection(&input, &[0, 3]));
	assert!(valid_selection(&input, &[0, 2]));
	let repeated = ["warning: same", "", "warning: same"];
	assert!(!valid_selection(&repeated, &[0, 1]));
}

#[test]
fn broken_parser_is_rejected_at_public_boundary() {
	let spill = Path::new("/tmp/full-output.log");
	let padding = "ordinary noise\n".repeat(1000);
	for important in [
		"error: essential",
		"warning: essential",
		"test result: FAILED",
		"--> source",
	] {
		let input = format!("{padding}{important}");
		assert!(digest_with(&input, "", spill, &[drop_everything]).is_none());
		assert!(digest_with("", &input, spill, &[drop_everything]).is_none());
	}
	let unordered: Parser = |_, _| {
		Some(Selection {
			stdout: vec![1, 0],
			stderr: Vec::new(),
		})
	};
	let duplicated: Parser = |_, _| {
		Some(Selection {
			stdout: vec![0, 0],
			stderr: Vec::new(),
		})
	};
	let out_of_bounds: Parser = |stdout, _| {
		Some(Selection {
			stdout: vec![stdout.len()],
			stderr: Vec::new(),
		})
	};
	for parser in [unordered, duplicated, out_of_bounds] {
		assert!(digest_with(&padding, "", spill, &[parser]).is_none());
	}
}

#[test]
fn savings_include_the_marker_and_stream_framing() {
	let input = format!("{}\nnoise", "x".repeat(MIN_OUTPUT_BYTES));
	assert!(digest_with(&input, "", Path::new("/tmp/output"), &[keep_first]).is_none());
	let input = format!("keep\n{}", "noise\n".repeat(1000));
	let spill = Path::new("/tmp/output");
	let digest = digest_with(&input, "", spill, &[keep_first]).unwrap();
	assert_eq!(
		digest,
		"keep\n[digest: 1001 lines omitted — full output: /tmp/output]"
	);
	assert!(digest_with(
		&input,
		"",
		Path::new(&"x".repeat(input.len())),
		&[keep_first]
	)
	.is_none());
}

#[test]
fn spill_path_must_fit_one_verbatim_marker_line() {
	let input = format!("keep\n{}", "noise\n".repeat(1000));
	assert!(digest_with(&input, "", Path::new("/tmp/multi\nline"), &[keep_first]).is_none());
	#[cfg(unix)]
	{
		use std::os::unix::ffi::OsStrExt;
		let path = std::ffi::OsStr::from_bytes(b"/tmp/non-utf8-\xff");
		assert!(digest_with(&input, "", Path::new(path), &[keep_first]).is_none());
	}
}
