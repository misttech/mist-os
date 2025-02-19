// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetIp, TargetIpPort};
use fidl_fuchsia_net::{IpAddress, Ipv4Address, Ipv6Address};
use netext::{scope_id_to_name, IsLocalAddr};
use std::cmp::Ordering;
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::str::FromStr;

#[derive(Clone, Debug, Copy)]
pub struct TargetAddr(SocketAddr);

// Only compare `TargetAddr` by ip and port, since we want to deduplicate targets if they are
// addressable over multiple IPv6 interfaces.
impl std::hash::Hash for TargetAddr {
    fn hash<H>(&self, state: &mut H)
    where
        H: std::hash::Hasher,
    {
        (self.0.ip(), self.0.port()).hash(state)
    }
}

impl PartialEq for TargetAddr {
    fn eq(&self, other: &Self) -> bool {
        self.0.ip() == other.0.ip() && self.0.port() == other.0.port()
    }
}

impl Eq for TargetAddr {}

impl Ord for TargetAddr {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.ip().cmp(&other.0.ip()).then(self.0.port().cmp(&other.0.port()))
    }
}

impl PartialOrd for TargetAddr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Into<TargetAddrInfo> for &TargetAddr {
    fn into(self) -> TargetAddrInfo {
        TargetAddrInfo::IpPort(TargetIpPort {
            ip: match self.ip() {
                IpAddr::V6(i) => IpAddress::Ipv6(Ipv6Address { addr: i.octets().into() }),
                IpAddr::V4(i) => IpAddress::Ipv4(Ipv4Address { addr: i.octets().into() }),
            },
            scope_id: self.scope_id(),
            port: self.port(),
        })
    }
}

impl Into<TargetAddrInfo> for TargetAddr {
    fn into(self) -> TargetAddrInfo {
        (&self).into()
    }
}

impl From<TargetAddrInfo> for TargetAddr {
    fn from(t: TargetAddrInfo) -> Self {
        (&t).into()
    }
}

impl From<TargetIp> for TargetAddr {
    fn from(t: TargetIp) -> Self {
        let (addr, scope): (IpAddr, u32) = match t.ip {
            IpAddress::Ipv6(Ipv6Address { addr }) => (addr.into(), t.scope_id),
            IpAddress::Ipv4(Ipv4Address { addr }) => (addr.into(), t.scope_id),
        };
        TargetAddr::new(addr, scope, 0)
    }
}

impl From<&TargetAddrInfo> for TargetAddr {
    fn from(t: &TargetAddrInfo) -> Self {
        let (addr, scope, port): (IpAddr, u32, u16) = match t {
            TargetAddrInfo::Ip(ip) => match ip.ip {
                IpAddress::Ipv6(Ipv6Address { addr }) => (addr.into(), ip.scope_id, 0),
                IpAddress::Ipv4(Ipv4Address { addr }) => (addr.into(), ip.scope_id, 0),
            },
            TargetAddrInfo::IpPort(ip) => match ip.ip {
                IpAddress::Ipv6(Ipv6Address { addr }) => (addr.into(), ip.scope_id, ip.port),
                IpAddress::Ipv4(Ipv4Address { addr }) => (addr.into(), ip.scope_id, ip.port),
            },
            // TODO(https://fxbug.dev/42130068): Add serial numbers.,
        };

        TargetAddr::new(addr, scope, port)
    }
}

impl From<TargetAddr> for SocketAddr {
    fn from(t: TargetAddr) -> Self {
        Self::from(&t)
    }
}

impl From<&TargetAddr> for SocketAddr {
    fn from(t: &TargetAddr) -> Self {
        t.0
    }
}

impl From<SocketAddr> for TargetAddr {
    fn from(s: SocketAddr) -> Self {
        Self(s)
    }
}

/// Construct a new TargetAddr from a string representation of the form
/// accepted by std::net::SocketAddr, e.g. 127.0.0.1:22, or [fe80::1%1]:0.
impl FromStr for TargetAddr {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sa = s.parse::<SocketAddr>()?;
        Ok(Self::from(sa))
    }
}

impl TargetAddr {
    // TODO(colnnelson): clean up with wrapper types for `scope` and `port` to
    // avoid the "zero is default" legacy.
    pub fn new(ip: IpAddr, scope_id: u32, port: u16) -> Self {
        match ip {
            IpAddr::V6(addr) => Self(SocketAddr::V6(SocketAddrV6::new(addr, port, 0, scope_id))),
            IpAddr::V4(addr) => Self(SocketAddr::V4(SocketAddrV4::new(addr, port))),
        }
    }

    pub fn scope_id(&self) -> u32 {
        match self.0 {
            SocketAddr::V6(v6) => v6.scope_id(),
            _ => 0,
        }
    }

    pub fn set_scope_id(&mut self, scope_id: u32) {
        match self.0 {
            SocketAddr::V6(mut v6) => v6.set_scope_id(scope_id),
            _ => {}
        }
    }

    pub fn ip(&self) -> IpAddr {
        self.0.ip()
    }

    pub fn port(&self) -> u16 {
        self.0.port()
    }

    pub fn set_port(&mut self, new_port: u16) {
        self.0.set_port(new_port);
    }

    pub fn optional_port_str(&self) -> String {
        match (self.0.ip(), self.0.port()) {
            (_, 0) | (_, 22) => format!("{self}"),
            (IpAddr::V6(_), p) => format!("[{self}]:{p}"),
            (_, p) => format!("{self}:{p}"),
        }
    }
}

impl std::fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.ip() {
            IpAddr::V4(ip) => {
                write!(f, "{}", ip)?;
            }
            IpAddr::V6(ip) => {
                write!(f, "{}", ip)?;
                if ip.is_link_local_addr() && self.scope_id() > 0 {
                    write!(f, "%{}", scope_id_to_name(self.scope_id()))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn test_port_str_with_ipv6_bracket() {
        let v6addr: std::net::SocketAddr = std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
            8080,
            0,
            1,
        )
        .into();
        let addr: TargetAddr = v6addr.into();
        let s = addr.optional_port_str();
        assert_eq!(&s, "[2001:db8::1]:8080");
    }
}
