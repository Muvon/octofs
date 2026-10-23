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

//! Access control for the HTTP transport. `--bind` exposes every tool, `shell` included, to
//! whoever reaches the port, so a non-loopback bind must carry a bearer token, and a token-less
//! loopback server refuses browser-origin requests so a visited web page cannot drive it.

use anyhow::{bail, Result};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;

/// Read when `--auth-token` is absent; keeps the secret out of `ps` output.
pub const AUTH_TOKEN_ENV: &str = "OCTOFS_AUTH_TOKEN";

/// The token from the CLI, else from the environment. Blank values count as unset, so an empty
/// `OCTOFS_AUTH_TOKEN=` can never pass as a credential.
pub fn resolve_auth_token(cli_token: Option<String>) -> Option<String> {
	cli_token
		.or_else(|| std::env::var(AUTH_TOKEN_ENV).ok())
		.map(|token| token.trim().to_string())
		.filter(|token| !token.is_empty())
}

/// Refuse a non-loopback bind without a token: unauthenticated, that is remote code execution
/// through `shell` for anyone who can reach the address.
pub fn check_bind_policy(addr: &SocketAddr, token: Option<&str>) -> Result<()> {
	if token.is_none() && !addr.ip().is_loopback() {
		bail!(
			"Refusing to serve HTTP on {addr} without authentication: every tool, shell included, would be open to \
			 anyone who can reach that address. Pass --auth-token <TOKEN> (or set {AUTH_TOKEN_ENV}), or bind to \
			 127.0.0.1."
		);
	}
	Ok(())
}

/// Without a token the only guard is "loopback only": keep rmcp's loopback Host allowlist (DNS
/// rebinding) and reject any request that carries an Origin, which browsers always send on
/// cross-origin calls and MCP clients don't. With a token the bearer check is the boundary, and
/// the loopback Host allowlist would only turn away honest non-loopback clients.
pub fn transport_config(config: StreamableHttpServerConfig, token: Option<&str>) -> StreamableHttpServerConfig {
	match token {
		Some(_) => config.disable_allowed_hosts(),
		None => config.enforce_origin_validation(),
	}
}

/// True when `value` is `Bearer <token>`. The scheme is case-insensitive (RFC 7235); the token
/// comparison is constant-time so response timing doesn't reveal how much of a guess matched.
pub fn bearer_matches(value: Option<&HeaderValue>, token: &str) -> bool {
	let Some(value) = value.and_then(|v| v.to_str().ok()) else {
		return false;
	};
	let Some((scheme, presented)) = value.split_once(' ') else {
		return false;
	};
	scheme.eq_ignore_ascii_case("bearer") && constant_time_eq(presented.trim().as_bytes(), token.as_bytes())
}

// Token length is not secret; only the content comparison must not short-circuit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_bearer(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
	if bearer_matches(request.headers().get(header::AUTHORIZATION), &token) {
		return next.run(request).await;
	}
	(
		StatusCode::UNAUTHORIZED,
		[(header::WWW_AUTHENTICATE, "Bearer")],
		"Unauthorized",
	)
		.into_response()
}

/// Put every route of `router` behind the bearer-token check.
pub fn with_bearer_auth(router: axum::Router, token: &str) -> axum::Router {
	router.layer(axum::middleware::from_fn_with_state(
		Arc::<str>::from(token),
		require_bearer,
	))
}

#[cfg(test)]
#[path = "http_auth_tests.rs"]
mod http_auth_tests;
