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

//! Path search and neighborhood traversal tests.

use super::*;
use crate::structural::graph::types::CodeRelationship;

fn graph_of(edges: &[(&str, &str, RelationType)]) -> CodeGraph {
	let mut graph = CodeGraph::default();
	for (source, target, relation) in edges {
		graph.relationships.push(CodeRelationship {
			source: source.to_string(),
			target: target.to_string(),
			relation: *relation,
			confidence: 1.0,
			line: None,
		});
	}
	graph
}

// Shims over the indexed API so each test reads as a plain graph query. Local
// items shadow the glob import of the real functions.
fn neighbors(
	graph: &CodeGraph,
	start_ids: &[String],
	relations: &[RelationType],
	direction: Direction,
	depth: usize,
) -> Vec<Vec<Hop>> {
	super::neighbors(
		graph,
		&GraphIndex::new(graph),
		start_ids,
		relations,
		direction,
		depth,
		usize::MAX,
	)
	.levels
}

fn find_paths(graph: &CodeGraph, source: &str, target: &str, max_depth: usize) -> Vec<Vec<String>> {
	let (sources, targets) = (vec![source.to_string()], vec![target.to_string()]);
	super::find_paths(graph, &GraphIndex::new(graph), &sources, &targets, &[], max_depth).paths
}

fn ids(level: &[Hop]) -> Vec<&str> {
	level.iter().map(|hop| hop.node_id.as_str()).collect()
}

fn starts(ids: &[&str]) -> Vec<String> {
	ids.iter().map(|id| id.to_string()).collect()
}

#[test]
fn path_search_is_bounded_and_avoids_cycles() {
	let graph = graph_of(&[
		("a", "b", RelationType::Calls),
		("b", "a", RelationType::Calls),
		("b", "c", RelationType::Calls),
	]);

	assert!(find_paths(&graph, "a", "c", 1).is_empty());
	assert_eq!(find_paths(&graph, "a", "c", 2), vec![vec!["a", "b", "c"]]);
}

#[test]
fn path_search_returns_shortest_paths_first_and_caps_the_result() {
	// Three layers of three nodes between s and t: 27 simple paths of length 4.
	let mut edges = Vec::new();
	let layers = [["a1", "a2", "a3"], ["b1", "b2", "b3"], ["c1", "c2", "c3"]];
	for node in layers[0] {
		edges.push(("s", node, RelationType::Calls));
	}
	for pair in layers.windows(2) {
		for from in pair[0] {
			for to in pair[1] {
				edges.push((from, to, RelationType::Calls));
			}
		}
	}
	for node in layers[2] {
		edges.push((node, "t", RelationType::Calls));
	}
	edges.push(("s", "t", RelationType::Calls));
	let graph = graph_of(&edges);

	let paths = find_paths(&graph, "s", "t", 6);

	assert_eq!(paths.len(), MAX_PATHS);
	assert_eq!(paths[0], vec!["s", "t"]);
}

#[test]
fn path_search_gives_up_on_an_unreachable_target_in_a_dense_graph() {
	// Nine fully connected nodes have 109,601 simple paths from one start:
	// more than MAX_EXPANSIONS, so only the cap ends the search.
	let names: Vec<String> = (0..9).map(|index| format!("n{index}")).collect();
	let mut edges = Vec::new();
	for from in &names {
		for to in &names {
			if from != to {
				edges.push((from.as_str(), to.as_str(), RelationType::Calls));
			}
		}
	}
	let graph = graph_of(&edges);

	assert!(find_paths(&graph, "n0", "missing", 9).is_empty());
}

fn sample_graph() -> CodeGraph {
	graph_of(&[
		("a", "b", RelationType::Calls),
		("a", "c", RelationType::Imports),
		("b", "d", RelationType::Calls),
		("d", "a", RelationType::Calls),
	])
}

#[test]
fn outgoing_neighbors_are_grouped_by_distance() {
	let graph = sample_graph();

	let levels = neighbors(&graph, &starts(&["a"]), &[], Direction::Outgoing, 5);

	assert_eq!(levels.len(), 2);
	assert_eq!(ids(&levels[0]), vec!["b", "c"]);
	assert_eq!(ids(&levels[1]), vec!["d"]);
	let to_d = &levels[1][0];
	assert_eq!(to_d.from_node_id, "b");
	assert_eq!(graph.relationships[to_d.via].target, "d");
}

#[test]
fn depth_limits_the_levels() {
	let graph = sample_graph();

	assert!(neighbors(&graph, &starts(&["a"]), &[], Direction::Outgoing, 0).is_empty());
	assert_eq!(neighbors(&graph, &starts(&["a"]), &[], Direction::Outgoing, 1).len(), 1);
}

#[test]
fn start_nodes_and_cycles_never_reappear() {
	let graph = sample_graph();

	let levels = neighbors(&graph, &starts(&["a"]), &[], Direction::Outgoing, 10);

	let all: Vec<&str> = levels.iter().flat_map(|level| ids(level)).collect();
	assert!(!all.contains(&"a"));
	let mut unique = all.clone();
	unique.sort_unstable();
	unique.dedup();
	assert_eq!(unique.len(), all.len());
}

#[test]
fn relation_filter_restricts_the_edges_followed() {
	let graph = sample_graph();

	let calls_only = neighbors(&graph, &starts(&["a"]), &[RelationType::Calls], Direction::Outgoing, 5);
	assert_eq!(ids(&calls_only[0]), vec!["b"]);
	assert_eq!(ids(&calls_only[1]), vec!["d"]);

	let imports_only = neighbors(
		&graph,
		&starts(&["a"]),
		&[RelationType::Imports],
		Direction::Outgoing,
		5,
	);
	assert_eq!(imports_only.len(), 1);
	assert_eq!(ids(&imports_only[0]), vec!["c"]);

	assert!(neighbors(
		&graph,
		&starts(&["a"]),
		&[RelationType::Extends],
		Direction::Outgoing,
		5
	)
	.is_empty());
}

#[test]
fn incoming_neighbors_follow_edges_backwards() {
	let graph = sample_graph();

	let levels = neighbors(&graph, &starts(&["a"]), &[], Direction::Incoming, 5);

	assert_eq!(ids(&levels[0]), vec!["d"]);
	assert_eq!(levels[0][0].from_node_id, "a");
	assert_eq!(ids(&levels[1]), vec!["b"]);
	assert_eq!(levels.len(), 2);

	let who_imports_c = neighbors(
		&graph,
		&starts(&["c"]),
		&[RelationType::Imports],
		Direction::Incoming,
		1,
	);
	assert_eq!(ids(&who_imports_c[0]), vec!["a"]);
}

#[test]
fn several_start_nodes_share_one_visited_set() {
	let graph = graph_of(&[
		("x", "shared", RelationType::Calls),
		("y", "shared", RelationType::Calls),
		("shared", "leaf", RelationType::Calls),
	]);

	let levels = neighbors(&graph, &starts(&["x", "y", "x"]), &[], Direction::Outgoing, 5);

	assert_eq!(ids(&levels[0]), vec!["shared"]);
	assert_eq!(levels[0][0].from_node_id, "x");
	assert_eq!(ids(&levels[1]), vec!["leaf"]);
}

#[test]
fn start_ids_missing_from_the_graph_yield_nothing() {
	let graph = sample_graph();

	assert!(neighbors(&graph, &starts(&["nope"]), &[], Direction::Outgoing, 3).is_empty());
	assert!(neighbors(&graph, &[], &[], Direction::Outgoing, 3).is_empty());
}

#[test]
fn a_hub_cannot_make_a_query_examine_more_edges_than_the_cap() {
	let hub_edges: Vec<(String, String)> = (0..MAX_NEIGHBOR_EXPANSIONS + 50)
		.map(|index| ("hub".to_string(), format!("leaf{index}")))
		.collect();
	let edges: Vec<(&str, &str, RelationType)> = hub_edges
		.iter()
		.map(|(source, target)| (source.as_str(), target.as_str(), RelationType::Calls))
		.collect();
	let graph = graph_of(&edges);

	let levels = neighbors(&graph, &starts(&["hub"]), &[], Direction::Outgoing, 3);

	assert_eq!(levels.len(), 1);
	assert_eq!(levels[0].len(), MAX_NEIGHBOR_EXPANSIONS);
}

#[test]
fn the_hop_cap_stops_the_walk_and_says_so() {
	let graph = graph_of(&[
		("hub", "a", RelationType::Calls),
		("hub", "b", RelationType::Calls),
		("hub", "c", RelationType::Calls),
		("a", "deep", RelationType::Calls),
	]);
	let index = GraphIndex::new(&graph);

	let capped = super::neighbors(&graph, &index, &starts(&["hub"]), &[], Direction::Outgoing, 5, 2);
	assert!(capped.capped);
	assert_eq!(capped.levels.len(), 1);
	assert_eq!(capped.levels[0].len(), 2);

	let whole = super::neighbors(
		&graph,
		&index,
		&starts(&["hub"]),
		&[],
		Direction::Outgoing,
		5,
		usize::MAX,
	);
	assert!(!whole.capped);
	assert_eq!(whole.levels.len(), 2);
}

#[test]
fn one_search_serves_many_starts_and_ends_and_honours_the_edge_filter() {
	let graph = graph_of(&[
		("s1", "m", RelationType::Calls),
		("s2", "m", RelationType::Imports),
		("m", "t1", RelationType::Calls),
		("m", "t2", RelationType::Extends),
	]);
	let index = GraphIndex::new(&graph);
	let (sources, targets) = (starts(&["s1", "s2"]), starts(&["t1", "t2"]));

	let all = super::find_paths(&graph, &index, &sources, &targets, &[], 3);
	assert_eq!(all.paths.len(), 4);
	assert!(!all.path_cap_hit && !all.work_cap_hit);

	let calls = super::find_paths(&graph, &index, &sources, &targets, &[RelationType::Calls], 3);
	assert_eq!(calls.paths, vec![vec!["s1", "m", "t1"]]);
}

#[test]
fn the_path_cap_and_the_work_cap_are_reported() {
	let capped = {
		let mut edges = Vec::new();
		for node in ["a", "b", "c", "d", "e"] {
			edges.push(("s", node, RelationType::Calls));
			edges.push((node, "t", RelationType::Calls));
		}
		let graph = graph_of(&edges);
		let index = GraphIndex::new(&graph);
		super::find_paths(&graph, &index, &starts(&["s"]), &starts(&["t"]), &[], 3)
	};
	assert_eq!(capped.paths.len(), 5);
	assert!(!capped.path_cap_hit);

	let names: Vec<String> = (0..9).map(|index| format!("n{index}")).collect();
	let mut edges = Vec::new();
	for from in &names {
		for to in &names {
			if from != to {
				edges.push((from.as_str(), to.as_str(), RelationType::Calls));
			}
		}
	}
	let graph = graph_of(&edges);
	let index = GraphIndex::new(&graph);
	let dense = super::find_paths(&graph, &index, &starts(&["n0"]), &starts(&["missing"]), &[], 9);
	assert!(dense.paths.is_empty());
	assert!(dense.work_cap_hit);
}
