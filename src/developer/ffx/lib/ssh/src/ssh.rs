// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::config::SshConfig;
use anyhow::Context as _;
use anyhow::{anyhow, Result};
use ffx_config::EnvironmentContext;
use std::{net::SocketAddr, path::PathBuf, process::Command};

const SSH_PRIV: &str = "ssh.priv";

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
    if let Ok(path) = ffx_config::get::<String, _>(SSH_AUTH_SOCK).await {
        cmd.env("SSH_AUTH_SOCK", path);
    }
}

pub async fn build_ssh_command_with_ssh_path(
    ssh_path: &str,
    addr: SocketAddr,
    command: Vec<&str>,
) -> Result<Command> {
    let config = SshConfig::new()?;
    build_ssh_command_with_ssh_config(ssh_path, addr, &config, command).await
}

pub async fn build_ssh_command_with_env(
    ssh_path: &str,
    addr: SocketAddr,
    env: &EnvironmentContext,
    command: Vec<&str>,
) -> Result<Command> {
    let config = SshConfig::new()?;
    build_ssh_command_with_ssh_config_and_env(ssh_path, addr, &config, command, Some(env)).await
}

pub async fn build_ssh_command_with_ssh_config(
    ssh_path: &str,
    addr: SocketAddr,
    config: &SshConfig,
    command: Vec<&str>,
) -> Result<Command> {
    build_ssh_command_with_ssh_config_and_env(ssh_path, addr, config, command, None).await
}

/// Builds the ssh command using the specified ssh configuration and path to the ssh command.
pub async fn build_ssh_command_with_ssh_config_and_env(
    ssh_path: &str,
    addr: SocketAddr,
    config: &SshConfig,
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

    #[fuchsia_async::run_singlethreaded(test)]
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

    #[fuchsia_async::run_singlethreaded(test)]
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

    #[fuchsia_async::run_singlethreaded(test)]
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

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_build_ssh_command_with_ssh_config() {
        let mut config = SshConfig::new().expect("default ssh config");
        let addr = "[fe80::12%5]:8022".parse().unwrap();

        // Override some options
        config.set("LogLevel", "DEBUG3").expect("setting loglevel");

        let result =
            build_ssh_command_with_ssh_config("ssh", addr, &config, vec!["ls"]).await.unwrap();
        let actual_args: Vec<_> =
            result.get_args().map(|a| a.to_string_lossy().to_string()).collect();

        // Check the default
        assert_eq!(config.get("CheckHostIP").expect("CheckHostIP value").unwrap(), "no");
        assert!(actual_args.contains(&"CheckHostIP=no".to_string()));

        // Check the override
        assert!(actual_args.contains(&"LogLevel=DEBUG3".to_string()));
    }
}
