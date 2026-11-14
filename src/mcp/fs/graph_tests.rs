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

//! Unit tests for the graph loader (walk, stamp, cache, skipped files) and
//! the rendering helpers.

use super::*;
use crate::structural::graph::CodeRelationship;
use serial_test::serial;
use std::sync::atomic::Ordering;

fn node(id: &str, name: &str, kind: &str, path: &str, owner: Option<&str>, line: u32) -> CodeNode {
	CodeNode {
		id: id.to_string(),
		name: name.to_string(),
		kind: kind.to_string(),
		path: path.to_string(),
		owner: owner.map(str::to_string),
		language: "rust".to_string(),
		start_line: line,
		end_line: line,
	}
}

fn edge(source: &str, target: &str, relation: RelationType, confidence: f32) -> CodeRelationship {
	CodeRelationship {
		source: source.to_string(),
		target: target.to_string(),
		relation,
		confidence,
		line: None,
	}
}

fn graph_of(nodes: Vec<CodeNode>, relationships: Vec<CodeRelationship>) -> CodeGraph {
	CodeGraph {
		nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
		relationships,
		skipped: Vec::new(),
		package_of: Default::default(),
	}
}

#[test]
fn stamp_changes_when_a_file_is_edited_added_or_removed() {
	let root = tempfile::tempdir().unwrap();
	let file = root.path().join("a.rs");
	std::fs::write(&file, "fn a() {}\n").unwrap();
	let before = stamp_of(&walk_source_files(root.path()).unwrap());
	assert_eq!(before, stamp_of(&walk_source_files(root.path()).unwrap()));

	// A longer body changes the size even when the clock does not tick.
	std::fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
	let edited = stamp_of(&walk_source_files(root.path()).unwrap());
	assert_ne!(before, edited);

	std::fs::write(root.path().join("b.rs"), "fn c() {}\n").unwrap();
	let added = stamp_of(&walk_source_files(root.path()).unwrap());
	assert_ne!(edited, added);

	std::fs::remove_file(root.path().join("b.rs")).unwrap();
	assert_eq!(edited, stamp_of(&walk_source_files(root.path()).unwrap()));
}

#[test]
fn walk_skips_unsupported_gitignored_and_non_graph_languages() {
	let root = tempfile::tempdir().unwrap();
	std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
	std::fs::write(root.path().join("notes.txt"), "text\n").unwrap();
	std::fs::write(root.path().join("README.md"), "# Title\n").unwrap();
	std::fs::write(root.path().join("data.json"), "{}\n").unwrap();
	std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
	std::fs::write(root.path().join("ignored.rs"), "fn x() {}\n").unwrap();

	let walked: Vec<String> = walk_source_files(root.path())
		.unwrap()
		.into_iter()
		.map(|file| file.path)
		.collect();
	assert_eq!(walked, vec!["a.rs".to_string()]);
}

#[test]
#[serial]
fn cache_is_reused_while_unchanged_and_rebuilt_after_an_edit() {
	let root = tempfile::tempdir().unwrap();
	let file = root.path().join("a.rs");
	std::fs::write(&file, "fn a() {}\n").unwrap();

	let first = load_graph_blocking(root.path()).unwrap();
	let second = load_graph_blocking(root.path()).unwrap();
	assert!(
		Arc::ptr_eq(&first, &second),
		"unchanged tree must reuse the cached graph"
	);
	assert!(first.graph.nodes.contains_key("a.rs"));

	std::fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
	let third = load_graph_blocking(root.path()).unwrap();
	assert!(!Arc::ptr_eq(&first, &third), "an edit must rebuild the graph");
}

#[test]
#[serial]
fn two_workdirs_do_not_evict_each_other() {
	let first_root = tempfile::tempdir().unwrap();
	let second_root = tempfile::tempdir().unwrap();
	std::fs::write(first_root.path().join("a.rs"), "fn a() {}\n").unwrap();
	std::fs::write(second_root.path().join("b.rs"), "fn b() {}\n").unwrap();

	let first = load_graph_blocking(first_root.path()).unwrap();
	let second = load_graph_blocking(second_root.path()).unwrap();
	assert!(first.graph.nodes.contains_key("a.rs") && !first.graph.nodes.contains_key("b.rs"));
	assert!(second.graph.nodes.contains_key("b.rs") && !second.graph.nodes.contains_key("a.rs"));
	assert!(Arc::ptr_eq(&first, &load_graph_blocking(first_root.path()).unwrap()));
	assert!(Arc::ptr_eq(&second, &load_graph_blocking(second_root.path()).unwrap()));
}

#[test]
#[serial]
fn the_least_recently_used_workdir_is_evicted_past_the_limit() {
	let roots: Vec<_> = (0..=MAX_CACHED_WORKDIRS)
		.map(|_| tempfile::tempdir().unwrap())
		.collect();
	let mut snapshots = Vec::new();
	for root in &roots {
		std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
		snapshots.push(load_graph_blocking(root.path()).unwrap());
	}
	let newest = load_graph_blocking(roots[MAX_CACHED_WORKDIRS].path()).unwrap();
	assert!(Arc::ptr_eq(&snapshots[MAX_CACHED_WORKDIRS], &newest));
	let oldest = load_graph_blocking(roots[0].path()).unwrap();
	assert!(
		!Arc::ptr_eq(&snapshots[0], &oldest),
		"the oldest entry must have been dropped"
	);
}

#[test]
#[serial]
fn a_same_size_edit_with_a_bumped_mtime_invalidates_the_cache() {
	let root = tempfile::tempdir().unwrap();
	let file = root.path().join("a.rs");
	std::fs::write(&file, "fn a() {}\n").unwrap();
	let first = load_graph_blocking(root.path()).unwrap();

	std::fs::write(&file, "fn b() {}\n").unwrap();
	let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
	std::fs::File::options()
		.write(true)
		.open(&file)
		.unwrap()
		.set_modified(bumped)
		.unwrap();
	let second = load_graph_blocking(root.path()).unwrap();
	assert!(!Arc::ptr_eq(&first, &second), "a same-size edit must rebuild the graph");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn concurrent_calls_build_the_graph_once() {
	let root = tempfile::tempdir().unwrap();
	std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
	let path = root.path().to_path_buf();
	let before = GRAPH_BUILDS.load(Ordering::SeqCst);

	let calls: Vec<_> = (0..8)
		.map(|_| {
			let path = path.clone();
			tokio::spawn(async move { load_graph(&path).await.unwrap() })
		})
		.collect();
	let mut snapshots = Vec::new();
	for call in calls {
		snapshots.push(call.await.unwrap());
	}
	assert_eq!(GRAPH_BUILDS.load(Ordering::SeqCst) - before, 1);
	assert!(snapshots.iter().all(|snapshot| Arc::ptr_eq(snapshot, &snapshots[0])));
}

#[test]
#[serial]
fn oversized_file_is_reported_not_parsed() {
	let root = tempfile::tempdir().unwrap();
	std::fs::write(root.path().join("small.rs"), "fn small() {}\n").unwrap();
	let big = vec![b'a'; MAX_GRAPH_FILE_BYTES as usize + 1];
	std::fs::write(root.path().join("big.rs"), big).unwrap();

	let snapshot = load_graph_blocking(root.path()).unwrap();
	assert!(snapshot.graph.nodes.contains_key("small.rs"));
	assert!(!snapshot.graph.nodes.contains_key("big.rs"));
	assert_eq!(snapshot.unread.len(), 1);
	assert_eq!(snapshot.unread[0].path, "big.rs");
	assert!(
		snapshot.unread[0].reason.contains("MB limit"),
		"{}",
		snapshot.unread[0].reason
	);

	let note = skipped_note(&snapshot).expect("skipped note");
	assert!(note.contains("big.rs"), "{note}");
	assert!(note.contains("1 file(s)"), "{note}");
}

#[test]
fn skipped_note_names_the_first_files_and_counts_the_rest() {
	let skipped = |path: &str, reason: &str| SkippedFile {
		path: path.to_string(),
		reason: reason.to_string(),
	};
	let snapshot = GraphSnapshot {
		graph: CodeGraph {
			skipped: vec![skipped("d.rs", UNREADABLE_REASON), skipped("e.rs", "syntax error")],
			..CodeGraph::default()
		},
		index: GraphIndex::new(&CodeGraph::default()),
		unread: vec![skipped("a.rs", "big"), skipped("b.rs", "big"), skipped("c.rs", "big")],
		left_out: 0,
		scoped: false,
	};
	let note = skipped_note(&snapshot).unwrap();
	assert_eq!(
		note,
		"[5 file(s) not in the graph: a.rs (big), b.rs (big), c.rs (big), +2 more]"
	);

	let graph_only = GraphSnapshot {
		unread: Vec::new(),
		..snapshot
	};
	let note = skipped_note(&graph_only).unwrap();
	assert!(
		note.contains("d.rs (unreadable)") && note.contains("e.rs (failed to parse)"),
		"{note}"
	);
}

#[test]
fn the_cap_note_counts_left_out_files() {
	let snapshot = GraphSnapshot {
		graph: CodeGraph::default(),
		index: GraphIndex::new(&CodeGraph::default()),
		unread: Vec::new(),
		left_out: 7,
		scoped: false,
	};
	let lines = notes(&snapshot);
	assert_eq!(lines.len(), 1);
	assert!(lines[0].contains("7 more file(s) left out"), "{}", lines[0]);
}

#[test]
fn start_nodes_match_name_or_qualified_name_inside_scope() {
	let graph = graph_of(
		vec![
			node("src/a.rs", "a.rs", FILE_KIND, "src/a.rs", None, 1),
			node(
				"src/a.rs::Store::flush",
				"flush",
				"function",
				"src/a.rs",
				Some("Store"),
				3,
			),
			node("lib/b.rs::flush", "flush", "function", "lib/b.rs", None, 1),
			node("external:flush", "flush", EXTERNAL_KIND, "", None, 0),
		],
		Vec::new(),
	);
	assert_eq!(start_nodes(&graph, "flush", None).len(), 2);
	assert_eq!(start_nodes(&graph, "flush", Some("")).len(), 2);
	assert_eq!(
		start_nodes(&graph, "flush", Some("src")),
		vec!["src/a.rs::Store::flush".to_string()]
	);
	assert_eq!(
		start_nodes(&graph, "flush", Some("lib/b.rs")),
		vec!["lib/b.rs::flush".to_string()]
	);
	assert_eq!(
		start_nodes(&graph, "Store::flush", None),
		vec!["src/a.rs::Store::flush".to_string()]
	);
	assert!(
		start_nodes(&graph, "flush", Some("sr")).is_empty(),
		"scope matches whole path components"
	);
	assert!(
		start_nodes(&graph, "a.rs", None).is_empty(),
		"file nodes are not symbols"
	);
}

#[test]
fn hop_labels_show_confidence_only_below_certain_and_external_imports_plainly() {
	let graph = graph_of(
		vec![
			node("a.rs::run", "run", "function", "a.rs", None, 4),
			node("a.rs::Store::flush", "flush", "function", "a.rs", Some("Store"), 9),
			node("external:serde", "serde", EXTERNAL_KIND, "", None, 0),
			node("b.rs", "b.rs", FILE_KIND, "b.rs", None, 1),
		],
		vec![
			edge(
				"a.rs::run",
				"a.rs::Store::flush",
				RelationType::Calls,
				CONFIDENCE_NAME_ONLY_MAX,
			),
			edge("a.rs::run", "a.rs::run", RelationType::Calls, 0.9),
			edge("b.rs", "external:serde", RelationType::Imports, 1.0),
			CodeRelationship {
				line: Some(12),
				..edge("b.rs", "a.rs::run", RelationType::Calls, 0.9)
			},
		],
	);
	let hop = |node_id: &str, via: usize| Hop {
		node_id: node_id.to_string(),
		via,
		from_node_id: String::new(),
	};
	assert_eq!(
		hop_label(&graph, &hop("a.rs::Store::flush", 0)),
		"a.rs:9  function  Store::flush (guess)"
	);
	assert_eq!(hop_label(&graph, &hop("a.rs::run", 1)), "a.rs:4  function  run");
	assert_eq!(hop_label(&graph, &hop("external:serde", 2)), "external: serde");
	assert_eq!(hop_label(&graph, &hop("b.rs", 2)), "b.rs  file");
	assert_eq!(hop_label(&graph, &hop("b.rs", 3)), "b.rs:12  file");
}

#[test]
fn chain_label_names_each_edge_type() {
	let graph = graph_of(
		vec![
			node("a.rs::run", "run", "function", "a.rs", None, 4),
			node("a.rs::Store", "Store", "struct", "a.rs", None, 1),
			node("b.rs::Base", "Base", "trait", "b.rs", None, 2),
		],
		vec![
			edge("a.rs::run", "a.rs::Store", RelationType::Calls, 1.0),
			edge("a.rs::Store", "b.rs::Base", RelationType::Implements, 1.0),
		],
	);
	let path = vec![
		"a.rs::run".to_string(),
		"a.rs::Store".to_string(),
		"b.rs::Base".to_string(),
	];
	assert_eq!(
		chain_label(&graph, &path),
		"a.rs:4 run -calls-> a.rs:1 Store -implements-> b.rs:2 Base"
	);
}

#[test]
fn report_keeps_whole_lines_under_the_response_budget_and_counts_the_rest() {
	let mut report = Report::default();
	let line = "x".repeat(1024);
	let pushed = MAX_GRAPH_RESPONSE_BYTES / line.len() + 100;
	let accepted = (0..pushed).filter(|_| report.push(line.clone())).count();
	assert!(accepted < pushed);
	assert_eq!(report.omitted, pushed - accepted);
	assert_eq!(report.lines.len(), accepted);
	assert!(report.finish().len() < MAX_GRAPH_RESPONSE_BYTES);
}

#[test]
fn a_cut_keeps_certain_hops_first_and_orders_each_group_by_path() {
	let graph = graph_of(
		vec![
			node("b.rs::f", "f", "function", "b.rs", None, 1),
			node("a.rs::f", "f", "function", "a.rs", None, 1),
			node("c.rs::f", "f", "function", "c.rs", None, 1),
			node("z.rs::t", "t", "function", "z.rs", None, 1),
		],
		vec![
			edge("z.rs::t", "b.rs::f", RelationType::Calls, 0.9),
			edge("z.rs::t", "a.rs::f", RelationType::Calls, CONFIDENCE_NAME_ONLY_MAX),
			edge("z.rs::t", "c.rs::f", RelationType::Calls, 0.9),
		],
	);
	let level: Vec<Hop> = ["a.rs::f", "b.rs::f", "c.rs::f"]
		.iter()
		.enumerate()
		.map(|(via, id)| Hop {
			node_id: id.to_string(),
			via: [1, 0, 2][via],
			from_node_id: "z.rs::t".to_string(),
		})
		.collect();
	let rows = ranked_rows(&graph, &level);
	assert_eq!(
		rows,
		vec![
			"b.rs:1  function  f".to_string(),
			"c.rs:1  function  f".to_string(),
			"a.rs:1  function  f (guess)".to_string(),
		]
	);
}

#[test]
fn a_name_with_many_definitions_is_listed_not_followed() {
	let nodes: Vec<CodeNode> = (0..MAX_EXPANDED_DEFINITIONS + 3)
		.map(|n| {
			node(
				&format!("f{n}.rs::new"),
				"new",
				"function",
				&format!("f{n}.rs"),
				Some("T"),
				1,
			)
		})
		.collect();
	let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
	let graph = graph_of(nodes, Vec::new());
	let text = list_definitions(&graph, "new", &ids);
	assert!(text.starts_with("`new` has 11 definitions"), "{text}");
	assert!(text.contains("Owner::name"), "{text}");
	assert_eq!(text.lines().count(), 12, "{text}");
	assert!(text.contains("f0.rs:1  function  T::new"), "{text}");
}

#[test]
fn effective_depth_defaults_per_relation_and_caps() {
	let request = |relation, depth| GraphRequest {
		relation,
		name: Some("a"),
		to: None,
		path: None,
		depth,
	};
	assert_eq!(effective_depth(&request(GraphRelation::Callers, None)), 1);
	assert_eq!(effective_depth(&request(GraphRelation::Path, None)), MAX_GRAPH_DEPTH);
	assert_eq!(effective_depth(&request(GraphRelation::Callees, Some(3))), 3);
	assert_eq!(
		effective_depth(&request(GraphRelation::Callees, Some(99))),
		MAX_GRAPH_DEPTH
	);
}

#[test]
fn relations_parse_by_their_wire_names() {
	for (name, relation) in [
		("callers", GraphRelation::Callers),
		("callees", GraphRelation::Callees),
		("imports", GraphRelation::Imports),
		("imported_by", GraphRelation::ImportedBy),
		("supertypes", GraphRelation::Supertypes),
		("subtypes", GraphRelation::Subtypes),
		("path", GraphRelation::Path),
	] {
		assert_eq!(GraphRelation::parse(name), Some(relation));
	}
	assert_eq!(GraphRelation::parse("definition"), None);
}
