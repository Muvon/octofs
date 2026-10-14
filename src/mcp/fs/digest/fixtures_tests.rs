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

//! Real offline Cargo captures shared by parser and shell integration tests.

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::OnceCell;

pub(crate) struct Capture {
	pub name: &'static str,
	pub stdout: String,
	pub stderr: String,
	pub expected_stdout: String,
	pub expected_stderr: String,
	pub stdout_path: PathBuf,
	pub stderr_path: PathBuf,
	pub exit_code: i32,
}

pub(crate) struct Captures {
	pub root: TempDir,
	pub logs: Vec<Capture>,
}

static CAPTURES: OnceCell<Captures> = OnceCell::const_new();

pub(crate) async fn captures() -> &'static Captures {
	CAPTURES.get_or_init(capture).await
}

async fn write(path: &Path, content: &str) {
	tokio::fs::write(path, content)
		.await
		.expect("write scratch fixture");
}

async fn capture() -> Captures {
	let root = tempfile::tempdir().expect("scratch crate");
	println!(
		"Real Cargo captures and golden streams: {}",
		root.path().display()
	);
	let mut manifest = include_str!("fixtures/Cargo.toml").to_string();
	// Real path dependencies create enough build progress to cross the digest
	// threshold without padding or duplicating captured output.
	for i in 0..100 {
		let name = format!("digest_dependency_{i:03}");
		let dir = root.path().join(&name);
		tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
		write(
			&dir.join("Cargo.toml"),
			&format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
		)
		.await;
		write(
			&dir.join("src/lib.rs"),
			include_str!("fixtures/dependency.rs"),
		)
		.await;
		manifest.push_str(&format!("{name} = {{ path = \"{name}\" }}\n"));
	}
	write(&root.path().join("Cargo.toml"), &manifest).await;
	tokio::fs::create_dir(root.path().join("src"))
		.await
		.unwrap();
	let mut source = include_str!("fixtures/lib.rs").to_string();
	for i in 0..240 {
		source.push_str(&format!(
			"\n#[test]\nfn passing_test_number_{i:03}() {{ assert_eq!(add(1, 1), 2); }}\n"
		));
	}
	write(&root.path().join("src/lib.rs"), &source).await;
	tokio::fs::create_dir(root.path().join("tests"))
		.await
		.unwrap();
	write(
		&root.path().join("tests/integration.rs"),
		include_str!("fixtures/integration.rs"),
	)
	.await;
	let lock = Command::new("cargo")
		.current_dir(root.path())
		.args(["generate-lockfile", "--offline"])
		.env("NO_COLOR", "1")
		.env("CARGO_TERM_COLOR", "never")
		.output()
		.await
		.expect("generate scratch lockfile");
	assert!(
		lock.status.success(),
		"{}",
		String::from_utf8_lossy(&lock.stderr)
	);

	let mut logs = Vec::new();
	for (name, tool, feature, expected_code) in [
		("pass", "test", None, 0),
		("fail", "test", Some("failures"), 101),
		("compile-error", "build", Some("compile_error"), 101),
		("warnings", "build", Some("warnings"), 0),
		("check", "check", Some("warnings"), 0),
		("clippy", "clippy", Some("warnings"), 0),
		("mixed", "test", Some("integration_failure"), 101),
	] {
		let mut command = Command::new("cargo");
		command
			.current_dir(root.path())
			.env("NO_COLOR", "1")
			.env("CARGO_TERM_COLOR", "never")
			.arg(tool)
			.arg("--offline")
			.arg("--target-dir")
			.arg(root.path().join(format!("target-{name}")));
		if let Some(feature) = feature {
			command.args(["--features", feature]);
		}
		if tool == "test" {
			command.args(["--", "--test-threads=1"]);
		}
		let output = command.output().await.expect("run real Cargo fixture");
		let exit_code = output.status.code().expect("Cargo exit code");
		assert_eq!(
			exit_code,
			expected_code,
			"{name}: {}",
			String::from_utf8_lossy(&output.stderr)
		);
		let stdout_path = root.path().join(format!("{name}.stdout"));
		let stderr_path = root.path().join(format!("{name}.stderr"));
		tokio::fs::write(&stdout_path, &output.stdout)
			.await
			.unwrap();
		tokio::fs::write(&stderr_path, &output.stderr)
			.await
			.unwrap();
		let stdout =
			crate::mcp::fs::shell::clean_terminal_noise(&String::from_utf8(output.stdout).unwrap());
		let stderr =
			crate::mcp::fs::shell::clean_terminal_noise(&String::from_utf8(output.stderr).unwrap());
		let (expected_stdout, expected_stderr) = golden(name, &stdout, &stderr);
		write(
			&root.path().join(format!("{name}.stdout.golden")),
			&expected_stdout,
		)
		.await;
		write(
			&root.path().join(format!("{name}.stderr.golden")),
			&expected_stderr,
		)
		.await;
		logs.push(Capture {
			name,
			stdout,
			stderr,
			expected_stdout,
			expected_stderr,
			stdout_path,
			stderr_path,
			exit_code,
		});
	}
	Captures { root, logs }
}

// Goldens are slices of authentic captures. Their boundaries follow the
// fixture's known scenarios, independently of parser recognition and counters.
fn golden(name: &str, stdout: &str, stderr: &str) -> (String, String) {
	match name {
		"pass" => (
			stdout
				.lines()
				.filter(|line| line.starts_with("test result:"))
				.collect::<Vec<_>>()
				.join("\n"),
			stderr
				.lines()
				.filter(|line| line.trim_start().starts_with("Finished "))
				.collect::<Vec<_>>()
				.join("\n"),
		),
		"fail" | "mixed" => {
			let (before, failures) = stdout
				.split_once("failures:")
				.expect("real failure section");
			let mut selected: Vec<_> = before
				.lines()
				.filter(|line| {
					line.starts_with("test result:")
						|| line.ends_with(" ... FAILED")
						|| (line.starts_with("running ")
							&& (name == "fail" || *line == "running 1 test"))
				})
				.collect();
			selected.push("failures:");
			let mut expected_stdout = selected.join("\n");
			expected_stdout.push_str(failures);
			let last_running = stderr
				.lines()
				.rev()
				.find(|line| line.trim_start().starts_with("Running "))
				.expect("failing binary header");
			let expected_stderr = stderr
				.lines()
				.filter(|line| {
					line.trim_start().starts_with("Finished ")
						|| (line.trim_start().starts_with("Running ")
							&& (name == "fail" || *line == last_running))
						|| line.starts_with("error: test failed")
				})
				.collect::<Vec<_>>()
				.join("\n");
			(expected_stdout, expected_stderr)
		}
		"compile-error" => {
			let start = stderr.find("error[").expect("real rustc error");
			(String::new(), stderr[start..].to_string())
		}
		"warnings" | "check" | "clippy" => {
			let start = stderr.find("warning:").expect("real warning");
			(String::new(), stderr[start..].to_string())
		}
		_ => panic!("unknown fixture"),
	}
}
