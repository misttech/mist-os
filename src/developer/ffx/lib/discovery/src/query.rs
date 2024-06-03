// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{desc::Description, DiscoverySources};
use addr::TargetAddr;
use fidl_fuchsia_developer_ffx::{TargetAddrInfo, TargetInfo, TargetIp, TargetIpPort};
use fidl_fuchsia_net as net;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};

#[derive(Debug, Clone)]
pub enum TargetInfoQuery {
    /// Attempts to match the nodename, falling back to serial (in that order).
    /// TODO(b/299345828): Make this an exact match by default, fall back to substring matching
    NodenameOrSerial(String),
    Addr(SocketAddr),
    First,
}

fn address_matcher(ours: &SocketAddr, theirs: &mut SocketAddr, ssh_port: u16) -> bool {
    // Use the SSH port if the target address' port is 0
    if theirs.port() == 0 {
        theirs.set_port(ssh_port)
    }

    // Clear the target address' port if the query has no port
    if ours.port() == 0 {
        theirs.set_port(0)
    }

    // Clear the target address' scope if the query has no scope
    if let (SocketAddr::V6(ours), SocketAddr::V6(theirs)) = (ours, &mut *theirs) {
        if ours.scope_id() == 0 {
            theirs.set_scope_id(0)
        }
    }

    theirs == ours
}

impl TargetInfoQuery {
    pub fn is_query_on_identity(&self) -> bool {
        matches!(self, TargetInfoQuery::NodenameOrSerial(..) | TargetInfoQuery::First)
    }

    pub fn is_query_on_address(&self) -> bool {
        matches!(self, TargetInfoQuery::Addr(..))
    }

    pub fn match_description(&self, t: &Description) -> bool {
        match self {
            Self::NodenameOrSerial(arg) => {
                if let Some(ref nodename) = t.nodename {
                    if nodename.contains(arg) {
                        return true;
                    }
                }
                if let Some(ref serial) = t.serial {
                    if serial.contains(arg) {
                        return true;
                    }
                }
                false
            }
            Self::Addr(addr) => t
                .addresses
                .iter()
                .map(SocketAddr::from)
                .any(|ref mut a| address_matcher(addr, a, t.ssh_port.unwrap_or(22))),
            Self::First => true,
        }
    }

    pub fn match_target_info(&self, t: &TargetInfo) -> bool {
        match self {
            Self::NodenameOrSerial(arg) => {
                if let Some(ref nodename) = t.nodename {
                    if nodename.contains(arg) {
                        return true;
                    }
                }
                if let Some(ref serial) = t.serial_number {
                    if serial.contains(arg) {
                        return true;
                    }
                }
                false
            }
            Self::Addr(addr) => t
                .addresses
                .as_ref()
                .map(|addresses| {
                    addresses.iter().any(|a| {
                        let mut a = target_addr_info_to_socket(a);
                        let ssh_port =
                            if let Some(TargetAddrInfo::IpPort(TargetIpPort { port: tp, .. })) =
                                t.ssh_address
                            {
                                tp
                            } else {
                                22
                            };
                        address_matcher(addr, &mut a, ssh_port)
                    })
                })
                .unwrap_or(false),
            Self::First => true,
        }
    }

    /// Return the invoke discovery on to resolve this query
    pub fn discovery_sources(&self) -> DiscoverySources {
        match self {
            TargetInfoQuery::Addr(_) => {
                DiscoverySources::MDNS | DiscoverySources::MANUAL | DiscoverySources::EMULATOR
            }
            _ => {
                DiscoverySources::MDNS
                    | DiscoverySources::MANUAL
                    | DiscoverySources::EMULATOR
                    | DiscoverySources::USB
            }
        }
    }
}

impl<T> From<Option<T>> for TargetInfoQuery
where
    T: Into<TargetInfoQuery>,
{
    fn from(o: Option<T>) -> Self {
        o.map(Into::into).unwrap_or(Self::First)
    }
}

impl From<&str> for TargetInfoQuery {
    fn from(s: &str) -> Self {
        String::from(s).into()
    }
}

impl From<String> for TargetInfoQuery {
    /// If the string can be parsed as some kind of IP address, will attempt to
    /// match based on that, else fall back to the nodename or serial matches.
    #[tracing::instrument]
    fn from(s: String) -> Self {
        if s == "" {
            return Self::First;
        }
        let (addr, scope, port) = match netext::parse_address_parts(s.as_str()) {
            Ok(r) => r,
            Err(e) => {
                tracing::trace!(
                    "Failed to parse address from '{s}'. Interpreting as nodename: {:?}",
                    e
                );
                return Self::NodenameOrSerial(s);
            }
        };
        // If no such interface exists, just return 0 for a best effort search.
        // This does mean it might be possible to include arbitrary inaccurate scope names for
        // looking up a target, however (like `fe80::1%nonsense`).
        let scope = scope.map(|s| netext::get_verified_scope_id(s).unwrap_or(0)).unwrap_or(0);
        Self::Addr(TargetAddr::new(addr, scope, port.unwrap_or(0)).into())
    }
}

impl From<TargetAddr> for TargetInfoQuery {
    fn from(t: TargetAddr) -> Self {
        Self::Addr(t.into())
    }
}

pub(crate) fn target_addr_info_to_socket(ti: &TargetAddrInfo) -> SocketAddr {
    let (target_ip, port) = match ti {
        TargetAddrInfo::Ip(a) => (a.clone(), 0),
        TargetAddrInfo::IpPort(ip) => (TargetIp { ip: ip.ip, scope_id: ip.scope_id }, ip.port),
    };
    let socket = match target_ip {
        TargetIp { ip: net::IpAddress::Ipv4(net::Ipv4Address { addr }), .. } => {
            SocketAddr::new(IpAddr::from(addr), port)
        }
        TargetIp { ip: net::IpAddress::Ipv6(net::Ipv6Address { addr }), scope_id, .. } => {
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(addr), port, 0, scope_id))
        }
    };
    socket
}

/// Convert a TargetAddrInfo to a SocketAddr preserving the port number if
/// provided, otherwise the returned SocketAddr will have port number 0.
pub fn target_addr_info_to_socketaddr(tai: TargetAddrInfo) -> SocketAddr {
    let mut sa = SocketAddr::from(TargetAddr::from(&tai));
    // TODO(raggi): the port special case needed here indicates a general problem in our
    // addressing strategy that is worth reviewing.
    if let TargetAddrInfo::IpPort(ref ipp) = tai {
        sa.set_port(ipp.port)
    }
    sa
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_discovery_sources() {
        let query = TargetInfoQuery::from("name");
        let sources = query.discovery_sources();
        assert_eq!(
            sources,
            DiscoverySources::MDNS
                | DiscoverySources::MANUAL
                | DiscoverySources::EMULATOR
                | DiscoverySources::USB
        );

        // IP Address shouldn't use USB source
        let query = TargetInfoQuery::from("1.2.3.4");
        let sources = query.discovery_sources();
        assert_eq!(
            sources,
            DiscoverySources::MDNS | DiscoverySources::MANUAL | DiscoverySources::EMULATOR
        );
    }

    #[test]
    fn test_target_addr_info_to_socketaddr() {
        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: net::IpAddress::Ipv4(net::Ipv4Address { addr: [127, 0, 0, 1] }),
            port: 8022,
            scope_id: 0,
        });

        let sa = "127.0.0.1:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::Ip(TargetIp {
            ip: net::IpAddress::Ipv4(net::Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
        });

        let sa = "127.0.0.1:0".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: net::IpAddress::Ipv6(net::Ipv6Address {
                addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            port: 8022,
            scope_id: 0,
        });

        let sa = "[::1]:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::Ip(TargetIp {
            ip: net::IpAddress::Ipv6(net::Ipv6Address {
                addr: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            scope_id: 1,
        });

        let sa = "[fe80::1%1]:0".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);

        let tai = TargetAddrInfo::IpPort(TargetIpPort {
            ip: net::IpAddress::Ipv6(net::Ipv6Address {
                addr: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
            port: 8022,
            scope_id: 1,
        });

        let sa = "[fe80::1%1]:8022".parse::<SocketAddr>().unwrap();

        assert_eq!(target_addr_info_to_socketaddr(tai), sa);
    }
}
