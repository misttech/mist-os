// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::Deref;

use fho::FhoTargetInfo;
use fidl_fuchsia_developer_ffx as ffx_fidl;

/// Holder struct for TargetInfo. This one is a little different since
/// it is referenced by the DeviceLookup trait, it implements a trait
/// provided by fho to decouple the crates.
#[derive(Debug, Clone)]
pub struct TargetInfoHolder(ffx_fidl::TargetInfo);

impl FhoTargetInfo for TargetInfoHolder {
    fn nodename(&self) -> Option<String> {
        self.0.nodename.clone()
    }

    fn serial_number(&self) -> Option<String> {
        self.0.serial_number.clone()
    }

    fn addresses(&self) -> Vec<std::net::SocketAddr> {
        let mut addrs = vec![];
        if let Some(address_list) = &self.0.addresses {
            for addr in address_list {
                let mut address: addr::TargetAddr = addr.into();
                address.set_port(0);
                addrs.push(address.into());
            }
        }
        addrs
    }

    fn ssh_address(&self) -> Option<std::net::SocketAddr> {
        if let Some(ssh_address) = &self.0.ssh_address {
            let address: addr::TargetAddr = ssh_address.into();
            Some(address.into())
        } else {
            None
        }
    }
}

impl From<ffx_fidl::TargetInfo> for TargetInfoHolder {
    fn from(value: ffx_fidl::TargetInfo) -> Self {
        Self(value)
    }
}

impl From<&ffx_fidl::TargetInfo> for TargetInfoHolder {
    fn from(value: &ffx_fidl::TargetInfo) -> Self {
        Self(value.clone())
    }
}

impl Deref for TargetInfoHolder {
    type Target = ffx_fidl::TargetInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_fidl::{TargetAddrInfo, TargetIpPort};
    use fidl_fuchsia_net::{IpAddress, Ipv4Address, Ipv6Address};
    use std::net::SocketAddr;

    #[test]
    fn test_new() {
        let ffx_info = ffx_fidl::TargetInfo {
            nodename: Some("somenodename".into()),
            serial_number: Some("S3R1AL".into()),
            ..Default::default()
        };
        let info: TargetInfoHolder = (&ffx_info).into();

        assert_eq!(info.nodename(), ffx_info.nodename);
        assert_eq!(info.serial_number(), ffx_info.serial_number);
        assert_eq!(info.addresses().is_empty(), ffx_info.addresses.is_none());
        assert_eq!(info.ssh_address(), None);
    }

    #[test]
    fn test_ssh_address() {
        let ssh_addr_info = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            port: 2022,
            scope_id: 0,
        });

        let sa = "127.0.0.1:2022".parse::<SocketAddr>().unwrap();

        let ffx_info = ffx_fidl::TargetInfo {
            nodename: Some("somenodename".into()),
            serial_number: Some("S3R1AL".into()),
            ssh_address: Some(ssh_addr_info),
            ..Default::default()
        };
        let info: TargetInfoHolder = (&ffx_info).into();

        assert_eq!(info.ssh_address(), Some(sa))
    }

    #[test]
    fn test_ssh_address_no_port() {
        let ssh_addr_info = TargetAddrInfo::Ip(fidl_fuchsia_developer_ffx::TargetIp {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
        });

        let sa = "127.0.0.1:0".parse::<SocketAddr>().unwrap();

        let ffx_info = ffx_fidl::TargetInfo {
            nodename: Some("somenodename".into()),
            serial_number: Some("S3R1AL".into()),
            ssh_address: Some(ssh_addr_info),
            ..Default::default()
        };
        let info: TargetInfoHolder = (&ffx_info).into();

        assert_eq!(info.ssh_address(), Some(sa))
    }

    #[test]
    fn test_ssh_address_ipv6() {
        let ssh_addr_info = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv6(Ipv6Address {
                addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            port: 2022,
            scope_id: 4,
        });

        let sa = "[::1%4]:2022".parse::<SocketAddr>().unwrap();

        let ffx_info = ffx_fidl::TargetInfo {
            nodename: Some("somenodename".into()),
            serial_number: Some("S3R1AL".into()),
            ssh_address: Some(ssh_addr_info),
            ..Default::default()
        };
        let info: TargetInfoHolder = (&ffx_info).into();

        assert_eq!(info.ssh_address(), Some(sa))
    }

    #[test]
    fn test_addresses() {
        let addrs = vec![
            TargetAddrInfo::IpPort(TargetIpPort {
                ip: IpAddress::Ipv6(Ipv6Address {
                    addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                }),
                port: 2022,
                scope_id: 4,
            }),
            TargetAddrInfo::Ip(fidl_fuchsia_developer_ffx::TargetIp {
                ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
                scope_id: 0,
            }),
        ];

        let expected_addrs: Vec<SocketAddr> = vec![
            "[::1%4]:0".parse::<SocketAddr>().unwrap(),
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ];

        let ffx_info = ffx_fidl::TargetInfo {
            nodename: Some("somenodename".into()),
            serial_number: Some("S3R1AL".into()),
            addresses: Some(addrs),
            ..Default::default()
        };
        let info: TargetInfoHolder = (&ffx_info).into();

        let info_addrs = info.addresses();

        assert_eq!(info_addrs.len(), expected_addrs.len());

        assert_eq!(info_addrs, expected_addrs);
    }
}
