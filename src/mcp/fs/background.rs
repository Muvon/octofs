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
//! A long-running command (a build, a test suite) does not have to block its
//! tool call. `background=true` detaches it, streams stdout+stderr to a per-job
//! log file, and returns a job handle plus the resource URI `octofs://jobs/<id>`.
//! The server exposes each job as a readable resource and, when the process
//! exits, emits `notifications/resources/updated` for that URI. A subscribed
//! client reads the resource to get the exit code and output tail — event-driven,
//! no polling. This module owns the registry and the log files; it knows nothing
//! about the MCP client. The completion signal is delivered through an opaque
//! callback the server layer supplies (it captures the rmcp peer there), so this
//! file stays free of any protocol/transport types.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
	pub log_path: PathBuf,
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

/// Spawn `command` detached, streaming combined output to a per-job log file.
/// `on_complete` is invoked exactly once, with the job's resource URI, after the
/// process exits — the server layer uses it to emit `resources/updated`.
pub fn spawn<F>(command: &str, working_dir: &Path, on_complete: F) -> Result<Job>
where
	F: FnOnce(String) + Send + 'static,
{
	use tokio::process::Command as TokioCommand;

	// Serialize background jobs by working directory. Two builds writing the same
	// tree corrupt each other's object archives, so identical commands come back
	// with mixed pass/fail — the exact trap a freed-up loop invites when a model
	// fires several builds at once. Reject the second here and point it at the
	// running job's resource so it waits for that completion event instead of
	// launching a competitor.
	// ponytail: per-cwd exclusion; add a write-intent flag only if concurrent
	// read-only background jobs in one dir ever become a real need.
	if let Some(running) = jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.values()
		.find(|j| j.working_dir.as_path() == working_dir && j.status() == JobStatus::Running)
	{
		return Err(anyhow!(
			"A background job is already running in this directory: {} (`{}`). \
			 Wait for its completion — you will get a resources/updated notification \
			 with its output — before starting another job here. Do not launch \
			 overlapping builds; concurrent writers to the same build tree corrupt \
			 each other's artifacts.",
			resource_uri(&running.id),
			running.command
		));
	}

	let dir = jobs_dir();
	std::fs::create_dir_all(&dir)
		.map_err(|e| anyhow!("Failed to create background job dir: {}", e))?;
	let id = next_id();
	let log_path = dir.join(format!("{id}.log"));
	let stdout_file = std::fs::File::create(&log_path)
		.map_err(|e| anyhow!("Failed to create background job log: {}", e))?;
	let stderr_file = stdout_file
		.try_clone()
		.map_err(|e| anyhow!("Failed to prepare background job log: {}", e))?;

	let mut cmd = if cfg!(target_os = "windows") {
		let mut c = TokioCommand::new("cmd");
		c.args(["/C", command]);
		c
	} else {
		let mut c = TokioCommand::new("sh");
		c.args(["-c", command]);
		c
	};
	cmd.current_dir(working_dir)
		.stdout(Stdio::from(stdout_file))
		.stderr(Stdio::from(stderr_file))
		.stdin(Stdio::null());
	// Own process group so the whole tree is killable and isolated from the
	// controlling terminal — same rationale as the foreground path.
	#[cfg(unix)]
	{
		cmd.process_group(0);
	}
	for (key, value) in super::shell::NONINTERACTIVE_ENVS {
		cmd.env(key, value);
	}

	let mut child = cmd
		.spawn()
		.map_err(|e| anyhow!("Failed to spawn background command: {}", e))?;
	let pid = child
		.id()
		.ok_or_else(|| anyhow!("Failed to get background process ID"))?;

	let status = Arc::new(Mutex::new(JobStatus::Running));
	let job = Job {
		id: id.clone(),
		command: command.to_string(),
		log_path: log_path.clone(),
		working_dir: working_dir.to_path_buf(),
		status: status.clone(),
		pid,
		started_unix: now_unix(),
	};
	jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.insert(id.clone(), job.clone());

	// A detached job must not outlive the server: register its process group so
	// shutdown cleanup (`kill_all_shell_children`) tears it down like any other
	// shell child, rather than orphaning a running build with nobody to read
	// its log. It survives its own tool call (`kill_on_drop` is not set on the
	// spawned child) but dies with the process; unregistered once it exits on
	// its own.
	super::shell::register_child(pid);

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

	Ok(job)
}

/// A point-in-time read of a job: its status and the tail of its output.
pub struct JobView {
	pub command: String,
	pub status: JobStatus,
	pub output: String,
	pub truncated: bool,
}

/// Read a job's current status and output tail. `None` if the id is unknown.
pub fn read(id: &str) -> Option<JobView> {
	let job = jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.get(id)
		.cloned()?;
	// Read only the tail, not the whole file: a build/test log can be hundreds
	// of MB, and pulling it all into memory just to keep the last 30 KB would
	// stall the request. Seek to the tail and read from there.
	use std::io::{Read, Seek, SeekFrom};
	let (tail, truncated) = match std::fs::File::open(&job.log_path) {
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
	};
	Some(JobView {
		command: job.command.clone(),
		status: job.status(),
		output: String::from_utf8_lossy(&tail).into_owned(),
		truncated,
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
