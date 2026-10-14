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
