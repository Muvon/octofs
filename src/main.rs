// Copyright 2025 Muvon Un Limited
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
		Commands::Mcp { path, bind } => {
			// Resolve working directory
			let working_directory = if let Some(p) = path {
				p.canonicalize().unwrap_or(p)
			} else {
				std::env::current_dir()?
			};

			// Stderr-only logging — no stdout (would corrupt stdio MCP protocol)
			init_mcp_logging();

			let server = mcp::server::McpServer::new(working_directory);
			match bind {
				Some(addr) => server.run_http(&addr).await?,
				None => server.run().await?,
			}
		}
	}

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
