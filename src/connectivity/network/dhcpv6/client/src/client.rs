// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements a DHCPv6 client.
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, SocketAddr},
    num::NonZeroU8,
    ops::Add,
    str::FromStr as _,
    time::Duration,
};

use fidl::endpoints::ServerEnd;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcpv6::{
    AddressConfig, ClientConfig, ClientMarker, ClientRequest, ClientRequestStream,
    ClientWatchAddressResponder, ClientWatchPrefixesResponder, ClientWatchServersResponder, Empty,
    InformationConfig, Lifetimes, NewClientParams, Prefix, PrefixDelegationConfig,
    RELAY_AGENT_AND_SERVER_LINK_LOCAL_MULTICAST_ADDRESS, RELAY_AGENT_AND_SERVER_PORT,
};
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_name as fnet_name;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    future::{AbortHandle, Abortable, Aborted},
    select, stream,
    stream::futures_unordered::FuturesUnordered,
    Future, FutureExt as _, StreamExt as _, TryStreamExt as _,
};

use anyhow::{Context as _, Result};
use assert_matches::assert_matches;
use async_utils::futures::{FutureExt as _, ReplaceValue};
use dns_server_watcher::DEFAULT_DNS_PORT;
use net_types::{
    ip::{Ip as _, Ipv6, Ipv6Addr, Subnet, SubnetError},
    MulticastAddress as _,
};
use packet::ParsablePacket;
use packet_formats_dhcp::v6;
use rand::{rngs::StdRng, SeedableRng};
use tracing::{debug, error, warn};

use dhcpv6_core;

/// A thin wrapper around `zx::Time` that implements `dhcpv6_core::Instant`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug)]
pub(crate) struct MonotonicTime(zx::Time);

impl MonotonicTime {
    fn now() -> MonotonicTime {
        MonotonicTime(zx::Time::get_monotonic())
    }
}

impl dhcpv6_core::Instant for MonotonicTime {
    fn duration_since(&self, MonotonicTime(earlier): MonotonicTime) -> Duration {
        let Self(this) = *self;

        let diff: zx::Duration = this - earlier;

        Duration::from_nanos(diff.into_nanos().try_into().unwrap_or_else(|e| {
            panic!(
                "failed to calculate duration since {:?} with instant {:?}: {}",
                earlier, this, e,
            )
        }))
    }

    fn checked_add(&self, duration: Duration) -> Option<MonotonicTime> {
        Some(self.add(duration))
    }
}

impl Add<Duration> for MonotonicTime {
    type Output = MonotonicTime;

    fn add(self, duration: Duration) -> MonotonicTime {
        let MonotonicTime(this) = self;
        MonotonicTime(this + duration.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("fidl error")]
    Fidl(#[source] fidl::Error),
    #[error("got watch request while the previous one is pending")]
    DoubleWatch,
    #[error("unsupported DHCPv6 configuration")]
    UnsupportedConfigs,
    #[error("socket receive error")]
    SocketRecv(#[source] std::io::Error),
    #[error("unimplemented DHCPv6 functionality: {:?}()", _0)]
    Unimplemented(String),
}

/// Theoretical size limit for UDP datagrams.
///
/// NOTE: This does not take [jumbograms](https://tools.ietf.org/html/rfc2675) into account.
const MAX_UDP_DATAGRAM_SIZE: usize = 65_535;

type TimerFut = ReplaceValue<fasync::Timer, dhcpv6_core::client::ClientTimerType>;

/// A DHCPv6 client.
pub(crate) struct Client<S: for<'a> AsyncSocket<'a>> {
    /// The interface the client is running on.
    interface_id: u64,
    /// Stores the hash of the last observed version of DNS servers by a watcher.
    ///
    /// The client uses this hash to determine whether new changes in DNS servers are observed and
    /// updates should be replied to the watcher.
    last_observed_dns_hash: u64,
    /// Stores a responder to send DNS server updates.
    dns_responder: Option<ClientWatchServersResponder>,
    /// Stores a responder to send acquired addresses.
    address_responder: Option<ClientWatchAddressResponder>,
    /// Holds the discovered prefixes and their lifetimes.
    prefixes: HashMap<fnet::Ipv6AddressWithPrefix, Lifetimes>,
    /// Indicates whether or not the prefixes has changed since last yielded.
    prefixes_changed: bool,
    /// Stores a responder to send acquired prefixes.
    prefixes_responder: Option<ClientWatchPrefixesResponder>,
    /// Maintains the state for the client.
    state_machine: dhcpv6_core::client::ClientStateMachine<MonotonicTime, StdRng>,
    /// The socket used to communicate with DHCPv6 servers.
    socket: S,
    /// The address to send outgoing messages to.
    server_addr: SocketAddr,
    /// A collection of abort handles to all currently scheduled timers.
    timer_abort_handles: HashMap<dhcpv6_core::client::ClientTimerType, AbortHandle>,
    /// A set of all scheduled timers.
    timer_futs: FuturesUnordered<Abortable<TimerFut>>,
    /// A stream of FIDL requests to this client.
    request_stream: ClientRequestStream,
}

/// A trait that allows stubbing [`fuchsia_async::net::UdpSocket`] in tests.
pub(crate) trait AsyncSocket<'a> {
    type RecvFromFut: Future<Output = Result<(usize, SocketAddr), std::io::Error>> + 'a;
    type SendToFut: Future<Output = Result<usize, std::io::Error>> + 'a;

    fn recv_from(&'a self, buf: &'a mut [u8]) -> Self::RecvFromFut;
    fn send_to(&'a self, buf: &'a [u8], addr: SocketAddr) -> Self::SendToFut;
}

impl<'a> AsyncSocket<'a> for fasync::net::UdpSocket {
    type RecvFromFut = fasync::net::UdpRecvFrom<'a>;
    type SendToFut = fasync::net::SendTo<'a>;

    fn recv_from(&'a self, buf: &'a mut [u8]) -> Self::RecvFromFut {
        self.recv_from(buf)
    }
    fn send_to(&'a self, buf: &'a [u8], addr: SocketAddr) -> Self::SendToFut {
        self.send_to(buf, addr)
    }
}

/// Converts `InformationConfig` to a collection of `v6::OptionCode`.
fn to_dhcpv6_option_codes(information_config: InformationConfig) -> Vec<v6::OptionCode> {
    let InformationConfig { dns_servers, .. } = information_config;
    let mut codes = Vec::new();

    if dns_servers.unwrap_or(false) {
        let () = codes.push(v6::OptionCode::DnsServers);
    }
    codes
}

fn to_configured_addresses(
    address_config: AddressConfig,
) -> Result<HashMap<v6::IAID, HashSet<Ipv6Addr>>, ClientError> {
    let AddressConfig { address_count, preferred_addresses, .. } = address_config;
    let address_count =
        address_count.and_then(NonZeroU8::new).ok_or(ClientError::UnsupportedConfigs)?;
    let preferred_addresses = preferred_addresses.unwrap_or(Vec::new());
    if preferred_addresses.len() > address_count.get().into() {
        return Err(ClientError::UnsupportedConfigs);
    }

    // TODO(https://fxbug.dev/77790): make IAID consistent across
    // configurations.
    Ok((0..)
        .map(v6::IAID::new)
        .zip(
            preferred_addresses
                .into_iter()
                .map(|fnet::Ipv6Address { addr, .. }| HashSet::from([Ipv6Addr::from(addr)]))
                .chain(std::iter::repeat_with(HashSet::new)),
        )
        .take(address_count.get().into())
        .collect())
}

// The client only supports a single IA_PD.
//
// TODO(https://fxbug.dev/114132): Support multiple IA_PDs.
const IA_PD_IAID: v6::IAID = v6::IAID::new(0);

/// Creates a state machine for the input client config.
fn create_state_machine(
    transaction_id: [u8; 3],
    config: ClientConfig,
) -> Result<
    (
        dhcpv6_core::client::ClientStateMachine<MonotonicTime, StdRng>,
        dhcpv6_core::client::Actions<MonotonicTime>,
    ),
    ClientError,
> {
    let ClientConfig {
        information_config,
        non_temporary_address_config,
        prefix_delegation_config,
        ..
    } = config;
    let information_config = information_config.map(to_dhcpv6_option_codes);
    let configured_non_temporary_addresses =
        non_temporary_address_config.map(to_configured_addresses).transpose()?;
    let configured_delegated_prefixes = prefix_delegation_config
        .map(|prefix_delegation_config| {
            let prefix = match prefix_delegation_config {
                PrefixDelegationConfig::Empty(Empty {}) => Ok(None),
                PrefixDelegationConfig::PrefixLength(prefix_len) => {
                    if prefix_len == 0 {
                        // Should have used `PrefixDelegationConfig::Empty`.
                        return Err(ClientError::UnsupportedConfigs);
                    }

                    Subnet::new(Ipv6::UNSPECIFIED_ADDRESS, prefix_len).map(Some)
                }
                PrefixDelegationConfig::Prefix(fnet::Ipv6AddressWithPrefix {
                    addr: fnet::Ipv6Address { addr, .. },
                    prefix_len,
                }) => {
                    let addr = Ipv6Addr::from_bytes(addr);
                    if addr == Ipv6::UNSPECIFIED_ADDRESS {
                        // Should have used `PrefixDelegationConfig::PrefixLength`.
                        return Err(ClientError::UnsupportedConfigs);
                    }

                    Subnet::new(addr, prefix_len).map(Some)
                }
            };

            match prefix {
                Ok(o) => Ok(HashMap::from([(IA_PD_IAID, HashSet::from_iter(o.into_iter()))])),
                Err(SubnetError::PrefixTooLong | SubnetError::HostBitsSet) => {
                    Err(ClientError::UnsupportedConfigs)
                }
            }
        })
        .transpose()?;

    let now = MonotonicTime::now();
    match (information_config, configured_non_temporary_addresses, configured_delegated_prefixes) {
        (None, None, None) => Err(ClientError::UnsupportedConfigs),
        (Some(information_config), None, None) => {
            Ok(dhcpv6_core::client::ClientStateMachine::start_stateless(
                transaction_id,
                information_config,
                StdRng::from_entropy(),
                now,
            ))
        }
        (information_config, configured_non_temporary_addresses, configured_delegated_prefixes) => {
            Ok(dhcpv6_core::client::ClientStateMachine::start_stateful(
                transaction_id,
                v6::duid_uuid(),
                configured_non_temporary_addresses.unwrap_or_else(Default::default),
                configured_delegated_prefixes.unwrap_or_else(Default::default),
                information_config.unwrap_or_else(Default::default),
                StdRng::from_entropy(),
                now,
            ))
        }
    }
}

/// Calculates a hash for the input.
fn hash<H: Hash>(h: &H) -> u64 {
    let mut dh = DefaultHasher::new();
    let () = h.hash(&mut dh);
    dh.finish()
}

fn subnet_to_address_with_prefix(prefix: Subnet<Ipv6Addr>) -> fnet::Ipv6AddressWithPrefix {
    fnet::Ipv6AddressWithPrefix {
        addr: fnet::Ipv6Address { addr: prefix.network().ipv6_bytes() },
        prefix_len: prefix.prefix(),
    }
}

impl<S: for<'a> AsyncSocket<'a>> Client<S> {
    /// Starts the client in `config`.
    ///
    /// Input `transaction_id` is used to label outgoing messages and match incoming ones.
    pub(crate) async fn start(
        transaction_id: [u8; 3],
        config: ClientConfig,
        interface_id: u64,
        socket: S,
        server_addr: SocketAddr,
        request_stream: ClientRequestStream,
    ) -> Result<Self, ClientError> {
        let (state_machine, actions) = create_state_machine(transaction_id, config)?;
        let mut client = Self {
            state_machine,
            interface_id,
            socket,
            server_addr,
            request_stream,
            timer_abort_handles: HashMap::new(),
            timer_futs: FuturesUnordered::new(),
            // Server watcher's API requires blocking iff the first call would return an empty list,
            // so initialize this field with a hash of an empty list.
            last_observed_dns_hash: hash(&Vec::<Ipv6Addr>::new()),
            dns_responder: None,
            address_responder: None,
            prefixes: Default::default(),
            prefixes_changed: false,
            prefixes_responder: None,
        };
        let () = client.run_actions(actions).await?;
        Ok(client)
    }

    /// Runs a list of actions sequentially.
    async fn run_actions(
        &mut self,
        actions: dhcpv6_core::client::Actions<MonotonicTime>,
    ) -> Result<(), ClientError> {
        stream::iter(actions)
            .map(Ok)
            .try_fold(self, |client, action| async move {
                match action {
                    dhcpv6_core::client::Action::SendMessage(buf) => {
                        let () = match client.socket.send_to(&buf, client.server_addr).await {
                            Ok(size) => assert_eq!(size, buf.len()),
                            Err(e) => warn!(
                                "failed to send message to {}: {}; will retransmit later",
                                client.server_addr, e
                            ),
                        };
                    }
                    dhcpv6_core::client::Action::ScheduleTimer(timer_type, timeout) => {
                        client.schedule_timer(timer_type, timeout)
                    }
                    dhcpv6_core::client::Action::CancelTimer(timer_type) => {
                        client.cancel_timer(timer_type)
                    }
                    dhcpv6_core::client::Action::UpdateDnsServers(servers) => {
                        let () = client.maybe_send_dns_server_updates(servers)?;
                    }
                    dhcpv6_core::client::Action::IaNaUpdates(_) => {
                        // TODO(https://fxbug.dev/96684): add actions to
                        // (re)schedule preferred and valid lifetime timers.
                        // TODO(https://fxbug.dev/96674): Add
                        // action to remove the previous address.
                        // TODO(https://fxbug.dev/95265): Add action to add
                        // the new address and cancel timers for old address.
                    }
                    dhcpv6_core::client::Action::IaPdUpdates(mut updates) => {
                        let updates = {
                            let ret =
                                updates.remove(&IA_PD_IAID).expect("Update missing for IAID");
                            debug_assert_eq!(updates, HashMap::new());
                            ret
                        };

                        let Self { prefixes, prefixes_changed, .. } = client;

                        let now = zx::Time::get_monotonic();
                        let nonzero_timevalue_to_zx_time = |tv| match tv {
                            v6::NonZeroTimeValue::Finite(tv) => {
                                now + zx::Duration::from_seconds(tv.get().into())
                            }
                            v6::NonZeroTimeValue::Infinity => zx::Time::INFINITE,
                        };

                        let calculate_lifetimes = |dhcpv6_core::client::Lifetimes {
                            preferred_lifetime,
                            valid_lifetime,
                        }| {
                            Lifetimes {
                                preferred_until: match preferred_lifetime {
                                    v6::TimeValue::Zero => zx::Time::ZERO,
                                    v6::TimeValue::NonZero(preferred_lifetime) => {
                                        nonzero_timevalue_to_zx_time(preferred_lifetime)
                                    },
                                }.into_nanos(),
                                valid_until: nonzero_timevalue_to_zx_time(valid_lifetime)
                                    .into_nanos(),
                            }
                        };

                        for (prefix, update) in updates.into_iter() {
                            let fidl_prefix = subnet_to_address_with_prefix(prefix);

                            match update {
                                dhcpv6_core::client::IaValueUpdateKind::Added(lifetimes) => {
                                    assert_matches!(
                                        prefixes.insert(
                                            fidl_prefix,
                                            calculate_lifetimes(lifetimes)
                                        ),
                                        None,
                                        "must not know about prefix {} to add it with lifetimes {:?}",
                                        prefix, lifetimes,
                                    );
                                }
                                dhcpv6_core::client::IaValueUpdateKind::UpdatedLifetimes(updated_lifetimes) => {
                                    assert_matches!(
                                        prefixes.get_mut(&fidl_prefix),
                                        Some(lifetimes) => {
                                            *lifetimes = calculate_lifetimes(updated_lifetimes);
                                        },
                                        "must know about prefix {} to update lifetimes with {:?}",
                                        prefix, updated_lifetimes,
                                    );
                                }
                                dhcpv6_core::client::IaValueUpdateKind::Removed => {
                                    assert_matches!(
                                        prefixes.remove(&fidl_prefix),
                                        Some(_),
                                        "must know about prefix {} to remove it",
                                        prefix
                                    );
                                }
                            }
                        }

                        // Mark the client has having updated prefixes so that
                        // callers of `WatchPrefixes` receive the update.
                        *prefixes_changed = true;
                        client.maybe_send_prefixes()?;
                    }
                };
                Ok(client)
            })
            .await
            .map(|_: &mut Client<S>| ())
    }

    /// Sends the latest DNS servers if a watcher is watching, and the latest set of servers are
    /// different from what the watcher has observed last time.
    fn maybe_send_dns_server_updates(&mut self, servers: Vec<Ipv6Addr>) -> Result<(), ClientError> {
        let servers_hash = hash(&servers);
        if servers_hash == self.last_observed_dns_hash {
            Ok(())
        } else {
            Ok(match self.dns_responder.take() {
                Some(responder) => {
                    self.send_dns_server_updates(responder, servers, servers_hash)?
                }
                None => (),
            })
        }
    }

    fn maybe_send_prefixes(&mut self) -> Result<(), ClientError> {
        let Self { prefixes, prefixes_changed, prefixes_responder, .. } = self;

        if !*prefixes_changed {
            return Ok(());
        }

        let responder = if let Some(responder) = prefixes_responder.take() {
            responder
        } else {
            return Ok(());
        };

        let prefixes = prefixes
            .iter()
            .map(|(prefix, lifetimes)| Prefix { prefix: *prefix, lifetimes: *lifetimes })
            .collect::<Vec<_>>();

        responder.send(&prefixes).map_err(ClientError::Fidl)?;
        *prefixes_changed = false;
        Ok(())
    }

    /// Sends a list of DNS servers to a watcher through the input responder and updates the last
    /// observed hash.
    fn send_dns_server_updates(
        &mut self,
        responder: ClientWatchServersResponder,
        servers: Vec<Ipv6Addr>,
        hash: u64,
    ) -> Result<(), ClientError> {
        let response: Vec<_> = servers
            .iter()
            .map(|addr| {
                let address = fnet::Ipv6Address { addr: addr.ipv6_bytes() };
                let zone_index =
                    if is_unicast_link_local_strict(&address) { self.interface_id } else { 0 };

                fnet_name::DnsServer_ {
                    address: Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                        address,
                        zone_index,
                        port: DEFAULT_DNS_PORT,
                    })),
                    source: Some(fnet_name::DnsServerSource::Dhcpv6(
                        fnet_name::Dhcpv6DnsServerSource {
                            source_interface: Some(self.interface_id),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }
            })
            .collect();
        let () = responder
            .send(&response)
            // The channel will be closed on error, so return an error to stop the client.
            .map_err(ClientError::Fidl)?;
        self.last_observed_dns_hash = hash;
        Ok(())
    }

    /// Schedules a timer for `timer_type` to fire at `instant`.
    ///
    /// If a timer for `timer_type` is already scheduled, the timer is
    /// updated to fire at the new time.
    fn schedule_timer(
        &mut self,
        timer_type: dhcpv6_core::client::ClientTimerType,
        MonotonicTime(instant): MonotonicTime,
    ) {
        let (handle, reg) = AbortHandle::new_pair();
        match self.timer_abort_handles.insert(timer_type, handle) {
            // Abort the previous handle.
            Some(handle) => handle.abort(),
            None => {}
        }

        self.timer_futs.push(Abortable::new(
            fasync::Timer::new(fasync::Time::from_zx(instant)).replace_value(timer_type),
            reg,
        ))
    }

    /// Cancels a previously scheduled timer for `timer_type`.
    ///
    /// If a timer was not previously scheduled for `timer_type`, this
    /// call is effectively a no-op.
    fn cancel_timer(&mut self, timer_type: dhcpv6_core::client::ClientTimerType) {
        match self.timer_abort_handles.remove(&timer_type) {
            Some(handle) => handle.abort(),
            None => {}
        }
    }

    /// Handles a timeout.
    async fn handle_timeout(
        &mut self,
        timer_type: dhcpv6_core::client::ClientTimerType,
    ) -> Result<(), ClientError> {
        // This timer just fired.
        self.cancel_timer(timer_type);

        let actions = self.state_machine.handle_timeout(timer_type, MonotonicTime::now());
        self.run_actions(actions).await
    }

    /// Handles a received message.
    async fn handle_message_recv(&mut self, mut msg: &[u8]) -> Result<(), ClientError> {
        let msg = match v6::Message::parse(&mut msg, ()) {
            Ok(msg) => msg,
            Err(e) => {
                // Discard invalid messages.
                //
                // https://tools.ietf.org/html/rfc8415#section-16.
                warn!("failed to parse received message: {}", e);
                return Ok(());
            }
        };
        let actions = self.state_machine.handle_message_receive(msg, MonotonicTime::now());
        self.run_actions(actions).await
    }

    /// Handles a FIDL request sent to this client.
    fn handle_client_request(&mut self, request: ClientRequest) -> Result<(), ClientError> {
        debug!("handling client request: {:?}", request);
        match request {
            ClientRequest::WatchServers { responder } => match self.dns_responder {
                Some(_) => {
                    // Drop the previous responder to close the channel.
                    self.dns_responder = None;
                    // Return an error to stop the client because the channel is closed.
                    Err(ClientError::DoubleWatch)
                }
                None => {
                    let dns_servers = self.state_machine.get_dns_servers();
                    let servers_hash = hash(&dns_servers);
                    if servers_hash != self.last_observed_dns_hash {
                        // Something has changed from the last time, update the watcher.
                        let () =
                            self.send_dns_server_updates(responder, dns_servers, servers_hash)?;
                    } else {
                        // Nothing has changed, update the watcher later.
                        self.dns_responder = Some(responder);
                    }
                    Ok(())
                }
            },
            ClientRequest::WatchAddress { responder } => match self.address_responder.take() {
                // The responder will be dropped and cause the channel to be closed.
                Some(ClientWatchAddressResponder { .. }) => Err(ClientError::DoubleWatch),
                None => {
                    // TODO(https://fxbug.dev/72701): Implement the address watcher.
                    warn!("WatchAddress call will block forever as it is unimplemented");
                    self.address_responder = Some(responder);
                    Ok(())
                }
            },
            ClientRequest::WatchPrefixes { responder } => match self.prefixes_responder.take() {
                // The responder will be dropped and cause the channel to be closed.
                Some(ClientWatchPrefixesResponder { .. }) => Err(ClientError::DoubleWatch),
                None => {
                    self.prefixes_responder = Some(responder);
                    self.maybe_send_prefixes()
                }
            },
            // TODO(https://fxbug.dev/72702): Implement Shutdown.
            ClientRequest::Shutdown { responder: _ } => {
                Err(ClientError::Unimplemented("Shutdown".to_string()))
            }
        }
    }

    /// Handles the next event and returns the result.
    ///
    /// Takes a pre-allocated buffer to avoid repeated allocation.
    ///
    /// The returned `Option` is `None` if `request_stream` on the client is closed.
    async fn handle_next_event(&mut self, buf: &mut [u8]) -> Result<Option<()>, ClientError> {
        select! {
            timer_res = self.timer_futs.select_next_some() => {
                match timer_res {
                    Ok(timer_type) => {
                        let () = self.handle_timeout(timer_type).await?;
                        Ok(Some(()))
                    },
                    // The timer was aborted, do nothing.
                    Err(Aborted) => {
                        debug!("timer aborted");
                        Ok(Some(()))
                    }
                }
            },
            recv_from_res = self.socket.recv_from(buf).fuse() => {
                let (size, _addr) = recv_from_res.map_err(ClientError::SocketRecv)?;
                let () = self.handle_message_recv(&buf[..size]).await?;
                Ok(Some(()))
            },
            request = self.request_stream.try_next() => {
                match request {
                    Ok(request) => {
                        request.map(|request| self.handle_client_request(request)).transpose()
                    }
                    Err(e) => {
                        Err(ClientError::Fidl(e))
                    }
                }
            }
        }
    }
}

/// Creates a socket listening on the input address.
async fn create_socket(addr: SocketAddr) -> Result<fasync::net::UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    // It is possible to run multiple clients on the same address.
    let () = socket.set_reuse_port(true)?;
    let () = socket.bind(&addr.into())?;
    fasync::net::UdpSocket::from_socket(socket.into()).context("converting socket")
}

/// Returns `true` if the input address is a link-local address (`fe80::/64`).
///
/// TODO(https://github.com/rust-lang/rust/issues/27709): use is_unicast_link_local_strict() in
/// stable rust when it's available.
fn is_unicast_link_local_strict(addr: &fnet::Ipv6Address) -> bool {
    addr.addr[..8] == [0xfe, 0x80, 0, 0, 0, 0, 0, 0]
}

/// Starts a client based on `params`.
///
/// `request` will be serviced by the client.
pub(crate) async fn serve_client(
    params: NewClientParams,
    request: ServerEnd<ClientMarker>,
) -> Result<()> {
    if let NewClientParams {
        interface_id: Some(interface_id),
        address: Some(address),
        config: Some(config),
        ..
    } = params
    {
        if Ipv6Addr::from(address.address.addr).is_multicast()
            || (is_unicast_link_local_strict(&address.address)
                && address.zone_index != interface_id)
        {
            return request
                .close_with_epitaph(zx::Status::INVALID_ARGS)
                .context("closing request channel with epitaph");
        }

        let fnet_ext::SocketAddress(addr) = fnet::SocketAddress::Ipv6(address).into();
        let servers_addr = IpAddr::from_str(RELAY_AGENT_AND_SERVER_LINK_LOCAL_MULTICAST_ADDRESS)
            .with_context(|| {
                format!(
                    "{} should be a valid IPv6 address",
                    RELAY_AGENT_AND_SERVER_LINK_LOCAL_MULTICAST_ADDRESS,
                )
            })?;
        let mut client = Client::<fasync::net::UdpSocket>::start(
            dhcpv6_core::client::transaction_id(),
            config,
            interface_id,
            create_socket(addr).await?,
            SocketAddr::new(servers_addr, RELAY_AGENT_AND_SERVER_PORT),
            request.into_stream().context("getting new client request stream from channel")?,
        )
        .await?;
        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        loop {
            match client.handle_next_event(&mut buf).await? {
                Some(()) => (),
                None => break Ok(()),
            }
        }
    } else {
        // All param fields are required.
        request
            .close_with_epitaph(zx::Status::INVALID_ARGS)
            .context("closing request channel with epitaph")
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, pin::Pin, task::Poll};

    use fidl::endpoints::{
        create_proxy, create_proxy_and_stream, create_request_stream, ClientEnd,
    };
    use fidl_fuchsia_net_dhcpv6::{self as fnet_dhcpv6, ClientProxy, DEFAULT_CLIENT_PORT};
    use fuchsia_async as fasync;
    use futures::{join, poll, TryFutureExt as _};

    use assert_matches::assert_matches;
    use net_declare::{
        fidl_ip_v6, fidl_ip_v6_with_prefix, fidl_socket_addr, fidl_socket_addr_v6, net_ip_v6,
        net_subnet_v6, std_socket_addr,
    };
    use net_types::ip::IpAddress as _;
    use packet::serialize::InnerPacketBuilder;
    use test_case::test_case;

    use super::*;

    /// Creates a test socket bound to an ephemeral port on localhost.
    fn create_test_socket() -> (fasync::net::UdpSocket, SocketAddr) {
        let addr: SocketAddr = std_socket_addr!("[::1]:0");
        let socket = std::net::UdpSocket::bind(addr).expect("failed to create test socket");
        let addr = socket.local_addr().expect("failed to get address of test socket");
        (fasync::net::UdpSocket::from_socket(socket).expect("failed to create test socket"), addr)
    }

    struct ReceivedMessage {
        transaction_id: [u8; 3],
        // Client IDs are optional in Information Request messages.
        //
        // Per RFC 8415 section 18.2.6,
        //
        //   The client SHOULD include a Client Identifier option (see
        //   Section 21.2) to identify itself to the server (however, see
        //   Section 4.3.1 of [RFC7844] for reasons why a client may not want to
        //   include this option).
        //
        // Per RFC 7844 section 4.3.1,
        //
        //   According to [RFC3315], a DHCPv6 client includes its client
        //   identifier in most of the messages it sends. There is one exception,
        //   however: the client is allowed to omit its client identifier when
        //   sending Information-request messages.
        client_id: Option<Vec<u8>>,
    }

    /// Asserts `socket` receives a message of `msg_type` from
    /// `want_from_addr`.
    async fn assert_received_message(
        socket: &fasync::net::UdpSocket,
        want_from_addr: SocketAddr,
        msg_type: v6::MessageType,
    ) -> ReceivedMessage {
        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        let (size, from_addr) =
            socket.recv_from(&mut buf).await.expect("failed to receive on test server socket");
        assert_eq!(from_addr, want_from_addr);
        let buf = &mut &buf[..size]; // Implements BufferView.
        let msg = v6::Message::parse(buf, ()).expect("failed to parse message");
        assert_eq!(msg.msg_type(), msg_type);

        let mut client_id = None;
        for opt in msg.options() {
            match opt {
                v6::ParsedDhcpOption::ClientId(id) => {
                    assert_eq!(core::mem::replace(&mut client_id, Some(id.to_vec())), None)
                }
                _ => {}
            }
        }

        ReceivedMessage { transaction_id: *msg.transaction_id(), client_id: client_id }
    }

    #[fuchsia::test]
    fn test_create_client_with_unsupported_config() {
        let information_configs = [None];

        let non_temporary_address_configs = [
            None,
            // No addresses but provided an address config.
            Some(AddressConfig { address_count: None, ..Default::default() }),
            // No addresses but provided an address config.
            Some(AddressConfig { address_count: Some(0), ..Default::default() }),
            // preferred addresses > address count.
            Some(AddressConfig {
                address_count: Some(1),
                preferred_addresses: Some(vec![fidl_ip_v6!("ff01::1"), fidl_ip_v6!("ff01::1")]),
                ..Default::default()
            }),
        ];
        let prefix_delegation_configs = [
            None,
            // Prefix length config without a non-zero length.
            Some(PrefixDelegationConfig::PrefixLength(0)),
            // Prefix length too long.
            Some(PrefixDelegationConfig::PrefixLength(Ipv6Addr::BYTES * 8 + 1)),
            // Network-bits unset.
            Some(PrefixDelegationConfig::Prefix(fidl_ip_v6_with_prefix!("::/64"))),
            // Host-bits set.
            Some(PrefixDelegationConfig::Prefix(fidl_ip_v6_with_prefix!("a::1/64"))),
        ];

        for information_config in information_configs.iter() {
            for non_temporary_address_config in non_temporary_address_configs.iter() {
                for prefix_delegation_config in prefix_delegation_configs.iter() {
                    assert_matches!(
                        create_state_machine(
                            [1, 2, 3],
                            ClientConfig {
                                information_config: information_config.clone(),
                                non_temporary_address_config: non_temporary_address_config.clone(),
                                prefix_delegation_config: prefix_delegation_config.clone(),
                                ..Default::default()
                            }
                        ),
                        Err(ClientError::UnsupportedConfigs),
                        "information_config={:?}, non_temporary_address_config={:?}, prefix_delegation_config={:?}",
                        information_config, non_temporary_address_config, prefix_delegation_config
                    );
                }
            }
        }
    }

    #[fuchsia::test]
    async fn test_client_stops_on_channel_close() {
        let (client_proxy, server_end) =
            create_proxy::<ClientMarker>().expect("failed to create test client proxy");

        let ((), client_res) = join!(
            async { drop(client_proxy) },
            serve_client(
                NewClientParams {
                    interface_id: Some(1),
                    address: Some(fidl_socket_addr_v6!("[::1]:546")),
                    config: Some(ClientConfig {
                        information_config: Some(InformationConfig::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                server_end,
            ),
        );
        client_res.expect("client future should return with Ok");
    }

    fn client_proxy_watch_servers(
        client_proxy: &fnet_dhcpv6::ClientProxy,
    ) -> impl Future<Output = Result<(), fidl::Error>> {
        client_proxy.watch_servers().map_ok(|_: Vec<fidl_fuchsia_net_name::DnsServer_>| ())
    }

    fn client_proxy_watch_address(
        client_proxy: &fnet_dhcpv6::ClientProxy,
    ) -> impl Future<Output = Result<(), fidl::Error>> {
        client_proxy.watch_address().map_ok(
            |_: (
                fnet::Subnet,
                fidl_fuchsia_net_interfaces_admin::AddressParameters,
                fidl::endpoints::ServerEnd<
                    fidl_fuchsia_net_interfaces_admin::AddressStateProviderMarker,
                >,
            )| (),
        )
    }

    fn client_proxy_watch_prefixes(
        client_proxy: &fnet_dhcpv6::ClientProxy,
    ) -> impl Future<Output = Result<(), fidl::Error>> {
        client_proxy.watch_prefixes().map_ok(|_: Vec<fnet_dhcpv6::Prefix>| ())
    }

    #[test_case(client_proxy_watch_servers; "watch_servers")]
    #[test_case(client_proxy_watch_address; "watch_address")]
    #[test_case(client_proxy_watch_prefixes; "watch_prefixes")]
    #[fuchsia::test]
    async fn test_client_should_return_error_on_double_watch<Fut, F>(watch: F)
    where
        Fut: Future<Output = Result<(), fidl::Error>>,
        F: Fn(&fnet_dhcpv6::ClientProxy) -> Fut,
    {
        let (client_proxy, server_end) =
            create_proxy::<ClientMarker>().expect("failed to create test client proxy");

        let (caller1_res, caller2_res, client_res) = join!(
            watch(&client_proxy),
            watch(&client_proxy),
            serve_client(
                NewClientParams {
                    interface_id: Some(1),
                    address: Some(fidl_socket_addr_v6!("[::1]:546")),
                    config: Some(ClientConfig {
                        information_config: Some(InformationConfig::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                server_end,
            )
        );

        assert_matches!(
            caller1_res,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::PEER_CLOSED, .. })
        );
        assert_matches!(
            caller2_res,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::PEER_CLOSED, .. })
        );
        assert!(client_res
            .expect_err("client should fail with double watch error")
            .to_string()
            .contains("got watch request while the previous one is pending"));
    }

    fn valid_information_configs() -> [Option<InformationConfig>; 1] {
        [Some(InformationConfig::default())]
    }

    const VALID_DELEGATED_PREFIX_CONFIGS: [Option<PrefixDelegationConfig>; 4] = [
        Some(PrefixDelegationConfig::Empty(Empty {})),
        Some(PrefixDelegationConfig::PrefixLength(1)),
        Some(PrefixDelegationConfig::PrefixLength(127)),
        Some(PrefixDelegationConfig::Prefix(fidl_ip_v6_with_prefix!("a::/64"))),
    ];

    // Can't be a const variable because we allocate a vector.
    fn get_valid_non_temporary_address_configs() -> [Option<AddressConfig>; 4] {
        [
            Some(AddressConfig {
                address_count: Some(1),
                preferred_addresses: None,
                ..Default::default()
            }),
            Some(AddressConfig {
                address_count: Some(1),
                preferred_addresses: Some(Vec::new()),
                ..Default::default()
            }),
            Some(AddressConfig {
                address_count: Some(1),
                preferred_addresses: Some(vec![fidl_ip_v6!("a::1")]),
                ..Default::default()
            }),
            Some(AddressConfig {
                address_count: Some(2),
                preferred_addresses: Some(vec![fidl_ip_v6!("a::2")]),
                ..Default::default()
            }),
        ]
    }

    #[fuchsia::test]
    fn test_client_starts_with_valid_args() {
        for information_config in valid_information_configs().iter() {
            for non_temporary_address_config in get_valid_non_temporary_address_configs().iter() {
                for prefix_delegation_config in VALID_DELEGATED_PREFIX_CONFIGS.iter() {
                    let mut exec = fasync::TestExecutor::new();

                    let (client_proxy, server_end) =
                        create_proxy::<ClientMarker>().expect("failed to create test client proxy");

                    let test_fut = async {
                        join!(
                            client_proxy.watch_servers(),
                            serve_client(
                                NewClientParams {
                                    interface_id: Some(1),
                                    address: Some(fidl_socket_addr_v6!("[::1]:546")),
                                    config: Some(ClientConfig {
                                        information_config: information_config.clone(),
                                        non_temporary_address_config: non_temporary_address_config
                                            .clone(),
                                        prefix_delegation_config: prefix_delegation_config.clone(),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                },
                                server_end
                            )
                        )
                    };
                    futures::pin_mut!(test_fut);
                    assert_matches!(
                        exec.run_until_stalled(&mut test_fut),
                        Poll::Pending,
                        "information_config={:?}, non_temporary_address_config={:?}, prefix_delegation_config={:?}",
                        information_config, non_temporary_address_config, prefix_delegation_config
                    );
                }
            }
        }
    }

    #[fuchsia::test]
    async fn test_client_starts_in_correct_mode() {
        for information_config in [None].into_iter().chain(valid_information_configs()) {
            for non_temporary_address_config in
                [None].into_iter().chain(get_valid_non_temporary_address_configs())
            {
                for prefix_delegation_config in
                    [None].into_iter().chain(VALID_DELEGATED_PREFIX_CONFIGS)
                {
                    let want_msg_type = if non_temporary_address_config.is_none()
                        && prefix_delegation_config.is_none()
                    {
                        if information_config.is_none() {
                            continue;
                        } else {
                            v6::MessageType::InformationRequest
                        }
                    } else {
                        v6::MessageType::Solicit
                    };

                    let (_, client_stream): (ClientEnd<ClientMarker>, _) =
                        create_request_stream::<ClientMarker>()
                            .expect("failed to create test fidl channel");

                    let (client_socket, client_addr) = create_test_socket();
                    let (server_socket, server_addr) = create_test_socket();
                    println!(
                        "{:?} {:?} {:?}",
                        information_config, non_temporary_address_config, prefix_delegation_config
                    );
                    let _: Client<fasync::net::UdpSocket> = Client::start(
                        [1, 2, 3], /* transaction ID */
                        ClientConfig {
                            information_config: information_config.clone(),
                            non_temporary_address_config: non_temporary_address_config.clone(),
                            prefix_delegation_config: prefix_delegation_config.clone(),
                            ..Default::default()
                        },
                        1, /* interface ID */
                        client_socket,
                        server_addr,
                        client_stream,
                    )
                    .await
                        .unwrap_or_else(|e| panic!(
                            "failed to create test client: {}; information_config={:?}, non_temporary_address_config={:?}, prefix_delegation_config={:?}",
                            e, information_config, non_temporary_address_config, prefix_delegation_config
                        ));

                    let _: ReceivedMessage =
                        assert_received_message(&server_socket, client_addr, want_msg_type).await;
                }
            }
        }
    }

    #[fuchsia::test]
    async fn test_client_fails_to_start_with_invalid_args() {
        for params in vec![
            // Missing required field.
            NewClientParams {
                interface_id: Some(1),
                address: None,
                config: Some(ClientConfig {
                    information_config: Some(InformationConfig::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            // Interface ID and zone index mismatch on link-local address.
            NewClientParams {
                interface_id: Some(2),
                address: Some(fnet::Ipv6SocketAddress {
                    address: fidl_ip_v6!("fe80::1"),
                    port: DEFAULT_CLIENT_PORT,
                    zone_index: 1,
                }),
                config: Some(ClientConfig {
                    information_config: Some(InformationConfig::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            // Multicast address is invalid.
            NewClientParams {
                interface_id: Some(1),
                address: Some(fnet::Ipv6SocketAddress {
                    address: fidl_ip_v6!("ff01::1"),
                    port: DEFAULT_CLIENT_PORT,
                    zone_index: 1,
                }),
                config: Some(ClientConfig {
                    information_config: Some(InformationConfig::default()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ] {
            let (client_proxy, server_end) =
                create_proxy::<ClientMarker>().expect("failed to create test client proxy");
            let () =
                serve_client(params, server_end).await.expect("start server failed unexpectedly");
            // Calling any function on the client proxy should fail due to channel closed with
            // `INVALID_ARGS`.
            assert_matches!(
                client_proxy.watch_servers().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
            );
        }
    }

    #[test]
    fn test_is_unicast_link_local_strict() {
        assert_eq!(is_unicast_link_local_strict(&fidl_ip_v6!("fe80::")), true);
        assert_eq!(is_unicast_link_local_strict(&fidl_ip_v6!("fe80::1")), true);
        assert_eq!(is_unicast_link_local_strict(&fidl_ip_v6!("fe80::ffff:1:2:3")), true);
        assert_eq!(is_unicast_link_local_strict(&fidl_ip_v6!("fe80::1:0:0:0:0")), false);
        assert_eq!(is_unicast_link_local_strict(&fidl_ip_v6!("fe81::")), false);
    }

    fn create_test_dns_server(
        address: fnet::Ipv6Address,
        source_interface: u64,
        zone_index: u64,
    ) -> fnet_name::DnsServer_ {
        fnet_name::DnsServer_ {
            address: Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                address,
                zone_index,
                port: DEFAULT_DNS_PORT,
            })),
            source: Some(fnet_name::DnsServerSource::Dhcpv6(fnet_name::Dhcpv6DnsServerSource {
                source_interface: Some(source_interface),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    async fn send_msg_with_options(
        socket: &fasync::net::UdpSocket,
        to_addr: SocketAddr,
        transaction_id: [u8; 3],
        msg_type: v6::MessageType,
        options: &[v6::DhcpOption<'_>],
    ) -> Result<()> {
        let builder = v6::MessageBuilder::new(msg_type, transaction_id, options);
        let mut buf = vec![0u8; builder.bytes_len()];
        let () = builder.serialize(&mut buf);
        let size = socket.send_to(&buf, to_addr).await?;
        assert_eq!(size, buf.len());
        Ok(())
    }

    #[fuchsia::test]
    fn test_client_should_respond_to_dns_watch_requests() {
        let mut exec = fasync::TestExecutor::new();
        let transaction_id = [1, 2, 3];

        let (client_proxy, client_stream) = create_proxy_and_stream::<ClientMarker>()
            .expect("failed to create test proxy and stream");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = exec
            .run_singlethreaded(Client::<fasync::net::UdpSocket>::start(
                transaction_id,
                ClientConfig {
                    information_config: Some(InformationConfig::default()),
                    ..Default::default()
                },
                1, /* interface ID */
                client_socket,
                server_addr,
                client_stream,
            ))
            .expect("failed to create test client");

        type WatchServersResponseFut = <fnet_dhcpv6::ClientProxy as fnet_dhcpv6::ClientProxyInterface>::WatchServersResponseFut;
        type WatchServersResponse = <WatchServersResponseFut as Future>::Output;

        struct Test<'a> {
            client: &'a mut Client<fasync::net::UdpSocket>,
            buf: Vec<u8>,
            watcher_fut: WatchServersResponseFut,
        }

        impl<'a> Test<'a> {
            fn new(
                client: &'a mut Client<fasync::net::UdpSocket>,
                client_proxy: &ClientProxy,
            ) -> Self {
                Self {
                    client,
                    buf: vec![0u8; MAX_UDP_DATAGRAM_SIZE],
                    watcher_fut: client_proxy.watch_servers(),
                }
            }

            async fn handle_next_event(&mut self) {
                self.client
                    .handle_next_event(&mut self.buf)
                    .await
                    .expect("test client failed to handle next event")
                    .expect("request stream closed");
            }

            async fn refresh_client(&mut self) {
                // Make the client ready for another reply immediately on signal, so it can
                // start receiving updates without waiting for the full refresh timeout which is
                // unrealistic in tests.
                if self
                    .client
                    .timer_abort_handles
                    .contains_key(&dhcpv6_core::client::ClientTimerType::Refresh)
                {
                    self.client
                        .handle_timeout(dhcpv6_core::client::ClientTimerType::Refresh)
                        .await
                        .expect("test client failed to handle timeout");
                } else {
                    panic!("no refresh timer is scheduled and refresh is requested in test");
                }
                // Allow the client to handle the timeout-aborted event.
                self.handle_next_event().await;
            }

            // Drive both the DHCPv6 client's event handling logic and the DNS server
            // watcher until the DNS server watcher receives an update from the client (or
            // the client unexpectedly exits).
            fn run(&mut self) -> Pin<Box<dyn Future<Output = WatchServersResponse> + '_>> {
                let Self { client, buf, watcher_fut } = self;
                Box::pin(async move {
                    let client_fut = async {
                        loop {
                            client
                                .handle_next_event(buf)
                                .await
                                .expect("test client failed to handle next event")
                                .expect("request stream closed");
                        }
                    }
                    .fuse();
                    futures::pin_mut!(client_fut);
                    let mut watcher_fut = watcher_fut.fuse();
                    select! {
                        () = client_fut => panic!("test client returned unexpectedly"),
                        r = watcher_fut => r,
                    }
                })
            }
        }

        {
            // No DNS configurations received yet.
            let mut test = Test::new(&mut client, &client_proxy);

            // Handle the WatchServers request.
            exec.run_singlethreaded(test.handle_next_event());
            assert!(
                test.client.dns_responder.is_some(),
                "WatchServers responder should be present"
            );

            // Send an empty list to the client, should not update watcher.
            let () = exec
                .run_singlethreaded(send_msg_with_options(
                    &server_socket,
                    client_addr,
                    transaction_id,
                    v6::MessageType::Reply,
                    &[v6::DhcpOption::ServerId(&[1, 2, 3]), v6::DhcpOption::DnsServers(&[])],
                ))
                .expect("failed to send test reply");
            // Wait for the client to handle the next event (processing the reply we just
            // sent). Note that it is not enough to simply drive the client future until it
            // is stalled as we do elsewhere in the test, because we have no guarantee that
            // the netstack has delivered the UDP packet to the client by the time the
            // `send_to` call returned.
            exec.run_singlethreaded(test.handle_next_event());
            assert_matches!(exec.run_until_stalled(&mut test.run()), Poll::Pending);

            // Send a list of DNS servers, the watcher should be updated accordingly.
            exec.run_singlethreaded(test.refresh_client());
            let dns_servers = [net_ip_v6!("fe80::1:2")];
            let () = exec
                .run_singlethreaded(send_msg_with_options(
                    &server_socket,
                    client_addr,
                    transaction_id,
                    v6::MessageType::Reply,
                    &[
                        v6::DhcpOption::ServerId(&[1, 2, 3]),
                        v6::DhcpOption::DnsServers(&dns_servers),
                    ],
                ))
                .expect("failed to send test reply");
            let want_servers = vec![create_test_dns_server(
                fidl_ip_v6!("fe80::1:2"),
                1, /* source interface */
                1, /* zone index */
            )];
            let servers = exec.run_singlethreaded(test.run()).expect("get servers");
            assert_eq!(servers, want_servers);
        } // drop `test_fut` so `client_fut` is no longer mutably borrowed.

        {
            // No new changes, should not update watcher.
            let mut test = Test::new(&mut client, &client_proxy);

            // Handle the WatchServers request.
            exec.run_singlethreaded(test.handle_next_event());
            assert!(
                test.client.dns_responder.is_some(),
                "WatchServers responder should be present"
            );

            // Send the same list of DNS servers, should not update watcher.
            exec.run_singlethreaded(test.refresh_client());
            let dns_servers = [net_ip_v6!("fe80::1:2")];
            let () = exec
                .run_singlethreaded(send_msg_with_options(
                    &server_socket,
                    client_addr,
                    transaction_id,
                    v6::MessageType::Reply,
                    &[
                        v6::DhcpOption::ServerId(&[1, 2, 3]),
                        v6::DhcpOption::DnsServers(&dns_servers),
                    ],
                ))
                .expect("failed to send test reply");
            // Wait for the client to handle the next event (processing the reply we just
            // sent). Note that it is not enough to simply drive the client future until it
            // is stalled as we do elsewhere in the test, because we have no guarantee that
            // the netstack has delivered the UDP packet to the client by the time the
            // `send_to` call returned.
            exec.run_singlethreaded(test.handle_next_event());
            assert_matches!(exec.run_until_stalled(&mut test.run()), Poll::Pending);

            // Send a different list of DNS servers, should update watcher.
            exec.run_singlethreaded(test.refresh_client());
            let dns_servers = [net_ip_v6!("fe80::1:2"), net_ip_v6!("1234::5:6")];
            let () = exec
                .run_singlethreaded(send_msg_with_options(
                    &server_socket,
                    client_addr,
                    transaction_id,
                    v6::MessageType::Reply,
                    &[
                        v6::DhcpOption::ServerId(&[1, 2, 3]),
                        v6::DhcpOption::DnsServers(&dns_servers),
                    ],
                ))
                .expect("failed to send test reply");
            let want_servers = vec![
                create_test_dns_server(
                    fidl_ip_v6!("fe80::1:2"),
                    1, /* source interface */
                    1, /* zone index */
                ),
                // Only set zone index for link local addresses.
                create_test_dns_server(
                    fidl_ip_v6!("1234::5:6"),
                    1, /* source interface */
                    0, /* zone index */
                ),
            ];
            let servers = exec.run_singlethreaded(test.run()).expect("get servers");
            assert_eq!(servers, want_servers);
        } // drop `test_fut` so `client_fut` is no longer mutably borrowed.

        {
            // Send an empty list of DNS servers, should update watcher,
            // because this is different from what the watcher has seen
            // last time.
            let mut test = Test::new(&mut client, &client_proxy);

            exec.run_singlethreaded(test.refresh_client());
            let () = exec
                .run_singlethreaded(send_msg_with_options(
                    &server_socket,
                    client_addr,
                    transaction_id,
                    v6::MessageType::Reply,
                    &[v6::DhcpOption::ServerId(&[1, 2, 3]), v6::DhcpOption::DnsServers(&[])],
                ))
                .expect("failed to send test reply");
            let want_servers = Vec::<fnet_name::DnsServer_>::new();
            assert_eq!(exec.run_singlethreaded(test.run()).expect("get servers"), want_servers);
        } // drop `test_fut` so `client_fut` is no longer mutably borrowed.
    }

    #[fuchsia::test]
    async fn test_client_should_respond_with_dns_servers_on_first_watch_if_non_empty() {
        let transaction_id = [1, 2, 3];

        let (client_proxy, client_stream) = create_proxy_and_stream::<ClientMarker>()
            .expect("failed to create test proxy and stream");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            transaction_id,
            ClientConfig {
                information_config: Some(InformationConfig::default()),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        let dns_servers = [net_ip_v6!("fe80::1:2"), net_ip_v6!("1234::5:6")];
        let () = send_msg_with_options(
            &server_socket,
            client_addr,
            transaction_id,
            v6::MessageType::Reply,
            &[v6::DhcpOption::ServerId(&[4, 5, 6]), v6::DhcpOption::DnsServers(&dns_servers)],
        )
        .await
        .expect("failed to send test message");

        // Receive non-empty DNS servers before watch.
        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        // Emit aborted timer.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));

        let want_servers = vec![
            create_test_dns_server(
                fidl_ip_v6!("fe80::1:2"),
                1, /* source interface */
                1, /* zone index */
            ),
            create_test_dns_server(
                fidl_ip_v6!("1234::5:6"),
                1, /* source interface */
                0, /* zone index */
            ),
        ];
        assert_matches!(
            join!(client.handle_next_event(&mut buf), client_proxy.watch_servers()),
            (Ok(Some(())), Ok(servers)) => assert_eq!(servers, want_servers)
        );
    }

    #[fuchsia::test]
    async fn watch_prefixes() {
        const SERVER_ID: [u8; 3] = [3, 4, 5];
        const PREFERRED_LIFETIME_SECS: u32 = 1000;
        const VALID_LIFETIME_SECS: u32 = 2000;
        // Use the smallest possible value to enter the Renewing state
        // as fast as possible to keep the test's run-time as low as possible.
        const T1: u32 = 1;
        const T2: u32 = 2000;

        let (client_proxy, client_stream) = create_proxy_and_stream::<ClientMarker>()
            .expect("failed to create test proxy and stream");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            [1, 2, 3],
            ClientConfig {
                prefix_delegation_config: Some(PrefixDelegationConfig::Empty(Empty {})),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        let client_fut = async {
            let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
            loop {
                select! {
                    res = client.handle_next_event(&mut buf).fuse() => {
                        match res.expect("test client failed to handle next event") {
                            Some(()) => (),
                            None => break (),
                        };
                    }
                }
            }
        }
        .fuse();
        futures::pin_mut!(client_fut);

        let update_prefix = net_subnet_v6!("a::/64");
        let remove_prefix = net_subnet_v6!("b::/64");
        let add_prefix = net_subnet_v6!("c::/64");

        // Go through the motions to assign a prefix.
        let client_id = {
            let ReceivedMessage { client_id, transaction_id } =
                assert_received_message(&server_socket, client_addr, v6::MessageType::Solicit)
                    .await;
            // Client IDs are mandatory in stateful DHCPv6.
            let client_id = client_id.unwrap();

            let ia_prefix = [
                v6::DhcpOption::IaPrefix(v6::IaPrefixSerializer::new(
                    PREFERRED_LIFETIME_SECS,
                    VALID_LIFETIME_SECS,
                    update_prefix,
                    &[],
                )),
                v6::DhcpOption::IaPrefix(v6::IaPrefixSerializer::new(
                    PREFERRED_LIFETIME_SECS,
                    VALID_LIFETIME_SECS,
                    remove_prefix,
                    &[],
                )),
            ];
            let () = send_msg_with_options(
                &server_socket,
                client_addr,
                transaction_id,
                v6::MessageType::Advertise,
                &[
                    v6::DhcpOption::ServerId(&SERVER_ID),
                    v6::DhcpOption::ClientId(&client_id),
                    v6::DhcpOption::Preference(u8::MAX),
                    v6::DhcpOption::IaPd(v6::IaPdSerializer::new(IA_PD_IAID, T1, T2, &ia_prefix)),
                ],
            )
            .await
            .expect("failed to send adv message");

            // Wait for the client to send a Request and send Reply so a prefix
            // is assigned.
            let transaction_id = select! {
                () = client_fut => panic!("should never return"),
                res = assert_received_message(
                    &server_socket,
                    client_addr,
                    v6::MessageType::Request,
                ).fuse() => {
                    let ReceivedMessage { client_id: req_client_id, transaction_id } = res;
                    assert_eq!(Some(&client_id), req_client_id.as_ref());
                    transaction_id
                },
            };

            let () = send_msg_with_options(
                &server_socket,
                client_addr,
                transaction_id,
                v6::MessageType::Reply,
                &[
                    v6::DhcpOption::ServerId(&SERVER_ID),
                    v6::DhcpOption::ClientId(&client_id),
                    v6::DhcpOption::IaPd(v6::IaPdSerializer::new(IA_PD_IAID, T1, T2, &ia_prefix)),
                ],
            )
            .await
            .expect("failed to send reply message");

            client_id
        };

        let check_watch_prefixes_result =
            |res: Result<Vec<Prefix>, _>,
             before_handling_reply,
             preferred_lifetime_secs: u32,
             valid_lifetime_secs: u32,
             expected_prefixes| {
                assert_matches!(
                    res.unwrap()[..],
                    [
                        Prefix {
                            prefix: got_prefix1,
                            lifetimes: Lifetimes {
                                preferred_until: preferred_until1,
                                valid_until: valid_until1,
                            },
                        },
                        Prefix {
                            prefix: got_prefix2,
                            lifetimes: Lifetimes {
                                preferred_until: preferred_until2,
                                valid_until: valid_until2,
                            },
                        },
                    ] => {
                        let now = zx::Time::get_monotonic();
                        let preferred_until = zx::Time::from_nanos(preferred_until1);
                        let valid_until = zx::Time::from_nanos(valid_until1);

                        let preferred_for = zx::Duration::from_seconds(
                            preferred_lifetime_secs.into(),
                        );
                        let valid_for = zx::Duration::from_seconds(valid_lifetime_secs.into());

                        assert_eq!(
                            HashSet::from([got_prefix1, got_prefix2]),
                            HashSet::from(expected_prefixes),
                        );
                        assert!(preferred_until >= before_handling_reply + preferred_for);
                        assert!(preferred_until <= now + preferred_for);
                        assert!(valid_until >= before_handling_reply + valid_for);
                        assert!(valid_until <= now + valid_for);

                        assert_eq!(preferred_until1, preferred_until2);
                        assert_eq!(valid_until1, valid_until2);
                    }
                )
            };

        // Wait for a prefix to become assigned from the perspective of the DHCPv6
        // FIDL client.
        {
            // watch_prefixes should not return before a lease is negotiated. Note
            // that the client has not yet handled the Reply message.
            let mut watch_prefixes = client_proxy.watch_prefixes().fuse();
            assert_matches!(poll!(&mut watch_prefixes), Poll::Pending);
            let before_handling_reply = zx::Time::get_monotonic();
            select! {
                () = client_fut => panic!("should never return"),
                res = watch_prefixes => check_watch_prefixes_result(
                    res,
                    before_handling_reply,
                    PREFERRED_LIFETIME_SECS,
                    VALID_LIFETIME_SECS,
                    [
                        subnet_to_address_with_prefix(update_prefix),
                        subnet_to_address_with_prefix(remove_prefix),
                    ],
                ),
            }
        }

        // Wait for the client to attempt to renew the lease and go through the
        // motions to update the lease.
        {
            let transaction_id = select! {
                () = client_fut => panic!("should never return"),
                res = assert_received_message(
                    &server_socket,
                    client_addr,
                    v6::MessageType::Renew,
                ).fuse() => {
                    let ReceivedMessage { client_id: ren_client_id, transaction_id } = res;
                    assert_eq!(ren_client_id.as_ref(), Some(&client_id));
                    transaction_id
                },
            };

            const NEW_PREFERRED_LIFETIME_SECS: u32 = 2 * PREFERRED_LIFETIME_SECS;
            const NEW_VALID_LIFETIME_SECS: u32 = 2 * VALID_LIFETIME_SECS;
            let ia_prefix = [
                v6::DhcpOption::IaPrefix(v6::IaPrefixSerializer::new(
                    NEW_PREFERRED_LIFETIME_SECS,
                    NEW_VALID_LIFETIME_SECS,
                    update_prefix,
                    &[],
                )),
                v6::DhcpOption::IaPrefix(v6::IaPrefixSerializer::new(
                    NEW_PREFERRED_LIFETIME_SECS,
                    NEW_VALID_LIFETIME_SECS,
                    add_prefix,
                    &[],
                )),
                v6::DhcpOption::IaPrefix(v6::IaPrefixSerializer::new(0, 0, remove_prefix, &[])),
            ];

            let () = send_msg_with_options(
                &server_socket,
                client_addr,
                transaction_id,
                v6::MessageType::Reply,
                &[
                    v6::DhcpOption::ServerId(&SERVER_ID),
                    v6::DhcpOption::ClientId(&client_id),
                    v6::DhcpOption::IaPd(v6::IaPdSerializer::new(
                        v6::IAID::new(0),
                        T1,
                        T2,
                        &ia_prefix,
                    )),
                ],
            )
            .await
            .expect("failed to send reply message");

            let before_handling_reply = zx::Time::get_monotonic();
            select! {
                () = client_fut => panic!("should never return"),
                res = client_proxy.watch_prefixes().fuse() => check_watch_prefixes_result(
                    res,
                    before_handling_reply,
                    NEW_PREFERRED_LIFETIME_SECS,
                    NEW_VALID_LIFETIME_SECS,
                    [
                        subnet_to_address_with_prefix(update_prefix),
                        subnet_to_address_with_prefix(add_prefix),
                    ],
                ),
            }
        }
    }

    #[fuchsia::test]
    async fn test_client_schedule_and_cancel_timers() {
        let (_client_end, client_stream) =
            create_request_stream::<ClientMarker>().expect("failed to create test request stream");

        let (client_socket, _client_addr) = create_test_socket();
        let (_server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            [1, 2, 3], /* transaction ID */
            ClientConfig {
                information_config: Some(InformationConfig::default()),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        // Stateless DHCP client starts by scheduling a retransmission timer.
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        let () = client.cancel_timer(dhcpv6_core::client::ClientTimerType::Retransmission);
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            Vec::<&dhcpv6_core::client::ClientTimerType>::new()
        );

        let now = MonotonicTime::now();
        let () = client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Refresh,
            now + Duration::from_nanos(1),
        );
        let () = client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Retransmission,
            now + Duration::from_nanos(2),
        );
        assert_eq!(
            client.timer_abort_handles.keys().collect::<HashSet<_>>(),
            vec![
                &dhcpv6_core::client::ClientTimerType::Retransmission,
                &dhcpv6_core::client::ClientTimerType::Refresh
            ]
            .into_iter()
            .collect()
        );

        // We are allowed to reschedule a timer to fire at a new time.
        let now = MonotonicTime::now();
        client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Refresh,
            now + Duration::from_nanos(1),
        );
        client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Retransmission,
            now + Duration::from_nanos(2),
        );

        let () = client.cancel_timer(dhcpv6_core::client::ClientTimerType::Refresh);
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Ok to cancel a timer that is not scheduled.
        client.cancel_timer(dhcpv6_core::client::ClientTimerType::Refresh);

        let () = client.cancel_timer(dhcpv6_core::client::ClientTimerType::Retransmission);
        assert_eq!(
            client
                .timer_abort_handles
                .keys()
                .collect::<Vec<&dhcpv6_core::client::ClientTimerType>>(),
            Vec::<&dhcpv6_core::client::ClientTimerType>::new()
        );

        // Ok to cancel a timer that is not scheduled.
        client.cancel_timer(dhcpv6_core::client::ClientTimerType::Retransmission);
    }

    #[fuchsia::test]
    async fn test_handle_next_event_on_stateless_client() {
        let (client_proxy, client_stream) = create_proxy_and_stream::<ClientMarker>()
            .expect("failed to create test proxy and stream");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            [1, 2, 3], /* transaction ID */
            ClientConfig {
                information_config: Some(InformationConfig::default()),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        // Starting the client in stateless should send an information request out.
        let ReceivedMessage { client_id, transaction_id: _ } = assert_received_message(
            &server_socket,
            client_addr,
            v6::MessageType::InformationRequest,
        )
        .await;
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        // Trigger a retransmission.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        let ReceivedMessage { client_id: got_client_id, transaction_id: _ } =
            assert_received_message(
                &server_socket,
                client_addr,
                v6::MessageType::InformationRequest,
            )
            .await;
        assert_eq!(got_client_id, client_id);
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Message targeting another transaction ID should be ignored.
        let () = send_msg_with_options(
            &server_socket,
            client_addr,
            [5, 6, 7],
            v6::MessageType::Reply,
            &[],
        )
        .await
        .expect("failed to send test message");
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Invalid messages should be discarded. Empty buffer is invalid.
        let size =
            server_socket.send_to(&[], client_addr).await.expect("failed to send test message");
        assert_eq!(size, 0);
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Message targeting this client should cause the client to transition state.
        let () = send_msg_with_options(
            &server_socket,
            client_addr,
            [1, 2, 3],
            v6::MessageType::Reply,
            &[v6::DhcpOption::ServerId(&[4, 5, 6])],
        )
        .await
        .expect("failed to send test message");
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Refresh]
        );
        // Discard aborted retransmission timer.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));

        // Reschedule a shorter timer for Refresh so we don't spend time waiting in test.
        client.cancel_timer(dhcpv6_core::client::ClientTimerType::Refresh);
        // Discard cancelled refresh timer.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Refresh,
            MonotonicTime::now() + Duration::from_nanos(1),
        );

        // Trigger a refresh.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        let ReceivedMessage { client_id, transaction_id: _ } = assert_received_message(
            &server_socket,
            client_addr,
            v6::MessageType::InformationRequest,
        )
        .await;
        assert_eq!(got_client_id, client_id,);
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        let test_fut = async {
            assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
            client
                .dns_responder
                .take()
                .expect("test client did not get a channel responder")
                .send(&[fnet_name::DnsServer_ {
                    address: Some(fidl_socket_addr!("[fe01::2:3]:42")),
                    source: Some(fnet_name::DnsServerSource::Dhcpv6(
                        fnet_name::Dhcpv6DnsServerSource {
                            source_interface: Some(42),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }])
                .expect("failed to send response on test channel");
        };
        let (watcher_res, ()) = join!(client_proxy.watch_servers(), test_fut);
        let servers = watcher_res.expect("failed to watch servers");
        assert_eq!(
            servers,
            vec![fnet_name::DnsServer_ {
                address: Some(fidl_socket_addr!("[fe01::2:3]:42")),
                source: Some(fnet_name::DnsServerSource::Dhcpv6(
                    fnet_name::Dhcpv6DnsServerSource {
                        source_interface: Some(42),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }]
        );

        // Drop the channel should cause `handle_next_event(&mut buf)` to return `None`.
        drop(client_proxy);
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(None));
    }

    #[fuchsia::test]
    async fn test_handle_next_event_on_stateful_client() {
        let (client_proxy, client_stream) =
            create_proxy_and_stream::<ClientMarker>().expect("failed to create test fidl channel");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            [1, 2, 3], /* transaction ID */
            ClientConfig {
                non_temporary_address_config: Some(AddressConfig {
                    address_count: Some(1),
                    preferred_addresses: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        // Starting the client in stateful should send out a solicit.
        let _: ReceivedMessage =
            assert_received_message(&server_socket, client_addr, v6::MessageType::Solicit).await;
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        // Drop the channel should cause `handle_next_event(&mut buf)` to return `None`.
        drop(client_proxy);
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(None));
    }

    #[fuchsia::test]
    #[should_panic]
    async fn test_handle_next_event_respects_timer_order() {
        let (_client_end, client_stream) =
            create_request_stream::<ClientMarker>().expect("failed to create test request stream");

        let (client_socket, client_addr) = create_test_socket();
        let (server_socket, server_addr) = create_test_socket();
        let mut client = Client::<fasync::net::UdpSocket>::start(
            [1, 2, 3], /* transaction ID */
            ClientConfig {
                information_config: Some(InformationConfig::default()),
                ..Default::default()
            },
            1, /* interface ID */
            client_socket,
            server_addr,
            client_stream,
        )
        .await
        .expect("failed to create test client");

        let mut buf = vec![0u8; MAX_UDP_DATAGRAM_SIZE];
        // A retransmission timer is scheduled when starting the client in stateless mode. Cancel
        // it and create a new one with a longer timeout so the test is not flaky.
        let () = client.cancel_timer(dhcpv6_core::client::ClientTimerType::Retransmission);
        // Discard cancelled retransmission timer.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        let () = client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Retransmission,
            MonotonicTime::now() + Duration::from_secs(1_000_000),
        );
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Trigger a message receive, the message is later discarded because transaction ID doesn't
        // match.
        let () = send_msg_with_options(
            &server_socket,
            client_addr,
            [5, 6, 7],
            v6::MessageType::Reply,
            &[],
        )
        .await
        .expect("failed to send test message");
        // There are now two pending events, the message receive is handled first because the timer
        // is far into the future.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
        // The retransmission timer is still here.
        assert_eq!(
            client.timer_abort_handles.keys().collect::<Vec<_>>(),
            vec![&dhcpv6_core::client::ClientTimerType::Retransmission]
        );

        // Inserts a refresh timer that precedes the retransmission.
        let () = client.schedule_timer(
            dhcpv6_core::client::ClientTimerType::Refresh,
            MonotonicTime::now() + Duration::from_nanos(1),
        );
        // This timer is scheduled.
        assert_eq!(
            client.timer_abort_handles.keys().collect::<HashSet<_>>(),
            vec![
                &dhcpv6_core::client::ClientTimerType::Retransmission,
                &dhcpv6_core::client::ClientTimerType::Refresh
            ]
            .into_iter()
            .collect()
        );

        // Now handle_next_event(&mut buf) should trigger a refresh because it
        // precedes retransmission. Refresh is not expected while in
        // InformationRequesting state and should lead to a panic.
        assert_matches!(client.handle_next_event(&mut buf).await, Ok(Some(())));
    }

    #[fuchsia::test]
    async fn test_handle_next_event_fails_on_recv_err() {
        struct StubSocket {}
        impl<'a> AsyncSocket<'a> for StubSocket {
            type RecvFromFut = futures::future::Ready<Result<(usize, SocketAddr), std::io::Error>>;
            type SendToFut = futures::future::Ready<Result<usize, std::io::Error>>;

            fn recv_from(&'a self, _buf: &'a mut [u8]) -> Self::RecvFromFut {
                futures::future::ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "test recv error",
                )))
            }
            fn send_to(&'a self, buf: &'a [u8], _addr: SocketAddr) -> Self::SendToFut {
                futures::future::ready(Ok(buf.len()))
            }
        }

        let (_client_end, client_stream) =
            create_request_stream::<ClientMarker>().expect("failed to create test request stream");

        let mut client = Client::<StubSocket>::start(
            [1, 2, 3], /* transaction ID */
            ClientConfig {
                information_config: Some(InformationConfig::default()),
                ..Default::default()
            },
            1, /* interface ID */
            StubSocket {},
            std_socket_addr!("[::1]:0"),
            client_stream,
        )
        .await
        .expect("failed to create test client");

        assert_matches!(
            client.handle_next_event(&mut [0u8]).await,
            Err(ClientError::SocketRecv(err)) if err.kind() == std::io::ErrorKind::Other
        );
    }
}
