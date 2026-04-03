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

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "octofs")]
#[command(version, author = "Muvon Un Limited <contact@muvon.io>")]
#[command(about = "Standalone MCP filesystem tools server", long_about = None)]
pub struct Cli {
	#[command(subcommand)]
	pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
	/// Start MCP server exposing filesystem tools (view, text_editor, batch_edit, shell, ast_grep, workdir, extract_lines)
	Mcp {
		/// Working directory for filesystem operations (default: current directory)
		#[arg(long, value_name = "PATH")]
		path: Option<PathBuf>,

		/// Bind to HTTP server on host:port instead of using stdin/stdout (e.g., "0.0.0.0:12345")
		#[arg(long, value_name = "HOST:PORT")]
		bind: Option<String>,

		/// Line identifier mode: "number" (default) for sequential line numbers, "hash" for content-based 4-char hex hashes
		#[arg(long, value_name = "MODE", default_value = "number")]
		line_mode: String,
	},
}
