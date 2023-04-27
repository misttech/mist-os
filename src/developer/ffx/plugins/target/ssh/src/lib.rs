// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::Result;
use async_trait::async_trait;
use ffx_ssh::ssh::get_ssh_key_paths;
use ffx_target_ssh_args::SshCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::TargetProxy;
use std::net::IpAddr;
use std::process::Command;
use std::time::Duration;
use timeout::timeout;

static DEFAULT_SSH_OPTIONS: &'static [&str] = &[
    // We do not want multiplexing
    "-o",
    "ControlPath none",
    "-o",
    "ControlMaster no",
    "-o",
    "ExitOnForwardFailure yes",
    "-o",
    "StreamLocalBindUnlink yes",
    "-o",
    "CheckHostIP=no",
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
];

#[derive(FfxTool)]
pub struct SshTool {
    #[command]
    cmd: SshCommand,
    target_proxy: TargetProxy,
}

fho::embedded_plugin!(SshTool);

#[async_trait(?Send)]
impl FfxMain for SshTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let addr_info = timeout(Duration::from_secs(1), self.target_proxy.get_ssh_address())
            .await
            .user_message("Timed out getting target ssh address")?
            .user_message("Failed to get target ssh address")?;

        let addr = TargetAddr::from(&addr_info);

        let mut ssh_cmd = build_ssh_command(self.cmd, addr)
            .await
            .bug_context("Building command to ssh to target")?;

        tracing::debug!("About to ssh with command: {:#?}", ssh_cmd);
        let mut ssh = ssh_cmd.spawn().user_message("Failed to run ssh command to target")?;
        ssh.wait().user_message("Command 'ssh' exited with error.")?;

        Ok(())
    }
}

async fn build_ssh_command(cmd: SshCommand, addr: TargetAddr) -> Result<Command> {
    let mut ssh_cmd = Command::new("ssh");

    match cmd.sshconfig {
        Some(f) => {
            ssh_cmd.arg("-F").arg(f);
        }
        None => {
            for arg in DEFAULT_SSH_OPTIONS {
                ssh_cmd.arg(arg);
            }
            match addr.ip() {
                IpAddr::V4(_) => {
                    ssh_cmd.arg("-o");
                    ssh_cmd.arg("AddressFamily inet");
                }
                IpAddr::V6(_) => {
                    ssh_cmd.arg("-o");
                    ssh_cmd.arg("AddressFamily inet6");
                }
            }
        }
    };

    match cmd.private_key {
        Some(pk) => {
            ssh_cmd.arg("-i").arg(pk);
        }
        None => {
            for k in get_ssh_key_paths().await? {
                ssh_cmd.arg("-i").arg(k);
            }
        }
    };

    // Port and host
    ssh_cmd.arg("-p").arg(format!("{}", addr.port())).arg(format!("{}", addr));

    // User passed commands to run on the target
    for arg in cmd.command {
        ssh_cmd.arg(arg);
    }

    Ok(ssh_cmd)
}

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_custom_private_key() -> Result<()> {
        let addr = TargetAddr::from_str("[fe80::1%1]:22")?;
        let cmd = SshCommand {
            private_key: Some("/foo/bar/baz.pk".to_string()),
            sshconfig: None,
            command: vec![],
        };
        let ssh_cmd = build_ssh_command(cmd, addr).await?;
        assert_eq!(ssh_cmd.get_program(), "ssh");
        assert_eq!(
            ssh_cmd.get_args().collect::<Vec<_>>(),
            vec![
                "-o",
                "ControlPath none",
                "-o",
                "ControlMaster no",
                "-o",
                "ExitOnForwardFailure yes",
                "-o",
                "StreamLocalBindUnlink yes",
                "-o",
                "CheckHostIP=no",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "AddressFamily inet6",
                "-i",
                "/foo/bar/baz.pk",
                "-p",
                "22",
                format!("{}", addr).as_str(),
            ]
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_address_family() -> Result<()> {
        let addr = TargetAddr::from_str("127.0.0.1:34522")?;
        let cmd = SshCommand {
            private_key: Some("/foo/bar/baz.pk".to_string()),
            sshconfig: None,
            command: vec![],
        };
        let ssh_cmd = build_ssh_command(cmd, addr).await?;
        assert_eq!(ssh_cmd.get_program(), "ssh");
        assert_eq!(
            ssh_cmd.get_args().collect::<Vec<_>>(),
            vec![
                "-o",
                "ControlPath none",
                "-o",
                "ControlMaster no",
                "-o",
                "ExitOnForwardFailure yes",
                "-o",
                "StreamLocalBindUnlink yes",
                "-o",
                "CheckHostIP=no",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "AddressFamily inet",
                "-i",
                "/foo/bar/baz.pk",
                "-p",
                "34522",
                format!("{}", addr).as_str(),
            ]
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_custom_config_file() -> Result<()> {
        let addr = TargetAddr::from_str("[fe80::1%1]:22")?;
        let cmd = SshCommand {
            private_key: Some("/foo/bar/baz.pk".to_string()),
            sshconfig: Some("/foo/bar/baz.conf".to_string()),
            command: vec!["echo".to_string(), "'foo'".to_string()],
        };
        let ssh_cmd = build_ssh_command(cmd, addr).await?;
        assert_eq!(ssh_cmd.get_program(), "ssh");
        assert_eq!(
            ssh_cmd.get_args().collect::<Vec<_>>(),
            vec![
                "-F",
                "/foo/bar/baz.conf",
                "-i",
                "/foo/bar/baz.pk",
                "-p",
                "22",
                format!("{}", addr).as_str(),
                "echo",
                "'foo'",
            ]
        );
        Ok(())
    }
}
