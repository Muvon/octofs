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

use super::*;

const AMPLE: usize = 64 * 1024 * 1024;

fn limits(memory_bytes: usize) -> IndexLimits {
	IndexLimits {
		max_roots: MAX_INDEXED_WORKDIRS,
		memory_bytes,
		idle: Duration::from_secs(INDEX_IDLE_SECS),
	}
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
	let root = tempfile::tempdir().unwrap();
	for (path, contents) in files {
		let full = root.path().join(path);
		std::fs::create_dir_all(full.parent().unwrap()).unwrap();
		std::fs::write(full, contents).unwrap();
	}
	root
}

/// `count` Rust files, each with `tokens` distinct identifiers of its own.
fn numbered_project(count: usize, tokens: usize) -> tempfile::TempDir {
	let root = tempfile::tempdir().unwrap();
	for file in 0..count {
		let body: String = (0..tokens).map(|n| format!("fn f{file}_{n}() {{}}\n")).collect();
		std::fs::write(root.path().join(format!("m{file}.rs")), body).unwrap();
	}
	root
}

fn bump_mtime(path: &Path) {
	let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
	file.set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
		.unwrap();
}

fn reads(index: &CodeIndex) -> usize {
	index.reads.load(Ordering::Relaxed)
}

#[test]
fn identifier_runs_split_names_at_non_identifier_bytes() {
	assert_eq!(identifier_runs("valid?"), vec!["valid"]);
	assert_eq!(identifier_runs("save!"), vec!["save"]);
	assert_eq!(identifier_runs("my-func"), vec!["my", "func"]);
	assert_eq!(identifier_runs("Owner::name"), vec!["Owner", "name"]);
	assert_eq!(identifier_runs("a_b9"), vec!["a_b9"]);
	assert_eq!(identifier_runs("twice twice"), vec!["twice"]);
	assert!(identifier_runs("?!-").is_empty());
}

#[test]
fn non_ascii_identifiers_are_single_tokens() {
	assert_eq!(identifier_runs("größe_x"), vec!["größe_x"]);
	assert_eq!(identifier_runs("名前 変数"), vec!["名前", "変数"]);
	let hashes = token_hashes("let größe = 1;".as_bytes());
	assert!(hashes.contains(&hash_run("größe".as_bytes(), false)));
	assert!(!hashes.contains(&hash_run("gr".as_bytes(), false)));
}

#[test]
fn a_dollar_prefixed_token_is_also_stored_with_its_dollar() {
	let hashes = token_hashes(b"echo $name; name2");
	assert!(hashes.contains(&hash_run(b"name", false)));
	assert!(hashes.contains(&hash_run(b"name", true)));
	assert!(!hashes.contains(&hash_run(b"name2", true)));
}

#[test]
fn the_filter_has_no_false_negatives_and_few_false_positives() {
	let present: Vec<u64> = {
		let mut hashes: Vec<u64> = (0..5000)
			.map(|n| hash_run(format!("token{n}").as_bytes(), false))
			.collect();
		hashes.sort_unstable();
		hashes.dedup();
		hashes
	};
	let filter = TokenFilter::build(&present, BITS_PER_TOKEN);
	assert!(present.iter().all(|hash| filter.contains(*hash)));
	let false_positives = (0..20_000)
		.filter(|n| filter.contains(hash_run(format!("absent{n}").as_bytes(), false)))
		.count();
	assert!(false_positives < 100, "{false_positives} of 20000 absent tokens hit");
}

#[test]
fn a_smaller_filter_still_has_no_false_negatives() {
	let mut hashes: Vec<u64> = (0..2000).map(|n| hash_run(format!("t{n}").as_bytes(), false)).collect();
	hashes.sort_unstable();
	let filter = TokenFilter::build(&hashes, MIN_BITS_PER_TOKEN);
	assert!(hashes.iter().all(|hash| filter.contains(*hash)));
	assert!(filter.heap_bytes() < TokenFilter::bytes_for(2000, BITS_PER_TOKEN));
}

#[tokio::test]
async fn files_spelling_requires_every_run_of_the_name() {
	let root = project(&[
		("a.rs", "fn my() {}\nfn func() {}\n"),
		("b.rs", "fn my() {}\n"),
		("c.rs", "fn other() {}\n"),
	]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	let hits = index.files_spelling(&["my-func"]).await.unwrap();
	assert_eq!(hits, vec![index.file_id("a.rs").unwrap()]);
	let both = index.files_spelling(&["my"]).await.unwrap();
	assert_eq!(both.len(), 2);
}

#[tokio::test]
async fn verification_drops_a_planted_false_positive() {
	let root = project(&[("real.rs", "fn needle() {}\n"), ("other.rs", "fn hay() {}\n")]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	let real = index.file_id("real.rs").unwrap();
	let other = index.file_id("other.rs").unwrap();
	assert_eq!(index.files_spelling(&["needle"]).await.unwrap(), vec![real]);

	// A filter that claims every token: the file does not spell the name.
	lock(&index.state).filters[other as usize] = Some(TokenFilter {
		bits: vec![u64::MAX; 4].into_boxed_slice(),
		hashes: 3,
	});
	let hits = index.files_spelling(&["needle"]).await.unwrap();
	let mut both = vec![real, other];
	both.sort_unstable();
	assert_eq!(hits, both);
	assert_eq!(index.files_spelling_verified(&["needle"]).await.unwrap(), vec![real]);
}

#[tokio::test]
async fn a_longer_identifier_is_not_a_verified_hit() {
	let root = project(&[("a.rs", "fn needle_in_haystack() {}\n"), ("b.rs", "fn needle() {}\n")]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	let verified = index.files_spelling_verified(&["needle"]).await.unwrap();
	assert_eq!(verified, vec![index.file_id("b.rs").unwrap()]);
}

#[tokio::test]
async fn refresh_reports_new_changed_and_gone_files_and_rereads_only_the_changed_one() {
	let root = project(&[
		("a.rs", "fn aaa() {}\n"),
		("b.rs", "fn bbb() {}\n"),
		("c.rs", "fn ccc() {}\n"),
	]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	let first = index.refresh().await.unwrap();
	assert_eq!(first.new.len(), 3);
	assert!(first.changed.is_empty() && first.gone.is_empty());
	assert!(index.refresh().await.unwrap().is_empty());

	index.files_spelling(&["aaa"]).await.unwrap();
	assert_eq!(reads(&index), 3, "the filters read every file once");
	index.files_spelling(&["bbb"]).await.unwrap();
	assert_eq!(reads(&index), 3, "a second query reads nothing");

	// Same length, so only the stamp can tell.
	std::fs::write(root.path().join("a.rs"), "fn zzz() {}\n").unwrap();
	bump_mtime(&root.path().join("a.rs"));
	std::fs::remove_file(root.path().join("c.rs")).unwrap();
	std::fs::write(root.path().join("d.rs"), "fn ddd() {}\n").unwrap();
	let a = index.file_id("a.rs").unwrap();
	let c = index.file_id("c.rs").unwrap();
	let changes = index.refresh().await.unwrap();
	assert_eq!(changes.changed, vec![a]);
	assert_eq!(changes.gone, vec![c]);
	assert_eq!(changes.new, vec![index.file_id("d.rs").unwrap()]);
	assert!(index.file_id("c.rs").is_none());

	let before = reads(&index);
	let hits = index.files_spelling(&["zzz"]).await.unwrap();
	assert_eq!(hits, vec![a]);
	assert_eq!(reads(&index) - before, 2, "only the changed and the new file are read");
}

#[tokio::test]
async fn the_parsed_cache_hits_on_unchanged_files_and_misses_on_changed_ones() {
	let root = project(&[("a.rs", "fn aaa() {}\n"), ("b.rs", "fn bbb() {}\n")]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	let a = index.file_id("a.rs").unwrap();
	let first = index.parsed(a).await.unwrap();
	let reads_after_parse = reads(&index);
	let second = index.parsed(a).await.unwrap();
	assert!(Arc::ptr_eq(&first, &second));
	assert_eq!(reads(&index), reads_after_parse, "a hit reads nothing");
	assert!(index.memory_use().parsed > 0);

	std::fs::write(root.path().join("a.rs"), "fn changed() {}\n").unwrap();
	index.refresh().await.unwrap();
	let third = index.parsed(a).await.unwrap();
	assert!(!Arc::ptr_eq(&first, &third));
	assert!(third.data().symbols.iter().any(|symbol| symbol.name == "changed"));
}

#[tokio::test]
async fn the_parsed_cache_evicts_to_stay_under_the_ceiling() {
	let root = numbered_project(20, 5);
	let ample = CodeIndexes::new(limits(AMPLE));
	let probe = ample.index(root.path());
	probe.refresh().await.unwrap();
	probe.files_spelling(&["f0_0"]).await.unwrap();
	for id in 0..20 {
		probe.parsed(id).await.unwrap();
	}
	let full = probe.memory_use();
	assert!(full.parsed > 0);

	let ceiling = full.table + full.filters + full.parsed / 2;
	let indexes = CodeIndexes::new(limits(ceiling));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	index.files_spelling(&["f0_0"]).await.unwrap();
	for id in 0..20 {
		index.parsed(id).await.unwrap();
		assert!(indexes.memory_used() <= ceiling);
	}
	assert!(indexes.memory_peak() <= ceiling);
	let kept = index.memory_use();
	assert!(kept.parsed > 0 && kept.parsed < full.parsed);
	assert_eq!(indexes.memory_used(), kept.total());
}

#[tokio::test]
async fn filters_shrink_to_fit_and_the_ceiling_is_never_passed() {
	let root = numbered_project(50, 200);
	let probe = CodeIndexes::new(limits(AMPLE));
	let probe_index = probe.index(root.path());
	probe_index.refresh().await.unwrap();
	probe_index.files_spelling(&["f0_0"]).await.unwrap();
	let ample = probe_index.memory_use();

	// Room for the table and about two thirds of the full-size filters.
	let ceiling = ample.table + ample.filters * 2 / 3;
	let indexes = CodeIndexes::new(limits(ceiling));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	assert_eq!(index.files_spelling_verified(&["f7_3"]).await.unwrap().len(), 1);
	assert!(index.is_usable());
	assert!(index.memory_use().filters < ample.filters);
	assert!(lock(&index.state).bits_per_token < BITS_PER_TOKEN);
	assert!(indexes.memory_peak() <= ceiling);
}

#[tokio::test]
async fn a_root_that_needs_room_takes_it_from_the_parsed_cache_of_another_root() {
	let first = numbered_project(20, 100);
	let second = numbered_project(20, 100);
	let probe = CodeIndexes::new(limits(AMPLE));
	let mut sizes = Vec::new();
	for root in [&first, &second] {
		let index = probe.index(root.path());
		index.refresh().await.unwrap();
		index.files_spelling(&["f0_0"]).await.unwrap();
		for id in 0..20 {
			index.parsed(id).await.unwrap();
		}
		sizes.push(index.memory_use());
	}
	// The first root holds everything; the second finds room for its table
	// and only half of its filters at full size.
	let ceiling = sizes[0].total() + sizes[1].table + sizes[1].filters / 2;
	let indexes = CodeIndexes::new(limits(ceiling));
	let a = indexes.index(first.path());
	a.refresh().await.unwrap();
	a.files_spelling(&["f0_0"]).await.unwrap();
	for id in 0..20 {
		a.parsed(id).await.unwrap();
	}
	assert!(a.memory_use().parsed > 0);
	let b = indexes.index(second.path());
	b.refresh().await.unwrap();
	assert_eq!(b.files_spelling_verified(&["f3_3"]).await.unwrap().len(), 1);
	assert_eq!(a.memory_use().parsed, 0, "the other root gave its parsed data up");
	assert_eq!(
		lock(&b.state).bits_per_token,
		BITS_PER_TOKEN,
		"so this root did not shrink"
	);
	assert!(a.is_usable() && b.is_usable());
	assert!(indexes.memory_peak() <= ceiling);
}

#[tokio::test]
async fn a_root_that_cannot_fit_is_not_indexed_and_gives_its_memory_back() {
	let root = numbered_project(50, 200);
	let probe = CodeIndexes::new(limits(AMPLE));
	let probe_index = probe.index(root.path());
	probe_index.refresh().await.unwrap();
	probe_index.files_spelling(&["f0_0"]).await.unwrap();
	let ample = probe_index.memory_use();

	// Even the smallest filters need more than this.
	let ceiling = ample.table + ample.filters / 4;
	let indexes = CodeIndexes::new(limits(ceiling));
	let index = indexes.index(root.path());
	index.refresh().await.unwrap();
	let error = index.files_spelling(&["f0_0"]).await.unwrap_err();
	assert!(error.downcast_ref::<IndexUnavailable>().is_some(), "{error:#}");
	assert!(!index.is_usable());
	assert_eq!(indexes.memory_used(), 0);
	assert!(indexes.memory_peak() <= ceiling);
	assert!(index
		.refresh()
		.await
		.unwrap_err()
		.downcast_ref::<IndexUnavailable>()
		.is_some());
}

#[tokio::test]
async fn two_roots_are_independent_and_a_third_evicts_the_least_recent() {
	let first = project(&[("one.rs", "fn one() {}\n")]);
	let second = project(&[("two.rs", "fn two() {}\n")]);
	let third = project(&[("three.rs", "fn three() {}\n")]);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let a = indexes.index(first.path());
	let b = indexes.index(second.path());
	a.refresh().await.unwrap();
	b.refresh().await.unwrap();
	assert!(a.file_id("one.rs").is_some() && a.file_id("two.rs").is_none());
	assert!(b.file_id("two.rs").is_some() && b.file_id("one.rs").is_none());

	tokio::time::sleep(Duration::from_millis(5)).await;
	assert!(Arc::ptr_eq(&a, &indexes.index(first.path())));
	tokio::time::sleep(Duration::from_millis(5)).await;
	indexes.index(third.path());
	assert_eq!(indexes.root_count(), MAX_INDEXED_WORKDIRS);
	assert!(Arc::ptr_eq(&a, &indexes.index(first.path())), "the touched root stays");
	assert!(
		!Arc::ptr_eq(&b, &indexes.index(second.path())),
		"the least recent root was dropped"
	);
}

#[tokio::test]
async fn concurrent_refreshes_run_once() {
	let root = numbered_project(10, 3);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	let (a, b, c, d) = tokio::join!(index.refresh(), index.refresh(), index.refresh(), index.refresh());
	for changes in [a, b, c, d] {
		assert_eq!(changes.unwrap().new.len(), 10);
	}
	assert_eq!(index.refresh_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn an_idle_root_is_dropped_by_the_sweeper_and_its_memory_returns() {
	let root = numbered_project(10, 20);
	let indexes = CodeIndexes::new(IndexLimits {
		max_roots: MAX_INDEXED_WORKDIRS,
		memory_bytes: AMPLE,
		idle: Duration::from_millis(80),
	});
	{
		let index = indexes.index(root.path());
		index.refresh().await.unwrap();
		index.files_spelling(&["f1_1"]).await.unwrap();
		index.parsed(0).await.unwrap();
		assert!(indexes.memory_used() > 0);
	}
	assert_eq!(indexes.root_count(), 1);
	for _ in 0..50 {
		if indexes.root_count() == 0 {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	assert_eq!(indexes.root_count(), 0);
	assert_eq!(indexes.memory_used(), 0);
	tokio::time::sleep(Duration::from_millis(40)).await;
	assert!(
		!lock(&indexes.registry).sweeper_running,
		"the sweeper ends with the last root"
	);

	// A new index starts a new sweeper.
	let index = indexes.index(root.path());
	assert!(lock(&indexes.registry).sweeper_running);
	drop(index);
}

#[test]
fn the_shipped_limits_are_the_named_constants_and_the_registry_is_shared() {
	let shipped = IndexLimits::shipped();
	assert_eq!(shipped.max_roots, MAX_INDEXED_WORKDIRS);
	assert_eq!(shipped.memory_bytes, INDEX_MEMORY_BYTES);
	assert_eq!(shipped.idle, Duration::from_secs(INDEX_IDLE_SECS));
	assert!(std::ptr::eq(CodeIndexes::global(), CodeIndexes::global()));
}

#[tokio::test]
async fn memory_use_counts_table_filters_and_parsed_data() {
	let root = numbered_project(5, 10);
	let indexes = CodeIndexes::new(limits(AMPLE));
	let index = indexes.index(root.path());
	assert_eq!(index.memory_use().total(), 0);
	index.refresh().await.unwrap();
	let table_only = index.memory_use();
	assert!(table_only.table > 0 && table_only.filters == 0 && table_only.parsed == 0);
	index.files_spelling(&["f0_0"]).await.unwrap();
	assert!(index.memory_use().filters > 0);
	assert_eq!(index.file_count(), 5);
	assert_eq!(indexes.memory_used(), index.memory_use().total());
}
