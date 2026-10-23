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

//! Tests for HTTP transport access control: bind policy, token resolution, bearer check.

use super::*;
use axum::routing::get;
use serial_test::serial;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn loopback_bind_without_token_is_allowed() {
	for addr in ["127.0.0.1:12345", "[::1]:12345"] {
		let addr: SocketAddr = addr.parse().unwrap();
		assert!(check_bind_policy(&addr, None).is_ok(), "{addr}");
	}
}

#[test]
fn non_loopback_bind_without_token_is_refused() {
	for addr in ["0.0.0.0:12345", "[::]:12345", "192.168.1.5:12345"] {
		let addr: SocketAddr = addr.parse().unwrap();
		let err = check_bind_policy(&addr, None).unwrap_err().to_string();
		assert!(err.contains("--auth-token"), "{addr}: {err}");
	}
}

#[test]
fn non_loopback_bind_with_token_is_allowed() {
	let addr: SocketAddr = "0.0.0.0:12345".parse().unwrap();
	assert!(check_bind_policy(&addr, Some("s3cret-token")).is_ok());
}

#[test]
#[serial]
fn cli_token_wins_and_blank_tokens_count_as_unset() {
	std::env::set_var(AUTH_TOKEN_ENV, "from-env");
	assert_eq!(
		resolve_auth_token(Some("from-cli".to_string())).as_deref(),
		Some("from-cli")
	);
	assert_eq!(resolve_auth_token(None).as_deref(), Some("from-env"));
	std::env::set_var(AUTH_TOKEN_ENV, "   ");
	assert_eq!(resolve_auth_token(None), None);
	std::env::remove_var(AUTH_TOKEN_ENV);
	assert_eq!(resolve_auth_token(None), None);
	assert_eq!(resolve_auth_token(Some(String::new())), None);
}

#[test]
fn bearer_matches_only_the_exact_token() {
	let token = "s3cret-token";
	assert!(bearer_matches(
		Some(&HeaderValue::from_static("Bearer s3cret-token")),
		token
	));
	assert!(bearer_matches(
		Some(&HeaderValue::from_static("bearer s3cret-token")),
		token
	));
	for bad in [
		"Bearer s3cret-tokeX",
		"Bearer s3cret",
		"Bearer s3cret-token-longer",
		"Basic s3cret-token",
		"s3cret-token",
		"Bearer ",
	] {
		assert!(!bearer_matches(Some(&HeaderValue::from_static(bad)), token), "{bad}");
	}
	assert!(!bearer_matches(None, token));
}

#[test]
fn token_lifts_the_loopback_host_allowlist_and_tokenless_keeps_it() {
	let with_token = transport_config(StreamableHttpServerConfig::default(), Some("s3cret-token"));
	assert!(with_token.allowed_hosts.is_empty());
	let tokenless = transport_config(StreamableHttpServerConfig::default(), None);
	assert!(tokenless.allowed_hosts.iter().any(|host| host == "127.0.0.1"));
}

// First line of the raw HTTP/1.1 response, e.g. "HTTP/1.1 401 Unauthorized".
async fn status_line(addr: SocketAddr, authorization: Option<&str>) -> String {
	let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
	let auth = authorization
		.map(|value| format!("Authorization: {value}\r\n"))
		.unwrap_or_default();
	let request = format!("GET / HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
	stream.write_all(request.as_bytes()).await.unwrap();
	let mut response = String::new();
	stream.read_to_string(&mut response).await.unwrap();
	response.lines().next().unwrap_or_default().to_string()
}

#[tokio::test]
async fn bearer_layer_rejects_requests_without_the_token() {
	let router = with_bearer_auth(axum::Router::new().route("/", get(|| async { "ok" })), "s3cret-token");
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

	assert!(status_line(addr, None).await.contains(" 401 "));
	assert!(status_line(addr, Some("Bearer wrong")).await.contains(" 401 "));
	assert!(status_line(addr, Some("Bearer s3cret-token")).await.contains(" 200 "));
}
