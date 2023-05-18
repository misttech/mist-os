// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::Result;
use async_trait::async_trait;
use ffx_config::environment::EnvironmentKind;
use ffx_ssh::ssh::get_ssh_key_paths;
use ffx_target_ssh_args::SshCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetIpPort, TargetProxy};
use fidl_fuchsia_net::{IpAddress, Ipv4Address};
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

        let addr = get_addr(&addr_info)?;
        let keys = get_ssh_key_paths().await?;
        let mut ssh_cmd = build_ssh_command(self.cmd, addr, keys)
            .await
            .bug_context("Building command to ssh to target")?;

        tracing::debug!("About to ssh with command: {:#?}", ssh_cmd);
        let mut ssh = ssh_cmd.spawn().user_message("Failed to run ssh command to target")?;
        let status = ssh.wait().user_message("Command 'ssh' exited with error.")?;

        // We want to give callers of this command a meaningful error code if it fails. First,
        // check if it was terminated due to a signal and report that. If the process terminated
        // normally, exit this process with its status code.
        #[cfg(unix)]
        {
            use nix::sys::signal::Signal;
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                // ssh exited due to receiving a signal, try to parse it for pretty-printing
                let signal = Signal::try_from(signal)
                    .with_bug_context(|| format!("parsing raw ssh signal code {signal}"))?;
                return Err(fho::Error::User(anyhow::format_err!(
                    "ssh process received {signal} signal."
                )));
            }
        }

        if let Some(status) = status.code() {
            std::process::exit(status);
        } else {
            return Err(fho::Error::Unexpected(anyhow::format_err!(
                "ssh exited without an exit status or due to receiving a signal."
            )));
        }
    }
}

/// If we're running in an isolated environment against an emulator with user networking,
/// pretend we're connecting to localhost because user networking is implemented as a port on the
/// host machine.
// TODO(https://fxbug.dev/127174) this should not be required
fn get_addr(addr_info: &TargetAddrInfo) -> fho::Result<TargetAddr> {
    Ok(
        match (
            ffx_config::global_env_context()
                .expect("ffx must run with a global env context")
                .env_kind(),
            addr_info,
        ) {
            // we only care about 10.* addresses if we're in an isolated environment
            (
                EnvironmentKind::Isolated { .. },
                TargetAddrInfo::IpPort(TargetIpPort {
                    ip: IpAddress::Ipv4(Ipv4Address { addr: [10, ..] }),
                    port,
                    ..
                }),
            ) => {
                format!("127.0.0.1:{port}").parse().bug_context("Parsing localhost ssh address")?
            }
            _ => TargetAddr::from(addr_info),
        },
    )
}

async fn build_ssh_command(
    cmd: SshCommand,
    addr: TargetAddr,
    keys: Vec<String>,
) -> Result<Command> {
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

    for k in keys {
        ssh_cmd.arg("-i").arg(k);
    }

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
    use ffx_config::environment::test_init;
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_address_family() -> Result<()> {
        let _env = test_init().await?;
        let keys = vec!["/tmp/path2".to_string(), "/tmp/path1".to_string()];
        let addr = TargetAddr::from_str("127.0.0.1:34522")?;
        let cmd = SshCommand { sshconfig: None, command: vec![] };
        let ssh_cmd = build_ssh_command(cmd, addr, keys).await?;
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
                "/tmp/path2",
                "-i",
                "/tmp/path1",
                "-p",
                "34522",
                format!("{}", addr).as_str(),
            ]
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_custom_config_file() -> Result<()> {
        let _env = test_init().await?;
        let keys = vec!["/tmp/path2".to_string(), "/tmp/path1".to_string()];
        let addr = TargetAddr::from_str("[fe80::1%1]:22")?;
        let cmd = SshCommand {
            sshconfig: Some("/foo/bar/baz.conf".to_string()),
            command: vec!["echo".to_string(), "'foo'".to_string()],
        };
        let ssh_cmd = build_ssh_command(cmd, addr, keys).await?;
        assert_eq!(ssh_cmd.get_program(), "ssh");
        assert_eq!(
            ssh_cmd.get_args().collect::<Vec<_>>(),
            vec![
                "-F",
                "/foo/bar/baz.conf",
                "-i",
                "/tmp/path2",
                "-i",
                "/tmp/path1",
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
