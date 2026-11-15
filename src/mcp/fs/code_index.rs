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

//! In-memory code index of a workdir, kept for the session and never written
//! to disk. It answers "which files spell these identifiers" without reading
//! the files again, and keeps the per-file parse results the graph builder
//! consumes.
//!
//! * A file table with a stamp per file (size, mtime and on unix ctime and
//!   inode); `refresh` re-walks and reports what is new, changed or gone.
//! * A bloom filter of the identifier tokens of every file, built lazily on
//!   the first query. A hit can be a false positive; `files_spelling_verified`
//!   re-reads the hits and drops them.
//! * A parsed-data cache keyed by file stamp, evicted least recently used.
//!
//! All roots together stay under one memory ceiling (`INDEX_MEMORY_BYTES`).
//! When the token filters of a root do not fit, their size per token shrinks
//! down to `MIN_BITS_PER_TOKEN`; below that the root is unusable: its graph
//! relations answer with an error saying so, while `definition` and
//! `references` read the files directly. An unusable root is built again
//! once other roots have given memory back. An index
//! nobody used for `INDEX_IDLE_SECS` is dropped by one sweeper task.

use super::graph::{walk_supported, SupportedFiles, MAX_GRAPH_FILE_BYTES};
use crate::structural::graph::registry::{heap_chunk, FileRegistry};
use crate::structural::graph::{parse_files, FileSpec, ParsedSource, SkippedFile};
use crate::structural::{line_may_import, line_may_inherit, line_may_reexport, spelling_rules, SpellingRules};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Workdirs indexed at once; the least recently used one is dropped.
pub(super) const MAX_INDEXED_WORKDIRS: usize = 2;

/// Heap bytes the token filters, parsed caches and file tables of ALL indexed
/// roots may hold together.
pub(super) const INDEX_MEMORY_BYTES: usize = 160 * 1024 * 1024;

/// Seconds an index may stay unused before the sweeper drops it.
pub(super) const INDEX_IDLE_SECS: u64 = 600;

/// Name lookups (files spelling a name, files with an import or inheritance
/// line) kept per index generation; the cache is cleared when it is full.
const MAX_CACHED_LOOKUPS: usize = 64;

/// Finished answers kept per index generation.
pub(super) const MAX_CACHED_ANSWERS: usize = 16;

/// Filter bits per distinct token: about 0.07% false positives with
/// `HASHES_PER_BIT_PERCENT` of hashes per bit.
const BITS_PER_TOKEN: usize = 15;

/// Smallest filter (bits per token) a root may shrink to before it is not
/// indexed at all; about 5% false positives.
const MIN_BITS_PER_TOKEN: usize = 6;

/// Hash functions per filter bit per token, in percent (ln 2 = 69%).
const HASHES_PER_BIT_PERCENT: usize = 69;

/// A filter has at least this many bits, whole `u64` words.
const MIN_FILTER_BITS: usize = 64;

/// Most threads that parse files at once. Every parse holds a syntax tree, so
/// the memory peak follows this number (4 threads peaked at 156 MB, all
/// cores at 350 MB on a 2000-file batch).
const MAX_PARSE_THREADS: usize = 6;

/// Files a query always parses, whatever the time budget says, so a repeated
/// query that ran out of time gets further (four per thread of a full pool).
pub(super) const PARSE_CHUNK: usize = 24;

/// A batch that really parses at least this many files hands freed memory
/// back to the operating system afterwards (glibc keeps it otherwise).
const PARSE_TRIM_THRESHOLD: usize = 16;

/// Source text the parse threads may hold at once. A syntax tree weighs many
/// times the text it came from, so a few large C files in flight at once set
/// the memory peak; a file larger than this parses alone.
const MAX_INFLIGHT_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

static INFLIGHT_SOURCE: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

/// Holds a share of `MAX_INFLIGHT_SOURCE_BYTES` while a file is parsed.
struct InflightPermit(u64);

impl InflightPermit {
	fn acquire(bytes: u64) -> Self {
		let (held, released) = &INFLIGHT_SOURCE;
		let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
		while *held > 0 && *held + bytes > MAX_INFLIGHT_SOURCE_BYTES {
			held = released.wait(held).unwrap_or_else(PoisonError::into_inner);
		}
		*held += bytes;
		Self(bytes)
	}
}

impl Drop for InflightPermit {
	fn drop(&mut self) {
		let (held, released) = &INFLIGHT_SOURCE;
		*held.lock().unwrap_or_else(PoisonError::into_inner) -= self.0;
		released.notify_all();
	}
}

/// Capacity a per-thread read buffer keeps between files.
const KEPT_BUFFER_BYTES: usize = 1024 * 1024;

/// Capacity a per-thread hash buffer keeps between files.
const KEPT_HASHES: usize = 64 * 1024;

const BITS_PER_WORD: usize = 64;

/// How often the sweeper looks, as a fraction of the idle time.
const SWEEPS_PER_IDLE_PERIOD: u32 = 4;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const MIX_MULTIPLIER: u64 = 0xff51_afd7_ed55_8ccd;
const MIX_SHIFT: u32 = 33;
const HALF_WORD_BITS: u32 = 32;

pub(super) type FileId = u32;

/// The root's index would not fit under the memory ceiling.
#[derive(Debug)]
pub(super) struct IndexUnavailable;

impl fmt::Display for IndexUnavailable {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"the code index of this workdir does not fit in the memory limit; it is built again after a minute or when another workdir is released (`definition` and `references` still work, `path` does not make the index smaller)"
		)
	}
}

impl std::error::Error for IndexUnavailable {}

/// Heap bytes held, per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MemoryUse {
	pub(super) table: usize,
	pub(super) filters: usize,
	pub(super) parsed: usize,
	/// The path registry that import resolution looks files up in.
	pub(super) registry: usize,
}

impl MemoryUse {
	pub(super) fn total(&self) -> usize {
		self.table + self.filters + self.parsed + self.registry
	}
}

// ── Memory ceiling ─────────────────────────────────────────────────────────

/// Bytes held by every index of the process, with a hard ceiling.
struct MemoryBudget {
	limit: usize,
	used: AtomicUsize,
	peak: AtomicUsize,
	/// Every live index, so one that needs room can ask the others for it.
	peers: Mutex<Vec<Weak<CodeIndex>>>,
}

impl MemoryBudget {
	fn new(limit: usize) -> Self {
		Self {
			limit,
			used: AtomicUsize::new(0),
			peak: AtomicUsize::new(0),
			peers: Mutex::new(Vec::new()),
		}
	}

	/// Drop the whole parsed cache of the least recently used index other than
	/// `except`. `false` when no other index holds parsed data.
	fn evict_least_recent_parsed(&self, except: &CodeIndex) -> bool {
		let mut peers = lock(&self.peers);
		peers.retain(|peer| peer.strong_count() > 0);
		let candidate = peers
			.iter()
			.filter_map(Weak::upgrade)
			.filter(|peer| !std::ptr::eq(Arc::as_ptr(peer), except))
			.filter_map(|peer| {
				let state = lock(&peer.state);
				(state.use_bytes.parsed > 0)
					.then(|| state.used_at)
					.map(|used_at| (used_at, peer.clone()))
			})
			.min_by_key(|(used_at, _)| *used_at);
		drop(peers);
		let Some((_, peer)) = candidate else {
			return false;
		};
		let mut state = lock(&peer.state);
		peer.drop_parsed_cache(&mut state);
		true
	}

	/// Reserve `bytes`; `false` when that would pass the ceiling.
	fn try_add(&self, bytes: usize) -> bool {
		let mut current = self.used.load(Ordering::Relaxed);
		loop {
			let Some(next) = current.checked_add(bytes).filter(|next| *next <= self.limit) else {
				return false;
			};
			match self
				.used
				.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed)
			{
				Ok(_) => {
					self.peak.fetch_max(next, Ordering::Relaxed);
					return true;
				}
				Err(actual) => current = actual,
			}
		}
	}

	fn sub(&self, bytes: usize) {
		self.used.fetch_sub(bytes, Ordering::AcqRel);
	}

	#[cfg(test)]
	fn used(&self) -> usize {
		self.used.load(Ordering::Acquire)
	}
}

// ── Tokens and filters ─────────────────────────────────────────────────────

/// Identifier bytes: ASCII letters and digits, `_`, and every byte of a
/// non-ASCII UTF-8 sequence, so identifiers in any script are one token.
fn is_identifier_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Calls `visit(run)` for every maximal identifier run.
fn for_each_identifier_run<'a>(bytes: &'a [u8], mut visit: impl FnMut(&'a [u8])) {
	let mut index = 0;
	while index < bytes.len() {
		if !is_identifier_byte(bytes[index]) {
			index += 1;
			continue;
		}
		let start = index;
		while index < bytes.len() && is_identifier_byte(bytes[index]) {
			index += 1;
		}
		visit(&bytes[start..index]);
	}
}

/// The identifier runs of a query name: `valid?` is `valid`, `my-func` is
/// `my` and `func`. Each distinct run once.
pub(super) fn identifier_runs(name: &str) -> Vec<&str> {
	let mut runs: Vec<&str> = Vec::new();
	for_each_identifier_run(name.as_bytes(), |run| {
		// A run of identifier bytes is whole UTF-8 sequences, so it is valid text.
		if let Ok(text) = std::str::from_utf8(run) {
			if !runs.contains(&text) {
				runs.push(text);
			}
		}
	});
	runs
}

/// The hash of a run.
fn hash_run(run: &[u8]) -> u64 {
	let mut hash = FNV_OFFSET;
	for byte in run {
		hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
	}
	hash ^= hash >> MIX_SHIFT;
	hash = hash.wrapping_mul(MIX_MULTIPLIER);
	hash ^ (hash >> MIX_SHIFT)
}

/// Bloom filter over the hashes of one file's tokens.
struct TokenFilter {
	bits: Box<[u64]>,
	hashes: u8,
}

impl TokenFilter {
	/// A filter that holds nothing: an unreadable file spells nothing.
	fn empty() -> Self {
		Self {
			bits: vec![0; 1].into_boxed_slice(),
			hashes: 1,
		}
	}

	#[cfg(test)]
	fn bytes_for(distinct_tokens: usize, bits_per_token: usize) -> usize {
		Self::words_for(distinct_tokens, bits_per_token) * std::mem::size_of::<u64>()
	}

	fn words_for(distinct_tokens: usize, bits_per_token: usize) -> usize {
		let bits = (distinct_tokens * bits_per_token).max(MIN_FILTER_BITS);
		bits.div_ceil(BITS_PER_WORD)
	}

	/// `sorted_hashes` must be sorted and distinct.
	fn build(sorted_hashes: &[u64], bits_per_token: usize) -> Self {
		let words = Self::words_for(sorted_hashes.len(), bits_per_token);
		let hashes = u8::try_from((bits_per_token * HASHES_PER_BIT_PERCENT / 100).max(1)).unwrap_or(u8::MAX);
		let mut filter = Self {
			bits: vec![0; words].into_boxed_slice(),
			hashes,
		};
		for hash in sorted_hashes {
			filter.insert(*hash);
		}
		filter
	}

	fn bit_count(&self) -> u64 {
		(self.bits.len() * BITS_PER_WORD) as u64
	}

	/// Double hashing: probe `i` is `h1 + i * h2`.
	fn positions(&self, hash: u64) -> impl Iterator<Item = u64> {
		let second = hash.rotate_left(HALF_WORD_BITS) | 1;
		let bit_count = self.bit_count();
		(0..u64::from(self.hashes)).map(move |probe| hash.wrapping_add(probe.wrapping_mul(second)) % bit_count)
	}

	fn insert(&mut self, hash: u64) {
		let positions: Vec<u64> = self.positions(hash).collect();
		for position in positions {
			self.bits[(position / BITS_PER_WORD as u64) as usize] |= 1 << (position % BITS_PER_WORD as u64);
		}
	}

	fn contains(&self, hash: u64) -> bool {
		self.positions(hash).all(|position| {
			self.bits[(position / BITS_PER_WORD as u64) as usize] & (1 << (position % BITS_PER_WORD as u64)) != 0
		})
	}

	fn heap_bytes(&self) -> usize {
		heap_chunk(self.bits.len() * std::mem::size_of::<u64>())
	}
}

thread_local! {
	/// Reused by every file a thread reads and hashes: one allocation per
	/// thread instead of one per file (the allocator, not the hashing, was the
	/// cost of the filter build on musl).
	static READ_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
	static HASH_BUFFER: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Reads `path` into `buffer`; `false` when it cannot be read or is over the
/// view size cap.
fn read_capped_into(path: &Path, buffer: &mut Vec<u8>) -> bool {
	use std::io::Read;
	buffer.clear();
	let limit = super::file_ops::MAX_VIEW_FILE_BYTES;
	let read = std::fs::File::open(path).and_then(|file| file.take(limit + 1).read_to_end(buffer));
	let fits = read.is_ok() && buffer.len() as u64 <= limit;
	buffer.shrink_to(KEPT_BUFFER_BYTES);
	fits
}

/// Calls `use_hashes` with the sorted distinct hashes of every token of
/// `bytes`.
fn with_token_hashes<T>(bytes: &[u8], use_hashes: impl FnOnce(&[u64]) -> T) -> T {
	HASH_BUFFER.with_borrow_mut(|hashes| {
		hashes.clear();
		for_each_identifier_run(bytes, |run| hashes.push(hash_run(run)));
		hashes.sort_unstable();
		hashes.dedup();
		let result = use_hashes(hashes);
		hashes.shrink_to(KEPT_HASHES);
		result
	})
}

#[cfg(test)]
fn token_hashes(bytes: &[u8]) -> Vec<u64> {
	with_token_hashes(bytes, <[u64]>::to_vec)
}

// ── Estimated size of parsed data ──────────────────────────────────────────

fn text_bytes(text: &String) -> usize {
	heap_chunk(text.capacity())
}

fn optional_text_bytes(text: &Option<String>) -> usize {
	text.as_ref().map_or(0, text_bytes)
}

/// The allocation behind a vector, by capacity; its elements' own heap data
/// is counted separately.
fn vec_bytes<T>(items: &Vec<T>) -> usize {
	heap_chunk(items.capacity() * std::mem::size_of::<T>())
}

/// Heap bytes of the extracted data of one file, by capacity and with the
/// allocator's per-block overhead (a short string costs a 32-byte block).
fn parsed_bytes(source: &ParsedSource) -> usize {
	let data = source.data();
	let mut bytes = std::mem::size_of::<ParsedSource>() + text_bytes(&data.path) + text_bytes(&data.language);
	bytes += vec_bytes(&data.imports) + data.imports.iter().map(text_bytes).sum::<usize>();
	for bindings in [&data.import_bindings, &data.inline_imports] {
		bytes += vec_bytes(bindings)
			+ bindings
				.iter()
				.map(|binding| {
					text_bytes(&binding.local_name)
						+ optional_text_bytes(&binding.imported_name)
						+ text_bytes(&binding.import_path)
				})
				.sum::<usize>();
	}
	bytes += vec_bytes(&data.calls)
		+ data
			.calls
			.iter()
			.map(|(_, call)| text_bytes(&call.name) + optional_text_bytes(&call.qualifier))
			.sum::<usize>();
	bytes += vec_bytes(&data.type_relations)
		+ data
			.type_relations
			.iter()
			.map(|relation| text_bytes(&relation.target_name) + optional_text_bytes(&relation.source_name))
			.sum::<usize>();
	bytes += vec_bytes(&data.symbols)
		+ data
			.symbols
			.iter()
			.map(|symbol| text_bytes(&symbol.name) + optional_text_bytes(&symbol.owner))
			.sum::<usize>();
	bytes += optional_text_bytes(&data.default_export);
	bytes
}

// ── Files ──────────────────────────────────────────────────────────────────

/// Inode and change time make a same-size, same-mtime rewrite visible; other
/// platforms have no such fields and rely on size and mtime.
type ChangeToken = (i64, i64, u64);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
	size: u64,
	modified_nanos: Option<u128>,
	change: ChangeToken,
}

#[cfg(unix)]
fn change_token(metadata: &std::fs::Metadata) -> ChangeToken {
	use std::os::unix::fs::MetadataExt;
	(metadata.ctime(), metadata.ctime_nsec(), metadata.ino())
}

#[cfg(not(unix))]
fn change_token(_metadata: &std::fs::Metadata) -> ChangeToken {
	(0, 0, 0)
}

fn stamp_of(root: &Path, path: &str) -> Option<Stamp> {
	let metadata = std::fs::metadata(root.join(path)).ok()?;
	let modified_nanos = metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map(|since_epoch| since_epoch.as_nanos());
	Some(Stamp {
		size: metadata.len(),
		modified_nanos,
		change: change_token(&metadata),
	})
}

struct FileEntry {
	path: String,
	language: &'static str,
	stamp: Stamp,
}

struct CachedParse {
	stamp: Stamp,
	source: Arc<ParsedSource>,
	bytes: usize,
	/// The part of `bytes` that is the file's import memo.
	memo_bytes: usize,
	last_used: u64,
}

struct State {
	/// Slot per file id; `None` once the file is gone.
	files: Vec<Option<FileEntry>>,
	by_path: HashMap<String, FileId>,
	/// Parallel to `files`; `None` until the filter is built.
	filters: Vec<Option<TokenFilter>>,
	parsed: HashMap<FileId, CachedParse>,
	/// Files some query has examined, with the stamp they had then. Kept
	/// when the parsed data is evicted, so a repeated query counts only the
	/// files it has not examined yet against its cap and always gets further.
	examined: HashMap<FileId, Stamp>,
	tick: u64,
	bits_per_token: usize,
	indexed: bool,
	use_bytes: MemoryUse,
	/// Last time parsed data of this root was asked for.
	used_at: Instant,
	/// The set of paths, built when first needed and dropped when a file
	/// appears or goes.
	registry: Option<Arc<FileRegistry>>,
	/// Paths of the module-root manifests, sorted: not indexed, but import
	/// resolution reads where the modules are.
	module_roots: Vec<String>,
	/// Lookups valid until some file changes: cleared by every refresh that
	/// found a change.
	spelling: HashMap<String, Arc<SpellingSet>>,
	marked: HashMap<(String, LineKind), Marked>,
	answers: HashMap<String, Arc<String>>,
	/// Counts the refreshes that found a change.
	generation: u64,
}

impl State {
	fn new() -> Self {
		Self {
			files: Vec::new(),
			by_path: HashMap::new(),
			filters: Vec::new(),
			parsed: HashMap::new(),
			examined: HashMap::new(),
			tick: 0,
			bits_per_token: BITS_PER_TOKEN,
			indexed: true,
			used_at: Instant::now(),
			use_bytes: MemoryUse {
				table: 0,
				filters: 0,
				parsed: 0,
				registry: 0,
			},
			module_roots: Vec::new(),
			registry: None,
			spelling: HashMap::new(),
			marked: HashMap::new(),
			answers: HashMap::new(),
			generation: 0,
		}
	}

	/// Heap bytes of the file table and the lookups kept between queries, by
	/// capacity and with the allocator's per-block overhead: slots, the path of
	/// each file (kept twice, as entry and as map key), the map itself, the
	/// examined-file map and the cached lookups and answers.
	fn table_bytes(&self) -> usize {
		let slots = self.files.capacity() * std::mem::size_of::<Option<FileEntry>>()
			+ self.filters.capacity() * std::mem::size_of::<Option<TokenFilter>>();
		let paths: usize = self
			.files
			.iter()
			.flatten()
			.map(|entry| heap_chunk(entry.path.capacity()) * 2)
			.sum();
		let map = self.by_path.capacity() * (std::mem::size_of::<String>() + std::mem::size_of::<FileId>() + 1);
		let examined = self.examined.capacity() * (std::mem::size_of::<(FileId, Stamp)>() + 1);
		// Per cached lookup: the map slot, the Arc block and its key text.
		let entry = std::mem::size_of::<(String, usize)>() + 1 + heap_chunk(std::mem::size_of::<usize>() * 2);
		let ids = |files: &Vec<FileId>| heap_chunk(files.capacity() * std::mem::size_of::<FileId>());
		let spelling: usize = self
			.spelling
			.iter()
			.map(|(name, set)| {
				entry + heap_chunk(name.capacity()) + ids(&set.files) + ids(&set.declaring) + ids(&set.unread)
			})
			.sum();
		let marked: usize = self
			.marked
			.iter()
			.map(|((token, _), marked)| {
				let unscanned = marked
					.unscanned
					.as_ref()
					.map_or(0, |ids| heap_chunk(ids.capacity() * std::mem::size_of::<FileId>()));
				entry + heap_chunk(token.capacity()) + ids(&marked.files) + unscanned
			})
			.sum();
		let answers: usize = self
			.answers
			.iter()
			.map(|(key, text)| entry + heap_chunk(key.capacity()) + heap_chunk(text.capacity()))
			.sum();
		let module_roots: usize = self
			.module_roots
			.iter()
			.map(|path| std::mem::size_of::<String>() + heap_chunk(path.capacity()))
			.sum();
		slots + paths + map + examined + spelling + marked + answers + module_roots
	}
}

/// How much work an index did, for the tests that check a query reads or
/// parses little. Only test builds count.
#[derive(Default)]
pub(super) struct WorkCounters {
	#[cfg(test)]
	refreshes: AtomicUsize,
	#[cfg(test)]
	reads: AtomicUsize,
	#[cfg(test)]
	parses: AtomicUsize,
}

impl WorkCounters {
	fn count_refresh(&self) {
		#[cfg(test)]
		self.refreshes.fetch_add(1, Ordering::AcqRel);
	}

	fn count_read(&self) {
		#[cfg(test)]
		self.reads.fetch_add(1, Ordering::Relaxed);
	}

	fn count_parse(&self) {
		#[cfg(test)]
		self.parses.fetch_add(1, Ordering::Relaxed);
	}

	#[cfg(test)]
	pub(super) fn refreshes(&self) -> usize {
		self.refreshes.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	pub(super) fn reads(&self) -> usize {
		self.reads.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	pub(super) fn parses(&self) -> usize {
		self.parses.load(Ordering::Relaxed)
	}
}

/// One root's index.
pub(super) struct CodeIndex {
	root: PathBuf,
	budget: Arc<MemoryBudget>,
	state: Mutex<State>,
	/// Held across a refresh: concurrent callers wait for the running one.
	refresh_gate: tokio::sync::Mutex<()>,
	/// Refreshes begun so far: a caller that waited reuses the result of a
	/// refresh only if it began after the caller arrived.
	refresh_started: AtomicUsize,
	/// Held while the missing filters are built, so two cold callers do not
	/// both read every file.
	filter_gate: Mutex<()>,
	pub(super) work: WorkCounters,
}

impl Drop for CodeIndex {
	fn drop(&mut self) {
		let held = match self.state.get_mut() {
			Ok(state) => state.use_bytes.total(),
			Err(poisoned) => poisoned.into_inner().use_bytes.total(),
		};
		self.budget.sub(held);
		release_freed_memory();
	}
}

/// Runs `work` on the blocking pool.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
	tokio::task::spawn_blocking(work)
		.await
		.map_err(|e| anyhow!("The code index task failed: {e}"))?
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum FilterOutcome {
	Built(TokenFilter, usize),
	NoMemory,
}

impl CodeIndex {
	fn new(root: PathBuf, budget: Arc<MemoryBudget>) -> Self {
		Self {
			root,
			budget,
			state: Mutex::new(State::new()),
			refresh_gate: tokio::sync::Mutex::new(()),
			refresh_started: AtomicUsize::new(0),
			filter_gate: Mutex::new(()),
			work: WorkCounters::default(),
		}
	}

	/// Indexed files over the graph's size limit: never parsed; queries name
	/// them so the answer does not silently miss them.
	pub(super) fn oversized_paths(&self) -> Vec<String> {
		let mut paths: Vec<String> = lock(&self.state)
			.files
			.iter()
			.flatten()
			.filter(|entry| entry.stamp.size > MAX_GRAPH_FILE_BYTES)
			.map(|entry| entry.path.clone())
			.collect();
		paths.sort_unstable();
		paths
	}

	fn read_file(&self, path: &str) -> Option<Vec<u8>> {
		self.work.count_read();
		super::directory::read_capped(&self.root.join(path))
	}

	pub(super) fn path_of(&self, file: FileId) -> Option<String> {
		lock(&self.state)
			.files
			.get(file as usize)?
			.as_ref()
			.map(|entry| entry.path.clone())
	}

	pub(super) fn file_id(&self, path: &str) -> Option<FileId> {
		lock(&self.state).by_path.get(path).copied()
	}

	/// Whether the index holds a file at or under the workdir-relative
	/// `directory` (empty: the workdir itself).
	pub(super) fn has_files_under(&self, directory: &str) -> bool {
		let prefix = if directory.is_empty() {
			String::new()
		} else {
			format!("{directory}/")
		};
		lock(&self.state).by_path.keys().any(|path| path.starts_with(&prefix))
	}

	#[cfg(test)]
	pub(super) fn file_count(&self) -> usize {
		lock(&self.state).by_path.len()
	}

	/// Whether the root fits under the memory ceiling; `false` means the
	/// caller answers without the index.
	pub(super) fn is_usable(&self) -> bool {
		lock(&self.state).indexed
	}

	/// Heap bytes this root holds, by kind.
	#[cfg(test)]
	pub(super) fn memory_use(&self) -> MemoryUse {
		lock(&self.state).use_bytes
	}

	/// Re-walk and re-stat the workdir. Concurrent callers share one run.
	pub(super) async fn refresh(self: &Arc<Self>) -> Result<()> {
		let arrived_after = self.refresh_started.load(Ordering::Acquire);
		let _single_flight = self.refresh_gate.lock().await;
		// A refresh that began after this call arrived has walked the files as
		// they are now; one that was already running when it arrived may have
		// missed an edit made just before the call.
		if self.refresh_started.load(Ordering::Acquire) != arrived_after {
			return Ok(());
		}
		self.refresh_started.fetch_add(1, Ordering::AcqRel);
		let this = self.clone();
		blocking(move || this.refresh_blocking()).await?;
		self.work.count_refresh();
		Ok(())
	}

	fn refresh_blocking(&self) -> Result<()> {
		if !self.is_usable() {
			return Err(IndexUnavailable.into());
		}
		let SupportedFiles {
			files: found,
			mut module_roots,
		} = walk_supported(&self.root)?;
		module_roots.sort_unstable();
		let walked: Vec<(String, &'static str, Stamp)> = found
			.into_par_iter()
			.filter_map(|(path, language)| {
				let stamp = stamp_of(&self.root, &path)?;
				Some((path, language, stamp))
			})
			.collect();

		let mut state = lock(&self.state);
		let mut content_changed = false;
		let mut file_set_changed = false;
		let mut seen = vec![false; state.files.len()];
		for (path, language, stamp) in walked {
			match state.by_path.get(&path).copied() {
				Some(id) => {
					seen[id as usize] = true;
					let unchanged = state.files[id as usize]
						.as_ref()
						.is_some_and(|entry| entry.stamp == stamp);
					if !unchanged {
						state.files[id as usize] = Some(FileEntry { path, language, stamp });
						self.drop_derived(&mut state, id);
						content_changed = true;
					}
				}
				None => {
					let id = FileId::try_from(state.files.len()).map_err(|_| anyhow!("Too many files to index"))?;
					state.by_path.insert(path.clone(), id);
					state.files.push(Some(FileEntry { path, language, stamp }));
					state.filters.push(None);
					file_set_changed = true;
				}
			}
		}
		for (index, was_seen) in seen.iter().enumerate() {
			if *was_seen || state.files[index].is_none() {
				continue;
			}
			let id = index as FileId;
			if let Some(entry) = state.files[index].take() {
				state.by_path.remove(&entry.path);
			}
			self.drop_derived(&mut state, id);
			file_set_changed = true;
		}
		// A new or removed manifest changes how imports resolve.
		if state.module_roots != module_roots {
			state.module_roots = module_roots;
			file_set_changed = true;
		}
		if content_changed || file_set_changed {
			state.generation += 1;
			state.spelling.clear();
			state.marked.clear();
			state.answers.clear();
			if file_set_changed {
				self.drop_registry(&mut state);
			}
		}
		if !self.recharge_table(&mut state) {
			self.release_all(&mut state);
			return Err(IndexUnavailable.into());
		}
		Ok(())
	}

	fn drop_registry(&self, state: &mut State) {
		self.budget.sub(state.use_bytes.registry);
		state.use_bytes.registry = 0;
		state.registry = None;
	}

	/// Forget the filter and parsed data of a file that changed or went.
	fn drop_derived(&self, state: &mut State, id: FileId) {
		if let Some(filter) = state.filters[id as usize].take() {
			let bytes = filter.heap_bytes();
			state.use_bytes.filters -= bytes;
			self.budget.sub(bytes);
		}
		if let Some(cached) = state.parsed.remove(&id) {
			state.use_bytes.parsed -= cached.bytes;
			self.budget.sub(cached.bytes);
		}
	}

	/// Charge the table after a cache insert. Over the ceiling the new item
	/// is dropped by `drop_item`, so nothing cached goes uncounted.
	fn charge_or_drop(&self, state: &mut State, drop_item: impl FnOnce(&mut State)) {
		if !self.recharge_table(state) {
			drop_item(state);
			self.recharge_table(state);
		}
	}

	/// Charge the file table at its current size. `false`: over the ceiling.
	fn recharge_table(&self, state: &mut State) -> bool {
		let now = state.table_bytes();
		let before = state.use_bytes.table;
		if now > before {
			if !self.budget.try_add(now - before) {
				return false;
			}
		} else {
			self.budget.sub(before - now);
		}
		state.use_bytes.table = now;
		true
	}

	/// Mark the root as not indexable and give everything back.
	fn release_all(&self, state: &mut State) {
		self.budget.sub(state.use_bytes.total());
		state.use_bytes = MemoryUse {
			table: 0,
			filters: 0,
			parsed: 0,
			registry: 0,
		};
		state.files = Vec::new();
		state.by_path = HashMap::new();
		state.module_roots = Vec::new();
		state.filters = Vec::new();
		state.parsed = HashMap::new();
		state.registry = None;
		state.spelling.clear();
		state.marked.clear();
		state.answers.clear();
		state.indexed = false;
	}

	fn drop_parsed_cache(&self, state: &mut State) {
		self.budget.sub(state.use_bytes.parsed);
		state.use_bytes.parsed = 0;
		state.parsed.clear();
	}

	fn build_filter(&self, path: &str, bits_per_token: usize) -> FilterOutcome {
		self.work.count_read();
		let full_path = self.root.join(path);
		let filter = READ_BUFFER.with_borrow_mut(|buffer| {
			if read_capped_into(&full_path, buffer) {
				with_token_hashes(buffer, |hashes| TokenFilter::build(hashes, bits_per_token))
			} else {
				TokenFilter::empty()
			}
		});
		let bytes = filter.heap_bytes();
		if self.budget.try_add(bytes) {
			FilterOutcome::Built(filter, bytes)
		} else {
			FilterOutcome::NoMemory
		}
	}

	/// Build the filter of every file that has none, reading each once and in
	/// parallel. Out of memory: first the parsed cache goes, then the filters
	/// get smaller, then the root is given up.
	pub(super) async fn ensure_ready(self: &Arc<Self>) -> Result<()> {
		let this = self.clone();
		blocking(move || this.ensure_filters()).await
	}

	fn ensure_filters(&self) -> Result<()> {
		// One build at a time: two cold callers would each read every file,
		// together exceed the ceiling and shrink the filters for good. The
		// second finds nothing left to build.
		let _single_flight = self
			.filter_gate
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let built = self.build_missing_filters();
		release_freed_memory();
		built
	}

	fn build_missing_filters(&self) -> Result<()> {
		loop {
			let (pending, bits_per_token) = {
				let state = lock(&self.state);
				if !state.indexed {
					return Err(IndexUnavailable.into());
				}
				let pending: Vec<(FileId, String, Stamp)> = state
					.files
					.iter()
					.enumerate()
					.filter(|(index, entry)| entry.is_some() && state.filters[*index].is_none())
					.filter_map(|(index, entry)| {
						let entry = entry.as_ref()?;
						Some((index as FileId, entry.path.clone(), entry.stamp))
					})
					.collect();
				(pending, state.bits_per_token)
			};
			if pending.is_empty() {
				return Ok(());
			}
			let outcomes: Vec<(FileId, Stamp, FilterOutcome)> = pending
				.into_par_iter()
				.map(|(id, path, stamp)| (id, stamp, self.build_filter(&path, bits_per_token)))
				.collect();
			let mut out_of_memory = false;
			let mut state = lock(&self.state);
			for (id, stamp, outcome) in outcomes {
				match outcome {
					FilterOutcome::Built(filter, bytes) => {
						// The file may have changed or the root been released while
						// the filter was built from the old content.
						let current = state
							.files
							.get(id as usize)
							.and_then(Option::as_ref)
							.is_some_and(|entry| entry.stamp == stamp);
						let slot_free = current && state.filters.get(id as usize).is_some_and(Option::is_none);
						if slot_free {
							state.filters[id as usize] = Some(filter);
							state.use_bytes.filters += bytes;
						} else {
							self.budget.sub(bytes);
						}
					}
					FilterOutcome::NoMemory => out_of_memory = true,
				}
			}
			if !out_of_memory {
				continue;
			}
			if state.use_bytes.parsed > 0 {
				self.drop_parsed_cache(&mut state);
				continue;
			}
			// Other roots' parsed data goes before this root shrinks, least
			// recently used first. Their locks are taken one at a time and never
			// while holding this one.
			drop(state);
			if self.budget.evict_least_recent_parsed(self) {
				continue;
			}
			let mut state = lock(&self.state);
			if state.bits_per_token > MIN_BITS_PER_TOKEN {
				state.bits_per_token = (state.bits_per_token * 3 / 4).max(MIN_BITS_PER_TOKEN);
				// The filters already built used the memory the smaller ones
				// need: every file is read again at the new size.
				for filter in state.filters.iter_mut().filter_map(Option::take) {
					self.budget.sub(filter.heap_bytes());
				}
				state.use_bytes.filters = 0;
			} else {
				self.release_all(&mut state);
				return Err(IndexUnavailable.into());
			}
		}
	}

	fn filter_hits(&self, names: &[&str]) -> Result<(Vec<FileId>, Vec<String>)> {
		let runs: Vec<String> = names
			.iter()
			.flat_map(|name| identifier_runs(name))
			.map(str::to_string)
			.collect::<HashSet<_>>()
			.into_iter()
			.collect();
		if runs.is_empty() {
			return Ok((Vec::new(), runs));
		}
		self.ensure_filters()?;
		let hashes: Vec<u64> = runs.iter().map(|run| hash_run(run.as_bytes())).collect();
		let state = lock(&self.state);
		let hits = state
			.filters
			.iter()
			.enumerate()
			.filter(|(_, filter)| {
				filter
					.as_ref()
					.is_some_and(|f| hashes.iter().all(|hash| f.contains(*hash)))
			})
			.map(|(index, _)| index as FileId)
			.collect();
		Ok((hits, runs))
	}

	/// Files whose filter holds every identifier run of every name; a hit can
	/// be a false positive. Nothing is read once the filters exist.
	pub(super) async fn files_spelling(self: &Arc<Self>, names: &[&str]) -> Result<Vec<FileId>> {
		let this = self.clone();
		let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
		blocking(move || {
			let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
			Ok(this.filter_hits(&borrowed)?.0)
		})
		.await
	}

	/// `files_spelling` with the hits read again, keeping the files where each
	/// run really occurs as a whole token.
	pub(super) async fn files_spelling_verified(self: &Arc<Self>, names: &[&str]) -> Result<Vec<FileId>> {
		if let [name] = names {
			return Ok(self.spelling(name, None).await?.files.clone());
		}
		let this = self.clone();
		let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
		blocking(move || {
			let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
			let (hits, runs) = this.filter_hits(&borrowed)?;
			Ok(this.verify_hits(hits, &runs))
		})
		.await
	}

	/// Of the candidate `ids`, the files that really spell every identifier run
	/// of `names` as a whole token.
	pub(super) async fn verify_spelling(self: &Arc<Self>, ids: Vec<FileId>, names: &[&str]) -> Result<Vec<FileId>> {
		let this = self.clone();
		let runs: Vec<String> = names
			.iter()
			.flat_map(|name| identifier_runs(name))
			.map(str::to_string)
			.collect::<HashSet<_>>()
			.into_iter()
			.collect();
		blocking(move || Ok(this.verify_hits(ids, &runs))).await
	}

	fn verify_hits(&self, hits: Vec<FileId>, runs: &[String]) -> Vec<FileId> {
		if runs.is_empty() {
			return Vec::new();
		}
		let wanted: HashSet<&[u8]> = runs.iter().map(String::as_bytes).collect();
		let paths: Vec<(FileId, String)> = hits
			.into_iter()
			.filter_map(|id| Some((id, self.path_of(id)?)))
			.collect();
		let mut verified: Vec<FileId> = paths
			.into_par_iter()
			.filter(|(_, path)| {
				self.work.count_read();
				let full_path = self.root.join(path);
				READ_BUFFER.with_borrow_mut(|buffer| {
					if !read_capped_into(&full_path, buffer) {
						return false;
					}
					let mut found = 0;
					let mut seen: HashSet<&[u8]> = HashSet::new();
					for_each_identifier_run(buffer, |run| {
						if let Some(token) = wanted.get(run) {
							if seen.insert(token) {
								found += 1;
							}
						}
					});
					found == wanted.len()
				})
			})
			.map(|(id, _)| id)
			.collect();
		verified.sort_unstable();
		verified
	}

	/// Every indexed path as the registry import resolution looks them up in.
	/// Built once per set of paths and shared by every query until a file
	/// appears or goes.
	pub(super) async fn registry(self: &Arc<Self>) -> Result<Arc<FileRegistry>> {
		if let Some(registry) = lock(&self.state).registry.clone() {
			return Ok(registry);
		}
		let this = self.clone();
		blocking(move || {
			let mut state = lock(&this.state);
			if let Some(registry) = state.registry.clone() {
				return Ok(registry);
			}
			// Sorted: where two paths normalize alike, the first one wins.
			let mut paths: Vec<String> = state.by_path.keys().cloned().collect();
			paths.extend(state.module_roots.iter().cloned());
			paths.sort_unstable();
			let registry = Arc::new(FileRegistry::new(&paths));
			let bytes = registry.heap_bytes();
			// Over the ceiling the registry still serves this query, uncached.
			if this.budget.try_add(bytes) {
				state.use_bytes.registry = bytes;
				state.registry = Some(registry.clone());
			}
			Ok(registry)
		})
		.await
	}

	/// The files in `directory` and in the directories directly below it.
	pub(super) fn files_near(&self, directory: &str) -> Vec<FileId> {
		let prefix = if directory.is_empty() {
			String::new()
		} else {
			format!("{directory}/")
		};
		let allowed_slashes = prefix.matches('/').count() + 1;
		lock(&self.state)
			.by_path
			.iter()
			.filter(|(path, _)| path.starts_with(&prefix) && path.matches('/').count() <= allowed_slashes)
			.map(|(_, id)| *id)
			.collect()
	}

	/// The files directly in `directory`, not in the directories below it.
	pub(super) fn files_in_directory(&self, directory: &str) -> Vec<FileId> {
		let prefix = if directory.is_empty() {
			String::new()
		} else {
			format!("{directory}/")
		};
		lock(&self.state)
			.by_path
			.iter()
			.filter(|(path, _)| path.strip_prefix(&prefix).is_some_and(|name| !name.contains('/')))
			.map(|(_, id)| *id)
			.collect()
	}

	/// How many refreshes found a change; a finished answer is valid for one
	/// generation.
	pub(super) fn generation(&self) -> u64 {
		lock(&self.state).generation
	}

	/// A finished answer of the current generation, by query key.
	pub(super) fn cached_answer(&self, key: &str) -> Option<Arc<String>> {
		lock(&self.state).answers.get(key).cloned()
	}

	/// Keep an answer computed under `generation`; dropped when a refresh
	/// found a change meanwhile.
	pub(super) fn store_answer(&self, key: String, answer: String, generation: u64) {
		let mut state = lock(&self.state);
		if state.generation != generation {
			return;
		}
		if state.answers.len() >= MAX_CACHED_ANSWERS {
			state.answers.clear();
		}
		state.answers.insert(key.clone(), Arc::new(answer));
		self.charge_or_drop(&mut state, |state| {
			state.answers.remove(&key);
		});
	}

	/// Whether `spelling(name)` is answered from the cache.
	pub(super) fn spelling_known(&self, name: &str) -> bool {
		lock(&self.state)
			.spelling
			.get(name)
			.is_some_and(|set| set.unread.is_empty())
	}

	/// The files that really spell `name` and the ones of them that look like
	/// they declare it, both in path order. The candidate files are read in
	/// chunks until `deadline`; the ones not read yet come back in `unread`,
	/// and a repeat goes on with those. A call always reads one chunk. Cached
	/// until a file changes, so a repeated query does not read the files again.
	pub(super) async fn spelling(self: &Arc<Self>, name: &str, deadline: Option<Instant>) -> Result<Arc<SpellingSet>> {
		let (generation, resume) = {
			let state = lock(&self.state);
			let cached = state.spelling.get(name);
			if let Some(set) = cached.filter(|set| set.unread.is_empty()) {
				return Ok(set.clone());
			}
			(state.generation, cached.cloned())
		};
		let this = self.clone();
		let key = name.to_string();
		let set = Arc::new(blocking(move || this.spelling_blocking(&key, resume, deadline)).await?);
		let mut state = lock(&self.state);
		if state.generation == generation {
			if state.spelling.len() >= MAX_CACHED_LOOKUPS {
				state.spelling.clear();
			}
			state.spelling.insert(name.to_string(), set.clone());
			self.charge_or_drop(&mut state, |state| {
				state.spelling.remove(name);
			});
		}
		Ok(set)
	}

	/// Reads the candidates of `name` (all of them, or the `unread` ones of an
	/// earlier partial `resume`) chunk by chunk until all are read or `deadline`
	/// has passed (after at least one chunk).
	fn spelling_blocking(
		&self,
		name: &str,
		resume: Option<Arc<SpellingSet>>,
		deadline: Option<Instant>,
	) -> Result<SpellingSet> {
		let runs = identifier_runs(name);
		let Some(simple) = runs.last().map(|run| run.to_string()) else {
			return Ok(SpellingSet::default());
		};
		let wanted: HashSet<&[u8]> = runs.iter().map(|run| run.as_bytes()).collect();
		let (mut pending, mut found): (Vec<FileId>, Vec<(FileId, bool)>) = match resume {
			Some(set) => {
				let declaring: HashSet<FileId> = set.declaring.iter().copied().collect();
				let found = set.files.iter().map(|id| (*id, declaring.contains(id))).collect();
				(set.unread.clone(), found)
			}
			None => (self.filter_hits(&[name])?.0, Vec::new()),
		};
		let mut next = 0;
		let mut chunks = 0;
		while next < pending.len() {
			let out_of_time = deadline.is_some_and(|deadline| Instant::now() >= deadline);
			if chunks > 0 && out_of_time {
				break;
			}
			let end = (next + SCAN_CHUNK).min(pending.len());
			found.extend(self.scan_spelling_chunk(&pending[next..end], &wanted, simple.as_bytes())?);
			next = end;
			chunks += 1;
		}
		let unread = pending.split_off(next);
		found.sort_by_cached_key(|(id, _)| self.path_of(*id));
		Ok(SpellingSet {
			declaring: found
				.iter()
				.filter(|(_, declares)| *declares)
				.map(|(id, _)| *id)
				.collect(),
			files: found.into_iter().map(|(id, _)| id).collect(),
			unread,
		})
	}

	fn scan_spelling_chunk(
		&self,
		ids: &[FileId],
		wanted: &HashSet<&[u8]>,
		simple: &[u8],
	) -> Result<Vec<(FileId, bool)>> {
		let files: Vec<(FileId, String, SpellingRules)> = {
			let state = lock(&self.state);
			ids.iter()
				.filter_map(|id| {
					let entry = state.files.get(*id as usize)?.as_ref()?;
					Some((*id, entry.path.clone(), spelling_rules(entry.language)))
				})
				.collect()
		};
		let pool = parse_pool().ok_or_else(|| anyhow!("The parse thread pool did not start"))?;
		Ok(pool.install(|| {
			files
				.into_par_iter()
				.filter_map(|(id, path, rules)| {
					self.work.count_read();
					let full_path = self.root.join(&path);
					READ_BUFFER.with_borrow_mut(|buffer| {
						if !read_capped_into(&full_path, buffer) {
							return None;
						}
						scan_spelling(buffer, wanted, simple, &rules).map(|declares| (id, declares))
					})
				})
				.collect()
		}))
	}

	/// The extracted data of a file, parsed on a miss. A changed file has a
	/// new stamp, so it misses.
	#[cfg(test)]
	pub(super) async fn parsed(self: &Arc<Self>, file: FileId) -> Result<Arc<ParsedSource>> {
		let batch = self
			.parse_many(
				vec![file],
				ParseLimit {
					max_fresh: 1,
					min_fresh: 0,
					deadline: None,
				},
			)
			.await?;
		match batch.parsed.into_iter().next() {
			Some((_, source)) => Ok(source),
			None => Err(anyhow!(batch.skipped.first().map_or_else(
				|| "The file is not in the index".to_string(),
				|s| format!("{}: {}", s.path, s.reason)
			))),
		}
	}

	/// The extracted data of `ids`: cached files cost nothing, and the others
	/// are parsed on the bounded parse pool as far as `limit` allows. The rest
	/// is counted in `left_over`, never parsed.
	pub(super) async fn parse_many(self: &Arc<Self>, ids: Vec<FileId>, limit: ParseLimit) -> Result<ParseBatch> {
		let this = self.clone();
		blocking(move || this.parse_many_blocking(&ids, limit)).await
	}

	fn parse_many_blocking(&self, ids: &[FileId], limit: ParseLimit) -> Result<ParseBatch> {
		let mut batch = ParseBatch::default();
		let mut missing: Vec<(FileId, String, &'static str, Stamp)> = Vec::new();
		{
			let mut state = lock(&self.state);
			if !state.indexed {
				return Err(IndexUnavailable.into());
			}
			state.used_at = Instant::now();
			for id in ids {
				let Some(entry) = state.files.get(*id as usize).and_then(Option::as_ref) else {
					continue;
				};
				let (path, language, stamp) = (entry.path.clone(), entry.language, entry.stamp);
				// Reported by `oversized_paths`, never parsed.
				if stamp.size > MAX_GRAPH_FILE_BYTES {
					continue;
				}
				state.tick += 1;
				let tick = state.tick;
				match state.parsed.get_mut(id).filter(|cached| cached.stamp == stamp) {
					Some(cached) => {
						cached.last_used = tick;
						batch.parsed.push((*id, cached.source.clone()));
					}
					None => missing.push((*id, path, language, stamp)),
				}
			}
		}
		let total_missing = missing.len();
		let affordable = total_missing.min(limit.max_fresh);
		missing.truncate(affordable);
		missing.sort_by_key(|(_, _, _, stamp)| stamp.size);
		if missing.is_empty() {
			batch.left_over = total_missing;
			return Ok(batch);
		}

		let pool = parse_pool().ok_or_else(|| anyhow!("The parse thread pool did not start"))?;
		// Smallest files first, so one large file cannot use the budget before
		// the small ones are done. Every thread takes the next outstanding file
		// until none is left or the deadline has passed; at least `min_fresh`
		// files are taken whatever the clock says.
		let next = AtomicUsize::new(0);
		let per_thread = pool.broadcast(|_| {
			let mut done: Vec<(usize, FreshParse)> = Vec::new();
			loop {
				let position = next.fetch_add(1, Ordering::Relaxed);
				let Some((id, path, language, stamp)) = missing.get(position) else {
					break;
				};
				let out_of_time = limit.deadline.is_some_and(|deadline| Instant::now() >= deadline);
				if out_of_time && position >= limit.min_fresh {
					break;
				}
				done.push((position, (*id, *stamp, self.parse_file(path, language))));
			}
			done
		});
		let mut taken: Vec<(usize, FreshParse)> = per_thread.into_iter().flatten().collect();
		taken.sort_unstable_by_key(|(position, _)| *position);
		let parsed: Vec<FreshParse> = taken.into_iter().map(|(_, fresh)| fresh).collect();
		batch.left_over = total_missing - parsed.len();

		let mut state = lock(&self.state);
		for (id, stamp, outcome) in parsed {
			match outcome {
				Ok(source) => {
					batch.fresh += 1;
					self.store_parsed(&mut state, id, stamp, &source);
					batch.parsed.push((id, source));
				}
				Err(skipped) => batch.skipped.push(skipped),
			}
		}
		drop(state);
		if batch.fresh >= PARSE_TRIM_THRESHOLD {
			release_freed_memory();
		}
		Ok(batch)
	}

	fn parse_file(&self, path: &str, language: &'static str) -> Result<Arc<ParsedSource>, SkippedFile> {
		self.work.count_parse();
		// Bytes become the text without a second copy: both alive at once is
		// twice the file in memory, per parsing thread.
		let read = |relative: &str| -> Result<String> {
			let bytes = self
				.read_file(relative)
				.ok_or_else(|| anyhow!("{relative} cannot be read"))?;
			Ok(String::from_utf8(bytes).unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
		};
		let size = std::fs::metadata(self.root.join(path)).map_or(0, |metadata| metadata.len());
		let _permit = InflightPermit::acquire(size);
		let (mut sources, mut skipped) = parse_files(
			vec![FileSpec {
				path: path.to_string(),
				language,
			}],
			&read,
		);
		match sources.pop() {
			Some(source) => Ok(Arc::new(source)),
			None => Err(skipped.pop().unwrap_or_else(|| SkippedFile {
				path: path.to_string(),
				reason: "failed to parse".to_string(),
			})),
		}
	}

	/// Keep `source` in the cache when the file did not change meanwhile,
	/// evicting the least recently used entries to make room.
	fn store_parsed(&self, state: &mut State, file: FileId, stamp: Stamp, source: &Arc<ParsedSource>) {
		let bytes = parsed_bytes(source);
		if !state.indexed || state.files.get(file as usize).and_then(Option::as_ref).map(|e| e.stamp) != Some(stamp) {
			return;
		}
		if let Some(previous) = state.parsed.remove(&file) {
			state.use_bytes.parsed -= previous.bytes;
			self.budget.sub(previous.bytes);
		}
		if !self.make_room(state, bytes) {
			// Nothing left to evict: the data is used but not kept.
			return;
		}
		state.tick += 1;
		state.used_at = Instant::now();
		let last_used = state.tick;
		state.use_bytes.parsed += bytes;
		state.parsed.insert(
			file,
			CachedParse {
				stamp,
				source: source.clone(),
				bytes,
				memo_bytes: 0,
				last_used,
			},
		);
	}

	/// Split `ids` into the files no query has examined at their current
	/// stamp and the ones already examined (whose data may have been evicted).
	pub(super) fn split_examined(&self, ids: &[FileId]) -> (Vec<FileId>, Vec<FileId>) {
		let state = lock(&self.state);
		let current = |id: FileId| {
			state
				.files
				.get(id as usize)
				.and_then(Option::as_ref)
				.map(|entry| entry.stamp)
		};
		ids.iter()
			.copied()
			.partition(|id| current(*id).is_none() || state.examined.get(id).copied() != current(*id))
	}

	pub(super) fn mark_examined(&self, ids: impl IntoIterator<Item = FileId>) {
		let mut state = lock(&self.state);
		for id in ids {
			if let Some(stamp) = state
				.files
				.get(id as usize)
				.and_then(Option::as_ref)
				.map(|entry| entry.stamp)
			{
				state.examined.insert(id, stamp);
			}
		}
		self.charge_or_drop(&mut state, |state| state.examined.clear());
	}

	/// Reserve `bytes` of the budget, evicting the least recently used parsed
	/// files until they fit. False when nothing is left to evict.
	fn make_room(&self, state: &mut State, bytes: usize) -> bool {
		while !self.budget.try_add(bytes) {
			let Some(oldest) = state
				.parsed
				.iter()
				.min_by_key(|(_, cached)| cached.last_used)
				.map(|(id, _)| *id)
			else {
				return false;
			};
			if let Some(evicted) = state.parsed.remove(&oldest) {
				state.use_bytes.parsed -= evicted.bytes;
				self.budget.sub(evicted.bytes);
			}
		}
		true
	}

	/// Charge the memory the files' import memos grew by since they were
	/// stored (a memo fills as queries resolve imports), so everything kept
	/// between calls stays inside the budget.
	pub(super) fn account_memos(&self, ids: impl IntoIterator<Item = FileId>) {
		let mut state = lock(&self.state);
		for id in ids {
			let Some(cached) = state.parsed.get(&id) else {
				continue;
			};
			let memo = cached.source.data().memo.bytes();
			let known = cached.memo_bytes;
			if memo > known {
				if !self.make_room(&mut state, memo - known) {
					continue;
				}
				match state.parsed.get_mut(&id) {
					Some(cached) => {
						cached.bytes += memo - known;
						cached.memo_bytes = memo;
						state.use_bytes.parsed += memo - known;
					}
					// Evicted to make room for itself.
					None => self.budget.sub(memo - known),
				}
			} else if memo < known {
				if let Some(cached) = state.parsed.get_mut(&id) {
					cached.bytes -= known - memo;
					cached.memo_bytes = memo;
					state.use_bytes.parsed -= known - memo;
					self.budget.sub(known - memo);
				}
			}
		}
	}

	/// The files with a line that holds the whole identifier `token` and may
	/// be an import (or parent-type line) in the file's language, in path
	/// order. The candidate files are read in chunks until `deadline`; the
	/// ones not read yet come back in `unscanned`, and a repeat goes on with
	/// those. A call always reads one chunk, so repeating gets further.
	/// Cached until a file changes.
	pub(super) async fn marked_lines(
		self: &Arc<Self>,
		token: &str,
		kind: LineKind,
		deadline: Option<Instant>,
	) -> Result<MarkedScan> {
		let key = (token.to_string(), kind);
		let name = token.to_string();
		self.cached_marked(key, move |this, resume| {
			this.marked_lines_blocking(&name, kind, resume, deadline)
		})
		.await
	}

	/// The files among the ones that spell `name` with a line that holds the
	/// identifier `token` and may be an import (or parent-type line): only
	/// those files are read, so it is cheap even if `token` is spelled
	/// everywhere. Partial and cached like `marked_lines`.
	pub(super) async fn marked_among(
		self: &Arc<Self>,
		name: &str,
		spelling: &[FileId],
		token: &str,
		kind: LineKind,
		deadline: Option<Instant>,
	) -> Result<MarkedScan> {
		// A key no plain token can have: identifiers hold no `>`.
		let key = (format!("{name}>{token}"), kind);
		let candidates = spelling.to_vec();
		let token = token.to_string();
		self.cached_marked(key, move |this, resume| {
			let start = resume.unwrap_or(ScanProgress {
				found: Vec::new(),
				pending: candidates,
			});
			this.marked_chunks(start, &token, kind, deadline)
		})
		.await
	}

	/// Runs `scan` on the files found and the candidates left by an earlier
	/// partial call (or `None` on a first call) and caches what it returns.
	/// Resuming works on the stored candidate ids themselves, not on a position
	/// in a list that a refresh in between may have changed.
	async fn cached_marked(
		self: &Arc<Self>,
		key: (String, LineKind),
		scan: impl FnOnce(&Self, Option<ScanProgress>) -> Result<ScanProgress> + Send + 'static,
	) -> Result<MarkedScan> {
		let (generation, resume) = {
			let state = lock(&self.state);
			let resume = match state.marked.get(&key) {
				Some(Marked { files, unscanned: None }) => {
					return Ok(MarkedScan {
						files: files.clone(),
						unscanned: Vec::new(),
					})
				}
				Some(Marked {
					files,
					unscanned: Some(left),
				}) => Some(ScanProgress {
					found: files.to_vec(),
					pending: left.to_vec(),
				}),
				None => None,
			};
			(state.generation, resume)
		};
		let this = self.clone();
		let progress = blocking(move || scan(this.as_ref(), resume)).await?;
		let (files, unscanned) = (progress.found, progress.pending);
		let files = Arc::new(files);
		let mut state = lock(&self.state);
		if state.generation == generation {
			if state.marked.len() >= MAX_CACHED_LOOKUPS {
				state.marked.clear();
			}
			state.marked.insert(
				key.clone(),
				Marked {
					files: files.clone(),
					unscanned: (!unscanned.is_empty()).then(|| Arc::new(unscanned.clone())),
				},
			);
			self.charge_or_drop(&mut state, |state| {
				state.marked.remove(&key);
			});
		}
		Ok(MarkedScan { files, unscanned })
	}

	/// The candidates of `token` are the files whose filter holds it, or the
	/// ones an earlier partial call left. Returns the files found so far in
	/// path order and the candidates left.
	fn marked_lines_blocking(
		&self,
		token: &str,
		kind: LineKind,
		resume: Option<ScanProgress>,
		deadline: Option<Instant>,
	) -> Result<ScanProgress> {
		let start = match resume {
			Some(progress) => progress,
			None => ScanProgress {
				found: Vec::new(),
				pending: self.filter_hits(&[token])?.0,
			},
		};
		self.marked_chunks(start, token, kind, deadline)
	}

	/// Reads the pending candidates chunk by chunk until all are read or
	/// `deadline` has passed (after at least one chunk), adding the matches to
	/// the files found.
	fn marked_chunks(
		&self,
		start: ScanProgress,
		token: &str,
		kind: LineKind,
		deadline: Option<Instant>,
	) -> Result<ScanProgress> {
		let ScanProgress { mut found, mut pending } = start;
		let mut next = 0;
		let mut chunks = 0;
		while next < pending.len() {
			let out_of_time = deadline.is_some_and(|deadline| Instant::now() >= deadline);
			if chunks > 0 && out_of_time {
				break;
			}
			let end = (next + SCAN_CHUNK).min(pending.len());
			found.extend(self.marked_in(&pending[next..end], token, kind)?);
			next = end;
			chunks += 1;
		}
		let unscanned = pending.split_off(next);
		found.sort_by_cached_key(|id| self.path_of(*id));
		found.dedup();
		Ok(ScanProgress {
			found,
			pending: unscanned,
		})
	}

	fn marked_in(&self, hits: &[FileId], token: &str, kind: LineKind) -> Result<Vec<FileId>> {
		let files: Vec<(FileId, String, &'static str)> = {
			let state = lock(&self.state);
			hits.iter()
				.filter_map(|id| {
					let entry = state.files.get(*id as usize)?.as_ref()?;
					Some((*id, entry.path.clone(), entry.language))
				})
				.collect()
		};
		let pool = parse_pool().ok_or_else(|| anyhow!("The parse thread pool did not start"))?;
		let mut kept: Vec<(FileId, &String)> = pool.install(|| {
			files
				.par_iter()
				.filter(|(_, path, language)| {
					self.work.count_read();
					let full_path = self.root.join(path);
					READ_BUFFER.with_borrow_mut(|buffer| {
						read_capped_into(&full_path, buffer) && has_marked_line(buffer, token, language, path, kind)
					})
				})
				.map(|(id, path, _)| (*id, path))
				.collect()
		});
		kept.sort_unstable_by(|a, b| a.1.cmp(b.1));
		Ok(kept.into_iter().map(|(id, _)| id).collect())
	}
}

/// Candidate files read between two looks at the clock.
const SCAN_CHUNK: usize = 400;

/// A cached line lookup: complete (`unscanned` is `None`) or the files found
/// so far with the candidate files not read yet.
pub(super) struct Marked {
	files: Arc<Vec<FileId>>,
	unscanned: Option<Arc<Vec<FileId>>>,
}

/// Where a line scan stands: the files found so far and the candidates not
/// read yet.
struct ScanProgress {
	found: Vec<FileId>,
	pending: Vec<FileId>,
}

/// What `marked_lines` found and the candidate files it did not read.
pub(super) struct MarkedScan {
	pub(super) files: Arc<Vec<FileId>>,
	pub(super) unscanned: Vec<FileId>,
}

/// Which line hint `marked_lines` checks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum LineKind {
	Import,
	Inheritance,
	/// A line that may re-export what a module declares.
	ReExport,
}

/// A file parsed by this call, with the stamp it had when it was read.
type FreshParse = (FileId, Stamp, Result<Arc<ParsedSource>, SkippedFile>);

/// How far `parse_many` may go.
#[derive(Clone, Copy)]
pub(super) struct ParseLimit {
	/// Files that may be parsed fresh.
	pub(super) max_fresh: usize,
	/// Files parsed even when the deadline has passed, so a query always
	/// makes progress and repeating it gets further.
	pub(super) min_fresh: usize,
	/// No fresh chunk starts after it.
	pub(super) deadline: Option<Instant>,
}

/// What `parse_many` returns.
#[derive(Default)]
pub(super) struct ParseBatch {
	/// Every requested file that is parsed, from the cache or fresh.
	pub(super) parsed: Vec<(FileId, Arc<ParsedSource>)>,
	/// Files really parsed by this call; cache hits are not counted.
	pub(super) fresh: usize,
	/// Files that could not be read or parsed.
	pub(super) skipped: Vec<SkippedFile>,
	/// Files not parsed because of the `max_fresh` limit.
	pub(super) left_over: usize,
}

/// Whether some line holds the whole identifier `token` and may be an import,
/// a re-export or a parent-type line of the file's language.
fn has_marked_line(bytes: &[u8], token: &str, language: &str, path: &str, kind: LineKind) -> bool {
	bytes.split(|byte| *byte == b'\n').any(|line| {
		contains_identifier(line, token.as_bytes()) && {
			let text = String::from_utf8_lossy(line);
			match kind {
				LineKind::Import => line_may_import(language, &text, token),
				LineKind::Inheritance => line_may_inherit(language, &text, token),
				LineKind::ReExport => line_may_reexport(language, &text, token, path),
			}
		}
	})
}

/// Bytes before an occurrence the declaration check reads: a minified file
/// is one line of megabytes, and scanning back to the line start for every
/// occurrence would be quadratic.
const DECLARATION_LOOKBACK_BYTES: usize = 200;

/// Whether the occurrence of the name at `start..end` looks like the
/// declaration of it. A guess that only orders the files to parse: a wrong
/// answer costs a repeat of the query, never a missed result.
fn looks_like_declaration(bytes: &[u8], start: usize, end: usize, rules: &SpellingRules) -> bool {
	let window_start = start.saturating_sub(DECLARATION_LOOKBACK_BYTES);
	// Whether `prefix` is the whole text before the name on its line.
	let (line_start, whole_line) = match bytes[window_start..start].iter().rposition(|byte| *byte == b'\n') {
		Some(index) => (window_start + index + 1, true),
		None => (window_start, window_start == 0),
	};
	let prefix = &bytes[line_start..start];
	let before_name = prefix.trim_ascii_end();
	let before_name = before_name.strip_suffix(b"*").unwrap_or(before_name).trim_ascii_end();
	let previous_word = before_name
		.rsplit(|byte| !is_identifier_byte(*byte))
		.next()
		.and_then(|word| std::str::from_utf8(word).ok())
		.unwrap_or("");
	if rules.declaration_keywords.contains(&previous_word) || rules.callable_keywords.contains(&previous_word) {
		return true;
	}
	let opens_parameters = bytes[end..].iter().find(|byte| !matches!(byte, b' ' | b'\t')) == Some(&b'(');
	if !opens_parameters {
		return false;
	}
	let starts_flush = whole_line
		&& prefix
			.first()
			.is_some_and(|byte| is_identifier_byte(*byte) || *byte == b'*');
	starts_flush
		|| prefix
			.split(|byte| !is_identifier_byte(*byte))
			.filter_map(|word| std::str::from_utf8(word).ok())
			.any(|word| rules.callable_keywords.contains(&word))
}

/// Reads one file for `spelling`: `None` when it does not spell every run in
/// `wanted` as a whole token, else whether one occurrence of `simple` looks
/// like a declaration.
fn scan_spelling(bytes: &[u8], wanted: &HashSet<&[u8]>, simple: &[u8], rules: &SpellingRules) -> Option<bool> {
	let mut seen: HashSet<&[u8]> = HashSet::new();
	let mut declares = false;
	for_each_identifier_run(bytes, |run| {
		if let Some(token) = wanted.get(run) {
			seen.insert(token);
		}
		if !declares && run == simple {
			let start = run.as_ptr() as usize - bytes.as_ptr() as usize;
			declares = looks_like_declaration(bytes, start, start + run.len(), rules);
		}
	});
	(seen.len() == wanted.len()).then_some(declares)
}

/// The files that spell a name, by path.
#[derive(Default)]
pub(super) struct SpellingSet {
	/// Every file where each identifier of the name occurs as a whole token.
	pub(super) files: Vec<FileId>,
	/// The files among them with a line that looks like a declaration.
	pub(super) declaring: Vec<FileId>,
	/// Candidate files not read yet when the clock ran out; a repeat reads them.
	pub(super) unread: Vec<FileId>,
}

fn contains_identifier(line: &[u8], token: &[u8]) -> bool {
	if token.is_empty() || line.len() < token.len() {
		return false;
	}
	line.windows(token.len()).enumerate().any(|(start, window)| {
		window == token
			&& (start == 0 || !is_identifier_byte(line[start - 1]))
			&& line
				.get(start + token.len())
				.is_none_or(|byte| !is_identifier_byte(*byte))
	})
}

/// The pool every parse and line scan runs on, so the memory peak follows
/// `MAX_PARSE_THREADS`, never the number of queries or blocking tasks.
fn parse_pool() -> Option<&'static rayon::ThreadPool> {
	static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
	POOL.get_or_init(|| {
		let cores = std::thread::available_parallelism().map_or(1, usize::from);
		rayon::ThreadPoolBuilder::new()
			.num_threads(cores.min(MAX_PARSE_THREADS))
			.thread_name(|index| format!("octofs-parse-{index}"))
			.build()
			.ok()
	})
	.as_ref()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
extern "C" {
	fn malloc_trim(pad: usize) -> i32;
}

/// glibc keeps freed memory in its arenas; give it back.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_freed_memory() {
	// SAFETY: `malloc_trim` takes no pointers and is safe to call at any time.
	unsafe {
		malloc_trim(0);
	}
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_freed_memory() {}

// ── Registry of roots ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub(super) struct IndexLimits {
	pub(super) max_roots: usize,
	pub(super) memory_bytes: usize,
	pub(super) idle: Duration,
}

impl IndexLimits {
	pub(super) fn shipped() -> Self {
		Self {
			max_roots: MAX_INDEXED_WORKDIRS,
			memory_bytes: INDEX_MEMORY_BYTES,
			idle: Duration::from_secs(INDEX_IDLE_SECS),
		}
	}
}

struct Slot {
	index: Arc<CodeIndex>,
	last_used: Instant,
	/// Memory in use when the root was first seen unusable (its filters did
	/// not fit): it can fit only once something else gave memory back.
	used_when_unfit: Option<usize>,
}

#[derive(Default)]
struct Registry {
	roots: HashMap<PathBuf, Slot>,
	sweeper_running: bool,
}

/// The indexed roots of the process and the memory they share.
pub(super) struct CodeIndexes {
	limits: IndexLimits,
	budget: Arc<MemoryBudget>,
	registry: Arc<Mutex<Registry>>,
}

impl CodeIndexes {
	pub(super) fn new(limits: IndexLimits) -> Self {
		Self {
			limits,
			budget: Arc::new(MemoryBudget::new(limits.memory_bytes)),
			registry: Arc::new(Mutex::new(Registry::default())),
		}
	}

	pub(super) fn global() -> &'static CodeIndexes {
		static INDEXES: OnceLock<CodeIndexes> = OnceLock::new();
		INDEXES.get_or_init(|| CodeIndexes::new(IndexLimits::shipped()))
	}

	/// Heap bytes held by every root.
	#[cfg(test)]
	pub(super) fn memory_used(&self) -> usize {
		self.budget.used()
	}

	/// Most bytes ever held at once.
	#[cfg(test)]
	pub(super) fn memory_peak(&self) -> usize {
		self.budget.peak.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	pub(super) fn root_count(&self) -> usize {
		lock(&self.registry).roots.len()
	}

	/// The index of `root`, created on first use. Must run inside a tokio
	/// runtime: the first index starts the idle sweeper.
	pub(super) fn index(&self, root: &Path) -> Arc<CodeIndex> {
		let mut registry = lock(&self.registry);
		let now = Instant::now();
		if let Some(slot) = registry.roots.get_mut(root) {
			if slot.index.is_usable() {
				slot.last_used = now;
				return slot.index.clone();
			}
			// A root that did not fit is not kept alive by use: it is built
			// again once memory was freed, and until then the idle sweeper
			// can drop it.
			let used = self.budget.used.load(Ordering::Acquire);
			let used_when_unfit = *slot.used_when_unfit.get_or_insert(used);
			if used >= used_when_unfit {
				return slot.index.clone();
			}
			let fresh = Arc::new(CodeIndex::new(root.to_path_buf(), self.budget.clone()));
			lock(&self.budget.peers).push(Arc::downgrade(&fresh));
			let replaced = std::mem::replace(&mut slot.index, fresh.clone());
			slot.last_used = now;
			slot.used_when_unfit = None;
			drop(registry);
			// Dropping an index trims the allocator: not under the registry lock.
			drop(replaced);
			return fresh;
		}
		let index = Arc::new(CodeIndex::new(root.to_path_buf(), self.budget.clone()));
		lock(&self.budget.peers).push(Arc::downgrade(&index));
		registry.roots.insert(
			root.to_path_buf(),
			Slot {
				index: index.clone(),
				last_used: now,
				used_when_unfit: None,
			},
		);
		let mut evicted: Vec<Slot> = Vec::new();
		while registry.roots.len() > self.limits.max_roots {
			let oldest = registry
				.roots
				.iter()
				.filter(|(key, _)| key.as_path() != root)
				.min_by_key(|(_, slot)| slot.last_used)
				.map(|(key, _)| key.clone());
			match oldest.and_then(|key| registry.roots.remove(&key)) {
				Some(slot) => evicted.push(slot),
				None => break,
			}
		}
		if !registry.sweeper_running {
			registry.sweeper_running = true;
			self.spawn_sweeper();
		}
		drop(registry);
		drop(evicted);
		index
	}

	/// One task for all roots: it wakes a few times per idle period, drops
	/// the idle roots and ends when none is left.
	fn spawn_sweeper(&self) {
		let registry = self.registry.clone();
		let budget = self.budget.clone();
		let idle = self.limits.idle;
		let period = (idle / SWEEPS_PER_IDLE_PERIOD).max(Duration::from_millis(1));
		tokio::spawn(async move {
			loop {
				tokio::time::sleep(period).await;
				if Self::drop_idle_roots(&registry, &budget, idle) {
					return;
				}
			}
		});
	}

	/// Drops the roots unused for `idle` or longer. True when none is left,
	/// which also stops the sweeper.
	fn drop_idle_roots(registry: &Mutex<Registry>, budget: &MemoryBudget, idle: Duration) -> bool {
		let (dropped, none_left) = {
			let mut registry = lock(registry);
			let idle_keys: Vec<PathBuf> = registry
				.roots
				.iter()
				.filter(|(_, slot)| slot.last_used.elapsed() >= idle)
				.map(|(key, _)| key.clone())
				.collect();
			let dropped: Vec<Slot> = idle_keys.iter().filter_map(|key| registry.roots.remove(key)).collect();
			let none_left = registry.roots.is_empty();
			if none_left {
				registry.sweeper_running = false;
			}
			(dropped, none_left)
		};
		// Dropping an index trims the allocator: not under the lock.
		drop(dropped);
		lock(&budget.peers).retain(|peer| peer.strong_count() > 0);
		none_left
	}
}

#[cfg(test)]
#[path = "code_index_tests.rs"]
mod code_index_tests;
