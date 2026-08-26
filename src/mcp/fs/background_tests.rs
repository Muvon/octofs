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
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[test]
fn uri_roundtrips_and_rejects_foreign_schemes() {
	let uri = resource_uri("1234-7");
	assert_eq!(uri, "octofs://jobs/1234-7");
	assert_eq!(job_id_from_uri(&uri), Some("1234-7"));
	assert_eq!(job_id_from_uri("octofs://other/1234-7"), None);
	assert_eq!(job_id_from_uri("file:///tmp/x"), None);
}

#[tokio::test]
async fn detached_job_captures_output_status_and_fires_completion() {
	let done = std::sync::Arc::new(AtomicBool::new(false));
	let flag = done.clone();
	// cmd.exe chains with `&`, not `;`, and sends to stderr via `1>&2`; a POSIX
	// `printf ...; ...; exit` line runs as one bogus command there and never sets
	// the exit code. Pick the shell's own idiom so the job exits 3 on both.
	let command = if cfg!(target_os = "windows") {
		"echo building... & echo done 1>&2 & exit 3"
	} else {
		"printf 'building...\\n'; printf 'done\\n' 1>&2; exit 3"
	};
	let job = spawn(command, &std::env::temp_dir(), move |uri| {
		assert!(uri.starts_with("octofs://jobs/"));
		flag.store(true, Ordering::Relaxed);
	})
	.expect("spawn background job");

	// Poll the resource until the process exits (bounded).
	let mut view = None;
	for _ in 0..250 {
		let v = read(&job.id).expect("job is registered");
		if matches!(v.status, JobStatus::Exited(_)) {
			view = Some(v);
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	let view = view.expect("job must exit within the bound");
	assert_eq!(view.status, JobStatus::Exited(3), "exit code surfaced");
	assert!(
		view.output.contains("building...") && view.output.contains("done"),
		"stdout and stderr both captured to the buffer: {:?}",
		view.output
	);

	// The completion callback (what the server turns into resources/updated)
	// fires after exit.
	for _ in 0..50 {
		if done.load(Ordering::Relaxed) {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	assert!(
		done.load(Ordering::Relaxed),
		"on_complete must fire once the job exits"
	);
}

#[test]
fn reading_an_unknown_job_is_none() {
	assert!(read("no-such-job").is_none());
}

#[tokio::test]
async fn refuses_a_second_job_in_the_same_working_dir() {
	// A dir unique to this test so it never collides with other tests' jobs.
	let dir = std::env::temp_dir().join(format!("octofs-guard-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();

	// A job that stays alive a couple of seconds so the second spawn overlaps it.
	// `ping -n` is cmd.exe's idiom for a bounded wait; `sleep` is the POSIX one.
	let hold = if cfg!(target_os = "windows") {
		"ping -n 3 127.0.0.1"
	} else {
		"sleep 2"
	};
	let first = spawn(hold, &dir, |_: String| {}).expect("first job spawns");
	let refused = spawn("echo second", &dir, |_: String| {});
	assert!(
		refused.is_err(),
		"a second job in the same dir is refused while the first runs"
	);
	assert!(
		refused
			.unwrap_err()
			.to_string()
			.contains(&resource_uri(&first.id)),
		"the rejection points at the running job's resource"
	);

	// A different directory is unaffected by the exclusion.
	let other = dir.join("nested");
	std::fs::create_dir_all(&other).unwrap();
	assert!(
		spawn("echo ok", &other, |_: String| {}).is_ok(),
		"a job in a different dir is allowed while the first runs"
	);

	// Once the first exits, its directory frees up again.
	for _ in 0..250 {
		if matches!(read(&first.id).unwrap().status, JobStatus::Exited(_)) {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	assert!(
		spawn("echo ok", &dir, |_: String| {}).is_ok(),
		"the dir accepts a new job after the first finishes"
	);
}
