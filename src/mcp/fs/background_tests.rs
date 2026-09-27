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
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

async fn spawn_test_job<F>(command: &str, working_dir: &Path, on_complete: F) -> Job
where
	F: FnOnce(String) + Send + 'static,
{
	use tokio::process::Command;

	let PreparedJob {
		mut job,
		stdout_file,
		stderr_file,
	} = prepare(command, working_dir).await.expect("prepare job");
	let mut command_process = if cfg!(target_os = "windows") {
		let mut process = Command::new("cmd");
		process.args(["/C", command]);
		process
	} else {
		let mut process = Command::new("sh");
		process.args(["-c", command]);
		process
	};
	command_process
		.current_dir(working_dir)
		.stdout(Stdio::from(stdout_file))
		.stderr(Stdio::from(stderr_file))
		.stdin(Stdio::null())
		.kill_on_drop(true);
	#[cfg(unix)]
	{
		command_process.process_group(0);
	}
	let child = command_process.spawn().expect("spawn test job");
	job.pid = child.id().expect("test job pid");
	super::super::shell::register_child(job.pid);
	promote(job.clone(), child, on_complete);
	job
}

#[test]
fn uri_roundtrips_and_rejects_foreign_schemes() {
	let uri = resource_uri("1234-7");
	assert_eq!(uri, "octofs://jobs/1234-7");
	assert_eq!(job_id_from_uri(&uri), Some("1234-7"));
	assert_eq!(job_id_from_uri("octofs://other/1234-7"), None);
	assert_eq!(job_id_from_uri("file:///tmp/x"), None);
}

#[tokio::test]
async fn promoted_job_captures_output_status_and_fires_completion() {
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
	let job = spawn_test_job(command, &std::env::temp_dir(), move |uri| {
		assert!(uri.starts_with("octofs://jobs/"));
		flag.store(true, Ordering::Relaxed);
	})
	.await;

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
async fn allows_distinct_jobs_but_refuses_a_duplicate_in_the_same_working_dir() {
	// A dir unique to this test so it never collides with other tests' jobs.
	let dir = std::env::temp_dir().join(format!("octofs-guard-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();

	// A job that stays alive a couple of seconds so the second prepare overlaps it.
	// `ping -n` is cmd.exe's idiom for a bounded wait; `sleep` is the POSIX one.
	let hold = if cfg!(target_os = "windows") {
		"ping -n 3 127.0.0.1"
	} else {
		"sleep 2"
	};
	let first = spawn_test_job(hold, &dir, |_: String| {}).await;
	let refused = prepare(hold, &dir).await;
	assert!(
		refused.is_err(),
		"an identical job in the same dir is refused while the first runs"
	);
	assert!(
		refused
			.unwrap_err()
			.to_string()
			.contains(&resource_uri(&first.id)),
		"the rejection points at the running job's resource"
	);

	// A distinct command in the same directory is independent and may overlap.
	let second_hold = if cfg!(target_os = "windows") {
		"ping -n 4 127.0.0.1"
	} else {
		"sleep 3"
	};
	let second = spawn_test_job(second_hold, &dir, |_: String| {}).await;
	assert_eq!(second.status(), JobStatus::Running);
	let running_ids: Vec<String> = list()
		.into_iter()
		.filter(|job| job.working_dir == dir && job.status() == JobStatus::Running)
		.map(|job| job.id)
		.collect();
	assert!(running_ids.contains(&first.id));
	assert!(running_ids.contains(&second.id));

	// The same command in a different directory is also independent.
	let other = dir.join("nested");
	std::fs::create_dir_all(&other).unwrap();
	let _other_job = spawn_test_job(hold, &other, |_: String| {}).await;

	// Once the first exits, the same command may be launched there again even
	// while another, distinct command is still running.
	for _ in 0..250 {
		if matches!(read(&first.id).unwrap().status, JobStatus::Exited(_)) {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	let _next_job = spawn_test_job(hold, &dir, |_: String| {}).await;
}
