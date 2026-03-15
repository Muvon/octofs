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

mod cli;
pub mod mcp;
pub mod utils;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
	dotenvy::dotenv().ok();

	let cli = Cli::parse();

	match cli.command {
		Commands::Mcp { path, bind } => {
			// Resolve working directory
			let working_directory = if let Some(p) = path {
				p.canonicalize().unwrap_or(p)
			} else {
				std::env::current_dir()?
			};

			// File-only logging for MCP server — no console output (would corrupt stdio protocol)
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
	use tracing_subscriber::EnvFilter;
	let filter =
		EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("octofs=warn"));
	tracing_subscriber::fmt()
		.with_env_filter(filter)
		.with_writer(std::io::stderr)
		.with_target(false)
		.init();
}
