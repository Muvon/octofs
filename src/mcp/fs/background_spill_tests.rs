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

//! Spill-file persistence and failure boundaries.

use super::*;
use crate::mcp::fs::digest::mod_tests::{Fixture, COMPLETION_TIMEOUT, FIXTURES};

#[tokio::test]
async fn foreground_spill_survives_capture_discard() {
	let prepared = prepare("spill-test", &std::env::temp_dir()).await.unwrap();
	let PreparedJob {
		job,
		stdout_file,
		stderr_file,
	} = prepared;
	drop(stdout_file);
	drop(stderr_file);
	let output = "stdout\n\nstderr:\nwarning: details";
	spill(&job, output).await.unwrap();
	discard(&job).await;
	let path = output_path(&job);
	assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), output);
	assert!(!tokio::fs::try_exists(&job.stdout_path).await.unwrap());
	assert!(!tokio::fs::try_exists(&job.stderr_path).await.unwrap());
	tokio::fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn spill_write_failure_names_the_path() {
	let prepared = prepare("spill-failure-test", &std::env::temp_dir())
		.await
		.unwrap();
	let PreparedJob {
		job,
		stdout_file,
		stderr_file,
	} = prepared;
	drop(stdout_file);
	drop(stderr_file);
	let path = output_path(&job);
	tokio::fs::create_dir(&path).await.unwrap();
	let error = spill(&job, "output").await.unwrap_err();
	assert!(error.to_string().contains(path.to_str().unwrap()));
	let fixture = &FIXTURES[1];
	let (stdout, stderr) = fixture.cleaned();
	let (output, stored) =
		process_output(fixture.stdout.as_bytes(), fixture.stderr.as_bytes(), &job).await;
	assert_eq!(output, super::super::digest::join_streams(&stdout, &stderr));
	assert!(
		stored.is_none(),
		"spill failure must return undigested output"
	);
	tokio::fs::remove_dir(&path).await.unwrap();
	discard(&job).await;
}

#[tokio::test]
async fn shutdown_directory_cleanup_removes_captures_and_spills() {
	let root = tempfile::tempdir().unwrap();
	let dir = root.path().join("jobs");
	tokio::fs::create_dir(&dir).await.unwrap();
	for name in ["1.stdout.log", "1.stderr.log", "1.output.log"] {
		tokio::fs::write(dir.join(name), "output").await.unwrap();
	}
	remove_jobs_dir(&dir).await.unwrap();
	assert!(!tokio::fs::try_exists(&dir).await.unwrap());
	remove_jobs_dir(&dir).await.unwrap();
}

#[tokio::test]
async fn directory_cleanup_failure_is_propagated() {
	let root = tempfile::tempdir().unwrap();
	let path = root.path().join("file");
	tokio::fs::write(&path, "output").await.unwrap();
	let error = remove_jobs_dir(&path).await.unwrap_err();
	assert!(error.to_string().contains(path.to_str().unwrap()));
}

#[tokio::test]
async fn background_digest_is_cached_before_completion_notification() {
	for fixture in [&FIXTURES[0], &FIXTURES[1], &FIXTURES[7], &FIXTURES[8]] {
		let root = tempfile::tempdir().unwrap();
		let command = fixture.replay(root.path()).await;
		let (sender, receiver) = tokio::sync::oneshot::channel();
		let job = super::background_tests::spawn_test_job(&command, root.path(), move |uri| {
			let id = job_id_from_uri(&uri).unwrap();
			sender.send(resource_text(id).unwrap()).unwrap();
		})
		.await;
		let notification = tokio::time::timeout(COMPLETION_TIMEOUT, receiver)
			.await
			.unwrap()
			.unwrap();
		let view = read(&job.id).unwrap();
		assert_eq!(view.status, JobStatus::Exited(fixture.exit_code));
		assert!(!view.truncated);
		let (stdout, stderr) = fixture.cleaned();
		let path = output_path(&job);
		let expected = super::super::digest::digest(&stdout, &stderr, &path).unwrap();
		assert_eq!(view.output, expected);
		assert_eq!(
			tokio::fs::read_to_string(&path).await.unwrap(),
			super::super::digest::join_streams(&stdout, &stderr)
		);
		assert!(notification.ends_with(&expected));
		assert_eq!(notification, resource_text(&job.id).unwrap());
		discard(&job).await;
		assert_eq!(
			read(&job.id).unwrap().output,
			expected,
			"reads use the cached digest"
		);
		tokio::fs::remove_file(path).await.unwrap();
	}
}

#[tokio::test]
async fn unrecognized_background_output_keeps_the_raw_tail() {
	let root = tempfile::tempdir().unwrap();
	let fixture = Fixture {
		name: "unknown",
		stdout: FIXTURES[7].stdout,
		stderr: "unrecognized tool output\n",
		golden: "",
		exit_code: 3,
	};
	let command = fixture.replay(root.path()).await;
	let (sender, receiver) = tokio::sync::oneshot::channel();
	let job = super::background_tests::spawn_test_job(&command, root.path(), move |uri| {
		sender
			.send(resource_text(job_id_from_uri(&uri).unwrap()).unwrap())
			.unwrap();
	})
	.await;
	let notification = tokio::time::timeout(COMPLETION_TIMEOUT, receiver)
		.await
		.unwrap()
		.unwrap();
	let view = read(&job.id).unwrap();
	assert_eq!(view.status, JobStatus::Exited(3));
	assert!(view.truncated);
	assert!(job.digest.get().is_none());
	let full = super::super::digest::join_streams(fixture.stdout, fixture.stderr);
	assert!(full.len() > MAX_TAIL_BYTES);
	assert_eq!(view.output, &full[full.len() - MAX_TAIL_BYTES..]);
	assert_eq!(notification, resource_text(&job.id).unwrap());
	assert!(!tokio::fs::try_exists(output_path(&job)).await.unwrap());
	discard(&job).await;
}

#[tokio::test]
async fn background_output_over_the_limit_is_not_digested() {
	let PreparedJob {
		job,
		stdout_file,
		stderr_file,
	} = prepare("large-digest-test", &std::env::temp_dir())
		.await
		.unwrap();
	drop(stdout_file);
	drop(stderr_file);
	let file = tokio::fs::File::create(&job.stdout_path).await.unwrap();
	file.set_len(MAX_RESULT_BYTES as u64 + 1).await.unwrap();
	drop(file);
	cache_digest(&job).await.unwrap();
	assert!(job.digest.get().is_none());
	assert!(!tokio::fs::try_exists(output_path(&job)).await.unwrap());
	discard(&job).await;
}
