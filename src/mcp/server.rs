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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{stdin, stdout, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::mcp::{self, McpFunction, McpToolCall};

// ── JSON-RPC types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
	#[serde(rename = "jsonrpc")]
	pub _jsonrpc: String,
	pub id: Option<Value>,
	pub method: String,
	pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
	pub jsonrpc: String,
	pub id: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub result: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
	pub code: i32,
	pub message: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub data: Option<Value>,
}

// ── Tool definitions ──────────────────────────────────────────────────────────

fn get_tool_definitions() -> Vec<Value> {
	crate::mcp::fs::get_all_functions()
		.into_iter()
		.map(|f: McpFunction| {
			json!({
				"name": f.name,
				"description": f.description,
				"inputSchema": f.parameters
			})
		})
		.collect()
}

// ── Server ────────────────────────────────────────────────────────────────────

pub struct McpServer {
	working_directory: PathBuf,
}

impl McpServer {
	pub fn new(working_directory: PathBuf) -> Self {
		// Set the session working directory for all tool calls
		mcp::set_session_working_directory(working_directory.clone());
		Self { working_directory }
	}

	/// Run the MCP server on stdio (default mode).
	///
	/// Exits cleanly on: EOF on stdin (parent closed pipe), or SIGTERM
	/// (parent requests graceful shutdown). Both paths drop the tokio
	/// runtime normally, which triggers `kill_on_drop` on any in-flight
	/// shell children — ensuring no orphan processes.
	pub async fn run(&self) -> Result<()> {
		let stdin = stdin();
		let mut stdout = stdout();
		let mut reader = BufReader::new(stdin);
		let mut line = String::new();

		// Listen for SIGTERM so the MCP client can ask us to shut down
		// gracefully (giving kill_on_drop a chance to fire) before
		// resorting to SIGKILL.
		#[cfg(unix)]
		let mut sigterm = {
			use tokio::signal::unix::{signal, SignalKind};
			signal(SignalKind::terminate()).expect("failed to register SIGTERM handler")
		};

		loop {
			line.clear();

			// Race: next JSON-RPC line vs SIGTERM
			let bytes_read;
			#[cfg(unix)]
			{
				tokio::select! {
					result = reader.read_line(&mut line) => {
						bytes_read = result?;
					}
					_ = sigterm.recv() => {
						tracing::debug!("SIGTERM received, shutting down gracefully");
						break;
					}
				}
			}
			#[cfg(not(unix))]
			{
				bytes_read = reader.read_line(&mut line).await?;
			}

			if bytes_read == 0 {
				tracing::debug!("EOF received, shutting down");
				break;
			}

			let trimmed = line.trim();
			if trimmed.is_empty() {
				continue;
			}

			tracing::debug!("Received: {}", trimmed);

			let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
				Ok(req) => req,
				Err(e) => {
					let resp = JsonRpcResponse {
						jsonrpc: "2.0".to_string(),
						id: None,
						result: None,
						error: Some(JsonRpcError {
							code: -32700,
							message: format!("Parse error: {}", e),
							data: None,
						}),
					};
					let json = serde_json::to_string(&resp)?;
					stdout.write_all(json.as_bytes()).await?;
					stdout.write_all(b"\n").await?;
					stdout.flush().await?;
					continue;
				}
			};

			if let Some(response) = self.handle_request(request).await {
				let json = serde_json::to_string(&response)?;
				stdout.write_all(json.as_bytes()).await?;
				stdout.write_all(b"\n").await?;
				stdout.flush().await?;
			}
		}

		// Kill all in-flight shell children's process groups (including
		// grandchildren) so nothing survives as an orphan after we exit.
		crate::mcp::fs::shell::kill_all_shell_children();

		Ok(())
	}

	/// Run the MCP server over HTTP.
	pub async fn run_http(&self, bind_addr: &str) -> Result<()> {
		let addr = bind_addr
			.parse::<std::net::SocketAddr>()
			.map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind_addr, e))?;

		let listener = TcpListener::bind(&addr)
			.await
			.map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

		tracing::debug!("MCP HTTP server listening on {}", addr);

		loop {
			match listener.accept().await {
				Ok((stream, _peer)) => {
					let working_dir = self.working_directory.clone();
					tokio::spawn(async move {
						if let Err(e) = handle_http_connection(stream, working_dir).await {
							tracing::debug!("HTTP connection error: {}", e);
						}
					});
				}
				Err(e) => {
					tracing::debug!("Accept error: {}", e);
				}
			}
		}
	}

	async fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
		let id = request.id.clone();
		let has_id = id.is_some();

		match request.method.as_str() {
			"initialize" => Some(JsonRpcResponse {
				jsonrpc: "2.0".to_string(),
				id,
				result: Some(json!({
					"protocolVersion": "2024-11-05",
					"capabilities": {
						"tools": { "listChanged": false }
					},
					"serverInfo": {
						"name": "octofs",
						"version": env!("CARGO_PKG_VERSION"),
						"description": "Standalone MCP filesystem tools server"
					},
					"instructions": "This server provides filesystem tools: view (read files/dirs), text_editor (create/str_replace/undo), batch_edit (multi-op line edits), extract_lines (copy lines between files), shell (execute commands), ast_grep (AST-aware code search/refactor), workdir (get/set working directory)."
				})),
				error: None,
			}),

			"tools/list" => Some(JsonRpcResponse {
				jsonrpc: "2.0".to_string(),
				id,
				result: Some(json!({ "tools": get_tool_definitions() })),
				error: None,
			}),

			"tools/call" => {
				let params = request.params.unwrap_or(json!({}));
				let tool_name = params
					.get("name")
					.and_then(|v| v.as_str())
					.unwrap_or("")
					.to_string();
				let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

				let mut call = McpToolCall {
					tool_name: tool_name.clone(),
					parameters: arguments,
					tool_id: crate::mcp::next_tool_id(),
				};

				let result = self.dispatch_tool(&mut call).await;

				match result {
					Ok(tool_result) => {
						// Drain any misuse hints and append as a note
						let hints = crate::mcp::hint_accumulator::drain_hints();
						let mut content = tool_result.result.clone();
						if !hints.is_empty() {
							// Append hints to the content array
							if let Some(arr) =
								content.get_mut("content").and_then(|c| c.as_array_mut())
							{
								for hint in &hints {
									arr.push(json!({ "type": "text", "text": hint }));
								}
							}
						}
						Some(JsonRpcResponse {
							jsonrpc: "2.0".to_string(),
							id,
							result: Some(content),
							error: None,
						})
					}
					Err(e) => Some(JsonRpcResponse {
						jsonrpc: "2.0".to_string(),
						id,
						result: None,
						error: Some(JsonRpcError {
							code: -32603,
							message: format!("Internal error: {}", e),
							data: None,
						}),
					}),
				}
			}

			_ => {
				if !has_id {
					None // notification — no response
				} else {
					Some(JsonRpcResponse {
						jsonrpc: "2.0".to_string(),
						id,
						result: None,
						error: Some(JsonRpcError {
							code: -32601,
							message: format!("Method not found: {}", request.method),
							data: None,
						}),
					})
				}
			}
		}
	}

	async fn dispatch_tool(&self, call: &mut McpToolCall) -> Result<crate::mcp::McpToolResult> {
		use crate::mcp::fs;

		let result = match call.tool_name.as_str() {
			"view" => fs::execute_view(call).await,
			"text_editor" => fs::execute_text_editor(call).await,
			"batch_edit" => fs::execute_batch_edit(call).await,
			"extract_lines" => fs::execute_extract_lines(call).await,
			"shell" => fs::execute_shell_command(call).await,
			"ast_grep" => fs::execute_ast_grep_command(call).await,
			"workdir" => fs::execute_workdir_command(call).await,
			_ => {
				return Ok(crate::mcp::McpToolResult::error(
					call.tool_name.clone(),
					call.tool_id.clone(),
					format!("Unknown tool: {}", call.tool_name),
				));
			}
		};

		match result {
			Ok(mut r) => {
				r.tool_id = call.tool_id.clone();
				Ok(r)
			}
			Err(e) => Ok(crate::mcp::McpToolResult::error(
				call.tool_name.clone(),
				call.tool_id.clone(),
				format!("Tool execution failed: {}", e),
			)),
		}
	}
}

// ── HTTP transport ────────────────────────────────────────────────────────────

async fn handle_http_connection(
	mut stream: tokio::net::TcpStream,
	working_directory: PathBuf,
) -> Result<()> {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let mut buf = vec![0u8; 65536];
	let n = stream.read(&mut buf).await?;
	let raw = String::from_utf8_lossy(&buf[..n]);

	// Extract JSON body from HTTP request (after double CRLF)
	let body = if let Some(pos) = raw.find("\r\n\r\n") {
		raw[pos + 4..].trim().to_string()
	} else {
		raw.trim().to_string()
	};

	if body.is_empty() {
		let response = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
		stream.write_all(response.as_bytes()).await?;
		return Ok(());
	}

	// Re-use the same server logic
	let server = McpServer::new(working_directory);
	let request: JsonRpcRequest = match serde_json::from_str(&body) {
		Ok(r) => r,
		Err(e) => {
			let err_body = serde_json::to_string(&json!({
				"jsonrpc": "2.0",
				"id": null,
				"error": { "code": -32700, "message": format!("Parse error: {}", e) }
			}))?;
			let http = format!(
				"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
				err_body.len(),
				err_body
			);
			stream.write_all(http.as_bytes()).await?;
			return Ok(());
		}
	};

	let response_json = if let Some(resp) = server.handle_request(request).await {
		serde_json::to_string(&resp)?
	} else {
		return Ok(());
	};

	let http = format!(
		"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
		response_json.len(),
		response_json
	);
	stream.write_all(http.as_bytes()).await?;
	Ok(())
}
