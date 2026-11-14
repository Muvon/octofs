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

//! Bounded graph queries: directed paths between two nodes and the
//! breadth-first neighborhood of a set of nodes.

use super::types::{CodeGraph, RelationType};
use std::collections::{HashMap, HashSet, VecDeque};

/// Most paths `find_paths` returns; stops the search once reached.
pub const MAX_PATHS: usize = 20;

/// Total-work bound for `find_paths`: the path cap alone only triggers on
/// reaching a target, so an unreachable target on a dense graph would still
/// enumerate every simple path up to the depth limit.
const MAX_EXPANSIONS: usize = 100_000;

/// Most edges `neighbors` examines in one call. A hub node on a large graph
/// would otherwise make a deep query touch every edge several times over.
const MAX_NEIGHBOR_EXPANSIONS: usize = 100_000;

/// Edge indexes per node, built once per graph so a query costs its own
/// degree instead of a scan (and a hash map) of every edge.
pub struct GraphIndex {
	outgoing: HashMap<String, Vec<usize>>,
	incoming: HashMap<String, Vec<usize>>,
}

impl GraphIndex {
	pub fn new(graph: &CodeGraph) -> Self {
		let mut outgoing: HashMap<String, Vec<usize>> = HashMap::new();
		let mut incoming: HashMap<String, Vec<usize>> = HashMap::new();
		for (index, relationship) in graph.relationships.iter().enumerate() {
			outgoing.entry(relationship.source.clone()).or_default().push(index);
			incoming.entry(relationship.target.clone()).or_default().push(index);
		}
		Self { outgoing, incoming }
	}

	fn edges(&self, node: &str, direction: Direction) -> &[usize] {
		let side = match direction {
			Direction::Outgoing => &self.outgoing,
			Direction::Incoming => &self.incoming,
		};
		side.get(node).map(Vec::as_slice).unwrap_or(&[])
	}
}

/// What a directed path search found and why it stopped.
#[derive(Debug, Default)]
pub struct PathSearch {
	pub paths: Vec<Vec<String>>,
	/// `MAX_PATHS` were found; more may exist.
	pub path_cap_hit: bool,
	/// The search gave up after `MAX_EXPANSIONS` steps; paths may be missing.
	pub work_cap_hit: bool,
}

/// Bounded breadth-first search for directed simple paths from any of
/// `source_ids` to any of `target_ids` over the edge types in `relations`
/// (empty: every type), shortest first. One search serves every start and end
/// pair; a node that is both start and end is not a path to itself.
pub fn find_paths(
	graph: &CodeGraph,
	index: &GraphIndex,
	source_ids: &[String],
	target_ids: &[String],
	relations: &[RelationType],
	max_depth: usize,
) -> PathSearch {
	let targets: HashSet<&str> = target_ids.iter().map(String::as_str).collect();
	let mut queue: VecDeque<Vec<String>> = source_ids.iter().map(|id| vec![id.clone()]).collect();
	let mut search = PathSearch::default();
	let mut expansions = 0usize;
	while let Some(path) = queue.pop_front() {
		expansions += 1;
		if expansions > MAX_EXPANSIONS {
			search.work_cap_hit = true;
			break;
		}
		let current = path.last().map(String::as_str).unwrap_or_default();
		if path.len() > 1 && targets.contains(current) {
			search.paths.push(path);
			if search.paths.len() >= MAX_PATHS {
				search.path_cap_hit = true;
				break;
			}
			continue;
		}
		if path.len().saturating_sub(1) >= max_depth {
			continue;
		}
		let mut next: Vec<&str> = index
			.edges(current, Direction::Outgoing)
			.iter()
			.map(|&edge| &graph.relationships[edge])
			.filter(|relationship| relations.is_empty() || relations.contains(&relationship.relation))
			.map(|relationship| relationship.target.as_str())
			.collect();
		next.sort_unstable();
		next.dedup();
		for neighbor in next {
			if path.iter().any(|node| node.as_str() == neighbor) {
				continue;
			}
			let mut extended = path.clone();
			extended.push(neighbor.to_string());
			queue.push_back(extended);
		}
	}
	search
}

/// Which way `neighbors` follows relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
	/// From a relationship's source to its target: what the node uses.
	Outgoing,
	/// From a relationship's target to its source: what uses the node.
	Incoming,
}

/// One node reached by `neighbors` and the edge that reached it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
	pub node_id: String,
	/// Index into `CodeGraph::relationships` of the edge that was followed.
	pub via: usize,
	/// The node the edge was followed from, one level closer to the start.
	pub from_node_id: String,
}

/// The levels `neighbors` found and whether a cap cut the walk short.
#[derive(Debug, Default)]
pub struct Neighborhood {
	/// Element `n` holds the nodes first reached at distance `n + 1`.
	pub levels: Vec<Vec<Hop>>,
	/// The edge-examination cap or the caller's hop cap ended the walk early.
	pub capped: bool,
}

/// Breadth-first neighborhood of `start_ids`: no node appears twice and the
/// start nodes never appear. An empty `relations` follows every relationship
/// type. Stops early, flagging `capped`, after `MAX_NEIGHBOR_EXPANSIONS` edges
/// or once `max_hops` nodes were reached (the caller's room for output).
pub fn neighbors(
	graph: &CodeGraph,
	index: &GraphIndex,
	start_ids: &[String],
	relations: &[RelationType],
	direction: Direction,
	depth: usize,
	max_hops: usize,
) -> Neighborhood {
	let mut visited: HashSet<&str> = HashSet::new();
	let mut frontier: Vec<&str> = Vec::new();
	for id in start_ids {
		if visited.insert(id.as_str()) {
			frontier.push(id.as_str());
		}
	}

	let mut result = Neighborhood::default();
	let mut expansions = 0usize;
	let mut hops = 0usize;
	for _ in 0..depth {
		let mut level = Vec::new();
		let mut next_frontier: Vec<&str> = Vec::new();
		'frontier: for &current in &frontier {
			for &edge in index.edges(current, direction) {
				let relationship = &graph.relationships[edge];
				if !relations.is_empty() && !relations.contains(&relationship.relation) {
					continue;
				}
				if expansions >= MAX_NEIGHBOR_EXPANSIONS || hops >= max_hops {
					result.capped = true;
					break 'frontier;
				}
				expansions += 1;
				let reached = match direction {
					Direction::Outgoing => relationship.target.as_str(),
					Direction::Incoming => relationship.source.as_str(),
				};
				if visited.insert(reached) {
					hops += 1;
					level.push(Hop {
						node_id: reached.to_string(),
						via: edge,
						from_node_id: current.to_string(),
					});
					next_frontier.push(reached);
				}
			}
		}
		if level.is_empty() {
			break;
		}
		result.levels.push(level);
		if result.capped {
			break;
		}
		frontier = next_frontier;
	}
	result
}

#[cfg(test)]
#[path = "traverse_tests.rs"]
mod traverse_tests;
