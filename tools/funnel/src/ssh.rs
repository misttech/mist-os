// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{errors::IntoExitCode, TargetInfo};
use addr::TargetAddr;
use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use home::home_dir;
use std::process::Command;
use std::process::Stdio;
use std::{os::unix::process::CommandExt, path::PathBuf};
use thiserror::Error;

const CONTROL_MASTER_PATH: &str = ".ssh/control/funnel_control_master";

const CLEANUP_COMMAND: &'static str = include_str!("cleanup_command");

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("Target {target} did not have any valid Ip Addresses associated with it.")]
    NoAddressesError { target: String },
    #[error("There may be a tunnel already running to {remote_host}. Try running `funnel cleanup-remote-host {remote_host}` to clean it up before retrying")]
    TunnelAlreadyRunning { remote_host: String },
    #[error("Ssh was terminated by a signal")]
    SshTerminatedFromSignal,
    #[error("User's home dir is not valid UTF8: {home_dir}")]
    InvalidHomeDir { home_dir: PathBuf },
    #[error("User does not have a Home Dir")]
    NoHomeDir,
    #[error("Control master path cannot be at root")]
    ControlMasterPathCannotBeAtRoot,
    #[error("Could not create path to control master: {source}")]
    CannotCreateControlMasterPath { source: std::io::Error },
    #[error("Ssh Did not start: {0}")]
    SshDidNotStart(#[from] std::io::Error),
    #[error("unknown error code during ssh: {0}")]
    SshError(i32),
}

impl IntoExitCode for TunnelError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::NoAddressesError { target: _ } => 31,
            Self::TunnelAlreadyRunning { remote_host: _ } => 32,
            Self::SshTerminatedFromSignal => 128,
            Self::InvalidHomeDir { home_dir: _ } => 33,
            Self::NoHomeDir => 34,
            Self::ControlMasterPathCannotBeAtRoot => 35,
            Self::CannotCreateControlMasterPath { source } => {
                source.raw_os_error().unwrap_or_else(|| 36)
            }
            Self::SshDidNotStart(e) => e.raw_os_error().unwrap_or_else(|| 37),
            Self::SshError(i) => *i,
        }
    }
}

fn get_control_path() -> Result<Utf8PathBuf, TunnelError> {
    let home_path = home_dir().ok_or(TunnelError::NoHomeDir)?;
    let home_path = Utf8PathBuf::from_path_buf(home_path)
        .map_err(|e| TunnelError::InvalidHomeDir { home_dir: e })?;
    let funnel_control_path = home_path.join(CONTROL_MASTER_PATH);
    let funnel_control_path_parent =
        funnel_control_path.parent().ok_or(TunnelError::ControlMasterPathCannotBeAtRoot)?;
    match funnel_control_path_parent.try_exists() {
        Ok(true) => {}
        Ok(false) => std::fs::create_dir_all(funnel_control_path_parent)
            .map_err(|e| TunnelError::CannotCreateControlMasterPath { source: e })?,
        Err(e) => return Err(TunnelError::CannotCreateControlMasterPath { source: e }),
    };
    Ok(funnel_control_path)
}

pub(crate) async fn do_ssh(
    host: String,
    target: TargetInfo,
    repo_port: Vec<u32>,
    additional_port_forwards: Vec<u32>,
) -> Result<(), TunnelError> {
    // Set up the control master
    let funnel_control_path = get_control_path()?;
    // Set up ssh command
    let mut ssh_cmd = &mut Command::new("ssh");
    for arg in
        build_ssh_args(target, funnel_control_path, repo_port, additional_port_forwards)?.iter()
    {
        ssh_cmd = ssh_cmd.arg(arg);
    }
    ssh_cmd = ssh_cmd.arg(host.clone());
    ssh_cmd = ssh_cmd.arg(format!(r#"echo 'Tunnel established';echo 'Do not forget to run `ffx target add [::1]:8022`'; sleep infinity"#));

    // Disable stdin
    ssh_cmd = ssh_cmd.stdin(Stdio::null());
    // Use the PID as the process group so that SIGINT doesnt get forwarded
    ssh_cmd = ssh_cmd.process_group(0);

    // Spawn
    tracing::debug!("About to ssh with command: {:#?}", ssh_cmd);
    let mut ssh = ssh_cmd.spawn()?;
    match ssh.wait() {
        Ok(e) => match e.code() {
            None => Err(TunnelError::SshTerminatedFromSignal),
            Some(255) => Err(TunnelError::TunnelAlreadyRunning { remote_host: host.clone() }),
            Some(0) => Ok(()),
            Some(i) => Err(TunnelError::SshError(i)),
        },
        Err(e) => Err(TunnelError::SshDidNotStart(e)),
    }
}

fn build_ssh_args(
    target: TargetInfo,
    control_master_path: impl AsRef<Utf8Path>,
    repo_port: Vec<u32>,
    additional_port_forwards: Vec<u32>,
) -> Result<Vec<String>, TunnelError> {
    let mut addrs: Vec<TargetAddr> = target.addresses.into_iter().collect::<Vec<TargetAddr>>();
    tracing::debug!("Discovered addresses for target: {:?}", addrs);
    // Flip the sorting so that Ipv6 comes before Ipv4 as we will take the first
    // address, and (generally) Ipv4 addresses from the Target are ephemeral
    addrs.sort_by(|a, b| b.cmp(a));

    let target_ip =
        addrs.first().ok_or(TunnelError::NoAddressesError { target: target.nodename.clone() })?;

    if addrs.len() > 1 {
        tracing::warn!(
            "Target: {} has {} addresses associated with it: {:?}. Choosing the first one: {}",
            target.nodename.clone(),
            addrs.len(),
            addrs,
            target_ip
        );
    }

    let mut res: Vec<String> = vec![
        // We want ipv6 binds for the port forwards
        "-o AddressFamily inet6".into(),
        // We do not want multiplexing
        format!("-o ControlPath {}", control_master_path.as_ref()),
        "-o ControlMaster auto".into(),
        // Disable pseudo-tty allocation for screen based programs over the SSH tunnel.
        "-o RequestTTY no".into(),
        "-o ExitOnForwardFailure yes".into(),
        "-o StreamLocalBindUnlink yes".into(),
        // Requests from the remote to ssh to localhost:8022 will be forwarded to the
        // target.
        format!("-o RemoteForward 8022 [{target_ip}]:22"),
        // zxdb & fidlcat requests from the remote to 2345 are forwarded to the target.
        format!("-o RemoteForward 2345 [{target_ip}]:2345"),
        // libassistant debug requests from the remote to 8007 are forwarded to the
        // target.
        format!("-o RemoteForward 8007 [{target_ip}]:8007"),
        format!("-o RemoteForward 8008 [{target_ip}]:8008"),
        format!("-o RemoteForward 8443 [{target_ip}]:8443"),
        // SL4F requests to port 9080 on the remote are forwarded to target port 80.
        format!("-o RemoteForward 9080 [{target_ip}]:80"),
        // UMA log requests to port 8888 on the remote are forwarded to target port 8888.
        format!("-o RemoteForward 8888 [{target_ip}]:8888"),
        // Some targets use Fastboot over TCP which listens on 5554
        format!("-o RemoteForward 5554 [{target_ip}]:5554"),
    ];

    let additional_forwards = additional_port_forwards
        .into_iter()
        .map(|p| format!("-o RemoteForward {p} [{target_ip}]:{p}"));
    res.extend(additional_forwards);
    let repo_forwards =
        repo_port.into_iter().map(|p| format!("-o LocalForward *:{p} localhost:{p}"));
    res.extend(repo_forwards);

    Ok(res)
}

pub(crate) async fn cleanup_remote_sshd(host: String) -> Result<()> {
    let mut ssh =
        Command::new("ssh").arg("-o RequestTTY yes").arg(host).arg(CLEANUP_COMMAND).spawn()?;
    ssh.wait()?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum CloseExistingTunnelError {
    #[error("{}", .0)]
    Error(#[from] anyhow::Error),
    #[error("{}", .0)]
    TunnelError(#[from] TunnelError),
    #[error("{}", .0)]
    SshError(#[from] std::io::Error),
}

impl IntoExitCode for CloseExistingTunnelError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Error(_) => 1,
            Self::TunnelError(e) => e.exit_code(),
            Self::SshError(e) => e.raw_os_error().unwrap_or_else(|| 1),
        }
    }
}

pub(crate) fn close_existing_tunnel(host: String) -> Result<(), CloseExistingTunnelError> {
    let funnel_control_path = get_control_path()?;
    let args = vec![
        "-o".to_string(),
        format!("ControlPath {}", funnel_control_path),
        "-O".to_string(),
        "exit".to_string(),
        host,
    ];
    tracing::info!("executing ssh command with args: {:?}", args);
    let mut ssh = Command::new("ssh").args(args).spawn()?;
    ssh.wait()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetIp};
    use fidl_fuchsia_net::{IpAddress, Ipv4Address, Ipv6Address};
    use pretty_assertions::assert_eq;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_make_args() -> Result<()> {
        let src = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0).octets(),
            }),
            scope_id: 2,
        });

        let src_ipv4 = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv4(Ipv4Address { addr: Ipv4Addr::new(127, 0, 0, 1).octets() }),
            scope_id: 0,
        });

        let target = TargetInfo {
            nodename: "kiriona".to_string(),
            addresses: vec![src_ipv4.into(), src.into()],
        };

        let got = build_ssh_args(target, "/foo", vec![8081], vec![5555])?;

        let want: Vec<&str> = vec![
            "-o AddressFamily inet6",
            "-o ControlPath /foo",
            "-o ControlMaster auto",
            "-o RequestTTY no",
            "-o ExitOnForwardFailure yes",
            "-o StreamLocalBindUnlink yes",
            "-o RemoteForward 8022 [ff00::]:22",
            "-o RemoteForward 2345 [ff00::]:2345",
            "-o RemoteForward 8007 [ff00::]:8007",
            "-o RemoteForward 8008 [ff00::]:8008",
            "-o RemoteForward 8443 [ff00::]:8443",
            "-o RemoteForward 9080 [ff00::]:80",
            "-o RemoteForward 8888 [ff00::]:8888",
            "-o RemoteForward 5554 [ff00::]:5554",
            "-o RemoteForward 5555 [ff00::]:5555",
            "-o LocalForward *:8081 localhost:8081",
        ];

        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn test_make_args_empty_nodename() -> Result<()> {
        let src = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0).octets(),
            }),
            scope_id: 2,
        });

        let target = TargetInfo {
            nodename: "ianthe".to_string(),
            addresses: vec![src.into()],
            ..Default::default()
        };

        let got = build_ssh_args(target, "/foo", vec![8081], vec![])?;

        let want: Vec<&str> = vec![
            "-o AddressFamily inet6",
            "-o ControlPath /foo",
            "-o ControlMaster auto",
            "-o RequestTTY no",
            "-o ExitOnForwardFailure yes",
            "-o StreamLocalBindUnlink yes",
            "-o RemoteForward 8022 [ff00::]:22",
            "-o RemoteForward 2345 [ff00::]:2345",
            "-o RemoteForward 8007 [ff00::]:8007",
            "-o RemoteForward 8008 [ff00::]:8008",
            "-o RemoteForward 8443 [ff00::]:8443",
            "-o RemoteForward 9080 [ff00::]:80",
            "-o RemoteForward 8888 [ff00::]:8888",
            "-o RemoteForward 5554 [ff00::]:5554",
            "-o LocalForward *:8081 localhost:8081",
        ];

        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn test_make_args_multiple_repos() -> Result<()> {
        let src = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0).octets(),
            }),
            scope_id: 2,
        });

        let target = TargetInfo {
            nodename: "ianthe".to_string(),
            addresses: vec![src.into()],
            ..Default::default()
        };

        let got = build_ssh_args(target, "/foo", vec![8081, 8085], vec![])?;

        let want: Vec<&str> = vec![
            "-o AddressFamily any",
            "-o ControlPath /foo",
            "-o ControlMaster auto",
            "-o RequestTTY no",
            "-o ExitOnForwardFailure yes",
            "-o StreamLocalBindUnlink yes",
            "-o RemoteForward 8022 [ff00::]:22",
            "-o RemoteForward 2345 [ff00::]:2345",
            "-o RemoteForward 8007 [ff00::]:8007",
            "-o RemoteForward 8008 [ff00::]:8008",
            "-o RemoteForward 8443 [ff00::]:8443",
            "-o RemoteForward 9080 [ff00::]:80",
            "-o RemoteForward 8888 [ff00::]:8888",
            "-o RemoteForward 5554 [ff00::]:5554",
            "-o LocalForward *:8081 localhost:8081",
            "-o LocalForward *:8085 localhost:8085",
        ];

        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn test_make_args_returns_err_on_no_addresses() {
        let nodename = "cytherea".to_string();
        {
            let target = TargetInfo { nodename: nodename.clone(), ..Default::default() };
            let res = build_ssh_args(target, "/foo", vec![9091], vec![]);
            assert!(res.is_err());
        }
        {
            let target = TargetInfo { nodename: nodename.clone(), ..Default::default() };
            let res = build_ssh_args(target, "/foo", vec![9091], vec![]);
            assert!(res.is_err());
        }
        {
            let target =
                TargetInfo { nodename: nodename.clone(), addresses: vec![], ..Default::default() };
            let res = build_ssh_args(target, "/foo", vec![9091], vec![]);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_make_args_returns_err_on_empty_addresses() {
        let target = TargetInfo {
            nodename: "cytherera".to_string(),
            addresses: vec![],
            ..Default::default()
        };
        let res = build_ssh_args(target, "/foo", vec![9091], vec![]);
        assert!(res.is_err());
    }
}
