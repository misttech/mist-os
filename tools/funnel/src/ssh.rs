// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, Result};
use fidl_fuchsia_developer_ffx::TargetInfo;
use std::process::Command;

pub(crate) fn do_ssh(host: String, target: TargetInfo, repo_port: u32) -> Result<()> {
    let mut ssh_cmd = &mut Command::new("ssh");
    for arg in build_ssh_args(target, repo_port)?.iter() {
        ssh_cmd = ssh_cmd.arg(arg);
    }
    ssh_cmd = ssh_cmd.arg(host);
    tracing::debug!("About to ssh with command: {:#?}", ssh_cmd);
    let mut ssh = ssh_cmd.spawn()?;
    ssh.wait()?;

    Ok(())
}

fn build_ssh_args(target: TargetInfo, repo_port: u32) -> Result<Vec<String>> {
    let target_ip: TargetAddr = target
        .addresses
        .ok_or("target address list uninitialized")
        .map_err(|e| anyhow!("Error getting target addresses: {}", e))?
        .first()
        .ok_or("target address list was empty")
        .map_err(|e| anyhow!("Error getting target addresses: {}", e))?
        .into();

    let res: Vec<String> = vec![
        // We want ipv6 binds for the port forwards
        "-o AddressFamily inet6".into(),
        // We do not want multiplexing
        "-o ControlPath none".into(),
        "-o ControlMaster no".into(),
        // Disable pseudo-tty allocation for screen based programs over the SSH tunnel.
        "-o RequestTTY yes".into(),
        "-o ExitOnForwardFailure yes".into(),
        "-o StreamLocalBindUnlink yes".into(),
        // Request to a package server on the local host are forwarded to the remote
        // host.
        format!("-o LocalForward *:{repo_port} localhost:{repo_port}"),
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

    // TODO: additional forwards

    Ok(res)
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetIp};
    use fidl_fuchsia_net::{IpAddress, Ipv6Address};
    use pretty_assertions::assert_eq;
    use std::net::Ipv6Addr;

    #[test]
    fn test_make_args() -> Result<()> {
        let src = TargetAddrInfo::Ip(TargetIp {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0).octets(),
            }),
            scope_id: 2,
        });

        let target = TargetInfo {
            nodename: Some("kiriona".to_string()),
            addresses: Some(vec![src]),
            ..Default::default()
        };

        let got = build_ssh_args(target, 8081)?;

        let want: Vec<&str> = vec![
            "-o AddressFamily inet6",
            "-o ControlPath none",
            "-o ControlMaster no",
            "-o RequestTTY yes",
            "-o ExitOnForwardFailure yes",
            "-o StreamLocalBindUnlink yes",
            "-o LocalForward *:8081 localhost:8081",
            "-o RemoteForward 8022 [ff00::]:22",
            "-o RemoteForward 2345 [ff00::]:2345",
            "-o RemoteForward 8007 [ff00::]:8007",
            "-o RemoteForward 8008 [ff00::]:8008",
            "-o RemoteForward 8443 [ff00::]:8443",
            "-o RemoteForward 9080 [ff00::]:80",
            "-o RemoteForward 8888 [ff00::]:8888",
            "-o RemoteForward 5554 [ff00::]:5554",
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

        let target =
            TargetInfo { nodename: None, addresses: Some(vec![src]), ..Default::default() };

        let got = build_ssh_args(target, 8081)?;

        let want: Vec<&str> = vec![
            "-o AddressFamily inet6",
            "-o ControlPath none",
            "-o ControlMaster no",
            "-o RequestTTY yes",
            "-o ExitOnForwardFailure yes",
            "-o StreamLocalBindUnlink yes",
            "-o LocalForward *:8081 localhost:8081",
            "-o RemoteForward 8022 [ff00::]:22",
            "-o RemoteForward 2345 [ff00::]:2345",
            "-o RemoteForward 8007 [ff00::]:8007",
            "-o RemoteForward 8008 [ff00::]:8008",
            "-o RemoteForward 8443 [ff00::]:8443",
            "-o RemoteForward 9080 [ff00::]:80",
            "-o RemoteForward 8888 [ff00::]:8888",
            "-o RemoteForward 5554 [ff00::]:5554",
        ];

        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn test_make_args_returns_err_on_no_addresses() {
        let nodename = Some("cytherea".to_string());
        {
            let target = TargetInfo { nodename: nodename.clone(), ..Default::default() };
            let res = build_ssh_args(target, 9091);
            assert!(res.is_err());
        }
        {
            let target =
                TargetInfo { nodename: nodename.clone(), addresses: None, ..Default::default() };
            let res = build_ssh_args(target, 9091);
            assert!(res.is_err());
        }
        {
            let target = TargetInfo {
                nodename: nodename.clone(),
                addresses: Some(vec![]),
                ..Default::default()
            };
            let res = build_ssh_args(target, 9091);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_make_args_returns_err_on_empty_addresses() {
        let target = TargetInfo {
            nodename: Some("cytherera".to_string()),
            addresses: None,
            ..Default::default()
        };
        let res = build_ssh_args(target, 9091);
        assert!(res.is_err());
    }
}
