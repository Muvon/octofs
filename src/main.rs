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

use anyhow::{Context, Result};
use clap::Parser;
use tracing::Level;

mod cli;
mod http_auth;
pub mod mcp;
pub mod utils;

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = Cli::parse();

	match cli.command {
		Commands::Mcp {
			path,
			bind,
			auth_token,
			ssh_key,
			ssh_timeout,
			hint_mode,
		} => {
			// Resolve working directory
			let working_directory = if let Some(p) = path {
				p.canonicalize().unwrap_or(p)
			} else {
				std::env::current_dir()?
			};

			// Stderr-only logging — no stdout (would corrupt stdio MCP protocol)
			init_mcp_logging();

			// Initialize SFTP pool for remote filesystem support.
			let config = mcp::fs::remote::SshConfig {
				key_path: ssh_key,
				timeout: std::time::Duration::from_secs(ssh_timeout.unwrap_or(30)),
			};
			mcp::fs::remote::init_sftp_pool(config);

			// Set the session root directory for all tool calls
			mcp::set_session_root_directory(working_directory.clone());

			mcp::set_hint_mode(if hint_mode == "hard" {
				mcp::HintMode::Hard
			} else {
				mcp::HintMode::Soft
			});

			match bind {
				Some(addr) => {
					// HTTP mode with Streamable HTTP transport
					let auth_token = http_auth::resolve_auth_token(auth_token);
					run_http_server(&addr, auth_token.as_deref()).await?;
				}
				None => {
					// STDIO mode (default)
					run_stdio_server(mcp::server::OctofsServer::new()).await?;
				}
			}
		}
	}

	Ok(())
}

/// Run the MCP server over STDIO (default mode).
///
/// Exits cleanly on: EOF on stdin (parent closed pipe), or SIGTERM
/// (parent requests graceful shutdown). Both paths call kill_all_shell_children()
/// to ensure no orphan processes survive after we exit.
async fn run_stdio_server(server: mcp::server::OctofsServer) -> Result<()> {
	use rmcp::{transport::stdio, ServiceExt};

	tracing::info!("Starting MCP server (STDIO mode)");

	let service = server.serve(stdio()).await.inspect_err(|e| {
		tracing::error!("serving error: {:?}", e);
	})?;

	// `waiting()` blocks until the transport closes naturally (EOF on stdin).
	// For SIGTERM we cancel via the service's cancellation token so the task
	// exits cleanly without dropping the handle mid-flight.
	#[cfg(unix)]
	{
		use tokio::signal::unix::{signal, SignalKind};
		let ct = service.cancellation_token();
		let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
		tokio::select! {
			_ = service.waiting() => {
				// EOF on stdin — clean shutdown
			}
			_ = sigterm.recv() => {
				tracing::debug!("SIGTERM received, shutting down gracefully");
				ct.cancel();
			}
		}
	}
	#[cfg(not(unix))]
	{
		service.waiting().await.ok();
	}

	// Kill all in-flight shell children's process groups so nothing survives as an orphan.
	mcp::fs::shell::kill_all_shell_children();
	mcp::fs::background::cleanup().await?;

	Ok(())
}

/// Registers the signals that stop the HTTP server; the returned future resolves when one
/// fires. Unix also stops on SIGTERM because that is how a supervisor ends a process, and
/// the cleanup after `axum::serve` only runs when the signal is handled.
#[cfg(unix)]
fn http_shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
	use tokio::signal::unix::{signal, SignalKind};

	let mut interrupt = signal(SignalKind::interrupt()).context("failed to register SIGINT handler")?;
	let mut terminate = signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;
	Ok(async move {
		tokio::select! {
			_ = interrupt.recv() => {}
			_ = terminate.recv() => {}
		}
	})
}

#[cfg(not(unix))]
fn http_shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
	let mut ctrl_c = tokio::signal::windows::ctrl_c().context("failed to register Ctrl-C handler")?;
	Ok(async move {
		ctrl_c.recv().await;
	})
}

/// Run the MCP server over HTTP with Streamable HTTP transport.
///
/// On Unix, SIGINT and SIGTERM cancel the server token so `axum::serve`
/// returns and the same shell and background cleanup as STDIO can run.
async fn run_http_server(bind_addr: &str, auth_token: Option<&str>) -> Result<()> {
	use rmcp::transport::streamable_http_server::{
		session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
	};

	let addr = bind_addr
		.parse::<std::net::SocketAddr>()
		.map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind_addr, e))?;
	// Checked before anything listens: an unauthenticated non-loopback bind is remote shell access.
	http_auth::check_bind_policy(&addr, auth_token)?;

	let ct = tokio_util::sync::CancellationToken::new();

	// Each HTTP session gets a fresh server instance with isolated workdir state.
	// The factory closure creates a new OctofsServer for each session.
	let service = StreamableHttpService::new(
		|| Ok(mcp::server::OctofsServer::new()),
		LocalSessionManager::default().into(),
		http_auth::transport_config(
			StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
			auth_token,
		),
	);

	let mut router = axum::Router::new().nest_service("/mcp", service);
	if let Some(token) = auth_token {
		router = http_auth::with_bearer_auth(router, token);
	}

	// Registered before the port opens so a signal right after startup still reaches cleanup.
	let shutdown = http_shutdown_signal()?;
	let tcp_listener = tokio::net::TcpListener::bind(&addr)
		.await
		.map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

	tracing::info!(
		"MCP HTTP server listening on {} (bearer auth: {})",
		addr,
		if auth_token.is_some() {
			"on"
		} else {
			"off, loopback only"
		}
	);

	let _ = axum::serve(tcp_listener, router)
		.with_graceful_shutdown(async move {
			shutdown.await;
			ct.cancel();
		})
		.await;

	// Same orphan cleanup as STDIO shutdown — in-flight shell children must not survive.
	mcp::fs::shell::kill_all_shell_children();
	mcp::fs::background::cleanup().await?;

	Ok(())
}

fn init_mcp_logging() {
	// Parse RUST_LOG for a level override; default to WARN so debug! is a no-op.
	let level = std::env::var("RUST_LOG")
		.ok()
		.and_then(|v| v.parse::<Level>().ok())
		.unwrap_or(Level::WARN);

	tracing::subscriber::set_global_default(StderrSubscriber { level }).expect("failed to set tracing subscriber");
}

/// Minimal tracing subscriber — writes level + message to stderr.
/// Avoids the heavy tracing-subscriber crate (regex-automata, sharded-slab, etc.).
struct StderrSubscriber {
	level: Level,
}

impl tracing::Subscriber for StderrSubscriber {
	fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
		metadata.level() <= &self.level
	}

	fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
		tracing::span::Id::from_u64(1)
	}

	fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

	fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

	fn event(&self, event: &tracing::Event<'_>) {
		use std::fmt::Write;
		let mut msg = String::new();
		let _ = write!(msg, "[{}] ", event.metadata().level());
		event.record(&mut MessageVisitor(&mut msg));
		eprintln!("{}", msg);
	}

	fn enter(&self, _span: &tracing::span::Id) {}
	fn exit(&self, _span: &tracing::span::Id) {}
}

struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
	fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
		if field.name() == "message" {
			let _ = std::fmt::write(self.0, format_args!("{:?}", value));
		}
	}

	fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
		if field.name() == "message" {
			self.0.push_str(value);
		}
	}
}
