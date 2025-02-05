// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::config::SshConfig;
use anyhow::{anyhow, Context as _, Result};
use ffx_config::EnvironmentContext;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

const SSH_PRIV: &str = "ssh.priv";
pub const KEEPALIVE_TIMEOUT_CONFIG: &str = "ssh.keepalive_timeout";

#[derive(thiserror::Error, Debug, Hash, Clone, PartialEq, Eq)]
pub enum SshError {
    #[error("unknown ssh error: {0}")]
    Unknown(String),
    #[error("permission denied")]
    PermissionDenied,
    #[error("connection refused")]
    ConnectionRefused,
    #[error("unknown name or service")]
    UnknownNameOrService,
    #[error("timeout")]
    Timeout,
    #[error("key verification failure")]
    KeyVerificationFailure,
    #[error("no route to host")]
    NoRouteToHost,
    #[error("network unreachable")]
    NetworkUnreachable,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("target not compatible")]
    TargetIncompatible,
    #[error("connection closed by remote host")]
    ConnectionClosedByRemoteHost,
}

impl From<String> for SshError {
    fn from(s: String) -> Self {
        if s.contains("Permission denied") {
            return Self::PermissionDenied;
        }
        if s.contains("Connection refused") {
            return Self::ConnectionRefused;
        }
        if s.contains("Name or service not known") {
            return Self::UnknownNameOrService;
        }
        if s.contains("Connection timed out") {
            return Self::Timeout;
        }
        if s.contains("Host key verification failed") {
            return Self::KeyVerificationFailure;
        }
        if s.contains("No route to host") {
            return Self::NoRouteToHost;
        }
        if s.contains("Network is unreachable") {
            return Self::NetworkUnreachable;
        }
        if s.contains("Invalid argument") {
            return Self::InvalidArgument;
        }
        if s.contains("not compatible") {
            return Self::TargetIncompatible;
        }
        if s.contains("Connection closed by remote host") {
            return Self::ConnectionClosedByRemoteHost;
        }
        return Self::Unknown(s);
    }
}

impl From<&str> for SshError {
    fn from(s: &str) -> Self {
        Self::from(s.to_owned())
    }
}

#[cfg(not(test))]
pub async fn get_ssh_key_paths() -> Result<Vec<String>> {
    use anyhow::Context;
    ffx_config::query(SSH_PRIV)
        .get_file()
        .await
        .context("getting path to an ssh private key from ssh.priv")
}

pub async fn get_ssh_key_paths_from_env(env: &EnvironmentContext) -> Result<Vec<String>> {
    env.query(SSH_PRIV)
        .get_file()
        .await
        .context("getting path to an ssh private key from ssh.priv from env context")
}

#[cfg(test)]
const TEST_SSH_KEY_PATH: &str = "ssh/ssh_key_in_test";
#[cfg(test)]
async fn get_ssh_key_paths() -> Result<Vec<String>> {
    Ok(vec![TEST_SSH_KEY_PATH.to_string()])
}

async fn apply_auth_sock(cmd: &mut Command) {
    const SSH_AUTH_SOCK: &str = "ssh.auth-sock";
    if let Ok(path) = ffx_config::get::<String, _>(SSH_AUTH_SOCK) {
        tracing::debug!("SSH_AUTH_SOCK retrieved via config: {}", path);
        cmd.env("SSH_AUTH_SOCK", path.as_str());
        if !std::fs::exists(path.as_str()).unwrap() {
            tracing::warn!("SSH_AUTH_SOCK file does not exist at: {}", path);
        }
    }
}

async fn build_ssh_command_with_ssh_path(
    ssh_path: &str,
    addr: SocketAddr,
    command: Vec<&str>,
) -> Result<Command> {
    let mut config = SshConfig::new()?;
    build_ssh_command_with_ssh_config(ssh_path, addr, &mut config, command).await
}

pub async fn build_ssh_command_with_env(
    ssh_path: &str,
    addr: SocketAddr,
    env: &EnvironmentContext,
    command: Vec<&str>,
) -> Result<Command> {
    let mut ssh_config = SshConfig::new()?;
    build_ssh_command_with_ssh_config_and_env(ssh_path, addr, &mut ssh_config, command, Some(env))
        .await
}

pub async fn build_ssh_command_with_ssh_config(
    ssh_path: &str,
    addr: SocketAddr,
    config: &mut SshConfig,
    command: Vec<&str>,
) -> Result<Command> {
    build_ssh_command_with_ssh_config_and_env(ssh_path, addr, config, command, None).await
}

/// Builds the ssh command using the specified ssh configuration and path to the ssh command.
async fn build_ssh_command_with_ssh_config_and_env(
    ssh_path: &str,
    addr: SocketAddr,
    config: &mut SshConfig,
    command: Vec<&str>,
    env: Option<&EnvironmentContext>,
) -> Result<Command> {
    if ssh_path.is_empty() {
        return Err(anyhow!("missing SSH command"));
    }

    let keys = if let Some(env) = env {
        get_ssh_key_paths_from_env(env).await?
    } else {
        get_ssh_key_paths().await?
    };

    if let Some(env) = env {
        if let Some(keepalive_timeout) = env.query(KEEPALIVE_TIMEOUT_CONFIG).get::<Option<u64>>()? {
            config.set_server_alive_count_max(keepalive_timeout as u16)?
        }
    }

    let mut c = Command::new(ssh_path);
    apply_auth_sock(&mut c).await;
    c.args(["-F", "none"]);
    c.args(config.to_args());

    for key in keys {
        c.arg("-i").arg(key);
    }

    match addr {
        SocketAddr::V4(_) => c.arg("-o").arg("AddressFamily=inet"),
        SocketAddr::V6(_) => c.arg("-o").arg("AddressFamily=inet6"),
    };

    let mut addr_str = format!("{}", addr);
    let colon_port = addr_str.split_off(addr_str.rfind(':').expect("socket format includes port"));

    // Remove the enclosing [] used in IPv6 socketaddrs
    let addr_start = if addr_str.starts_with("[") { 1 } else { 0 };
    let addr_end = addr_str.len() - if addr_str.ends_with("]") { 1 } else { 0 };
    let addr_arg = &addr_str[addr_start..addr_end];

    c.arg("-p").arg(&colon_port[1..]);
    c.arg(addr_arg);

    c.args(&command);

    return Ok(c);
}

/// Build the ssh command using the default ssh command and configuration.
pub async fn build_ssh_command(addr: SocketAddr, command: Vec<&str>) -> Result<Command> {
    build_ssh_command_with_ssh_path("ssh", addr, command).await
}

/// Build the ssh command using a provided sshconfig file.
pub async fn build_ssh_command_with_config_file(
    config_file: &PathBuf,
    addr: SocketAddr,
    command: Vec<&str>,
) -> Result<Command> {
    let keys = get_ssh_key_paths().await?;

    let mut c = Command::new("ssh");
    apply_auth_sock(&mut c).await;
    c.arg("-F").arg(config_file);

    for k in keys {
        c.arg("-i").arg(k);
    }

    let mut addr_str = format!("{}", addr);
    let colon_port = addr_str.split_off(addr_str.rfind(':').expect("socket format includes port"));

    // Remove the enclosing [] used in IPv6 socketaddrs
    let addr_start = if addr_str.starts_with("[") { 1 } else { 0 };
    let addr_end = addr_str.len() - if addr_str.ends_with("]") { 1 } else { 0 };
    let addr_arg = &addr_str[addr_start..addr_end];

    c.arg("-p").arg(&colon_port[1..]);
    c.arg(addr_arg);

    c.args(&command);

    return Ok(c);
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::ConfigLevel;
    use pretty_assertions::assert_eq;
    use std::io::BufRead;

    #[fuchsia::test]
    async fn test_build_ssh_command_ipv4() {
        let config = SshConfig::new().expect("default ssh config");
        let addr = "192.168.0.1:22".parse().unwrap();

        let result = build_ssh_command(addr, vec!["ls"]).await.unwrap();
        let actual_args: Vec<_> = result.get_args().map(|a| a.to_string_lossy()).collect();
        let mut expected_args: Vec<String> = vec!["-F".into(), "none".into()];
        expected_args.extend(config.to_args());

        expected_args.extend(
            ["-i", TEST_SSH_KEY_PATH, "-o", "AddressFamily=inet", "-p", "22", "192.168.0.1", "ls"]
                .map(String::from),
        );

        assert_eq!(actual_args, expected_args);
    }

    #[fuchsia::test]
    async fn test_build_ssh_command_ipv6() {
        let config = SshConfig::new().expect("default ssh config");
        let addr = "[fe80::12%5]:8022".parse().unwrap();

        let result = build_ssh_command(addr, vec!["ls"]).await.unwrap();
        let actual_args: Vec<_> = result.get_args().map(|a| a.to_string_lossy()).collect();
        let mut expected_args: Vec<String> = vec!["-F".into(), "none".into()];
        expected_args.extend(config.to_args());

        expected_args.extend(
            [
                "-i",
                TEST_SSH_KEY_PATH,
                "-o",
                "AddressFamily=inet6",
                "-p",
                "8022",
                "fe80::12%5",
                "ls",
            ]
            .map(String::from),
        );

        assert_eq!(actual_args, expected_args);
    }

    #[fuchsia::test]
    async fn test_apply_auth_sock() {
        let env = ffx_config::test_init().await.unwrap();
        let expect_path =
            env.isolate_root.path().join("ssh-auth.sock").to_string_lossy().to_string();
        env.context
            .query("ssh.auth-sock")
            .level(Some(ConfigLevel::User))
            .set(expect_path.clone().into())
            .await
            .expect("setting auth sock config");

        let mut cmd = Command::new("env");
        apply_auth_sock(&mut cmd).await;
        let lines =
            cmd.output().unwrap().stdout.lines().filter_map(|res| res.ok()).collect::<Vec<_>>();

        let expected_var = format!("SSH_AUTH_SOCK={}", expect_path);
        assert!(
            lines.iter().any(|line| line.starts_with(&expected_var)),
            "Looking for {} in {}",
            expected_var,
            lines.join("\n")
        );
    }

    #[fuchsia::test]
    async fn test_build_ssh_command_with_ssh_config() {
        let mut config = SshConfig::new().expect("default ssh config");
        let addr = "[fe80::12%5]:8022".parse().unwrap();

        // Override some options
        config.set("LogLevel", "DEBUG3").expect("setting loglevel");

        let result =
            build_ssh_command_with_ssh_config("ssh", addr, &mut config, vec!["ls"]).await.unwrap();
        let actual_args: Vec<_> =
            result.get_args().map(|a| a.to_string_lossy().to_string()).collect();

        // Check the default
        assert_eq!(config.get("CheckHostIP").expect("CheckHostIP value").unwrap(), "no");
        assert!(actual_args.contains(&"CheckHostIP=no".to_string()));

        // Check the override
        assert!(actual_args.contains(&"LogLevel=DEBUG3".to_string()));
    }

    #[fuchsia::test]
    fn test_host_pipe_err_from_str() {
        assert_eq!(SshError::from("Permission denied"), SshError::PermissionDenied);
        assert_eq!(SshError::from("Connection refused"), SshError::ConnectionRefused);
        assert_eq!(SshError::from("Name or service not known"), SshError::UnknownNameOrService);
        assert_eq!(SshError::from("Connection timed out"), SshError::Timeout);
        assert_eq!(
            SshError::from("Host key verification failedddddd"),
            SshError::KeyVerificationFailure
        );
        assert_eq!(SshError::from("There is No route to host"), SshError::NoRouteToHost);
        assert_eq!(SshError::from("The Network is unreachable"), SshError::NetworkUnreachable);
        assert_eq!(SshError::from("Invalid argument"), SshError::InvalidArgument);
        assert_eq!(SshError::from("ABI 123 is not compatible"), SshError::TargetIncompatible);

        let unknown_str = "OIHWOFIHOIWHFW";
        assert_eq!(SshError::from(unknown_str), SshError::Unknown(String::from(unknown_str)));
    }
}
