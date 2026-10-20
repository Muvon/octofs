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

use super::sftp::{apply_ssh_host_config, parse_jump_target, parse_ssh_host_config, SshHostConfig};
use super::*;

#[test]
fn parses_connection_and_authentication_options() {
	let home = std::path::Path::new("/home/me");
	let text = r#"
Host *
  IdentityAgent ~/.ssh/agent.sock
  Port 2200
Host dev
  HostName 192.0.2.10
  User box
  Port 2222
  ProxyJump jump
  IdentityFile ~/.ssh/dev_ed25519
"#;
	let config = parse_ssh_host_config(text, "dev", home);
	let expected_agent = home.join(".ssh/agent.sock").to_string_lossy().into_owned();
	let expected_identity = home.join(".ssh/dev_ed25519").to_string_lossy().into_owned();

	assert_eq!(config.host_name.as_deref(), Some("192.0.2.10"));
	assert_eq!(config.user.as_deref(), Some("box"));
	// OpenSSH uses the first value obtained, so Host * wins here.
	assert_eq!(config.port, Some(2200));
	assert_eq!(config.proxy_jump.as_deref(), Some("jump"));
	assert_eq!(config.identity_agent.as_deref(), Some(expected_agent.as_str()));
	assert_eq!(config.identity_files, vec![expected_identity]);
}

#[test]
fn explicit_url_user_and_port_override_ssh_config() {
	let config = SshHostConfig {
		host_name: Some("192.0.2.10".to_string()),
		user: Some("configured-user".to_string()),
		port: Some(2200),
		..Default::default()
	};
	let target = apply_ssh_host_config("dev", 2222, "url-user", true, true, config);

	assert_eq!(target.host, "192.0.2.10");
	assert_eq!(target.user, "url-user");
	assert_eq!(target.port, 2222);
}

#[test]
fn parses_proxy_jump_forms() {
	assert_eq!(parse_jump_target("jump").unwrap(), ("jump", 22, "", false, false));
	assert_eq!(
		parse_jump_target("user@jump:2222").unwrap(),
		("jump", 2222, "user", true, true)
	);
	assert_eq!(
		parse_jump_target("user@[2001:db8::1]:2222").unwrap(),
		("2001:db8::1", 2222, "user", true, true)
	);
	assert!(parse_jump_target("one,two").is_err());
}

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
			host, port, user, path, ..
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
			host, port, user, path, ..
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
			host, port, user, path, ..
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
			host, port, user, path, ..
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
			host, port, user, path, ..
		} => {
			assert_eq!(host, "host");
			assert_eq!(port, 2222);
			assert_eq!(user, "user");
			assert_eq!(path, "/~");
		}
		_ => panic!("expected Remote"),
	}
	assert_eq!(parse_path_source("ssh://dev").display(), "ssh://dev/~");
}

#[test]
fn test_sftp_path_home_relative() {
	let sftp = |url: &str| parse_path_source(url).sftp_path().to_string();
	assert_eq!(sftp("ssh://dev"), ".");
	assert_eq!(sftp("ssh://dev/~"), ".");
	assert_eq!(sftp("ssh://dev/~/"), ".");
	assert_eq!(sftp("ssh://dev/~/src/main.rs"), "src/main.rs");
	assert_eq!(sftp("ssh://dev/"), "/");
	assert_eq!(sftp("ssh://dev/etc/hosts"), "/etc/hosts");
	assert_eq!(sftp("ssh://dev/~box/x"), "/~box/x");
	let workdir = PathBuf::from("ssh://dev");
	assert_eq!(resolve_path_source("src/main.rs", &workdir).sftp_path(), "src/main.rs");
}

#[test]
fn test_ssh_ipv6_with_port() {
	let result = parse_path_source("ssh://user@[::1]:2222/path");
	match result {
		PathSource::Remote {
			host, port, user, path, ..
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
			host, port, user, path, ..
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
fn test_remote_display_preserves_explicit_authority_parts() {
	assert_eq!(parse_path_source("ssh://dev/path").display(), "ssh://dev/path");
	assert_eq!(parse_path_source("ssh://box@dev/path").display(), "ssh://box@dev/path");
	assert_eq!(
		parse_path_source("ssh://box@dev:2222/path").display(),
		"ssh://box@dev:2222/path"
	);
	assert_eq!(
		parse_path_source("ssh://[2001:db8::1]/path").display(),
		"ssh://[2001:db8::1]/path"
	);
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
			host, port, user, path, ..
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

#[test]
fn test_as_local_path() {
	let local = parse_path_source("/local/path");
	assert_eq!(local.as_local_path(), Some(Path::new("/local/path")));
	let remote = parse_path_source("ssh://host/path");
	assert_eq!(remote.as_local_path(), None);
}

#[test]
fn test_parent_local_and_remote() {
	// Local parent of a nested path.
	let local = parse_path_source("/local/dir/file.txt");
	match local.parent() {
		Some(PathSource::Local(p)) => assert_eq!(p, Path::new("/local/dir")),
		other => panic!("expected Local parent, got {other:?}"),
	}
	// Root has no parent.
	assert!(parse_path_source("/").parent().is_none());

	// Remote parent keeps authority parts and strips the last component.
	let remote = parse_path_source("ssh://user@host:2222/dir/file.txt");
	match remote.parent() {
		Some(PathSource::Remote {
			host, port, user, path, ..
		}) => {
			assert_eq!(host, "host");
			assert_eq!(port, 2222);
			assert_eq!(user, "user");
			assert_eq!(path, "/dir");
		}
		other => panic!("expected Remote parent, got {other:?}"),
	}
	// Remote root has no parent.
	assert!(parse_path_source("ssh://host/").parent().is_none());
}

#[test]
fn test_sftp_path_home_relative_and_absolute() {
	// /~ and /~/x resolve relative to the login home.
	assert_eq!(parse_path_source("ssh://host/~").sftp_path(), ".");
	assert_eq!(parse_path_source("ssh://host/~/sub").sftp_path(), "sub");
	// Absolute paths pass through unchanged.
	assert_eq!(parse_path_source("ssh://host/etc").sftp_path(), "/etc");
}

#[test]
fn jump_target_rejects_malformed_hosts_and_ports() {
	// Unclosed IPv6 bracket.
	assert!(parse_jump_target("user@[2001:db8::1:2222").is_err());
	// Garbage after the closing bracket.
	assert!(parse_jump_target("[2001:db8::1]x").is_err());
	// Non-numeric ports.
	assert!(parse_jump_target("jump:notaport").is_err());
	assert!(parse_jump_target("user@[2001:db8::1]:nope").is_err());
	// Whitespace is trimmed before parsing.
	assert_eq!(parse_jump_target("  jump  ").unwrap(), ("jump", 22, "", false, false));
}

#[test]
fn ssh_config_host_patterns_match_with_negation_and_globs() {
	// Glob patterns, case-insensitivity and `!` negation.
	let text = r#"
Host *.example.com !bad.example.com
  User star
Host other
  User other
"#;
	let home = std::path::Path::new("/home/me");
	assert_eq!(
		parse_ssh_host_config(text, "foo.example.com", home).user.as_deref(),
		Some("star")
	);
	// A negated pattern excludes even a host another pattern matches.
	assert_eq!(parse_ssh_host_config(text, "bad.example.com", home).user, None);
	// `Match` blocks stop option application until the next Host line.
	let text = "Match all\n  User matched\nHost other\n  User other\n";
	assert_eq!(parse_ssh_host_config(text, "anyhost", home).user, None);
	// IdentityAgent `none` and `SSH_AUTH_SOCK` keep the env default.
	let text = "Host *\n  IdentityAgent none\n";
	assert_eq!(parse_ssh_host_config(text, "h", home).identity_agent, None);
	// ProxyCommand is recorded for the transport check.
	let text = "Host via\n  ProxyCommand nc %h %p\n";
	assert_eq!(
		parse_ssh_host_config(text, "via", home).proxy_command.as_deref(),
		Some("nc %h %p")
	);
}

#[test]
fn relative_resolution_joins_remote_paths_with_forward_slashes_only() {
	// A workdir path ending in '/' must not produce a double slash.
	let workdir = PathBuf::from("ssh://user@host:2222/root/");
	match resolve_path_source("src/main.rs", &workdir) {
		PathSource::Remote { path, .. } => assert_eq!(path, "/root/src/main.rs"),
		_ => panic!("expected Remote"),
	}
	// Windows-style separators in the relative path are normalized.
	match resolve_path_source("src\\main.rs", &workdir) {
		PathSource::Remote { path, .. } => assert_eq!(path, "/root/src/main.rs"),
		_ => panic!("expected Remote"),
	}
}

#[test]
fn malformed_ipv6_urls_fall_back_to_port_22() {
	// An unclosed bracket keeps the whole host_part as the host.
	match parse_path_source("ssh://user@[2001:db8::1/path") {
		PathSource::Remote { host, port, .. } => {
			assert_eq!(host, "[2001:db8::1");
			assert_eq!(port, 22);
		}
		_ => panic!("expected Remote"),
	}
	// A non-numeric port falls back to 22 but stays marked explicit.
	match parse_path_source("ssh://host:notaport/x") {
		PathSource::Remote {
			host,
			port,
			port_explicit,
			..
		} => {
			assert_eq!(host, "host");
			assert_eq!(port, 22);
			assert!(port_explicit);
		}
		_ => panic!("expected Remote"),
	}
}

#[test]
fn remote_parent_of_root_path_is_none() {
	// "/" has no parent even with an authority attached.
	assert!(parse_path_source("ssh://host/").parent().is_none());
}
