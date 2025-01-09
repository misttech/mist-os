// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net::{IpAddress, SocketAddress};
use fidl_fuchsia_netpol_socketproxy::{
    DnsServerList, FuchsiaNetworkInfo, Network, NetworkDnsServers, NetworkInfo, StarnixNetworkInfo,
};

fn dns_server_list(id: u32) -> DnsServerList {
    DnsServerList { source_network_id: Some(id), addresses: Some(vec![]), ..Default::default() }
}

fn starnix_network_info(mark: u32) -> NetworkInfo {
    NetworkInfo::Starnix(StarnixNetworkInfo {
        mark: Some(mark),
        handle: Some(0),
        ..Default::default()
    })
}

fn starnix_network(network_id: u32) -> Network {
    Network {
        network_id: Some(network_id),
        info: Some(starnix_network_info(network_id)),
        dns_servers: Some(Default::default()),
        ..Default::default()
    }
}

fn fuchsia_network(network_id: u32) -> Network {
    Network {
        network_id: Some(network_id),
        info: Some(NetworkInfo::Fuchsia(FuchsiaNetworkInfo { ..Default::default() })),
        dns_servers: Some(Default::default()),
        ..Default::default()
    }
}

pub trait ToNetwork {
    fn to_network(self, registry: RegistryType) -> Network;
}

pub trait ToDnsServerList {
    fn to_dns_server_list(self) -> DnsServerList;
}

impl ToNetwork for u32 {
    fn to_network(self, registry: RegistryType) -> Network {
        match registry {
            RegistryType::Starnix => starnix_network(self),
            RegistryType::Fuchsia => fuchsia_network(self),
        }
    }
}

impl ToDnsServerList for u32 {
    fn to_dns_server_list(self) -> DnsServerList {
        dns_server_list(self)
    }
}

pub enum RegistryType {
    Starnix,
    Fuchsia,
}

impl ToNetwork for (u32, Vec<IpAddress>) {
    fn to_network(self, registry: RegistryType) -> Network {
        let (v4, v6) = self.1.iter().fold((Vec::new(), Vec::new()), |(mut v4s, mut v6s), s| {
            match s {
                IpAddress::Ipv4(v4) => v4s.push(*v4),
                IpAddress::Ipv6(v6) => v6s.push(*v6),
            }
            (v4s, v6s)
        });
        let base = match registry {
            RegistryType::Starnix => starnix_network(self.0),
            RegistryType::Fuchsia => fuchsia_network(self.0),
        };
        Network {
            dns_servers: Some(NetworkDnsServers {
                v4: Some(v4),
                v6: Some(v6),
                ..Default::default()
            }),
            ..base
        }
    }
}

impl ToDnsServerList for (u32, Vec<SocketAddress>) {
    fn to_dns_server_list(self) -> DnsServerList {
        DnsServerList { addresses: Some(self.1), ..dns_server_list(self.0) }
    }
}

impl<N: ToNetwork + Clone> ToNetwork for &N {
    fn to_network(self, registry: RegistryType) -> Network {
        self.clone().to_network(registry)
    }
}

impl<D: ToDnsServerList + Clone> ToDnsServerList for &D {
    fn to_dns_server_list(self) -> DnsServerList {
        self.clone().to_dns_server_list()
    }
}
