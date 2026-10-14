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

//! Real docker captures, golden selections, and incomplete/foreign output rejection.

use super::super::{digest_with, lines, render};
use super::*;
use crate::mcp::fs::shell::clean_terminal_noise;
use std::path::Path;

use super::super::build_capture_tests as captures;
use captures::{Capture, SPILL};

const CAPTURES: &[Capture] = &[
	Capture {
		name: "docker-build-build-copy-fail",
		stdout: include_str!("fixtures/docker/docker-build-build-copy-fail.stdout"),
		stderr: include_str!("fixtures/docker/docker-build-build-copy-fail.stderr"),
		merged: include_str!("fixtures/docker/docker-build-build-copy-fail.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-copy-fail.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-copy-fail.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "docker-build-build-ok-cached",
		stdout: include_str!("fixtures/docker/docker-build-build-ok-cached.stdout"),
		stderr: include_str!("fixtures/docker/docker-build-build-ok-cached.stderr"),
		merged: include_str!("fixtures/docker/docker-build-build-ok-cached.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-ok-cached.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-ok-cached.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "docker-build-build-ok-nocache",
		stdout: include_str!("fixtures/docker/docker-build-build-ok-nocache.stdout"),
		stderr: include_str!("fixtures/docker/docker-build-build-ok-nocache.stderr"),
		merged: include_str!("fixtures/docker/docker-build-build-ok-nocache.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-ok-nocache.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-ok-nocache.merged.digest"
		)),
		split_digest: true,
		merged_digest: true,
	},
	Capture {
		name: "docker-build-build-run-fail-legacy",
		stdout: include_str!("fixtures/docker/docker-build-build-run-fail-legacy.stdout"),
		stderr: include_str!("fixtures/docker/docker-build-build-run-fail-legacy.stderr"),
		merged: include_str!("fixtures/docker/docker-build-build-run-fail-legacy.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-run-fail-legacy.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-run-fail-legacy.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "docker-build-build-run-fail",
		stdout: include_str!("fixtures/docker/docker-build-build-run-fail.stdout"),
		stderr: include_str!("fixtures/docker/docker-build-build-run-fail.stderr"),
		merged: include_str!("fixtures/docker/docker-build-build-run-fail.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-run-fail.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-build-build-run-fail.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "docker-install-build-cached",
		stdout: include_str!("fixtures/docker/docker-install-build-cached.stdout"),
		stderr: include_str!("fixtures/docker/docker-install-build-cached.stderr"),
		merged: include_str!("fixtures/docker/docker-install-build-cached.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-cached.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-cached.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
	Capture {
		name: "docker-install-build-fail",
		stdout: include_str!("fixtures/docker/docker-install-build-fail.stdout"),
		stderr: include_str!("fixtures/docker/docker-install-build-fail.stderr"),
		merged: include_str!("fixtures/docker/docker-install-build-fail.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-fail.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-fail.merged.digest"
		)),
		split_digest: true,
		merged_digest: true,
	},
	Capture {
		name: "docker-install-build-fresh",
		stdout: include_str!("fixtures/docker/docker-install-build-fresh.stdout"),
		stderr: include_str!("fixtures/docker/docker-install-build-fresh.stderr"),
		merged: include_str!("fixtures/docker/docker-install-build-fresh.merged"),
		split_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-fresh.digest"
		)),
		merged_golden: Some(include_str!(
			"fixtures/docker/docker-install-build-fresh.merged.digest"
		)),
		split_digest: false,
		merged_digest: false,
	},
];

#[test]
fn real_captures_match_goldens_and_framework_limits() {
	captures::check(CAPTURES, select);
}

#[test]
fn incomplete_steps_recaps_and_legacy_builds_are_rejected() {
	let success = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-build-build-ok-nocache.stderr"
	));
	let failure = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-install-build-fail.stderr"
	));
	let legacy = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-build-build-run-fail-legacy.merged"
	));
	for truncated in [
		&success[..success.rfind("#12 DONE ").unwrap()],
		&failure[..failure.rfind("ERROR: failed to build:").unwrap()],
		&failure[..failure.find("------").unwrap()],
		&legacy[..legacy.rfind("The command '").unwrap()],
	] {
		assert!(select(&lines(truncated), &[]).is_none());
		assert!(digest_with(truncated, "", Path::new(SPILL), &[select]).is_none());
	}
	for corrupted in [
		failure.replace(
			"| >>> RUN pip install psycopg2==2.9.10",
			"| RUN pip install psycopg2==2.9.10",
		),
		failure.replace(
			" > [7/8] RUN pip install psycopg2==2.9.10:",
			" > [6/8] COPY app.py .:",
		),
		success.replace("#9 DONE 5.1s", "#9 PENDING"),
		success.replace("[7/7]", "[6/7]"),
		legacy.replace("Step 4/7 :", "Step 5/7 :"),
		legacy.replace("returned a non-zero code: 2", "returned a non-zero code: 0"),
		format!("{legacy}\nSuccessfully tagged image:latest"),
	] {
		assert!(select(&lines(&corrupted), &[]).is_none());
	}
}

#[test]
fn legacy_success_keeps_final_image_status() {
	let output = "Step 1/2 : FROM alpine:3.20\n ---> 0123456789ab\nStep 2/2 : RUN echo ready\n ---> Running in abcdef012345\nready\n ---> Removed intermediate container abcdef012345\n ---> 123456789abc\nSuccessfully built 123456789abc\nSuccessfully tagged image:latest";
	let selection = select(&lines(output), &[]).unwrap();
	assert_eq!(
		render(&lines(output), &selection.stdout),
		"Successfully built 123456789abc\nSuccessfully tagged image:latest"
	);
}

#[test]
fn failing_step_and_recap_are_kept_in_full_after_terminal_cleanup() {
	let raw = include_str!("fixtures/docker/docker-install-build-fail.stderr");
	assert!(raw.contains('\r'));
	let clean = clean_terminal_noise(raw);
	assert!(!clean.contains('\r'));
	let selection = select(&[], &lines(&clean)).unwrap();
	let output = render(&lines(&clean), &selection.stderr);
	assert!(output.contains(&clean[clean.find("#11 [7/8]").unwrap()..]));
	assert!(output.contains("WARNING: Running pip as the 'root' user"));
	assert!(digest_with("", &clean, Path::new(SPILL), &[select]).is_some());
}

#[test]
fn framework_rejects_provable_noise_with_important_substrings() {
	let fresh = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-install-build-fresh.stderr"
	));
	let selection = select(&[], &lines(&fresh)).unwrap();
	let output = render(&lines(&fresh), &selection.stderr);
	assert!(fresh.contains("liberror-perl"));
	assert!(!output.contains("liberror-perl"));
	assert!(digest_with("", &fresh, Path::new(SPILL), &[select]).is_none());
	let stdout = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-build-build-run-fail-legacy.stdout"
	));
	let stderr = clean_terminal_noise(include_str!(
		"fixtures/docker/docker-build-build-run-fail-legacy.stderr"
	));
	let stdout_lines = lines(&stdout);
	let selection = select(&stdout_lines, &lines(&stderr)).unwrap();
	assert!(!selection.stdout.contains(
		&stdout_lines
			.iter()
			.position(|line| *line == " ---> bf8527eb54c3")
			.unwrap()
	));
	assert!(!super::super::valid_selection(
		&stdout_lines,
		&selection.stdout
	));
	assert!(digest_with(&stdout, &stderr, Path::new(SPILL), &[select]).is_none());
}
