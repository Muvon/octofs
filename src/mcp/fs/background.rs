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
//! resource to get the exit code and the foreground result — event-driven, no polling.
//! A client that declared the MCP tasks extension gets the job as a task instead
//! and polls it; the server answers from the same registry (`read`, `cancel`).
//! This module owns the registry and the log files; it knows nothing about the
//! MCP client. The completion signal is delivered through an opaque callback
//! the server layer supplies (it captures the rmcp peer there), so this file
//! stays free of any protocol/transport types.

use crate::utils::truncation::MAX_RESULT_BYTES;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Running jobs expose only a progress tail; finished jobs retain the foreground result.
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
	finished_output: Arc<OnceLock<String>>,
	pub pid: u32,
	pub started_unix: u64,
	finished_unix: Arc<OnceLock<u64>>,
	/// Set when a client stopped the job while it was still running.
	cancel_requested: Arc<AtomicBool>,
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
	if let Some(running) =
		jobs().lock().expect("jobs registry mutex poisoned").values().find(|j| {
			j.working_dir.as_path() == working_dir && j.command == command && j.status() == JobStatus::Running
		}) {
		return Err(anyhow!(
			"`{}` is already running as background job {}. Wait for its completion \
			 notification instead of starting it again or a variant of it; unrelated commands \
			 may run concurrently.",
			running.command,
			resource_uri(&running.id)
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
			finished_output: Arc::new(OnceLock::new()),
			pid: 0,
			started_unix: now_unix(),
			finished_unix: Arc::new(OnceLock::new()),
			cancel_requested: Arc::new(AtomicBool::new(false)),
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

pub(super) fn output_path(job: &Job) -> PathBuf {
	job.stdout_path.with_file_name(format!("{}.output.log", job.id))
}

/// Cleaned full output survives capture-file removal until shutdown.
pub(super) async fn spill(job: &Job, output: &str) -> Result<()> {
	let path = output_path(job);
	tokio::fs::write(&path, output)
		.await
		.with_context(|| format!("Failed to persist full shell output: {}", path.display()))?;
	// The model has only seen the digest; its first view of the new spill must be full.
	super::delta::note_write(&super::remote::PathSource::from(path.as_path()), "", output);
	Ok(())
}

pub(super) async fn process_output(stdout: &[u8], stderr: &[u8], job: &Job) -> (String, Option<PathBuf>) {
	let stdout = super::shell::clean_terminal_noise(&String::from_utf8_lossy(stdout));
	let stderr = super::shell::clean_terminal_noise(&String::from_utf8_lossy(stderr));
	let full_output = super::digest::join_streams(&stdout, &stderr);
	let path = output_path(job);
	if let Some(output) = super::digest::digest(&stdout, &stderr, &path) {
		match spill(job, &full_output).await {
			Ok(()) => return (output, Some(path)),
			Err(error) => warn!("Failed to spill shell output for job {}: {error:#}", job.id),
		}
	}
	(full_output, None)
}

async fn read_finished_output(job: &Job, code: i32) -> Result<String> {
	let stdout = tokio::fs::read(&job.stdout_path)
		.await
		.map_err(|e| anyhow!("Failed to read command stdout: {}", e))?;
	let stderr = tokio::fs::read(&job.stderr_path)
		.await
		.map_err(|e| anyhow!("Failed to read command stderr: {}", e))?;
	let (output, path) = process_output(&stdout, &stderr, job).await;
	if output.len() > MAX_RESULT_BYTES {
		return Ok(super::shell::oversized_output_message(
			code,
			output.len(),
			path.as_deref(),
		));
	}
	Ok(output)
}

/// The directory may be absent when no shell command ran in this process.
pub async fn cleanup() -> Result<()> {
	remove_jobs_dir(&jobs_dir()).await
}

async fn remove_jobs_dir(dir: &Path) -> Result<()> {
	match tokio::fs::remove_dir_all(dir).await {
		Ok(()) => Ok(()),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(error) => Err(error).with_context(|| format!("Failed to remove shell job directory: {}", dir.display())),
	}
}

/// Register an already-running child as a background job and keep waiting for
/// it. `on_complete` is invoked exactly once after exit, with the resource URI.
pub(super) fn promote<F>(job: Job, mut child: tokio::process::Child, on_complete: F)
where
	F: FnOnce(String) + Send + 'static,
{
	let id = job.id.clone();
	let completed_job = job.clone();
	let pid = job.pid;
	jobs()
		.lock()
		.expect("jobs registry mutex poisoned")
		.insert(id.clone(), job);

	let uri = resource_uri(&id);
	tokio::spawn(async move {
		let result = child.wait().await;
		super::shell::unregister_child(pid);
		let (code, output) = match result {
			Ok(status) => {
				let code = status.code().unwrap_or(-1);
				let output = match read_finished_output(&completed_job, code).await {
					Ok(output) => output,
					Err(error) => error.to_string(),
				};
				(code, output)
			}
			Err(error) => (-1, format!("Error: Command execution failed: {error}")),
		};
		completed_job.finished_output.get_or_init(|| output);
		completed_job.finished_unix.get_or_init(now_unix);
		if let Ok(mut guard) = completed_job.status.lock() {
			*guard = JobStatus::Exited(code);
		}
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

/// Ask a running job to stop with SIGTERM to its process group, the same as the
/// `kill -- -<pid>` the shell response offers. The wait task still records the
/// exit, so a stopped job reads as finished. `None` if the id is unknown; a job
/// that already exited is left as it is.
pub async fn cancel(id: &str) -> Option<()> {
	let job = jobs().lock().expect("jobs registry mutex poisoned").get(id).cloned()?;
	if job.status() == JobStatus::Running {
		job.cancel_requested.store(true, Ordering::Relaxed);
		terminate(job.pid).await;
	}
	Some(())
}

#[cfg(unix)]
async fn terminate(pid: u32) {
	// SAFETY: kill is always safe with valid arguments; a negative pid targets
	// the job's process group (the child is a session leader, PGID == PID).
	unsafe {
		libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
	}
}

#[cfg(not(unix))]
async fn terminate(pid: u32) {
	let status = tokio::process::Command::new("taskkill")
		.args(["/PID", &pid.to_string(), "/T", "/F"])
		.status()
		.await;
	if let Err(error) = status {
		warn!("Failed to stop background job process {pid}: {error}");
	}
}

/// A point-in-time read: status and the foreground result if finished, a tail if running.
pub struct JobView {
	pub command: String,
	pub status: JobStatus,
	pub output: String,
	pub truncated: bool,
	pub started_unix: u64,
	/// Set once the process has exited.
	pub finished_unix: Option<u64>,
	pub cancel_requested: bool,
	pub pid: u32,
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

/// Read the finished foreground result or a running job's tail. `None` if the id is unknown.
pub fn read(id: &str) -> Option<JobView> {
	let job = jobs().lock().expect("jobs registry mutex poisoned").get(id).cloned()?;
	let status = job.status();
	if matches!(status, JobStatus::Exited(_)) {
		return Some(JobView {
			command: job.command.clone(),
			status,
			output: job.finished_output.get()?.clone(),
			truncated: false,
			started_unix: job.started_unix,
			finished_unix: job.finished_unix.get().copied(),
			cancel_requested: job.cancel_requested.load(Ordering::Relaxed),
			pid: job.pid,
		});
	}
	// Read only the tails, not whole files: build/test logs can be hundreds of
	// MB. Keeping stdout and stderr separate preserves the foreground response
	// framing even after automatic promotion.
	let (stdout, stdout_truncated) = read_tail(&job.stdout_path);
	let (stderr, stderr_truncated) = read_tail(&job.stderr_path);
	let stdout = String::from_utf8_lossy(&stdout);
	let stderr = String::from_utf8_lossy(&stderr);
	let output = super::digest::join_streams(&stdout, &stderr);
	let output_truncated = output.len() > MAX_TAIL_BYTES;
	let output = if output_truncated {
		String::from_utf8_lossy(&output.as_bytes()[output.len() - MAX_TAIL_BYTES..]).into_owned()
	} else {
		output
	};
	Some(JobView {
		command: job.command.clone(),
		status,
		output,
		truncated: stdout_truncated || stderr_truncated || output_truncated,
		started_unix: job.started_unix,
		finished_unix: None,
		cancel_requested: job.cancel_requested.load(Ordering::Relaxed),
		pid: job.pid,
	})
}

/// A job's resource text: status and retained output. One rendering for every way a
/// job is read — `resources/read`, `view` on its link, a completion push.
pub fn resource_text(id: &str) -> Option<String> {
	let view = read(id)?;
	let status = match view.status {
		JobStatus::Running => "running".to_string(),
		JobStatus::Exited(code) => format!("exited with code {code}"),
	};
	let truncated = if view.truncated {
		format!("\n[earlier output dropped — showing the last {MAX_TAIL_BYTES} bytes]")
	} else {
		String::new()
	};
	Some(format!(
		"job {id}\ncommand: {}\nstatus: {status}{truncated}\n\n{}",
		view.command, view.output
	))
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

#[cfg(test)]
#[path = "background_spill_tests.rs"]
mod background_spill_tests;
