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
//! all other paths are [`PathSource::Local`]. The SFTP connection pool and remote
//! file operations are gated behind the `remote` feature flag.

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
			} => {
				format!("ssh://{user}@{host}:{port}{path}")
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
			} => {
				let parent = Path::new(path).parent()?;
				Some(PathSource::Remote {
					host: host.clone(),
					port: *port,
					user: user.clone(),
					path: parent.to_string_lossy().to_string(),
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
/// If no scheme is present, the path is treated as local and returned as-is
/// (the caller is responsible for resolving it against the workdir).
///
/// Defaults: port 22, user from `$USER` (or "root" if unset).
pub fn parse_path_source(path: &str) -> PathSource {
	let rest = path
		.strip_prefix("ssh://")
		.or_else(|| path.strip_prefix("sftp://"));

	let Some(rest) = rest else {
		return PathSource::Local(PathBuf::from(path));
	};

	// Split user from host part
	let (user, host_part) = match rest.find('@') {
		Some(idx) => (rest[..idx].to_string(), &rest[idx + 1..]),
		None => (
			std::env::var("USER").unwrap_or_else(|_| "root".to_string()),
			rest,
		),
	};

	// Split host:port from path (first '/' separates)
	let (host_port, remote_path) = match host_part.find('/') {
		Some(idx) => (&host_part[..idx], &host_part[idx..]),
		None => (host_part, "/"),
	};

	// Split host from port — handle IPv6 brackets
	let (host, port) = if let Some(inner) = host_port.strip_prefix('[') {
		match inner.find(']') {
			Some(idx) => {
				let addr = &inner[..idx];
				let port = inner[idx + 1..]
					.strip_prefix(':')
					.and_then(|s| s.parse::<u16>().ok())
					.unwrap_or(22);
				(addr.to_string(), port)
			}
			None => (host_port.to_string(), 22),
		}
	} else {
		match host_port.rfind(':') {
			Some(idx) => {
				let port = host_port[idx + 1..].parse::<u16>().unwrap_or(22);
				(host_port[..idx].to_string(), port)
			}
			None => (host_port.to_string(), 22),
		}
	};

	PathSource::Remote {
		host,
		port,
		user,
		path: remote_path.to_string(),
	}
}

/// Resolve a path string into a PathSource, resolving local paths against workdir.
/// Remote paths (ssh://, sftp://) are returned as-is; local relative paths are
/// resolved against the workdir (same as `core::resolve_path`).
///
/// The workdir itself may be a remote URL (`--path ssh://...`), carried as a
/// PathBuf — re-parse the joined result so relative paths under a remote
/// workdir resolve to Remote sources. Absolute local paths stay local; use a
/// full ssh:// URL to address a remote absolute path explicitly.
pub fn resolve_path_source(path_str: &str, workdir: &Path) -> PathSource {
	match parse_path_source(path_str) {
		PathSource::Local(p) if p.is_absolute() => PathSource::Local(p),
		PathSource::Local(p) => parse_path_source(&workdir.join(p).to_string_lossy()),
		remote => remote,
	}
}

// ── SFTP connection pool (feature-gated) ──────────────────────────────

#[cfg(feature = "remote")]
mod sftp {
	use std::collections::HashMap;
	use std::sync::Arc;
	use std::time::Duration;

	use anyhow::{anyhow, Result};
	use russh::client;
	use russh::keys::agent::client::AgentClient;
	use russh::keys::ssh_key;
	use russh::keys::PrivateKeyWithHashAlg;
	use russh_sftp::client::SftpSession;
	use tokio::sync::Mutex;

	/// SSH handler that accepts all server keys.
	///
	/// TODO: known_hosts verification (planned for CLI flags task).
	struct SshHandler;

	impl client::Handler for SshHandler {
		type Error = anyhow::Error;

		async fn check_server_key(
			&mut self,
			_server_public_key: &ssh_key::PublicKey,
		) -> Result<bool, Self::Error> {
			Ok(true)
		}
	}

	/// SSH connection configuration for the SFTP pool.
	#[derive(Clone)]
	pub struct SshConfig {
		pub key_path: Option<String>,
		pub password: Option<String>,
		pub timeout: Duration,
	}

	impl Default for SshConfig {
		fn default() -> Self {
			Self {
				key_path: None,
				password: None,
				timeout: Duration::from_secs(30),
			}
		}
	}

	/// SFTP connection pool caching sessions per (host, port, user).
	///
	/// Sessions are wrapped in `Arc<Mutex<SftpSession>>` so concurrent tool
	/// calls targeting the same host serialize on the same SSH channel.
	pub struct SftpPool {
		sessions: Mutex<HashMap<String, Arc<Mutex<SftpSession>>>>,
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
		pub async fn get_or_connect(
			&self,
			host: &str,
			port: u16,
			user: &str,
		) -> Result<Arc<Mutex<SftpSession>>> {
			let key = format!("{user}@{host}:{port}");
			{
				let sessions = self.sessions.lock().await;
				if let Some(session) = sessions.get(&key) {
					return Ok(session.clone());
				}
			}

			let session = self.connect_sftp(host, port, user).await?;
			let session = Arc::new(Mutex::new(session));

			let mut sessions = self.sessions.lock().await;
			sessions.insert(key, session.clone());
			Ok(session)
		}

		async fn connect_sftp(&self, host: &str, port: u16, user: &str) -> Result<SftpSession> {
			let config = client::Config {
				inactivity_timeout: Some(self.config.timeout),
				..Default::default()
			};

			let mut handle = tokio::time::timeout(
				self.config.timeout,
				client::connect(Arc::new(config), (host, port), SshHandler),
			)
			.await
			.map_err(|_| anyhow!("SSH connect to {host}:{port} timed out"))?
			.map_err(|e| anyhow!("SSH connect to {host}:{port} failed: {e}"))?;

			let authed = self.try_auth(&mut handle, user).await?;
			if !authed {
				anyhow::bail!("SSH authentication failed for {user}@{host}:{port}");
			}

			let channel = handle
				.channel_open_session()
				.await
				.map_err(|e| anyhow!("Failed to open SSH channel: {e}"))?;
			channel
				.request_subsystem(true, "sftp")
				.await
				.map_err(|e| anyhow!("Failed to request SFTP subsystem: {e}"))?;

			SftpSession::new(channel.into_stream())
				.await
				.map_err(|e| anyhow!("Failed to create SFTP session: {e}"))
		}

		/// Try authentication methods in order: SSH agent → key file → password.
		async fn try_auth(
			&self,
			handle: &mut client::Handle<SshHandler>,
			user: &str,
		) -> Result<bool> {
			// 1. SSH agent (SSH_AUTH_SOCK)
			if let Ok(mut agent) = AgentClient::connect_env().await {
				if let Ok(identities) = agent.request_identities().await {
					for identity in &identities {
						let pub_key = identity.public_key().into_owned();
						if let Ok(auth) = handle
							.authenticate_publickey_with(user, pub_key, None, &mut agent)
							.await
						{
							if auth.success() {
								return Ok(true);
							}
						}
					}
				}
			}

			// 2. Private key file
			if let Some(key_path) = &self.config.key_path {
				if let Ok(key) = russh::keys::load_secret_key(key_path, None) {
					let hash_alg = if matches!(key.algorithm(), ssh_key::Algorithm::Rsa { .. }) {
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
					if let Ok(auth) = handle.authenticate_publickey(user, key_with_hash).await {
						if auth.success() {
							return Ok(true);
						}
					}
				}
			}

			// 3. Password
			if let Some(password) = &self.config.password {
				if let Ok(auth) = handle.authenticate_password(user, password).await {
					if auth.success() {
						return Ok(true);
					}
				}
			}

			Ok(false)
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
	/// Get a fingerprint (mtime, size) for staleness detection.
	pub async fn remote_fingerprint(
		source: &super::PathSource,
	) -> Option<(std::time::SystemTime, u64)> {
		match remote_metadata(source).await {
			Ok(meta) => meta.modified.map(|m| (m, meta.size)),
			Err(_) => None,
		}
	}

	/// Extract (host, port, user, path) from a remote PathSource.
	fn remote_parts(source: &super::PathSource) -> (&str, u16, &str, &str) {
		match source {
			super::PathSource::Remote {
				host,
				port,
				user,
				path,
			} => (host, *port, user, path),
			_ => panic!("remote_parts called on non-remote PathSource"),
		}
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
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
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
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
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
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
		sftp.try_exists(path)
			.await
			.map_err(|e| anyhow!("SFTP exists check failed for {path}: {e}"))
	}

	/// Get metadata for a remote path.
	pub async fn remote_metadata(source: &super::PathSource) -> Result<RemoteMetadata> {
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
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
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
		let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
		let mut current = String::new();
		for part in parts {
			current = if current.is_empty() {
				format!("/{part}")
			} else {
				format!("{current}/{part}")
			};
			let _ = sftp.create_dir(&current).await;
		}
		Ok(())
	}

	/// Remove a remote file.
	pub async fn remote_remove_file(source: &super::PathSource) -> Result<()> {
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
		sftp.remove_file(path)
			.await
			.map_err(|e| anyhow!("SFTP remove failed for {path}: {e}"))
	}

	/// List a remote directory. Returns (name, metadata) for each entry, sorted by name.
	pub async fn remote_list_dir(
		source: &super::PathSource,
	) -> Result<Vec<(String, RemoteMetadata)>> {
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
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
		let (host, port, user, path) = remote_parts(source);
		let pool = sftp_pool();
		let session = pool.get_or_connect(host, port, user).await?;
		let sftp = session.lock().await;
		sftp.canonicalize(path)
			.await
			.map_err(|e| anyhow!("SFTP canonicalize failed for {path}: {e}"))
	}
}

#[cfg(feature = "remote")]
pub use sftp::{
	init_sftp_pool, remote_canonicalize, remote_create_dir_all, remote_exists, remote_fingerprint,
	remote_list_dir, remote_metadata, remote_read, remote_read_to_string, remote_remove_file,
	remote_write, sftp_pool, RemoteMetadata, SftpPool, SshConfig,
};

// ── Unified I/O dispatch layer ─────────────────────────────────────────
// These functions accept &PathSource and dispatch to tokio::fs for local
// paths or SFTP for remote paths. When the `remote` feature is disabled,
// remote paths return an error.

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
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_read_to_string(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Read a file as raw bytes.
pub async fn io_read(source: &PathSource) -> Result<Vec<u8>> {
	match source {
		PathSource::Local(p) => tokio::fs::read(p)
			.await
			.map_err(|e| anyhow!("Failed to read '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_read(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Write content to a file (creates or truncates).
pub async fn io_write(source: &PathSource, content: &[u8]) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::write(p, content)
			.await
			.map_err(|e| anyhow!("Failed to write '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_write(source, content).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Check if a path exists.
pub async fn io_exists(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.exists()),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_exists(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Check if a path is a directory.
pub async fn io_is_dir(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.is_dir()),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				Ok(sftp::remote_metadata(source).await?.is_dir)
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Check if a path is a regular file.
pub async fn io_is_file(source: &PathSource) -> Result<bool> {
	match source {
		PathSource::Local(p) => Ok(p.is_file()),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				Ok(sftp::remote_metadata(source).await?.is_file)
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
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
			#[cfg(feature = "remote")]
			{
				let m = sftp::remote_metadata(source).await?;
				Ok(IoMetadata {
					size: m.size,
					is_dir: m.is_dir,
					is_file: m.is_file,
					modified: m.modified,
				})
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Create directories recursively.
pub async fn io_create_dir_all(source: &PathSource) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::create_dir_all(p)
			.await
			.map_err(|e| anyhow!("Failed to create dirs for '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_create_dir_all(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Remove a file.
pub async fn io_remove_file(source: &PathSource) -> Result<()> {
	match source {
		PathSource::Local(p) => tokio::fs::remove_file(p)
			.await
			.map_err(|e| anyhow!("Failed to remove '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_remove_file(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// Get a fingerprint (mtime, size) for staleness detection.
pub async fn io_fingerprint(source: &PathSource) -> Option<(SystemTime, u64)> {
	match source {
		PathSource::Local(p) => {
			let meta = std::fs::metadata(p).ok()?;
			Some((meta.modified().ok()?, meta.len()))
		}
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_fingerprint(source).await
			}
			#[cfg(not(feature = "remote"))]
			{
				None
			}
		}
	}
}

/// Canonicalize a path. Returns the canonical path as a string.
pub async fn io_canonicalize(source: &PathSource) -> Result<PathBuf> {
	match source {
		PathSource::Local(p) => tokio::fs::canonicalize(p)
			.await
			.map_err(|e| anyhow!("Failed to canonicalize '{}': {}", source.display(), e)),
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
				sftp::remote_canonicalize(source).await.map(PathBuf::from)
			}
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

/// List a remote directory. Returns (name, metadata) for each entry, sorted by name.
pub async fn io_list_dir(source: &PathSource) -> Result<Vec<(String, IoMetadata)>> {
	match source {
		PathSource::Local(_) => {
			bail!("io_list_dir is for remote paths only; use directory::list_directory for local")
		}
		PathSource::Remote { .. } => {
			#[cfg(feature = "remote")]
			{
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
			#[cfg(not(feature = "remote"))]
			{
				bail!("Remote filesystem support is not enabled (rebuild with --features remote)")
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_local_absolute_path() {
		let result = parse_path_source("/home/user/file.txt");
		assert!(matches!(result, PathSource::Local(_)));
		assert_eq!(result.path_str(), "/home/user/file.txt");
	}

	#[test]
	fn test_local_relative_path() {
		let result = parse_path_source("src/main.rs");
		assert!(matches!(result, PathSource::Local(_)));
		assert_eq!(result.path_str(), "src/main.rs");
	}

	#[test]
	fn test_ssh_full_url() {
		let result = parse_path_source("ssh://user@host:2222/path/to/file.txt");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "host");
				assert_eq!(port, 2222);
				assert_eq!(user, "user");
				assert_eq!(path, "/path/to/file.txt");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_sftp_scheme() {
		let result = parse_path_source("sftp://admin@example.com/var/log/syslog");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "example.com");
				assert_eq!(port, 22);
				assert_eq!(user, "admin");
				assert_eq!(path, "/var/log/syslog");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_ssh_no_port() {
		let result = parse_path_source("ssh://user@host/path");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "host");
				assert_eq!(port, 22);
				assert_eq!(user, "user");
				assert_eq!(path, "/path");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_ssh_no_user() {
		let result = parse_path_source("ssh://host:2222/path");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "host");
				assert_eq!(port, 2222);
				// user defaults to $USER or "root"
				assert!(!user.is_empty());
				assert_eq!(path, "/path");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_ssh_no_path() {
		let result = parse_path_source("ssh://user@host:2222");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "host");
				assert_eq!(port, 2222);
				assert_eq!(user, "user");
				assert_eq!(path, "/");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_ssh_ipv6_with_port() {
		let result = parse_path_source("ssh://user@[::1]:2222/path");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "::1");
				assert_eq!(port, 2222);
				assert_eq!(user, "user");
				assert_eq!(path, "/path");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_ssh_ipv6_no_port() {
		let result = parse_path_source("ssh://user@[::1]/path");
		match result {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "::1");
				assert_eq!(port, 22);
				assert_eq!(user, "user");
				assert_eq!(path, "/path");
			}
			_ => panic!("expected Remote"),
		}
	}

	#[test]
	fn test_is_remote() {
		assert!(!parse_path_source("/local/path").is_remote());
		assert!(parse_path_source("ssh://host/path").is_remote());
		assert!(parse_path_source("sftp://host/path").is_remote());
	}

	#[test]
	fn test_lock_key_local() {
		let source = parse_path_source("/local/path");
		assert_eq!(source.lock_key(), "/local/path");
	}

	#[test]
	fn test_lock_key_remote() {
		let source = parse_path_source("ssh://user@host:2222/path");
		assert_eq!(source.lock_key(), "host:2222/path");
	}

	#[test]
	fn test_resolve_relative_under_remote_workdir() {
		let workdir = PathBuf::from("ssh://user@host:2222/root");
		match resolve_path_source("src/main.rs", &workdir) {
			PathSource::Remote {
				host,
				port,
				user,
				path,
			} => {
				assert_eq!(host, "host");
				assert_eq!(port, 2222);
				assert_eq!(user, "user");
				assert_eq!(path, "/root/src/main.rs");
			}
			_ => panic!("expected Remote"),
		}
		// A local workdir keeps relative paths local.
		assert!(!resolve_path_source("src/main.rs", Path::new("/tmp/w")).is_remote());
	}
}
