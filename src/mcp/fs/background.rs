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

//! Background shell jobs, surfaced as MCP resources.
//!
//! A command starts in the foreground. If it outlasts the foreground window,
//! the same process is registered here as a background job and its tool call
//! returns a resource URI `octofs://jobs/<id>`. The server exposes each job as a
//! readable resource and, when the process exits, emits
//! `notifications/resources/updated` for that URI. A subscribed client reads the
//! resource to get the exit code and output tail — event-driven, no polling.
//! This module owns the registry and the log files; it knows nothing about the
//! MCP client. The completion signal is delivered through an opaque callback
//! the server layer supplies (it captures the rmcp peer there), so this file
//! stays free of any protocol/transport types.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The most output any single resource read returns. Build logs run long; the
/// tail is what carries the verdict (errors, the final test summary), so the
/// head is dropped when the log is bigger than this.
const MAX_TAIL_BYTES: usize = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
	Running,
	Exited(i32),
}

#[derive(Debug, Clone)]
pub struct Job {
	pub id: String,
	pub command: String,
	pub stdout_path: PathBuf,
	pub stderr_path: PathBuf,
	pub working_dir: PathBuf,
	pub status: Arc<Mutex<JobStatus>>,
	pub pid: u32,
	pub started_unix: u64,
}

impl Job {
	pub fn status(&self) -> JobStatus {
		*self.status.lock().expect("job status mutex poisoned")
	}
}

static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
	JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn jobs_dir() -> PathBuf {
	std::env::temp_dir().join(format!("octofs-jobs-{}", std::process::id()))
}

/// A job id unique within this octofs process: pid keeps it distinct from a
/// concurrent octofs, the counter distinct from a sibling job. No randomness —
/// the process is single-writer to the counter.
fn next_id() -> String {
	let n = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
	format!("{}-{}", std::process::id(), n)
}

pub fn resource_uri(id: &str) -> String {
	format!("octofs://jobs/{id}")
}

/// The job id inside an `octofs://jobs/<id>` URI, if it is one.
pub fn job_id_from_uri(uri: &str) -> Option<&str> {
	uri.strip_prefix("octofs://jobs/")
}

fn now_unix() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_secs())
		.unwrap_or(0)
}

/// Files and metadata prepared before a shell child starts. Output is durable
/// from byte one, so crossing the foreground deadline never loses output or
/// requires restarting the command.
#[derive(Debug)]
pub(super) struct PreparedJob {
	pub job: Job,
	pub stdout_file: std::fs::File,
	pub stderr_file: std::fs::File,
}

/// Prepare durable output capture for a command that initially runs in the
/// foreground. Only an identical command already running in the same directory
/// is rejected; independent commands may run concurrently.
pub(super) async fn prepare(command: &str, working_dir: &Path) -> Result<PreparedJob> {
	if let Some(running) = jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.values()
		.find(|j| {
			j.working_dir.as_path() == working_dir
				&& j.command == command
				&& j.status() == JobStatus::Running
		}) {
		return Err(anyhow!(
			"The same shell command is already running as background job {} (`{}`). \
			 Wait for its completion — you will get a resources/updated notification \
			 with its output — instead of starting a duplicate. Independent commands may \
			 run concurrently in this directory.",
			resource_uri(&running.id),
			running.command
		));
	}

	let dir = jobs_dir();
	tokio::fs::create_dir_all(&dir)
		.await
		.map_err(|e| anyhow!("Failed to create background job dir: {}", e))?;
	let id = next_id();
	let stdout_path = dir.join(format!("{id}.stdout.log"));
	let stderr_path = dir.join(format!("{id}.stderr.log"));
	let stdout_file = tokio::fs::File::create(&stdout_path)
		.await
		.map_err(|e| anyhow!("Failed to create shell stdout log: {}", e))?
		.into_std()
		.await;
	let stderr_file = match tokio::fs::File::create(&stderr_path).await {
		Ok(file) => file.into_std().await,
		Err(error) => {
			drop(stdout_file);
			let _ = tokio::fs::remove_file(&stdout_path).await;
			return Err(anyhow!("Failed to create shell stderr log: {}", error));
		}
	};

	Ok(PreparedJob {
		job: Job {
			id,
			command: command.to_string(),
			stdout_path,
			stderr_path,
			working_dir: working_dir.to_path_buf(),
			status: Arc::new(Mutex::new(JobStatus::Running)),
			pid: 0,
			started_unix: now_unix(),
		},
		stdout_file,
		stderr_file,
	})
}

/// Remove capture files for a command that completed inside the foreground
/// window or failed to spawn.
pub(super) async fn discard(job: &Job) {
	let _ = tokio::fs::remove_file(&job.stdout_path).await;
	let _ = tokio::fs::remove_file(&job.stderr_path).await;
}

/// Register an already-running child as a background job and keep waiting for
/// it. `on_complete` is invoked exactly once after exit, with the resource URI.
pub(super) fn promote<F>(job: Job, mut child: tokio::process::Child, on_complete: F)
where
	F: FnOnce(String) + Send + 'static,
{
	let id = job.id.clone();
	let status = job.status.clone();
	let pid = job.pid;
	jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.insert(id.clone(), job);

	let uri = resource_uri(&id);
	tokio::spawn(async move {
		let code = match child.wait().await {
			Ok(status) => status.code().unwrap_or(-1),
			Err(_) => -1,
		};
		if let Ok(mut guard) = status.lock() {
			*guard = JobStatus::Exited(code);
		}
		super::shell::unregister_child(pid);
		on_complete(uri);
	});
}

/// Return the current status for a registered job without reading its logs.
pub(crate) fn status(id: &str) -> Option<JobStatus> {
	jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.get(id)
		.map(Job::status)
}

/// A point-in-time read of a job: its status and the tail of its output.
pub struct JobView {
	pub command: String,
	pub status: JobStatus,
	pub output: String,
	pub truncated: bool,
}

fn read_tail(path: &Path) -> (Vec<u8>, bool) {
	use std::io::{Read, Seek, SeekFrom};

	match std::fs::File::open(path) {
		Ok(mut file) => {
			let len = file.metadata().map(|m| m.len()).unwrap_or(0);
			let truncated = len > MAX_TAIL_BYTES as u64;
			if truncated {
				let _ = file.seek(SeekFrom::End(-(MAX_TAIL_BYTES as i64)));
			}
			let mut buf = Vec::with_capacity(MAX_TAIL_BYTES.min(len as usize));
			let _ = file.read_to_end(&mut buf);
			(buf, truncated)
		}
		Err(_) => (Vec::new(), false),
	}
}

/// Read a job's current status and output tail. `None` if the id is unknown.
pub fn read(id: &str) -> Option<JobView> {
	let job = jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.get(id)
		.cloned()?;
	// Read only the tails, not whole files: build/test logs can be hundreds of
	// MB. Keeping stdout and stderr separate preserves the foreground response
	// framing even after automatic promotion.
	let (stdout, stdout_truncated) = read_tail(&job.stdout_path);
	let (stderr, stderr_truncated) = read_tail(&job.stderr_path);
	let stdout = String::from_utf8_lossy(&stdout);
	let stderr = String::from_utf8_lossy(&stderr);
	let output = if stderr.is_empty() {
		stdout.into_owned()
	} else if stdout.is_empty() {
		stderr.into_owned()
	} else {
		format!("{stdout}\n\nError: {stderr}")
	};
	let output_truncated = output.len() > MAX_TAIL_BYTES;
	let output = if output_truncated {
		String::from_utf8_lossy(&output.as_bytes()[output.len() - MAX_TAIL_BYTES..]).into_owned()
	} else {
		output
	};
	Some(JobView {
		command: job.command.clone(),
		status: job.status(),
		output,
		truncated: stdout_truncated || stderr_truncated || output_truncated,
	})
}

/// Every registered job, for `resources/list`.
pub fn list() -> Vec<Job> {
	jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.values()
		.cloned()
		.collect()
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod background_tests;
