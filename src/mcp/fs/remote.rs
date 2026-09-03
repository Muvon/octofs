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

//! Remote filesystem support — SSH/SFTP path abstraction.
//!
//! Paths prefixed with `ssh://` or `sftp://` are parsed into [`PathSource::Remote`];
//! all other paths are [`PathSource::Local`]. Remote is a native capability —
//! always compiled in, exactly like local file access.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Whether a path targets the local filesystem or a remote SSH host.
#[derive(Debug, Clone)]
pub enum PathSource {
	/// A local filesystem path (already resolved against the workdir).
	Local(PathBuf),
	/// A remote path accessed via SFTP over SSH.
	Remote {
		host: String,
		port: u16,
		user: String,
		path: String,
		user_explicit: bool,
		port_explicit: bool,
	},
}

impl PathSource {
	/// Returns the path component as a string slice.
	pub fn path_str(&self) -> &str {
		match self {
			PathSource::Local(p) => p.to_str().unwrap_or(""),
			PathSource::Remote { path, .. } => path,
		}
	}

	/// True if this is a remote path.
	pub fn is_remote(&self) -> bool {
		matches!(self, PathSource::Remote { .. })
	}

	/// Path as sent over SFTP. `/~` (URL form `ssh://host/~/x`) is the login
	/// home: SFTP resolves relative paths against it, so `/~` becomes `.` and
	/// `/~/x` becomes `x`. Absolute paths pass through unchanged.
	pub fn sftp_path(&self) -> &str {
		let path = self.path_str();
		let tilde = path.strip_prefix('/').unwrap_or(path);
		match tilde {
			"~" | "~/" => ".",
			_ => tilde.strip_prefix("~/").unwrap_or(path),
		}
	}

	/// Returns a lock key unique per host+path to avoid cross-host collisions.
	pub fn lock_key(&self) -> String {
		match self {
			PathSource::Local(p) => p.to_string_lossy().to_string(),
			PathSource::Remote {
				host, port, path, ..
			} => {
				format!("{host}:{port}{path}")
			}
		}
	}

	/// Human-readable path for error messages.
	pub fn display(&self) -> String {
		match self {
			PathSource::Local(p) => p.display().to_string(),
			PathSource::Remote {
				host,
				port,
				user,
				path,
				user_explicit,
				port_explicit,
			} => {
				let user = user_explicit
					.then(|| format!("{user}@"))
					.unwrap_or_default();
				let host = if host.contains(':') {
					format!("[{host}]")
				} else {
					host.clone()
				};
				let port = port_explicit
					.then(|| format!(":{port}"))
					.unwrap_or_default();
				format!("ssh://{user}{host}{port}{path}")
			}
		}
	}

	/// Returns the local Path if this is a local source.
	pub fn as_local_path(&self) -> Option<&Path> {
		match self {
			PathSource::Local(p) => Some(p),
			PathSource::Remote { .. } => None,
		}
	}

	/// Returns the parent directory as a PathSource, if any.
	pub fn parent(&self) -> Option<PathSource> {
		match self {
			PathSource::Local(p) => p.parent().map(|pp| PathSource::Local(pp.to_path_buf())),
			PathSource::Remote {
				host,
				port,
				user,
				path,
				user_explicit,
				port_explicit,
			} => {
				let parent = Path::new(path).parent()?;
				Some(PathSource::Remote {
					host: host.clone(),
					port: *port,
					user: user.clone(),
					path: parent.to_string_lossy().to_string(),
					user_explicit: *user_explicit,
					port_explicit: *port_explicit,
				})
			}
		}
	}
}

impl From<&Path> for PathSource {
	fn from(p: &Path) -> Self {
		PathSource::Local(p.to_path_buf())
	}
}

impl From<&PathBuf> for PathSource {
	fn from(p: &PathBuf) -> Self {
		PathSource::Local(p.clone())
	}
}

/// Parse a path string into a [`PathSource`].
///
/// Recognized remote schemes:
/// - `ssh://user@host:port/path`
/// - `sftp://user@host:port/path`
///
/// `host` may be an OpenSSH config alias. A path of `/~` or `/~/x` is relative
/// to the login home (see [`PathSource::sftp_path`]); no path at all means `/~`,
/// matching `ssh host` / `scp host:`.
///
/// If no scheme is present, the path is treated as local and returned as-is
/// (the caller is responsible for resolving it against the workdir).
///
/// Defaults: port 22, user from `$USER` (or "root" if unset); both are
/// overridden by `~/.ssh/config` when not explicit in the URL.
pub fn parse_path_source(path: &str) -> PathSource {
	let rest = path
		.strip_prefix("ssh://")
		.or_else(|| path.strip_prefix("sftp://"));

	let Some(rest) = rest else {
		return PathSource::Local(PathBuf::from(path));
	};

	// Split user from host part
	let (user, host_part, user_explicit) = match rest.find('@') {
		Some(idx) => (rest[..idx].to_string(), &rest[idx + 1..], true),
		None => (
			std::env::var("USER").unwrap_or_else(|_| "root".to_string()),
			rest,
			false,
		),
	};

	// Split host:port from path (first '/' separates)
	let (host_port, remote_path) = match host_part.find('/') {
		Some(idx) => (&host_part[..idx], &host_part[idx..]),
		None => (host_part, "/~"),
	};

	// Split host from port — handle IPv6 brackets
	let (host, port, port_explicit) = if let Some(inner) = host_port.strip_prefix('[') {
		match inner.find(']') {
			Some(idx) => {
				let addr = &inner[..idx];
				let explicit = inner[idx + 1..].strip_prefix(':');
				let port = explicit.and_then(|s| s.parse::<u16>().ok()).unwrap_or(22);
				(addr.to_string(), port, explicit.is_some())
			}
			None => (host_port.to_string(), 22, false),
		}
	} else {
		match host_port.rfind(':') {
			Some(idx) => {
				let port = host_port[idx + 1..].parse::<u16>().unwrap_or(22);
				(host_port[..idx].to_string(), port, true)
			}
			None => (host_port.to_string(), 22, false),
		}
	};

	PathSource::Remote {
		host,
		port,
		user,
		path: remote_path.to_string(),
		user_explicit,
		port_explicit,
	}
}

/// Resolve a path string into a PathSource, resolving local paths against workdir.
/// Remote paths (ssh://, sftp://) are returned as-is; local relative paths are
/// resolved against the workdir (same as `core::resolve_path`).
///
/// The workdir itself may be a remote URL (`--path ssh://...`), carried as a
/// PathBuf — relative paths under a remote workdir are joined with '/' manually
/// (PathBuf::join would insert '\' on Windows and corrupt the URL) and resolve
/// to Remote sources. Absolute local paths stay local; use a
/// full ssh:// URL to address a remote absolute path explicitly.
pub fn resolve_path_source(path_str: &str, workdir: &Path) -> PathSource {
	match parse_path_source(path_str) {
		PathSource::Local(p) if p.is_absolute() => PathSource::Local(p),
		PathSource::Local(p) => match parse_path_source(&workdir.to_string_lossy()) {
			PathSource::Remote {
				host,
				port,
				user,
				path,
				user_explicit,
				port_explicit,
			} => {
				let rel = path_str.replace('\\', "/");
				let joined = if path.ends_with('/') {
					format!("{path}{rel}")
				} else {
					format!("{path}/{rel}")
				};
				PathSource::Remote {
					host,
					port,
					user,
					path: joined,
					user_explicit,
					port_explicit,
				}
			}
			_ => PathSource::Local(workdir.join(p)),
		},
		remote => remote,
	}
}

// ── SFTP connection pool ──────────────────────────────────────────────

mod sftp {
	use std::collections::HashMap;
	use std::sync::Arc;
	use std::time::Duration;

	use anyhow::{anyhow, Result};
	use russh::client;
	use russh::keys::agent::client::AgentClient;
	use russh::keys::ssh_key;
	use russh::keys::PrivateKeyWithHashAlg;
	use russh::keys::PublicKeyOrCertificate;
	use russh_sftp::client::SftpSession;
	use tokio::sync::Mutex;

	/// SSH handler that verifies the server's host key against `~/.ssh/known_hosts`.
	///
	/// Policy matches OpenSSH `StrictHostKeyChecking=accept-new` — the only sane
	/// non-interactive default for an MCP server (no way to prompt the user):
	/// known+matching → accept; unknown → record and accept (trust on first use);
	/// mismatch or any check error → fail closed.
	struct SshHandler {
		host: String,
		port: u16,
	}

	impl client::Handler for SshHandler {
		type Error = anyhow::Error;

		async fn check_server_key(
			&mut self,
			server_public_key: &PublicKeyOrCertificate,
		) -> Result<bool, Self::Error> {
			// A certificate is trusted via its certified key: russh verifies the
			// certificate signature before this call, and known_hosts stores plain keys.
			let server_key = server_public_key.public_key();
			match russh::keys::check_known_hosts(&self.host, self.port, &server_key) {
				Ok(true) => Ok(true),
				Ok(false) => {
					russh::keys::known_hosts::learn_known_hosts(
						&self.host,
						self.port,
					&server_key,
					)
					.map_err(|e| {
						anyhow!(
							"Failed to record host key for {}:{} in known_hosts: {e}",
							self.host,
							self.port
						)
					})?;
					Ok(true)
				}
				Err(e) => Err(anyhow!(
					"Host key verification failed for {}:{}: {e}. The presented key does not match ~/.ssh/known_hosts — possible man-in-the-middle attack. If the host key legitimately changed, remove the old entry (`ssh-keygen -R '{}'`) and reconnect.",
					self.host,
					self.port,
					self.host
				)),
			}
		}
	}

	/// Connection and authentication settings read from `~/.ssh/config`.
	#[derive(Clone, Default)]
	pub(super) struct SshHostConfig {
		pub(super) host_name: Option<String>,
		pub(super) user: Option<String>,
		pub(super) port: Option<u16>,
		pub(super) proxy_jump: Option<String>,
		pub(super) proxy_command: Option<String>,
		pub(super) identity_agent: Option<String>,
		pub(super) identity_files: Vec<String>,
	}

	pub(super) struct ResolvedSshTarget {
		alias: String,
		pub(super) host: String,
		pub(super) port: u16,
		pub(super) user: String,
		config: SshHostConfig,
	}

	fn home_dir() -> Option<std::path::PathBuf> {
		std::env::var_os("HOME")
			.or_else(|| std::env::var_os("USERPROFILE"))
			.map(std::path::PathBuf::from)
	}

	fn expand_tilde(value: &str, home: &std::path::Path) -> String {
		match value.strip_prefix("~/") {
			Some(rest) => home.join(rest).to_string_lossy().to_string(),
			None => value.to_string(),
		}
	}

	/// Minimal `*`/`?` glob for OpenSSH Host patterns (host names are short,
	/// so the naive recursion is fine).
	fn glob_match(pat: &str, text: &str) -> bool {
		fn rec(p: &[u8], t: &[u8]) -> bool {
			match p.first() {
				None => t.is_empty(),
				Some(b'*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
				Some(b'?') => !t.is_empty() && rec(&p[1..], &t[1..]),
				Some(c) => t.first() == Some(c) && rec(&p[1..], &t[1..]),
			}
		}
		rec(pat.as_bytes(), text.as_bytes())
	}

	/// OpenSSH `Host` line: space-separated patterns, `!` negates, matching
	/// is case-insensitive.
	fn host_line_matches(patterns: &str, host: &str) -> bool {
		let host = host.to_ascii_lowercase();
		let mut matched = false;
		for pat in patterns.split_whitespace() {
			let pat = pat.trim_matches('"').to_ascii_lowercase();
			if let Some(neg) = pat.strip_prefix('!') {
				if glob_match(neg, &host) {
					return false;
				}
			} else if glob_match(&pat, &host) {
				matched = true;
			}
		}
		matched
	}

	/// Parse OpenSSH config text for `host`. Scalar options use OpenSSH's
	/// first-obtained-value rule; IdentityFile entries accumulate.
	pub(super) fn parse_ssh_host_config(
		text: &str,
		host: &str,
		home: &std::path::Path,
	) -> SshHostConfig {
		let mut cfg = SshHostConfig::default();
		// Options before the first Host line apply to every host.
		let mut applies = true;
		for line in text.lines() {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				continue;
			}
			let Some((keyword, value)) = line.split_once(|c: char| c.is_whitespace() || c == '=')
			else {
				continue;
			};
			let value = value.trim().trim_matches('"');
			match keyword.to_ascii_lowercase().as_str() {
				"host" => applies = host_line_matches(value, host),
				"match" => applies = false,
				"hostname" if applies && cfg.host_name.is_none() => {
					cfg.host_name = Some(value.to_string());
				}
				"user" if applies && cfg.user.is_none() => {
					cfg.user = Some(value.to_string());
				}
				"port" if applies && cfg.port.is_none() => {
					cfg.port = value.parse::<u16>().ok();
				}
				"proxyjump" if applies && cfg.proxy_jump.is_none() => {
					cfg.proxy_jump = Some(value.to_string());
				}
				"proxycommand" if applies && cfg.proxy_command.is_none() => {
					cfg.proxy_command = Some(value.to_string());
				}
				// `none` disables the agent; SSH_AUTH_SOCK means the env
				// default, which is our fallback anyway.
				"identityagent"
					if applies
						&& cfg.identity_agent.is_none()
						&& !value.eq_ignore_ascii_case("none")
						&& !value.eq_ignore_ascii_case("SSH_AUTH_SOCK") =>
				{
					cfg.identity_agent = Some(expand_tilde(value, home));
				}
				"identityfile" if applies => {
					cfg.identity_files.push(expand_tilde(value, home));
				}
				_ => {}
			}
		}
		cfg
	}

	async fn ssh_host_config(host: &str) -> SshHostConfig {
		let Some(home) = home_dir() else {
			return SshHostConfig::default();
		};
		// Let OpenSSH flatten Include, Match, token expansion, and platform-specific
		// config locations. `-G` only prints configuration; it never connects.
		let resolution = tokio::process::Command::new("ssh")
			.args(["-G", "--", host])
			.output();
		if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(5), resolution).await {
			if output.status.success() {
				if let Ok(text) = String::from_utf8(output.stdout) {
					return parse_ssh_host_config(&text, host, &home);
				}
			}
		}

		// Keep native direct-host support when an OpenSSH client is unavailable.
		let Ok(text) = tokio::fs::read_to_string(home.join(".ssh").join("config")).await else {
			return SshHostConfig::default();
		};
		parse_ssh_host_config(&text, host, &home)
	}

	async fn resolve_ssh_target(
		host: &str,
		port: u16,
		user: &str,
		user_explicit: bool,
		port_explicit: bool,
	) -> ResolvedSshTarget {
		let config = ssh_host_config(host).await;
		apply_ssh_host_config(host, port, user, user_explicit, port_explicit, config)
	}

	pub(super) fn apply_ssh_host_config(
		host: &str,
		port: u16,
		user: &str,
		user_explicit: bool,
		port_explicit: bool,
		config: SshHostConfig,
	) -> ResolvedSshTarget {
		ResolvedSshTarget {
			alias: host.to_string(),
			host: config.host_name.clone().unwrap_or_else(|| host.to_string()),
			port: if port_explicit {
				port
			} else {
				config.port.unwrap_or(port)
			},
			user: if user_explicit {
				user.to_string()
			} else {
				config.user.clone().unwrap_or_else(|| user.to_string())
			},
			config,
		}
	}

	pub(super) fn parse_jump_target(value: &str) -> Result<(&str, u16, &str, bool, bool)> {
		if value.contains(',') {
			return Err(anyhow!(
				"Multiple ProxyJump hops are not supported; configure a single jump host"
			));
		}
		let value = value.trim();
		let (user, host_port, user_explicit) = match value.rsplit_once('@') {
			Some((user, host)) => (user, host, true),
			None => ("", value, false),
		};
		let (host, port, port_explicit) = if let Some(inner) = host_port.strip_prefix('[') {
			let end = inner
				.find(']')
				.ok_or_else(|| anyhow!("Invalid bracketed IPv6 ProxyJump host: {value}"))?;
			let suffix = &inner[end + 1..];
			match suffix.strip_prefix(':') {
				Some(port) => (&inner[..end], port.parse::<u16>()?, true),
				None if suffix.is_empty() => (&inner[..end], 22, false),
				None => return Err(anyhow!("Invalid ProxyJump host: {value}")),
			}
		} else {
			match host_port.rsplit_once(':') {
				Some((host, port)) => (host, port.parse::<u16>()?, true),
				None => (host_port, 22, false),
			}
		};
		Ok((host, port, user, user_explicit, port_explicit))
	}

	/// SSH connection configuration for the SFTP pool.
	#[derive(Clone)]
	pub struct SshConfig {
		pub key_path: Option<String>,
		pub timeout: Duration,
	}

	impl Default for SshConfig {
		fn default() -> Self {
			Self {
				key_path: None,
				timeout: Duration::from_secs(30),
			}
		}
	}

	/// Transport keepalive frequency; a dead peer is detected after
	/// `keepalive_max` (russh default: 3) missed replies.
	const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

	/// A pooled connection: the transport handle is kept alongside the SFTP
	/// session so liveness can be checked without a network round trip.
	struct PooledSession {
		handle: client::Handle<SshHandler>,
		// A target reached through ProxyJump depends on the jump transport.
		proxy_handle: Option<client::Handle<SshHandler>>,
		sftp: Arc<SftpSession>,
	}

	/// SFTP connection pool caching sessions per (host, port, user).
	///
	/// Sessions are shared as `Arc<SftpSession>`: SFTP multiplexes requests by
	/// id and every session method takes `&self`, so concurrent tool calls to
	/// the same host pipeline on one channel instead of serializing.
	pub struct SftpPool {
		sessions: Mutex<HashMap<String, PooledSession>>,
		config: SshConfig,
	}

	impl SftpPool {
		pub fn new(config: SshConfig) -> Self {
			Self {
				sessions: Mutex::new(HashMap::new()),
				config,
			}
		}

		/// Get a cached SFTP session or establish a new connection.
		/// Sessions whose SSH transport has died (network drop, server restart,
		/// keepalive_max exceeded) are evicted and replaced — without this a
		/// cached dead session would fail every call to that host forever.
		///
		/// The pool lock is held across connection setup on purpose: concurrent
		/// calls to the same host would otherwise each open a connection and
		/// each trigger an agent approval prompt (e.g. 1Password).
		pub async fn get_or_connect(
			&self,
			host: &str,
			port: u16,
			user: &str,
			user_explicit: bool,
			port_explicit: bool,
		) -> Result<Arc<SftpSession>> {
			let key = format!(
				"{user}@{host}:{port}:user-explicit={user_explicit}:port-explicit={port_explicit}"
			);
			let mut sessions = self.sessions.lock().await;
			match sessions.get(&key) {
				Some(pooled)
					if !pooled.handle.is_closed()
						&& pooled
							.proxy_handle
							.as_ref()
							.is_none_or(|handle| !handle.is_closed()) =>
				{
					return Ok(pooled.sftp.clone());
				}
				Some(_) => {
					sessions.remove(&key);
				}
				None => {}
			}

			let target = resolve_ssh_target(host, port, user, user_explicit, port_explicit).await;
			let (handle, proxy_handle, session) = self.connect_sftp(&target).await?;
			let sftp = Arc::new(session);
			sessions.insert(
				key,
				PooledSession {
					handle,
					proxy_handle,
					sftp: sftp.clone(),
				},
			);
			Ok(sftp)
		}

		async fn connect_sftp(
			&self,
			target: &ResolvedSshTarget,
		) -> Result<(
			client::Handle<SshHandler>,
			Option<client::Handle<SshHandler>>,
			SftpSession,
		)> {
			// Keepalives keep idle NAT/firewall paths open and let russh detect a
			// dead peer (the transport closes after keepalive_max missed replies,
			// which get_or_connect turns into a reconnect). config.timeout bounds
			// the WHOLE session setup below — TCP connect, auth (including a
			// possibly-pending agent approval prompt), and the SFTP handshake.
			// Transport-route setup and target auth/SFTP setup each get this bound;
			// idle pooled sessions do not. Without bounded auth, an unanswered agent
			// prompt hangs the tool call forever.
			let config = client::Config {
				keepalive_interval: Some(KEEPALIVE_INTERVAL),
				..Default::default()
			};

			let config = Arc::new(config);
			let (mut handle, proxy_handle) =
				tokio::time::timeout(self.config.timeout, self.connect_transport(target, config))
					.await
					.map_err(|_| {
						anyhow!(
							"SSH transport setup for {} ({}:{}) timed out after {}s while connecting or authenticating the configured route. Check ProxyJump reachability and approve any SSH-agent prompt; --ssh-timeout raises the limit",
							target.alias,
							target.host,
							target.port,
							self.config.timeout.as_secs()
						)
					})??;
			tracing::debug!("ssh: connected, starting auth");

			let setup = async {
				self.try_auth(&mut handle, &target.config, &target.user)
					.await
					.map_err(|e| {
						anyhow!(
							"SSH authentication failed for {}@{} ({}:{}): {e}",
							target.user,
							target.alias,
							target.host,
							target.port
						)
					})?;
				tracing::debug!("ssh: auth ok, opening channel");

				let channel = handle
					.channel_open_session()
					.await
					.map_err(|e| anyhow!("Failed to open SSH channel: {e}"))?;
				tracing::debug!("ssh: channel open, requesting sftp subsystem");
				channel
					.request_subsystem(true, "sftp")
					.await
					.map_err(|e| anyhow!("Failed to request SFTP subsystem: {e}"))?;
				tracing::debug!("ssh: sftp subsystem up, starting sftp handshake");

				SftpSession::new(channel.into_stream())
					.await
					.map_err(|e| anyhow!("Failed to create SFTP session: {e}"))
			};

			// On timeout or failure, disconnect explicitly. Abandoned half-open
			// connections (e.g. an agent holding a signature request) otherwise
			// pile up server-side until sshd starts dropping ALL new connections
			// from this address.
			match tokio::time::timeout(self.config.timeout, setup).await {
				Ok(Ok(session)) => {
					tracing::debug!(
						"ssh: sftp session ready for {}:{}",
						target.host,
						target.port
					);
					Ok((handle, proxy_handle, session))
				}
				Ok(Err(e)) => {
					let _ = handle
						.disconnect(russh::Disconnect::ByApplication, "", "en")
						.await;
					Err(e)
				}
				Err(_) => {
					let _ = handle
						.disconnect(russh::Disconnect::ByApplication, "", "en")
						.await;
					Err(anyhow!(
						"SSH session setup for {}:{} timed out after {}s. Most likely your SSH agent (e.g. 1Password) is holding the signature request until you authorize octofs: unlock the agent app, look for its authorization prompt, approve it, and retry — later calls are instant. --ssh-timeout raises the limit",
						target.host,
						target.port,
						self.config.timeout.as_secs()
					))
				}
			}
		}

		async fn connect_transport(
			&self,
			target: &ResolvedSshTarget,
			config: Arc<client::Config>,
		) -> Result<(
			client::Handle<SshHandler>,
			Option<client::Handle<SshHandler>>,
		)> {
			if target
				.config
				.proxy_command
				.as_deref()
				.is_some_and(|value| !value.eq_ignore_ascii_case("none"))
			{
				return Err(anyhow!(
					"SSH alias '{}' uses ProxyCommand, which octofs does not support; use a direct host or a single ProxyJump",
					target.alias
				));
			}
			let proxy = target
				.config
				.proxy_jump
				.as_deref()
				.filter(|value| !value.eq_ignore_ascii_case("none"));
			let Some(proxy) = proxy else {
				tracing::debug!("ssh: connecting to {}:{}", target.host, target.port);
				let handle = client::connect(
					config,
					(target.host.as_str(), target.port),
					SshHandler {
						host: target.host.clone(),
						port: target.port,
					},
				)
				.await
				.map_err(|e| {
					anyhow!(
						"SSH connect to {} ({}:{}) failed: {e}",
						target.alias,
						target.host,
						target.port
					)
				})?;
				return Ok((handle, None));
			};

			let (jump_host, jump_port, jump_user, user_explicit, port_explicit) =
				parse_jump_target(proxy)?;
			let default_user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
			let jump_user = if user_explicit {
				jump_user
			} else {
				&default_user
			};
			let jump = resolve_ssh_target(
				jump_host,
				jump_port,
				jump_user,
				user_explicit,
				port_explicit,
			)
			.await;
			if jump
				.config
				.proxy_jump
				.as_deref()
				.is_some_and(|value| !value.eq_ignore_ascii_case("none"))
			{
				return Err(anyhow!(
					"Nested ProxyJump on jump host '{}' is not supported",
					jump.alias
				));
			}

			tracing::debug!(
				"ssh: connecting to jump host {} ({}:{})",
				jump.alias,
				jump.host,
				jump.port
			);
			let mut jump_handle = client::connect(
				config.clone(),
				(jump.host.as_str(), jump.port),
				SshHandler {
					host: jump.host.clone(),
					port: jump.port,
				},
			)
			.await
			.map_err(|e| {
				anyhow!(
					"SSH connect to jump host {} ({}:{}) failed: {e}",
					jump.alias,
					jump.host,
					jump.port
				)
			})?;
			self.try_auth(&mut jump_handle, &jump.config, &jump.user)
				.await
				.map_err(|e| {
					anyhow!(
						"SSH authentication failed for jump host {}@{}: {e}",
						jump.user,
						jump.alias
					)
				})?;
			let channel = jump_handle
				.channel_open_direct_tcpip(&target.host, u32::from(target.port), "127.0.0.1", 0)
				.await
				.map_err(|e| {
					anyhow!(
						"ProxyJump '{}' could not reach {}:{}: {e}",
						jump.alias,
						target.host,
						target.port
					)
				})?;
			let handle = client::connect_stream(
				config,
				channel.into_stream(),
				SshHandler {
					host: target.host.clone(),
					port: target.port,
				},
			)
			.await
			.map_err(|e| {
				anyhow!(
					"SSH connect to {} through ProxyJump '{}' failed: {e}",
					target.alias,
					jump.alias
				)
			})?;
			Ok((handle, Some(jump_handle)))
		}

		/// Authenticate automatically, the way OpenSSH does. Agent first — the
		/// one `~/.ssh/config` names for this host via `IdentityAgent` (e.g.
		/// the 1Password agent), else `$SSH_AUTH_SOCK`. Then key files:
		/// `--ssh-key` if given, the host's `IdentityFile` entries, then the
		/// default identities in `~/.ssh`. If plain `ssh host` works in a
		/// terminal, this works with zero configuration.
		/// On failure the error lists why each method didn't work — a swallowed
		/// key-load error (wrong path, passphrase-protected key) is otherwise
		/// indistinguishable from a server-side rejection.
		async fn try_auth(
			&self,
			handle: &mut client::Handle<SshHandler>,
			host_cfg: &SshHostConfig,
			user: &str,
		) -> Result<()> {
			let mut reasons: Vec<String> = Vec::new();

			// 1. SSH agent
			#[cfg(unix)]
			let (agent_result, agent_label) = match &host_cfg.identity_agent {
				Some(sock) => (
					AgentClient::connect_uds(sock).await,
					format!("agent '{sock}'"),
				),
				None => (AgentClient::connect_env().await, "ssh-agent".to_string()),
			};
			// Windows has no unix sockets (russh's connect_env is unix-only):
			// the OpenSSH agent listens on a named pipe. IdentityAgent from
			// ssh config overrides the standard pipe path.
			#[cfg(not(unix))]
			let (agent_result, agent_label) = {
				let pipe = host_cfg
					.identity_agent
					.clone()
					.unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".to_string());
				(
					AgentClient::connect_named_pipe(&pipe).await,
					format!("agent '{pipe}'"),
				)
			};

			match agent_result {
				Ok(mut agent) => match agent.request_identities().await {
					Ok(identities) => {
						tracing::debug!(
							"ssh: {agent_label}: {} identities offered",
							identities.len()
						);
						if identities.is_empty() {
							reasons.push(format!("{agent_label} has no identities loaded"));
						}
						for identity in &identities {
							let pub_key = identity.public_key().into_owned();
							tracing::debug!(
								"ssh: trying agent identity {} via {agent_label}",
								pub_key.algorithm()
							);
							match handle
								.authenticate_publickey_with(user, pub_key, None, &mut agent)
								.await
							{
								Ok(auth) if auth.success() => {
									tracing::debug!("ssh: agent identity accepted");
									return Ok(());
								}
								Ok(auth) => {
									tracing::debug!("ssh: agent identity not accepted: {auth:?}")
								}
								Err(e) => {
									tracing::debug!("ssh: agent auth attempt errored: {e}")
								}
							}
						}
						if !identities.is_empty() {
							reasons.push(format!(
								"{agent_label}: none of {} identities accepted by server",
								identities.len()
							));
						}
					}
					Err(e) => {
						reasons.push(format!("{agent_label}: failed to list identities: {e}"))
					}
				},
				Err(e) => reasons.push(format!("{agent_label} unavailable: {e}")),
			}

			// 2. Key files: --ssh-key override, then ssh-config IdentityFile
			// entries for this host, then OpenSSH default identities.
			let mut candidates: Vec<String> = Vec::new();
			if let Some(p) = &self.config.key_path {
				candidates.push(p.clone());
			}
			candidates.extend(host_cfg.identity_files.iter().cloned());
			if let Some(home) = home_dir() {
				for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
					let p = home.join(".ssh").join(name);
					if p.exists() {
						candidates.push(p.to_string_lossy().to_string());
					}
				}
			}
			let mut key_paths: Vec<String> = Vec::new();
			for k in candidates {
				if !key_paths.contains(&k) {
					key_paths.push(k);
				}
			}
			if key_paths.is_empty() {
				reasons.push(
					"no key files: --ssh-key not set, no IdentityFile in ~/.ssh/config and ~/.ssh has no id_ed25519, id_ecdsa or id_rsa"
						.to_string(),
				);
			}

			for key_path in &key_paths {
				tracing::debug!("ssh: trying key file '{key_path}'");
				match russh::keys::load_secret_key(key_path, None) {
					Ok(key) => {
						let hash_alg =
							if matches!(key.algorithm(), ssh_key::Algorithm::Rsa { .. }) {
								handle
									.best_supported_rsa_hash()
									.await
									.ok()
									.flatten()
									.flatten()
							} else {
								None
							};
						let key_with_hash = PrivateKeyWithHashAlg::new(key.into(), hash_alg);
						match handle.authenticate_publickey(user, key_with_hash).await {
							Ok(auth) if auth.success() => return Ok(()),
							Ok(_) => {
								reasons.push(format!("key '{key_path}' rejected by server"))
							}
							Err(e) => reasons.push(format!("key '{key_path}': {e}")),
						}
					}
					Err(e) => reasons.push(format!(
						"failed to load key '{key_path}': {e} (passphrase-protected keys are not supported — add the key to ssh-agent instead)"
					)),
				}
			}

			Err(anyhow!("{}", reasons.join("; ")))
		}
	}

	/// Global SFTP pool singleton.
	static SFTP_POOL: std::sync::OnceLock<SftpPool> = std::sync::OnceLock::new();

	/// Initialize the global SFTP pool with the given SSH config.
	/// Called once at startup when the `remote` feature is enabled.
	pub fn init_sftp_pool(config: SshConfig) {
		let _ = SFTP_POOL.set(SftpPool::new(config));
	}

	/// Get a reference to the global SFTP pool.
	/// Panics if `init_sftp_pool` was not called — it should be called at startup.
	pub fn sftp_pool() -> &'static SftpPool {
		SFTP_POOL
			.get()
			.expect("SFTP pool not initialized — call init_sftp_pool at startup")
	}
	/// Extract connection and path fields from a remote PathSource.
	fn remote_parts(source: &super::PathSource) -> (&str, u16, &str, &str, bool, bool) {
		match source {
			super::PathSource::Remote {
				host,
				port,
				user,
				path,
				user_explicit,
				port_explicit,
			} => (host, *port, user, path, *user_explicit, *port_explicit),
			_ => panic!("remote_parts called on non-remote PathSource"),
		}
	}

	async fn remote_session(source: &super::PathSource) -> Result<Arc<SftpSession>> {
		let (host, port, user, _, user_explicit, port_explicit) = remote_parts(source);
		sftp_pool()
			.get_or_connect(host, port, user, user_explicit, port_explicit)
			.await
	}

	/// Remote file metadata mirroring fields we need from std::fs::Metadata.
	#[derive(Debug, Clone)]
	pub struct RemoteMetadata {
		pub size: u64,
		pub is_dir: bool,
		pub is_file: bool,
		pub modified: Option<std::time::SystemTime>,
	}

	/// Read a remote file as bytes.
	pub async fn remote_read(source: &super::PathSource) -> Result<Vec<u8>> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		sftp.read(path)
			.await
			.map_err(|e| anyhow!("SFTP read failed for {path}: {e}"))
	}

	/// Read a remote file as a string. Strict UTF-8, matching local
	/// `read_to_string` — a lossy read here would let an edit write
	/// replacement characters back and silently corrupt the file.
	pub async fn remote_read_to_string(source: &super::PathSource) -> Result<String> {
		let bytes = remote_read(source).await?;
		String::from_utf8(bytes)
			.map_err(|_| anyhow!("{}: stream did not contain valid UTF-8", source.display()))
	}

	/// Write content to a remote file (creates or truncates).
	pub async fn remote_write(source: &super::PathSource, content: &[u8]) -> Result<()> {
		use tokio::io::AsyncWriteExt;
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		let mut file = sftp
			.create(path)
			.await
			.map_err(|e| anyhow!("SFTP create failed for {path}: {e}"))?;
		file.write_all(content)
			.await
			.map_err(|e| anyhow!("SFTP write failed for {path}: {e}"))?;
		file.flush()
			.await
			.map_err(|e| anyhow!("SFTP flush failed for {path}: {e}"))
	}

	/// Check if a remote path exists.
	pub async fn remote_exists(source: &super::PathSource) -> Result<bool> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		sftp.try_exists(path)
			.await
			.map_err(|e| anyhow!("SFTP exists check failed for {path}: {e}"))
	}

	/// Get metadata for a remote path.
	pub async fn remote_metadata(source: &super::PathSource) -> Result<RemoteMetadata> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		let attrs = sftp
			.metadata(path)
			.await
			.map_err(|e| anyhow!("SFTP metadata failed for {path}: {e}"))?;
		Ok(RemoteMetadata {
			size: attrs.len(),
			is_dir: attrs.is_dir(),
			is_file: attrs.is_regular(),
			modified: attrs.modified().ok(),
		})
	}

	/// Create directories recursively (SFTP create_dir only creates a single dir).
	pub async fn remote_create_dir_all(source: &super::PathSource) -> Result<()> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		// Home-relative paths (`/~/x`) must stay relative so SFTP resolves them
		// against the login home.
		let mut current = String::new();
		for part in path.split('/').filter(|s| !s.is_empty()) {
			current = match (current.is_empty(), path.starts_with('/')) {
				(true, true) => format!("/{part}"),
				(true, false) => part.to_string(),
				(false, _) => format!("{current}/{part}"),
			};
			let _ = sftp.create_dir(&current).await;
		}
		Ok(())
	}

	/// Remove a remote file.
	pub async fn remote_remove_file(source: &super::PathSource) -> Result<()> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		sftp.remove_file(path)
			.await
			.map_err(|e| anyhow!("SFTP remove failed for {path}: {e}"))
	}

	/// List a remote directory. Returns (name, metadata) for each entry, sorted by name.
	pub async fn remote_list_dir(
		source: &super::PathSource,
	) -> Result<Vec<(String, RemoteMetadata)>> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		let read_dir = sftp
			.read_dir(path)
			.await
			.map_err(|e| anyhow!("SFTP read_dir failed for {path}: {e}"))?;
		let mut entries = Vec::new();
		for entry in read_dir {
			let name = entry.file_name();
			let meta = entry.metadata();
			entries.push((
				name,
				RemoteMetadata {
					size: meta.len(),
					is_dir: meta.is_dir(),
					is_file: meta.is_regular(),
					modified: meta.modified().ok(),
				},
			));
		}
		entries.sort_by(|a, b| a.0.cmp(&b.0));
		Ok(entries)
	}

	/// Canonicalize a remote path.
	pub async fn remote_canonicalize(source: &super::PathSource) -> Result<String> {
		let path = source.sftp_path();
		let sftp = remote_session(source).await?;
		sftp.canonicalize(path)
			.await
			.map_err(|e| anyhow!("SFTP canonicalize failed for {path}: {e}"))
	}
}

pub use sftp::{
	init_sftp_pool, remote_canonicalize, remote_create_dir_all, remote_exists, remote_list_dir,
	remote_metadata, remote_read, remote_read_to_string, remote_remove_file, remote_write,
	sftp_pool, RemoteMetadata, SftpPool, SshConfig,
};

// ── Unified I/O dispatch layer ─────────────────────────────────────────
// These functions accept &PathSource and dispatch to tokio::fs for local
// paths or SFTP for remote paths.

/// Unified metadata mirroring the fields we need from std::fs::Metadata.
#[derive(Debug, Clone)]
pub struct IoMetadata {
	pub size: u64,
	pub is_dir: bool,
	pub is_file: bool,
	pub modified: Option<SystemTime>,
}

/// Read a file as a string (strict UTF-8 for both local and remote).
pub async fn io_read_to_string(source: &PathSource) -> Result<String> {
	match source {
		PathSource::Local(p) => tokio::fs::read_to_string(p)
			.await
			.map_err(|e| anyhow!("Failed to read '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_read_to_string(source).await,
	}
}

/// Read a file as raw bytes.
pub async fn io_read(source: &PathSource) -> Result<Vec<u8>> {
	match source {
		PathSource::Local(p) => tokio::fs::read(p)
			.await
			.map_err(|e| anyhow!("Failed to read '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_read(source).await,
	}
}

/// Write content to a file (creates or truncates).
pub async fn io_write(source: &PathSource, content: &[u8]) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::write(p, content)
			.await
			.map_err(|e| anyhow!("Failed to write '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_write(source, content).await,
	}
}

/// Check if a path exists.
pub async fn io_exists(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.exists()),
		PathSource::Remote { .. } => sftp::remote_exists(source).await,
	}
}

/// Check if a path is a directory.
pub async fn io_is_dir(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.is_dir()),
		PathSource::Remote { .. } => Ok(sftp::remote_metadata(source).await?.is_dir),
	}
}

/// Check if a path is a regular file.
pub async fn io_is_file(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.is_file()),
		PathSource::Remote { .. } => Ok(sftp::remote_metadata(source).await?.is_file),
	}
}

/// Get metadata for a path.
pub async fn io_metadata(source: &PathSource) -> Result<IoMetadata> {
	match source {
		PathSource::Local(p) => {
			let meta = tokio::fs::metadata(p)
				.await
				.map_err(|e| anyhow!("Failed to stat '{}': {}", source.display(), e))?;
			Ok(IoMetadata {
				size: meta.len(),
				is_dir: meta.is_dir(),
				is_file: meta.is_file(),
				modified: meta.modified().ok(),
			})
		}
		PathSource::Remote { .. } => {
			let m = sftp::remote_metadata(source).await?;
			Ok(IoMetadata {
				size: m.size,
				is_dir: m.is_dir,
				is_file: m.is_file,
				modified: m.modified,
			})
		}
	}
}

/// Create directories recursively.
pub async fn io_create_dir_all(source: &PathSource) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::create_dir_all(p)
			.await
			.map_err(|e| anyhow!("Failed to create dirs for '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_create_dir_all(source).await,
	}
}

/// Remove a file.
pub async fn io_remove_file(source: &PathSource) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::remove_file(p)
			.await
			.map_err(|e| anyhow!("Failed to remove '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_remove_file(source).await,
	}
}

/// Canonicalize a path. Returns the canonical path as a string.
pub async fn io_canonicalize(source: &PathSource) -> Result<PathBuf> {
	match source {
		PathSource::Local(p) => tokio::fs::canonicalize(p)
			.await
			.map_err(|e| anyhow!("Failed to canonicalize '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => sftp::remote_canonicalize(source).await.map(PathBuf::from),
	}
}

/// List a remote directory. Returns (name, metadata) for each entry, sorted by name.
pub async fn io_list_dir(source: &PathSource) -> Result<Vec<(String, IoMetadata)>> {
	match source {
		PathSource::Local(_) => {
			bail!("io_list_dir is for remote paths only; use directory::list_directory for local")
		}
		PathSource::Remote { .. } => {
			let entries = sftp::remote_list_dir(source).await?;
			Ok(entries
				.into_iter()
				.map(|(name, m)| {
					(
						name,
						IoMetadata {
							size: m.size,
							is_dir: m.is_dir,
							is_file: m.is_file,
							modified: m.modified,
						},
					)
				})
				.collect())
		}
	}
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod remote_tests;
