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

//! Unit tests for symbol result rendering: full vs preview definitions,
//! reference grouping, compact modes, truncation.

use super::*;

fn hit(line: usize, line_count: usize, is_definition: bool, breadcrumb: Option<&str>) -> SymbolHit {
	SymbolHit {
		line,
		line_count,
		column: 0,
		is_definition,
		breadcrumb: breadcrumb.map(str::to_string),
	}
}

#[test]
fn small_exact_definitions_render_in_full_with_real_ids() {
	let contents = "fn small() {\n\t1\n}\n";
	let hits = vec![hit(1, 3, true, None)];
	let (body, omitted) = render_definition_hits(contents, &hits, true, MAX_RESULT_BYTES);
	assert_eq!(omitted, 0);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines.len(), 3);
	// The ids are exactly those of a normal view of the same lines.
	assert_eq!(lines[0], format!("{}|fn small() {{", line_id(1, "fn small() {")));
	assert_eq!(lines[2], format!("{}|}}", line_id(3, "}")));
}

#[test]
fn long_definitions_render_a_bounded_preview() {
	// 40-line fn: 4 preview lines, the omitted-lines marker, the last line.
	let mut contents = String::from("fn big() {\n");
	for i in 1..=38 {
		contents.push_str(&format!("\tlet v{i} = {i};\n"));
	}
	contents.push_str("}\n");
	let hits = vec![hit(1, 40, true, None)];
	let (body, omitted) = render_definition_hits(&contents, &hits, true, MAX_RESULT_BYTES);
	assert_eq!(omitted, 0);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines.len(), 6);
	assert_eq!(lines[4], "… 35 lines omitted …");
	assert_eq!(lines[5], format!("{}|}}", line_id(40, "}")));
}

#[test]
fn wildcard_definitions_always_render_a_preview() {
	// Six lines, wildcard: 4 preview lines, marker, last line — even though
	// an exact name would have returned the whole definition.
	let contents = "fn six() {\n\t2\n\t3\n\t4\n\t5\n}\n";
	let hits = vec![hit(1, 6, true, None)];
	let (body, _) = render_definition_hits(contents, &hits, false, MAX_RESULT_BYTES);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines.len(), 6);
	assert_eq!(lines[4], "… 1 lines omitted …");
	assert_eq!(lines[5], format!("{}|}}", line_id(6, "}")));
}

#[test]
fn nested_definitions_carry_their_breadcrumb_header() {
	let contents = "impl Store {\n\tpub fn flush(&mut self) {\n\t\tself.clear();\n\t}\n}\n";
	let hits = vec![hit(2, 3, true, Some("impl Store"))];
	let (body, _) = render_definition_hits(contents, &hits, true, MAX_RESULT_BYTES);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines[0], "[impl Store]");
	assert_eq!(
		lines[1],
		format!(
			"{}|\tpub fn flush(&mut self) {{",
			line_id(2, "\tpub fn flush(&mut self) {")
		)
	);
}

#[test]
fn references_group_by_breadcrumb_and_tag_definition_sites() {
	let contents = "fn flush() {\n\tok\n}\nfn drain() {\n\tflush();\n}\n";
	let hits = vec![hit(1, 1, true, Some("fn flush")), hit(5, 1, false, Some("fn drain"))];
	let (body, omitted) = render_reference_hits(contents, &hits, 0, MAX_RESULT_BYTES);
	assert_eq!(omitted, 0);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines[0], "[fn flush] [def]");
	assert_eq!(lines[1], format!("{}|fn flush() {{", line_id(1, "fn flush() {")));
	assert_eq!(lines[2], "[fn drain]");
	assert_eq!(lines[3], format!("{}|\tflush();", line_id(5, "\tflush();")));
}

#[test]
fn top_level_definition_sites_get_a_bare_def_header() {
	let contents = "const MAX: usize = 1;\n\nfn main() {\n\tlet x = MAX;\n}\n";
	let hits = vec![hit(1, 1, true, None), hit(4, 1, false, Some("fn main"))];
	let (body, _) = render_reference_hits(contents, &hits, 0, MAX_RESULT_BYTES);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines[0], "[def]");
	assert_eq!(
		lines[1],
		format!("{}|const MAX: usize = 1;", line_id(1, "const MAX: usize = 1;"))
	);
	assert_eq!(lines[2], "[fn main]");
}

#[test]
fn reference_context_expands_and_never_repeats_a_line() {
	let contents = "1\n2\n3\n4\n5\n6\n";
	// Hits on lines 2 and 5 with one context line each: ranges (1,3) and
	// (4,6) merge into every line exactly once.
	let hits = vec![hit(2, 1, false, None), hit(5, 1, false, None)];
	let (body, _) = render_reference_hits(contents, &hits, 1, MAX_RESULT_BYTES);
	let lines: Vec<&str> = body.split('\n').collect();
	assert_eq!(lines.len(), 6);
	for (i, line) in lines.iter().enumerate() {
		assert_eq!(
			*line,
			format!("{}|{}", line_id(i + 1, contents.lines().nth(i).unwrap()), i + 1)
		);
	}
}

#[test]
fn count_and_files_only_bodies() {
	let contents = "fn a() {\n}\n";
	let hits = vec![hit(1, 2, true, None)];
	let matcher = NameMatcher::new("a").unwrap();
	let count = ViewFlags {
		output: SearchOutput::Count,
		..ViewFlags::default()
	};
	assert_eq!(
		render_hits_body(contents, &hits, &matcher, count, 0).as_deref(),
		Some("1")
	);
	let files_only = ViewFlags {
		output: SearchOutput::FilesOnly,
		..ViewFlags::default()
	};
	assert_eq!(
		render_hits_body(contents, &hits, &matcher, files_only, 0).as_deref(),
		Some("")
	);
	assert_eq!(render_hits_body(contents, &[], &matcher, count, 0), None);
}

#[test]
fn budget_truncation_counts_omitted_hits() {
	let contents = "fn big() {\n\t2\n\t3\n}\n";
	let hits = vec![hit(1, 4, true, None)];
	// A budget that cannot hold even one block: everything is omitted.
	let (body, omitted) = render_definition_hits(contents, &hits, true, 260);
	assert!(body.is_empty());
	assert_eq!(omitted, 1);
}

#[test]
fn no_match_message_names_the_symbol_and_suggests_alternatives() {
	let defs = no_symbol_matches(ViewFlags::default(), "flush", "in a.rs");
	assert!(
		defs.starts_with("No definitions found for symbol \"flush\" in a.rs."),
		"{defs}"
	);
	assert!(defs.contains("Try a `*` wildcard"), "{defs}");
	let refs = no_symbol_matches(
		ViewFlags {
			references: true,
			..ViewFlags::default()
		},
		"flush",
		"under src",
	);
	assert!(
		refs.starts_with("No references found for symbol \"flush\" under src."),
		"{refs}"
	);
}

#[test]
fn directory_join_reports_empty_and_compact_results() {
	let empty = join_symbol_blocks("src", Vec::new(), ViewFlags::default(), "flush");
	assert!(
		empty.starts_with("No definitions found for symbol \"flush\" under src."),
		"{empty}"
	);
	let compact = ViewFlags {
		output: SearchOutput::Count,
		..ViewFlags::default()
	};
	let joined = join_symbol_blocks(
		"src",
		vec!["a.rs\t1".to_string(), "b.rs\t2".to_string()],
		compact,
		"flush",
	);
	assert_eq!(joined, "a.rs\t1\nb.rs\t2");
}
