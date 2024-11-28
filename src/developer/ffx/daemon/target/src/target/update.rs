// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use chrono::Utc;
use ffx_daemon_events::TargetConnectionState;

use ffx_ssh::parse::HostAddr;
use std::net::{Ipv4Addr, Ipv6Addr};

use super::{
    BuildConfig, Identity, SharedIdentity, TargetAddrEntry, TargetAddrStatus, TargetProtocol,
    TargetTransport,
};
use std::net::{IpAddr, SocketAddr};
use std::time::{Instant, SystemTime};

#[derive(Clone, Debug)]
enum ConnectionKind {
    // No longer know what is supported due to expiry.
    Disconnected,
    // A device has been found/discovered that supports these
    // protocols.
    Found { protocol: TargetProtocol, transport: TargetTransport },
    Rcs(rcs::RcsConnection),
}

#[derive(Clone, Debug, Default)]
struct SshConfig {
    port: Option<Option<u16>>,
    host_addr: Option<Option<HostAddr>>,
}

#[derive(Clone, Debug)]
struct Connection<'a> {
    kind: ConnectionKind,
    /// Addresses to update the target with.
    net_address: &'a [SocketAddr],
}

impl<'a> Default for Connection<'a> {
    fn default() -> Self {
        Self { kind: ConnectionKind::Disconnected, net_address: &[] }
    }
}

#[derive(Clone, Debug, Default)]
struct Trivia {
    build_config: Option<Option<BuildConfig>>,
    boot_timestamp_nanos: Option<Option<u64>>,

    rcs_compatibility: Option<Option<compat_info::CompatibilityInfo>>,
}

/// Records how to update a target.
#[derive(Clone, Debug, Default)]
pub struct TargetUpdate<'a> {
    /// Whether this is a manual target vs an expiring target.
    ///
    /// Targets cannot be unmarked as manual.
    manual_target: bool,
    transient_target: bool,
    enabled: Option<bool>,
    last_seen: Option<Instant>,
    expiry: Option<SystemTime>,
    ids: &'a [u64],

    /// The known identity of the target.
    ///
    /// NOTE: Replaces previous identity.
    pub(crate) identity: Option<SharedIdentity>,

    /// The target's new connection state.
    connection: Option<Connection<'a>>,

    ssh_config: Option<SshConfig>,

    /// Extra information about a target.
    trivia: Trivia,
}

impl<'a> TargetUpdate<'a> {
    fn determine_addr_status(&self) -> Option<TargetAddrStatus> {
        let connection = self.connection.as_ref()?;

        use ConnectionKind::*;
        let mut status = match connection.kind {
            Disconnected => return None,
            // An assumption that may not be correct going forward.
            Rcs(_) => TargetAddrStatus::ssh(),
            Found { protocol: TargetProtocol::Fastboot, transport } => {
                TargetAddrStatus::fastboot(transport)
            }
            Found { protocol: TargetProtocol::Ssh, .. } => TargetAddrStatus::ssh(),
            Found { protocol: TargetProtocol::Netsvc, .. } => TargetAddrStatus::netsvc(),
        };

        if self.manual_target {
            if let Some(expiry) = self.expiry {
                status = status.manually_added_until(expiry);
            } else {
                status = status.manually_added();
            }
        }

        Some(status)
    }
}

#[derive(Clone, Debug)]
pub struct TargetUpdateBuilder<'a>(TargetUpdate<'a>);

impl<'a> std::ops::Deref for TargetUpdateBuilder<'a> {
    type Target = TargetUpdate<'a>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> std::ops::DerefMut for TargetUpdateBuilder<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a> TargetUpdateBuilder<'a> {
    pub fn new() -> Self {
        Self(Default::default())
    }

    pub fn from_rcs_identify(
        rcs: rcs::RcsConnection,
        identify: &'a super::IdentifyHostResponse,
    ) -> (Self, Vec<SocketAddr>) {
        // NOTE: Addresses from Overnet may not be routable so we only use them as a filter.
        // In device tunneling scenarios Private/LLA/ULA addresses may not be reachable.
        // This is a non-issue when multiple SSH addresses are tested, instead of a single one.

        use fidl_fuchsia_net as net;

        let addrs = identify
            .addresses
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|&net::Subnet { addr, .. }| {
                let addr: IpAddr = match addr {
                    net::IpAddress::Ipv4(net::Ipv4Address { addr }) => Ipv4Addr::from(addr).into(),
                    net::IpAddress::Ipv6(net::Ipv6Address { addr }) => Ipv6Addr::from(addr).into(),
                };

                // Filter out addresses that are device-local or not unicast.
                match addr {
                    _ if addr.is_loopback() || addr.is_unspecified() || addr.is_multicast() => {
                        return None
                    }
                    _ => {}
                }

                Some(SocketAddr::new(addr, 0))
            })
            .collect::<Vec<_>>();

        let mut update = Self::new().rcs(rcs).ids(identify.ids.as_deref().unwrap_or(&[]));

        let identity = Identity::try_from_name_serial(
            identify.nodename.clone(),
            identify.serial_number.clone(),
        );

        if let Some(identity) = identity {
            update = update.identity(identity);
        }

        update = update.boot_timestamp_nanos(identify.boot_timestamp_nanos);

        update = update.build_config(match (&identify.product_config, &identify.board_config) {
            (None, None) => None,
            (product, board) => Some(BuildConfig {
                product_config: product.as_deref().unwrap_or("<unknown>").into(),
                board_config: board.as_deref().unwrap_or("<unknown>").into(),
            }),
        });

        (update, addrs)
    }

    pub fn manual_target(mut self, expiry: Option<SystemTime>) -> Self {
        self.manual_target = true;
        self.expiry = expiry;
        self
    }

    pub fn transient_target(mut self) -> Self {
        self.transient_target = true;
        self
    }

    pub fn enable(mut self) -> Self {
        self.enabled = Some(true);
        self
    }

    pub fn last_seen(mut self, last_seen: Instant) -> Self {
        self.last_seen = Some(last_seen);
        self
    }

    pub fn ids(mut self, ids: &'a [u64]) -> Self {
        self.ids = ids;
        self
    }

    pub fn identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity.into());
        self
    }

    fn connection_mut(&mut self) -> &mut Connection<'a> {
        self.connection.get_or_insert_with(Default::default)
    }

    pub fn net_addresses(mut self, addrs: &'a [SocketAddr]) -> Self {
        self.connection_mut().net_address = addrs;
        self
    }

    pub fn discovered(mut self, protocol: TargetProtocol, transport: TargetTransport) -> Self {
        self.connection_mut().kind = ConnectionKind::Found { protocol, transport };
        self
    }

    pub fn rcs(mut self, connection: rcs::RcsConnection) -> Self {
        self.connection_mut().kind = ConnectionKind::Rcs(connection);
        self
    }

    pub fn disconnected(mut self) -> Self {
        self.connection_mut().kind = ConnectionKind::Disconnected;
        self
    }

    fn ssh_config_mut(&mut self) -> &mut SshConfig {
        self.ssh_config.get_or_insert_with(Default::default)
    }

    pub fn ssh_port(mut self, port: Option<u16>) -> Self {
        self.ssh_config_mut().port = Some(port);
        self
    }

    pub fn ssh_host_addr(mut self, host: Option<HostAddr>) -> Self {
        self.ssh_config_mut().host_addr = Some(host);
        self
    }

    pub fn build_config(mut self, config: Option<BuildConfig>) -> Self {
        self.trivia.build_config = Some(config);
        self
    }

    pub fn boot_timestamp_nanos(mut self, ts_nanos: Option<u64>) -> Self {
        self.trivia.boot_timestamp_nanos = Some(ts_nanos);
        self
    }

    pub fn rcs_compatibility(mut self, info: Option<compat_info::CompatibilityInfo>) -> Self {
        self.trivia.rcs_compatibility = Some(info);
        self
    }

    pub fn build(self) -> TargetUpdate<'a> {
        self.0
    }
}

impl super::Target {
    /// Updates the state of the target with the changes described in `update`.
    pub fn apply_update(&self, mut update: TargetUpdate<'_>) {
        let addr_status = update.determine_addr_status();

        match update.enabled {
            _ if update.manual_target => self.enable(),
            Some(true) => self.enable(),
            Some(false) => self.disable(),
            _ => {}
        }

        if update.transient_target {
            self.mark_transient();
        }

        if !update.ids.is_empty() {
            self.merge_ids(update.ids.iter());
        }

        if let Some(identity) = update.identity {
            self.replace_shared_identity(identity);
        }

        if let Some(connection) = update.connection.take() {
            if let Some(status) = addr_status {
                let now = Utc::now();

                self.addrs_extend(
                    connection
                        .net_address
                        .iter()
                        .map(|addr| TargetAddrEntry::new((*addr).into(), now, status)),
                );
            }

            let last_seen = update.last_seen.unwrap_or_else(Instant::now);

            match connection.kind {
                ConnectionKind::Rcs(rcs) => {
                    // RCS connected.
                    let has_host_pipe = self.is_host_pipe_running();
                    let id = rcs.overnet_id.id;
                    if id != self.id {
                        self.ids.borrow_mut().insert(id);
                    }

                    // Only replace if we don't own a host pipe.
                    if self.id == id || !has_host_pipe {
                        self.update_connection_state(|_| TargetConnectionState::Rcs(rcs));
                    }
                }
                ConnectionKind::Found { protocol, transport } => match protocol {
                    TargetProtocol::Ssh if update.manual_target => {
                        self.update_connection_state(|_| {
                            TargetConnectionState::Manual(update.expiry.map(|_| last_seen))
                        });
                    }
                    TargetProtocol::Ssh => {
                        self.update_connection_state(|_| TargetConnectionState::Mdns(last_seen));
                    }
                    TargetProtocol::Fastboot => {
                        self.update_connection_state(|_| {
                            TargetConnectionState::Fastboot(last_seen)
                        });

                        *self.fastboot_interface.borrow_mut() = Some(transport.into());
                    }
                    TargetProtocol::Netsvc => {
                        self.update_connection_state(|_| TargetConnectionState::Zedboot(last_seen));
                    }
                },
                ConnectionKind::Disconnected => {
                    // Disconnected. Clear other known IDs and remove connection.
                    // State transition is handled by disconnect.
                    self.disconnect();
                }
            }
        }

        if let Some(ssh_config) = update.ssh_config.take() {
            if let Some(port) = ssh_config.port {
                self.set_ssh_port(port);
            }

            if let Some(host) = ssh_config.host_addr {
                self.ssh_host_address.replace(host);
            }
        }

        let Trivia { build_config, boot_timestamp_nanos, rcs_compatibility } = update.trivia;

        if let Some(build_config) = build_config {
            self.build_config.replace(build_config);
        }

        if let Some(ts) = boot_timestamp_nanos {
            self.boot_timestamp_nanos.replace(ts);
        }

        if let Some(info) = rcs_compatibility {
            self.compatibility_status.replace(info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    use assert_matches::assert_matches;
    use fidl_fuchsia_overnet_protocol::NodeId;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn update_simple() {
        let target = Target::new();

        assert!(!target.is_transient());
        assert!(!target.has_identity());
        assert_eq!(target.serial(), None);
        assert_eq!(target.nodename(), None);

        assert_eq!(*target.ssh_port.borrow(), None);
        assert_eq!(*target.ssh_host_address.borrow(), None);
        assert_eq!(*target.build_config.borrow(), None);
        assert_eq!(*target.boot_timestamp_nanos.borrow(), None);
        assert_eq!(*target.compatibility_status.borrow(), None);

        let host_addr = HostAddr::from("foo".to_string());
        let build_config =
            BuildConfig { product_config: "product".into(), board_config: "board".into() };

        let compatibility = compat_info::CompatibilityInfo {
            status: compat_info::CompatibilityState::Error,
            platform_abi: 0,
            message: "Error message here".into(),
        };

        target.apply_update(
            TargetUpdateBuilder::new()
                .transient_target()
                .ids(&[1, 2, 3])
                .identity(
                    Identity::from_name("nodename").join_with(Identity::from_serial("serial")),
                )
                .ssh_port(Some(2122))
                .ssh_host_addr(Some(host_addr.clone()))
                .build_config(Some(build_config.clone()))
                .boot_timestamp_nanos(Some(1234))
                .rcs_compatibility(Some(compatibility.clone()))
                .build(),
        );

        assert!(target.has_id([1, 2, 3].iter()));

        assert!(target.is_transient());
        assert!(target.has_identity());
        assert_eq!(target.serial().as_deref(), Some("serial"));
        assert_eq!(target.nodename().as_deref(), Some("nodename"));

        assert_eq!(*target.ssh_port.borrow(), Some(2122));
        assert_eq!(*target.ssh_host_address.borrow(), Some(host_addr));
        assert_eq!(*target.build_config.borrow(), Some(build_config));
        assert_eq!(*target.boot_timestamp_nanos.borrow(), Some(1234));
        assert_eq!(*target.compatibility_status.borrow(), Some(compatibility));

        target.apply_update(
            TargetUpdateBuilder::new()
                .ssh_port(None)
                .ssh_host_addr(None)
                .build_config(None)
                .boot_timestamp_nanos(None)
                .rcs_compatibility(None)
                .build(),
        );

        assert_eq!(*target.ssh_port.borrow(), None);
        assert_eq!(*target.ssh_host_address.borrow(), None);
        assert_eq!(*target.build_config.borrow(), None);
        assert_eq!(*target.boot_timestamp_nanos.borrow(), None);
        assert_eq!(*target.compatibility_status.borrow(), None);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn update_discovered() {
        let now = Instant::now();

        const ADDRS: &[SocketAddr] = &[
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8022),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8022),
        ];

        #[track_caller]
        fn expect_addr_type(target: &Target, addr_type: TargetAddrStatus) {
            let addrs = target.addrs.borrow();
            for addr in addrs.iter() {
                assert_eq!(addr.status, addr_type);
            }
        }

        {
            let target = Target::new();

            target.apply_update(
                TargetUpdateBuilder::new()
                    .last_seen(now)
                    .discovered(TargetProtocol::Ssh, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_eq!(target.get_connection_state(), TargetConnectionState::Mdns(now));
            expect_addr_type(&target, TargetAddrStatus::ssh());
        }

        {
            let target = Target::new();

            target.apply_update(
                TargetUpdateBuilder::new()
                    .last_seen(now)
                    .discovered(TargetProtocol::Fastboot, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_eq!(target.get_connection_state(), TargetConnectionState::Fastboot(now));
            expect_addr_type(&target, TargetAddrStatus::fastboot(TargetTransport::Network));
        }

        {
            let target = Target::new();

            target.apply_update(
                TargetUpdateBuilder::new()
                    .last_seen(now)
                    .discovered(TargetProtocol::Netsvc, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_eq!(target.get_connection_state(), TargetConnectionState::Zedboot(now));
            expect_addr_type(&target, TargetAddrStatus::netsvc());
        }

        {
            let target = Target::new();

            target.apply_update(
                TargetUpdateBuilder::new()
                    .manual_target(None)
                    .last_seen(now)
                    .discovered(TargetProtocol::Ssh, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_eq!(target.get_connection_state(), TargetConnectionState::Manual(None));
            expect_addr_type(&target, TargetAddrStatus::ssh().manually_added());
        }

        {
            let target = Target::new();

            target.apply_update(
                TargetUpdateBuilder::new()
                    .manual_target(None)
                    .last_seen(now)
                    .discovered(TargetProtocol::Fastboot, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_matches!(target.get_connection_state(), TargetConnectionState::Fastboot(..));
            expect_addr_type(
                &target,
                TargetAddrStatus::fastboot(TargetTransport::Network).manually_added(),
            );
            assert!(target.is_manual());
        }

        {
            let target = Target::new();
            let expire = SystemTime::now() + Duration::from_secs(60);

            target.apply_update(
                TargetUpdateBuilder::new()
                    .manual_target(Some(expire))
                    .last_seen(now)
                    .discovered(TargetProtocol::Ssh, TargetTransport::Network)
                    .net_addresses(ADDRS)
                    .build(),
            );

            assert_eq!(target.get_connection_state(), TargetConnectionState::Manual(Some(now)));
            expect_addr_type(&target, TargetAddrStatus::ssh().manually_added_until(expire));
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn update_disconnect() {
        let now = Instant::now();

        let target = Target::new();

        target.apply_update(
            TargetUpdateBuilder::new()
                .last_seen(now)
                .discovered(TargetProtocol::Ssh, TargetTransport::Network)
                .build(),
        );

        assert_eq!(target.get_connection_state(), TargetConnectionState::Mdns(now));

        target.apply_update(TargetUpdateBuilder::new().disconnected().build());

        assert_eq!(target.get_connection_state(), TargetConnectionState::Disconnected);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn update_rcs() {
        let now = Instant::now();

        let target = Target::new();

        target.apply_update(
            TargetUpdateBuilder::new()
                .last_seen(now)
                .discovered(TargetProtocol::Ssh, TargetTransport::Network)
                .build(),
        );

        assert_eq!(target.get_connection_state(), TargetConnectionState::Mdns(now));

        use fidl_fuchsia_developer_remotecontrol as rcs;

        let local_node = overnet_core::Router::new(None).unwrap();
        let (proxy, _stream) =
            fidl::endpoints::create_proxy_and_stream::<rcs::RemoteControlMarker>();
        let conn = RcsConnection::new_with_proxy(local_node, proxy, &NodeId { id: 123456 });

        target.apply_update(TargetUpdateBuilder::new().rcs(conn.clone()).build());

        assert_eq!(target.get_connection_state(), TargetConnectionState::Rcs(conn));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn from_rcs_identify() {
        use fidl_fuchsia_developer_remotecontrol as rcs;

        let local_node = overnet_core::Router::new(None).unwrap();
        let (proxy, _stream) =
            fidl::endpoints::create_proxy_and_stream::<rcs::RemoteControlMarker>();
        let conn = RcsConnection::new_with_proxy(local_node, proxy, &NodeId { id: 123456 });

        use fidl_fuchsia_net as net;

        fn to_subnet(addr: IpAddr) -> net::Subnet {
            net::Subnet {
                addr: match addr {
                    IpAddr::V4(addr) => {
                        net::IpAddress::Ipv4(net::Ipv4Address { addr: addr.octets() })
                    }
                    IpAddr::V6(addr) => {
                        net::IpAddress::Ipv6(net::Ipv6Address { addr: addr.octets() })
                    }
                },
                prefix_len: 0,
            }
        }

        let invalid: &[IpAddr] = &[
            // IPv4
            Ipv4Addr::LOCALHOST.into(),
            Ipv4Addr::UNSPECIFIED.into(),
            Ipv4Addr::new(224, 254, 0, 0).into(), // Multicast
            // IPv6
            Ipv6Addr::LOCALHOST.into(),
            Ipv6Addr::UNSPECIFIED.into(),
            Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0).into(), // Multicast
        ];

        let valid: &[IpAddr] = &[
            // IPv4 Private Network
            Ipv4Addr::new(192, 168, 1, 5).into(),
            // IPv4 Link-Local
            Ipv4Addr::new(169, 254, 0, 0).into(),
            // IPv4 Internet
            Ipv4Addr::new(22, 22, 22, 22).into(),
            // IPv6 Unique Local Address
            "fd00::1".parse().unwrap(),
            // IPv6 Unicast Link-Local
            "fe80::1234".parse().unwrap(),
            // IPv6 Internet
            "2001::1234".parse().unwrap(),
        ];

        let identify_host = IdentifyHostResponse {
            nodename: Some("name!".into()),
            boot_timestamp_nanos: Some(1234),
            serial_number: Some("cereal".into()),
            ids: Some(vec![1, 2, 3]),
            product_config: Some("product".into()),
            board_config: Some("board".into()),
            addresses: Some(invalid.iter().chain(valid.iter()).cloned().map(to_subnet).collect()),
            ..Default::default()
        };

        let (update, addr_filter) = TargetUpdateBuilder::from_rcs_identify(conn, &identify_host);

        let target = Target::new();

        target.apply_update(update.build());

        assert_eq!(target.nodename().as_deref(), Some("name!"));
        assert_eq!(target.serial().as_deref(), Some("cereal"));
        assert_eq!(target.boot_timestamp_nanos(), Some(1234));
        assert!(target.has_id([1, 2, 3].iter()));
        assert_eq!(
            target.build_config(),
            Some(BuildConfig { product_config: "product".into(), board_config: "board".into() })
        );

        assert_eq!(addr_filter, valid.iter().map(|ip| SocketAddr::new(*ip, 0)).collect::<Vec<_>>());
    }
}
