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

use anyhow::Result;
use clap::Parser;
use tracing::Level;

mod cli;
pub mod mcp;
pub mod utils;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = Cli::parse();

	match cli.command {
		Commands::Mcp {
			path,
			bind,
			line_mode,
		} => {
			// Resolve working directory
			let working_directory = if let Some(p) = path {
				p.canonicalize().unwrap_or(p)
			} else {
				std::env::current_dir()?
			};

			// Stderr-only logging — no stdout (would corrupt stdio MCP protocol)
			init_mcp_logging();

			// Set line identifier mode
			let mode = match line_mode.as_str() {
				"hash" => utils::line_hash::LineMode::Hash,
				_ => utils::line_hash::LineMode::Number,
			};
			utils::line_hash::set_line_mode(mode);

			// Set the session root directory for all tool calls
			mcp::set_session_root_directory(working_directory.clone());

			// Create the MCP server
			let server = mcp::server::OctofsServer::new();

			match bind {
				Some(addr) => {
					// HTTP mode with Streamable HTTP transport
					run_http_server(&addr).await?;
				}
				None => {
					// STDIO mode (default)
					run_stdio_server(server).await?;
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
		let mut sigterm =
			signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
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

	Ok(())
}

/// Run the MCP server over HTTP with Streamable HTTP transport
async fn run_http_server(bind_addr: &str) -> Result<()> {
	use rmcp::transport::streamable_http_server::{
		session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
	};

	let ct = tokio_util::sync::CancellationToken::new();

	// Each HTTP session gets a fresh server instance with isolated workdir state.
	// The factory closure creates a new OctofsServer for each session.
	let service = StreamableHttpService::new(
		|| Ok(mcp::server::OctofsServer::new()),
		LocalSessionManager::default().into(),
		StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
	);

	let router = axum::Router::new().nest_service("/mcp", service);
	let addr = bind_addr
		.parse::<std::net::SocketAddr>()
		.map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind_addr, e))?;

	let tcp_listener = tokio::net::TcpListener::bind(&addr)
		.await
		.map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

	tracing::info!("MCP HTTP server listening on {}", addr);

	let _ = axum::serve(tcp_listener, router)
		.with_graceful_shutdown(async move {
			tokio::signal::ctrl_c().await.unwrap();
			ct.cancel();
		})
		.await;

	Ok(())
}

fn init_mcp_logging() {
	// Parse RUST_LOG for a level override; default to WARN so debug! is a no-op.
	let level = std::env::var("RUST_LOG")
		.ok()
		.and_then(|v| v.parse::<Level>().ok())
		.unwrap_or(Level::WARN);

	tracing::subscriber::set_global_default(StderrSubscriber { level })
		.expect("failed to set tracing subscriber");
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
