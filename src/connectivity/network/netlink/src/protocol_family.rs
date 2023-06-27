// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing protocol-specific aspects of Netlink.

use netlink_packet_core::{NetlinkMessage, NetlinkPayload, NetlinkSerializable};

use std::fmt::Debug;

// TODO(https://github.com/rust-lang/rust/issues/91611): Replace this with
// #![feature(async_fn_in_trait)] once it supports `Send` bounds. See
// https://blog.rust-lang.org/inside-rust/2023/05/03/stabilizing-async-fn-in-trait.html.
use async_trait::async_trait;
use tracing::{debug, warn};

use crate::{
    client::{ExternalClient, InternalClient},
    messaging::Sender,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastCapableNetlinkFamily,
    },
    NETLINK_LOG_TAG,
};

/// A type representing a Netlink Protocol Family.
pub(crate) trait ProtocolFamily:
    MulticastCapableNetlinkFamily + Send + Sized + 'static
{
    /// The message type associated with the protocol family.
    type InnerMessage: Clone + Debug + NetlinkSerializable + Send + 'static;
    /// The implementation for handling requests from this protocol family.
    type RequestHandler<S: Sender<Self::InnerMessage>>: NetlinkFamilyRequestHandler<Self, S>;

    const NAME: &'static str;
}

#[async_trait]
/// A request handler implementation for a particular Netlink protocol family.
pub(crate) trait NetlinkFamilyRequestHandler<F: ProtocolFamily, S: Sender<F::InnerMessage>>:
    Clone + Send + 'static
{
    /// Handles the given request and generates the associated response(s).
    async fn handle_request(
        &mut self,
        req: NetlinkMessage<F::InnerMessage>,
        client: &mut InternalClient<F, S>,
    );
}

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

    use std::{fmt::Display, num::NonZeroU32};

    use futures::{
        channel::{mpsc, oneshot},
        sink::SinkExt as _,
    };
    use net_types::{
        ip::{
            AddrSubnetEither, AddrSubnetError, IpAddr, IpAddress as _, IpVersion, Ipv4Addr,
            Ipv6Addr,
        },
        SpecifiedAddress as _,
    };

    use crate::{
        interfaces,
        netlink_packet::{self, errno::Errno},
        routes,
    };

    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::rtnl::{
        address::{nlas::Nla as AddressNla, AddressMessage},
        constants::{
            AF_INET, AF_INET6, AF_UNSPEC, IFA_F_NOPREFIXROUTE, RTNLGRP_DCB, RTNLGRP_DECNET_IFADDR,
            RTNLGRP_DECNET_ROUTE, RTNLGRP_DECNET_RULE, RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE,
            RTNLGRP_IPV4_MROUTE_R, RTNLGRP_IPV4_NETCONF, RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE,
            RTNLGRP_IPV6_IFADDR, RTNLGRP_IPV6_IFINFO, RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R,
            RTNLGRP_IPV6_NETCONF, RTNLGRP_IPV6_PREFIX, RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE,
            RTNLGRP_LINK, RTNLGRP_MDB, RTNLGRP_MPLS_NETCONF, RTNLGRP_MPLS_ROUTE,
            RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH, RTNLGRP_NONE, RTNLGRP_NOP2, RTNLGRP_NOP4,
            RTNLGRP_NOTIFY, RTNLGRP_NSID, RTNLGRP_PHONET_IFADDR, RTNLGRP_PHONET_ROUTE, RTNLGRP_TC,
        },
        RtnlMessage,
    };

    /// An implementation of the Netlink Route protocol family.
    pub(crate) enum NetlinkRoute {}

    impl MulticastCapableNetlinkFamily for NetlinkRoute {
        fn is_valid_group(ModernGroup(group): &ModernGroup) -> bool {
            match *group {
                RTNLGRP_DCB
                | RTNLGRP_DECNET_IFADDR
                | RTNLGRP_DECNET_ROUTE
                | RTNLGRP_DECNET_RULE
                | RTNLGRP_IPV4_IFADDR
                | RTNLGRP_IPV4_MROUTE
                | RTNLGRP_IPV4_MROUTE_R
                | RTNLGRP_IPV4_NETCONF
                | RTNLGRP_IPV4_ROUTE
                | RTNLGRP_IPV4_RULE
                | RTNLGRP_IPV6_IFADDR
                | RTNLGRP_IPV6_IFINFO
                | RTNLGRP_IPV6_MROUTE
                | RTNLGRP_IPV6_MROUTE_R
                | RTNLGRP_IPV6_NETCONF
                | RTNLGRP_IPV6_PREFIX
                | RTNLGRP_IPV6_ROUTE
                | RTNLGRP_IPV6_RULE
                | RTNLGRP_LINK
                | RTNLGRP_MDB
                | RTNLGRP_MPLS_NETCONF
                | RTNLGRP_MPLS_ROUTE
                | RTNLGRP_ND_USEROPT
                | RTNLGRP_NEIGH
                | RTNLGRP_NONE
                | RTNLGRP_NOP2
                | RTNLGRP_NOP4
                | RTNLGRP_NOTIFY
                | RTNLGRP_NSID
                | RTNLGRP_PHONET_IFADDR
                | RTNLGRP_PHONET_ROUTE
                | RTNLGRP_TC => true,
                _ => false,
            }
        }
    }

    impl ProtocolFamily for NetlinkRoute {
        type InnerMessage = RtnlMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> = NetlinkRouteRequestHandler<S>;

        const NAME: &'static str = "NETLINK_ROUTE";
    }

    #[derive(Clone)]
    pub(crate) struct NetlinkRouteRequestHandler<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    > {
        pub(crate) interfaces_request_sink: mpsc::Sender<interfaces::Request<S>>,
        pub(crate) v4_routes_request_sink: mpsc::Sender<routes::Request<S>>,
        pub(crate) v6_routes_request_sink: mpsc::Sender<routes::Request<S>>,
    }

    struct ExtractedAddressRequest {
        address_and_interface_id: interfaces::AddressAndInterfaceArgs,
        addr_flags: u32,
    }

    fn extract_if_id_and_addr_from_addr_message(
        message: &AddressMessage,
        client: &impl Display,
        req: &RtnlMessage,
        // `true` for new address requests; `false` for delete address requests.
        is_new: bool,
    ) -> Result<Option<ExtractedAddressRequest>, Errno> {
        let kind = if is_new { "new" } else { "del" };

        let interface_id = match NonZeroU32::new(message.header.index) {
            Some(interface_id) => interface_id,
            None => {
                debug!(
                    "unspecified interface ID in address {} request from {}: {:?}",
                    kind, client, req,
                );
                return Err(Errno::EINVAL);
            }
        };

        let mut address_bytes = None;
        let mut local_bytes = None;
        let mut addr_flags = None;
        message.nlas.iter().for_each(|nla| match nla {
            AddressNla::Address(bytes) => address_bytes = Some(bytes),
            AddressNla::Local(bytes) => local_bytes = Some(bytes),
            AddressNla::Flags(flags) => addr_flags = Some(*flags),
            nla => {
                warn!(
                    "unexpected Address NLA in {} request from {}: {:?}; req = {:?}",
                    kind, client, nla, req,
                );
            }
        });

        // Linux supports the notion of a "peer" address which is used for
        // pointtopoint interfaces. Fuchsia does not support this so we do not
        // allow different-valued `IFA_LOCAL` and `IFA_ADDRESS` values.
        //
        // Per https://www.man7.org/linux/man-pages/man8/ip-address.8.html,
        //
        //   ip address add - add new protocol address.
        //       dev IFNAME
        //            the name of the device to add the address to.
        //
        //       local ADDRESS (default)
        //            the address of the interface. The format of the address
        //            depends on the protocol. It is a dotted quad for IP and a
        //            sequence of hexadecimal halfwords separated by colons for
        //            IPv6. The ADDRESS may be followed by a slash and a decimal
        //            number which encodes the network prefix length.
        //
        //       peer ADDRESS
        //            the address of the remote endpoint for pointopoint
        //            interfaces. Again, the ADDRESS may be followed by a slash
        //            and a decimal number, encoding the network prefix length.
        //            If a peer address is specified, the local address cannot
        //            have a prefix length. The network prefix is associated
        //            with the peer rather than with the local address.
        //
        //   ...
        //
        //   ip address delete - delete protocol address
        //       Arguments: coincide with the arguments of ip addr add. The
        //       device name is a required argument. The rest are optional. If
        //       no arguments are given, the first address is deleted.
        //
        // Note that when only one of `IFA_LOCAL` or `IFA_ADDRESS` is included
        // in a message, it is treated as the "local" address on the interface
        // to be added/removed. When both are included, `IFA_LOCAL` is treated
        // as the "local" address and `IFA_ADDRESS` is treated as the "peer".
        // TODO(https://fxbug.dev/129502): Support peer addresses.
        let address_bytes = match (local_bytes, address_bytes) {
            (Some(local), Some(address)) => {
                if local == address {
                    address
                } else {
                    debug!(
                    "got different `IFA_ADDRESS` and `IFA_LOCAL` values for {} address request from {}: {:?}",
                    kind, client, req,
                );
                    return Err(Errno::ENOTSUP);
                }
            }
            (Some(bytes), None) | (None, Some(bytes)) => bytes,
            (None, None) => {
                debug!(
                    "missing `IFA_ADDRESS` and `IFA_LOCAL` in address {} request from {}: {:?}",
                    kind, client, req,
                );
                return Err(Errno::EINVAL);
            }
        };

        let addr = match message.header.family.into() {
            AF_INET => {
                const BYTES: usize = Ipv4Addr::BYTES as usize;
                if address_bytes.len() < BYTES {
                    return Err(Errno::EINVAL);
                }

                let mut bytes = [0; BYTES as usize];
                bytes.copy_from_slice(&address_bytes[..BYTES]);
                let addr: Ipv4Addr = bytes.into();
                if addr.is_specified() {
                    IpAddr::V4(addr)
                } else {
                    // Linux treats adding the unspecified IPv4 address as a
                    // no-op.
                    return Ok(None);
                }
            }
            AF_INET6 => {
                const BYTES: usize = Ipv6Addr::BYTES as usize;
                if address_bytes.len() < BYTES {
                    return Err(Errno::EINVAL);
                }

                let mut bytes = [0; BYTES];
                bytes.copy_from_slice(&address_bytes[..BYTES]);
                let addr: Ipv6Addr = bytes.into();
                if addr.is_specified() {
                    IpAddr::V6(bytes.into())
                } else {
                    // Linux returns this error when adding the unspecified IPv6
                    // address.
                    return Err(Errno::EADDRNOTAVAIL);
                }
            }
            family => {
                debug!(
                    "invalid address family ({}) in address {} request from {}: {:?}",
                    family, kind, client, req,
                );
                return Err(Errno::EINVAL);
            }
        };

        let address = match AddrSubnetEither::new(addr, message.header.prefix_len) {
            Ok(address) => address,
            Err(
                AddrSubnetError::PrefixTooLong
                | AddrSubnetError::NotUnicastInSubnet
                | AddrSubnetError::InvalidWitness,
            ) => {
                debug!("invalid address in address {} request from {}: {:?}", kind, client, req);
                return Err(Errno::EINVAL);
            }
        };

        Ok(Some(ExtractedAddressRequest {
            address_and_interface_id: interfaces::AddressAndInterfaceArgs { address, interface_id },
            addr_flags: addr_flags.unwrap_or_else(|| message.header.flags.into()),
        }))
    }

    #[async_trait]
    impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>
        NetlinkFamilyRequestHandler<NetlinkRoute, S> for NetlinkRouteRequestHandler<S>
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<RtnlMessage>,
            client: &mut InternalClient<NetlinkRoute, S>,
        ) {
            let Self { interfaces_request_sink, v4_routes_request_sink, v6_routes_request_sink } =
                self;

            let (req_header, payload) = req.into_parts();
            let req = match payload {
                NetlinkPayload::InnerMessage(p) => p,
                p => {
                    warn!(
                        tag = NETLINK_LOG_TAG,
                        "Ignoring request from client {} with unexpected payload: {:?}", client, p
                    );
                    return;
                }
            };

            let is_dump = req_header.flags & NLM_F_DUMP == NLM_F_DUMP;
            let expects_ack = req_header.flags & NLM_F_ACK == NLM_F_ACK;

            use RtnlMessage::*;
            match req {
                GetLink(_) if is_dump => {
                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Link(
                            interfaces::LinkRequestArgs::Get(
                                interfaces::GetLinkArgs::Dump,
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    waiter
                        .await
                        .expect("interfaces event loop should have handled the request")
                        .expect("link dump requests are infallible");
                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                GetAddress(ref message) if is_dump => {
                    let ip_version_filter = match message.header.family.into() {
                        AF_UNSPEC => None,
                        AF_INET => Some(IpVersion::V4),
                        AF_INET6 => Some(IpVersion::V6),
                        family => {
                            debug!(
                                "invalid address family ({}) in address dump request from {}: {:?}",
                                family, client, req,
                            );
                            client.send_unicast(
                                netlink_packet::new_error(Errno::EINVAL, req_header));
                            return;
                        }
                    };

                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::Get(
                                interfaces::GetAddressArgs::Dump {
                                    ip_version_filter,
                                },
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    waiter
                        .await
                        .expect("interfaces event loop should have handled the request")
                        .expect("addr dump requests are infallible");
                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                NewAddress(ref message) => {
                    let extracted_request = match extract_if_id_and_addr_from_addr_message(
                        message,
                        client,
                        &req,
                        true,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            return client.send_unicast(netlink_packet::new_error(e, req_header));
                        }
                    };
                    let result = if let Some(ExtractedAddressRequest {
                        address_and_interface_id,
                        addr_flags,
                    }) = extracted_request {
                        let (completer, waiter) = oneshot::channel();
                        let add_subnet_route = addr_flags & IFA_F_NOPREFIXROUTE != IFA_F_NOPREFIXROUTE;
                        interfaces_request_sink.send(interfaces::Request {
                            args: interfaces::RequestArgs::Address(
                                interfaces::AddressRequestArgs::New(
                                    interfaces::NewAddressArgs {
                                        address_and_interface_id,
                                        add_subnet_route,
                                    },
                                ),
                            ),
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                            completer,
                        }).await.expect("interface event loop should never terminate");
                        waiter
                            .await
                            .expect("interfaces event loop should have handled the request")
                    } else {
                        Ok(())
                    };

                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_ack(req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(e.into_errno(), req_header)),
                    }
                }
                DelAddress(ref message) => {
                    let extracted_request = match extract_if_id_and_addr_from_addr_message(
                        message,
                        client,
                        &req,
                        false,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            return client.send_unicast(netlink_packet::new_error(e, req_header));
                        }
                    };

                    let result = if let Some(ExtractedAddressRequest {
                        address_and_interface_id,
                        addr_flags: _,
                    }) = extracted_request {
                        let (completer, waiter) = oneshot::channel();
                        interfaces_request_sink.send(interfaces::Request {
                            args: interfaces::RequestArgs::Address(
                                interfaces::AddressRequestArgs::Del(
                                    interfaces::DelAddressArgs {
                                        address_and_interface_id,
                                    },
                                ),
                            ),
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                            completer,
                        }).await.expect("interface event loop should never terminate");
                        waiter
                            .await
                            .expect("interfaces event loop should have handled the request")
                    } else {
                        Ok(())
                    };
                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_ack(req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(e.into_errno(), req_header))
                    }
                }
                GetRoute(ref message) if is_dump => {
                    match message.header.address_family.into() {
                        AF_UNSPEC => {
                            // V4 routes are requested prior to V6 routes to conform
                            // with `ip list` output.
                            process_dump_for_routes_worker(v4_routes_request_sink, client, req_header).await;
                            process_dump_for_routes_worker(v6_routes_request_sink, client, req_header).await;
                        },
                        AF_INET => {
                            process_dump_for_routes_worker(v4_routes_request_sink, client, req_header).await;
                        },
                        AF_INET6 => {
                            process_dump_for_routes_worker(v6_routes_request_sink, client, req_header).await;
                        },
                        family => {
                            debug!("invalid address family ({}) in route dump request from {}: {:?}", family, client, req);
                            client.send_unicast(
                                netlink_packet::new_error(Errno::EINVAL, req_header));
                            return;
                        }
                    };

                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                NewLink(_)
                | DelLink(_)
                | NewLinkProp(_)
                | DelLinkProp(_)
                | NewNeighbourTable(_)
                | SetNeighbourTable(_)
                | NewTrafficClass(_)
                | DelTrafficClass(_)
                | NewTrafficFilter(_)
                | DelTrafficFilter(_)
                | NewTrafficChain(_)
                | DelTrafficChain(_)
                | NewNsId(_)
                | DelNsId(_)
                // TODO(https://issuetracker.google.com/285127790): Implement NewNeighbour.
                | NewNeighbour(_)
                // TODO(https://issuetracker.google.com/285127790): Implement DelNeighbour.
                | DelNeighbour(_)
                // TODO(https://issuetracker.google.com/283136220): Implement SetLink.
                | SetLink(_)
                // TODO(https://issuetracker.google.com/283136222): Implement NewRoute.
                | NewRoute(_)
                // TODO(https://issuetracker.google.com/283136222): Implement DelRoute.
                | DelRoute(_)
                // TODO(https://issuetracker.google.com/283137907): Implement NewQueueDiscipline.
                | NewQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283137907): Implement DelQueueDiscipline.
                | DelQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283134947): Implement NewRule.
                | NewRule(_)
                // TODO(https://issuetracker.google.com/283134947): Implement DelRule.
                | DelRule(_) => {
                    if expects_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                            req,
                        );
                        client.send_unicast(netlink_packet::new_ack(req_header))
                    } else {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request that does not expect an Ack: {:?}",
                            req,
                        )
                    }
                }
                GetNeighbourTable(_)
                | GetTrafficClass(_)
                | GetTrafficFilter(_)
                | GetTrafficChain(_)
                | GetNsId(_)
                // TODO(https://issuetracker.google.com/285127384): Implement GetNeighbour.
                | GetNeighbour(_)
                // TODO(https://issuetracker.google.com/283134954): Implement GetLink.
                | GetLink(_)
                // TODO(https://issuetracker.google.com/283134032): Implement GetAddress.
                | GetAddress(_)
                // Non-dump GetRoute is not currently necessary for our use.
                | GetRoute(_)
                // TODO(https://issuetracker.google.com/283137907): Implement GetQueueDiscipline.
                | GetQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283134947): Implement GetRule.
                | GetRule(_) => {
                    if is_dump {
                        warn!(
                            "Received unsupported NETLINK_ROUTE DUMP request; responding with Done: {:?}",
                            req
                        );
                        client.send_unicast(netlink_packet::new_done(req_header))
                    } else if expects_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request: responding with Ack {:?}",
                            req
                        );
                        client.send_unicast(netlink_packet::new_ack(req_header))
                    } else {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request that does not expect an Ack {:?}",
                            req
                        )
                    }
                },
                req => panic!("unexpected RtnlMessage: {:?}", req),
            }
        }
    }

    async fn process_dump_for_routes_worker<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        sink: &mut mpsc::Sender<routes::Request<S>>,
        client: &mut InternalClient<NetlinkRoute, S>,
        req_header: NetlinkHeader,
    ) {
        let (completer, waiter) = oneshot::channel();
        sink.send(routes::Request {
            args: routes::RequestArgs::Route(routes::RouteRequestArgs::Get(
                routes::GetRouteArgs::Dump,
            )),
            sequence_number: req_header.sequence_number,
            client: client.clone(),
            completer,
        })
        .await
        .expect("route event loop should never terminate");
        waiter
            .await
            .expect("routes event loop should have handled the request")
            .expect("route dump requests are infallible");
    }

    /// A connection to the Route Netlink Protocol family.
    pub struct NetlinkRouteClient(pub(crate) ExternalClient<NetlinkRoute>);

    impl NetlinkRouteClient {
        /// Sets the PID assigned to the client.
        pub fn set_pid(&self, pid: NonZeroU32) {
            let NetlinkRouteClient(client) = self;
            client.set_port_number(pid)
        }

        /// Adds the given multicast group membership.
        pub fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.add_membership(group)
        }

        /// Deletes the given multicast group membership.
        pub fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.del_membership(group)
        }

        /// Sets the legacy multicast group memberships.
        pub fn set_legacy_memberships(
            &self,
            legacy_memberships: LegacyGroups,
        ) -> Result<(), InvalidLegacyGroupsError> {
            let NetlinkRouteClient(client) = self;
            client.set_legacy_memberships(legacy_memberships)
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use netlink_packet_core::NetlinkHeader;

    pub(crate) const LEGACY_GROUP1: u32 = 0x00000001;
    pub(crate) const LEGACY_GROUP2: u32 = 0x00000002;
    pub(crate) const LEGACY_GROUP3: u32 = 0x00000004;
    pub(crate) const INVALID_LEGACY_GROUP: u32 = 0x00000008;
    pub(crate) const MODERN_GROUP1: ModernGroup = ModernGroup(1);
    pub(crate) const MODERN_GROUP2: ModernGroup = ModernGroup(2);
    pub(crate) const MODERN_GROUP3: ModernGroup = ModernGroup(3);
    pub(crate) const INVALID_MODERN_GROUP: ModernGroup = ModernGroup(4);

    #[derive(Debug)]
    pub(crate) enum FakeProtocolFamily {}

    impl MulticastCapableNetlinkFamily for FakeProtocolFamily {
        fn is_valid_group(group: &ModernGroup) -> bool {
            match *group {
                MODERN_GROUP1 | MODERN_GROUP2 | MODERN_GROUP3 => true,
                _ => false,
            }
        }
    }

    pub(crate) fn new_fake_netlink_message() -> NetlinkMessage<FakeNetlinkInnerMessage> {
        NetlinkMessage::new(
            NetlinkHeader::default(),
            NetlinkPayload::InnerMessage(FakeNetlinkInnerMessage),
        )
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    pub(crate) struct FakeNetlinkInnerMessage;

    impl NetlinkSerializable for FakeNetlinkInnerMessage {
        fn message_type(&self) -> u16 {
            u16::MAX
        }

        fn buffer_len(&self) -> usize {
            0
        }

        fn serialize(&self, _buffer: &mut [u8]) {}
    }

    /// Handler of [`FakeNetlinkInnerMessage`] requests.
    ///
    /// Reflects the given request back as the response.
    #[derive(Clone)]
    pub(crate) struct FakeNetlinkRequestHandler;

    #[async_trait]
    impl<S: Sender<FakeNetlinkInnerMessage>> NetlinkFamilyRequestHandler<FakeProtocolFamily, S>
        for FakeNetlinkRequestHandler
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<FakeNetlinkInnerMessage>,
            client: &mut InternalClient<FakeProtocolFamily, S>,
        ) {
            client.send_unicast(req)
        }
    }

    impl ProtocolFamily for FakeProtocolFamily {
        type InnerMessage = FakeNetlinkInnerMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> = FakeNetlinkRequestHandler;

        const NAME: &'static str = "FAKE_PROTOCOL_FAMILY";
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::num::NonZeroU32;

    use assert_matches::assert_matches;
    use futures::{channel::mpsc, future::FutureExt as _, stream::StreamExt as _};
    use net_declare::net_addr_subnet;
    use net_types::{
        ip::{AddrSubnetEither, IpVersion},
        Witness as _,
    };
    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::{
        rtnl::address::nlas::Nla as AddressNla, AddressMessage, LinkMessage, RouteMessage,
        RtnlMessage, TcMessage, AF_INET, AF_INET6, AF_PACKET, AF_UNSPEC, IFA_F_NOPREFIXROUTE,
    };
    use test_case::test_case;

    use crate::{
        interfaces,
        messaging::testutil::{FakeSender, SentMessage},
        netlink_packet::{self, errno::Errno},
        protocol_family::route::{NetlinkRoute, NetlinkRouteRequestHandler},
        routes,
    };

    enum ExpectedResponse {
        Ack,
        Error(Errno),
        Done,
    }

    fn header_with_flags(flags: u16) -> NetlinkHeader {
        let mut header = NetlinkHeader::default();
        header.flags = flags;
        header
    }

    /// Tests that unhandled requests are treated as a no-op.
    ///
    /// Get requests are responded to with a Done message if the dump flag
    /// is set, an Ack message if the ack flag is set or nothing. New/Del
    /// requests are responded to with an Ack message if the ack flag is set
    /// or nothing.
    #[test_case(
        RtnlMessage::GetTrafficChain,
        0,
        None; "get_with_no_flags")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "get_with_ack_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_dump_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_ack_and_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        0,
        None; "new_with_no_flags")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_DUMP,
        None; "new_with_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "new_with_ack_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack); "new_with_ack_and_dump_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        0,
        None; "del_with_no_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_DUMP,
        None; "del_with_dump_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "del_with_ack_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack); "del_with_ack_and_dump_flags")]
    #[fuchsia::test]
    async fn test_handle_unsupported_request_response(
        tc_fn: fn(TcMessage) -> RtnlMessage,
        flags: u16,
        expected_response: Option<ExpectedResponse>,
    ) {
        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };

        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let header = header_with_flags(flags);

        handler
            .handle_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(tc_fn(TcMessage::default())),
                ),
                &mut client,
            )
            .await;

        match expected_response {
            Some(ExpectedResponse::Ack) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_ack(header))]
                )
            }
            Some(ExpectedResponse::Error(e)) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_error(e, header))]
                )
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_done(header))]
                )
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }

    struct RequestAndResponse<R> {
        request: R,
        response: Result<(), interfaces::RequestError>,
    }

    async fn test_request(
        request: NetlinkMessage<RtnlMessage>,
        req_and_resp: Option<RequestAndResponse<interfaces::RequestArgs>>,
    ) -> Vec<SentMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let (interfaces_request_sink, mut interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };

        let ((), ()) = futures::future::join(handler.handle_request(request, &mut client), async {
            let next = interfaces_request_stream.next();
            match req_and_resp {
                Some(RequestAndResponse { request, response }) => {
                    let interfaces::Request { args, sequence_number: _, client: _, completer } =
                        next.await.expect("handler should send request");
                    assert_eq!(args, request);
                    completer.send(response).expect("handler should be alive");
                }
                None => assert_matches!(next.now_or_never(), None),
            }
        })
        .await;

        client_sink.take_messages()
    }

    /// Test RTM_GETLINK.
    #[test_case(
        0,
        None,
        None; "no_flags")]
    #[test_case(
        NLM_F_ACK,
        None,
        Some(ExpectedResponse::Ack); "ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        Some(interfaces::GetLinkArgs::Dump),
        Some(ExpectedResponse::Done); "dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        Some(interfaces::GetLinkArgs::Dump),
        Some(ExpectedResponse::Done); "dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_link(
        flags: u16,
        expected_request_args: Option<interfaces::GetLinkArgs>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetLink(LinkMessage::default())),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Get(a)),
                    response: Ok(()),
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| {
                    SentMessage::unicast(match expected_response {
                        ExpectedResponse::Ack => netlink_packet::new_ack(header),
                        ExpectedResponse::Error(e) => netlink_packet::new_error(e, header),
                        ExpectedResponse::Done => netlink_packet::new_done(header),
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Test RTM_GETADDR.
    #[test_case(
        0,
        AF_UNSPEC,
        None,
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        None,
        Some(ExpectedResponse::Ack); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        None,
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        None,
        Some(ExpectedResponse::Ack); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        None,
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        None,
        Some(ExpectedResponse::Ack); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        None,
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_addr(
        flags: u16,
        family: u16,
        expected_request_args: Option<interfaces::GetAddressArgs>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetAddress(address_message)),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Address(interfaces::AddressRequestArgs::Get(
                        a
                    )),
                    response: Ok(()),
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| SentMessage::unicast(match expected_response {
                    ExpectedResponse::Ack => netlink_packet::new_ack(header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(e, header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    enum AddressRequestKind {
        New { add_subnet_route: bool },
        Del,
    }

    struct TestAddrCase {
        kind: AddressRequestKind,
        flags: u16,
        family: u16,
        nlas: Vec<AddressNla>,
        prefix_len: u8,
        interface_id: u32,
        expected_request_args: Option<RequestAndResponse<interfaces::AddressAndInterfaceArgs>>,
        expected_response: Option<ExpectedResponse>,
    }

    fn bytes_from_addr(a: AddrSubnetEither) -> Vec<u8> {
        match a {
            AddrSubnetEither::V4(a) => a.addr().get().ipv4_bytes().to_vec(),
            AddrSubnetEither::V6(a) => a.addr().get().ipv6_bytes().to_vec(),
        }
    }

    fn prefix_from_addr(a: AddrSubnetEither) -> u8 {
        let (_addr, prefix) = a.addr_prefix();
        prefix
    }

    fn interface_id_as_u32(id: u64) -> u32 {
        id.try_into().unwrap()
    }

    fn valid_new_del_addr_request(
        kind: AddressRequestKind,
        ack: bool,
        addr: AddrSubnetEither,
        extra_nlas: impl IntoIterator<Item = AddressNla>,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        TestAddrCase {
            kind,
            flags: if ack { NLM_F_ACK } else { 0 },
            family: match addr {
                AddrSubnetEither::V4(_) => AF_INET,
                AddrSubnetEither::V6(_) => AF_INET6,
            },
            nlas: [AddressNla::Local(bytes_from_addr(addr))]
                .into_iter()
                .chain(extra_nlas)
                .collect(),
            prefix_len: prefix_from_addr(addr),
            interface_id: interface_id_as_u32(interface_id),
            expected_request_args: Some(RequestAndResponse {
                request: interfaces::AddressAndInterfaceArgs {
                    address: addr,
                    interface_id: NonZeroU32::new(interface_id_as_u32(interface_id)).unwrap(),
                },
                response,
            }),
            expected_response: ack.then_some(ExpectedResponse::Ack),
        }
    }

    fn valid_new_addr_request_with_extra_nlas(
        ack: bool,
        addr: AddrSubnetEither,
        extra_nlas: impl IntoIterator<Item = AddressNla>,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_del_addr_request(
            AddressRequestKind::New { add_subnet_route: true },
            ack,
            addr,
            extra_nlas,
            interface_id,
            response,
        )
    }

    fn valid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_addr_request_with_extra_nlas(ack, addr, None, interface_id, response)
    }

    fn invalid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        errno: Errno,
    ) -> TestAddrCase {
        TestAddrCase {
            expected_request_args: None,
            expected_response: Some(ExpectedResponse::Error(errno)),
            ..valid_new_addr_request(ack, addr, interface_id, Ok(()))
        }
    }

    fn valid_del_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_del_addr_request(AddressRequestKind::Del, ack, addr, None, interface_id, response)
    }

    fn invalid_del_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        errno: Errno,
    ) -> TestAddrCase {
        TestAddrCase {
            expected_request_args: None,
            expected_response: Some(ExpectedResponse::Error(errno)),
            ..valid_del_addr_request(ack, addr, interface_id, Ok(()))
        }
    }

    /// Test RTM_NEWADDR and RTM_DELADDR
    // Add address tests cases.
    #[test_case(
        TestAddrCase {
            expected_request_args: None,
            ..valid_new_addr_request(
                true,
                net_addr_subnet!("0.0.0.0/0"),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()))
        }; "new_v4_unspecified_address_zero_prefix_ok_ack")]
    #[test_case(
        TestAddrCase {
            expected_request_args: None,
            ..valid_new_addr_request(
                false,
                net_addr_subnet!("0.0.0.0/24"),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()))
        }; "new_v4_unspecified_address_non_zero_prefix_ok_no_ack")]
    #[test_case(
        invalid_new_addr_request(
            true,
            net_addr_subnet!("::/0"),
            interfaces::testutil::ETH_INTERFACE_ID,
            Errno::EADDRNOTAVAIL); "new_v6_unspecified_address_zero_prefix_ack")]
    #[test_case(
        invalid_new_addr_request(
            false,
            net_addr_subnet!("::/64"),
            interfaces::testutil::ETH_INTERFACE_ID,
            Errno::EADDRNOTAVAIL); "new_v6_unspecified_address_non_zero_prefix_no_ack")]
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "new_v4_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "new_v6_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "new_v4_ok_no_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "new_v6_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v4_local_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_same_local_and_address_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Address(Vec::new()),
            ],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "new_v6_valid_local_and_empty_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(Vec::new()),
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "new_v4_empty_local_and_valid_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            kind: AddressRequestKind::New { add_subnet_route: true },
            ..valid_new_addr_request_with_extra_nlas(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                [AddressNla::Flags(0)],
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v4_with_route_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            kind: AddressRequestKind::New { add_subnet_route: false },
            ..valid_new_addr_request_with_extra_nlas(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                [AddressNla::Flags(IFA_F_NOPREFIXROUTE)],
                interfaces::testutil::LO_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_without_route_ok_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::InvalidRequest),
            )
        }; "new_v4_invalid_response_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EEXIST)),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::AlreadyExists),
            )
        }; "new_v6_exist_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Err(interfaces::RequestError::UnrecognizedInterface),
            )
        }; "new_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EADDRNOTAVAIL)),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Err(interfaces::RequestError::AddressNotFound),
            )
        }; "new_v4_not_found_response_ck")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_zero_interface_id_ack")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_zero_interface_id_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_no_nlas_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_no_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_missing_address_and_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_missing_address_and_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_address_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_address_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                true,
                net_addr_subnet!("192.0.2.123/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "new_zero_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                false,
                net_addr_subnet!("2001:db8::1324/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "new_zero_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_family_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_family_no_ack")]
    // Delete address tests cases.
    #[test_case(
        valid_del_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "del_v4_ok_ack")]
    #[test_case(
        valid_del_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "del_v6_ok_ack")]
    #[test_case(
        valid_del_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "del_v4_ok_no_ack")]
    #[test_case(
        valid_del_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "del_v6_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::InvalidRequest),
            )
        }; "del_v4_invalid_response_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v4_local_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v6_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v4_same_local_and_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Address(Vec::new()),
            ],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "del_v6_valid_local_and_empty_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(Vec::new()),
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "del_v4_empty_local_and_valid_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EEXIST)),
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::AlreadyExists),
            )
        }; "del_v6_exist_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Err(interfaces::RequestError::UnrecognizedInterface),
            )
        }; "del_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EADDRNOTAVAIL)),
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Err(interfaces::RequestError::AddressNotFound),
            )
        }; "del_v4_not_found_response_ck")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_zero_interface_id_ack")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_zero_interface_id_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_no_nlas_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_no_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_missing_address_and_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_missing_address_and_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_address_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_address_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_del_addr_request(
                true,
                net_addr_subnet!("192.0.2.123/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "del_zero_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_del_addr_request(
                false,
                net_addr_subnet!("2001:db8::1324/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "del_zero_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_family_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_family_no_ack")]
    #[fuchsia::test]
    async fn test_new_del_addr(test_case: TestAddrCase) {
        let TestAddrCase {
            kind,
            flags,
            family,
            nlas,
            prefix_len,
            interface_id,
            expected_request_args,
            expected_response,
        } = test_case;

        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message.header.index = interface_id;
            message.header.prefix_len = prefix_len;
            message.nlas = nlas;
            message
        };

        let (message, request) = match kind {
            AddressRequestKind::New { add_subnet_route } => (
                RtnlMessage::NewAddress(address_message),
                expected_request_args.map(|RequestAndResponse { request, response }| {
                    RequestAndResponse {
                        request: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::New(interfaces::NewAddressArgs {
                                address_and_interface_id: request,
                                add_subnet_route,
                            }),
                        ),
                        response,
                    }
                }),
            ),
            AddressRequestKind::Del => (
                RtnlMessage::DelAddress(address_message),
                expected_request_args.map(|RequestAndResponse { request, response }| {
                    RequestAndResponse {
                        request: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::Del(interfaces::DelAddressArgs {
                                address_and_interface_id: request,
                            }),
                        ),
                        response,
                    }
                }),
            ),
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(header, NetlinkPayload::InnerMessage(message),),
                request,
            )
            .await,
            expected_response
                .into_iter()
                .map(|response| SentMessage::unicast(match response {
                    ExpectedResponse::Ack => netlink_packet::new_ack(header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(e, header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    async fn test_route_request(
        family: u16,
        request: NetlinkMessage<RtnlMessage>,
        expected_request: Option<routes::RequestArgs>,
    ) -> Vec<SentMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, mut v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, mut v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };
        let ((), ()) = futures::future::join(handler.handle_request(request, &mut client), async {
            if family == AF_UNSPEC || family == AF_INET {
                let next = v4_routes_request_stream.next();
                match expected_request {
                    Some(expected_request) => {
                        let routes::Request { args, sequence_number: _, client: _, completer } =
                            next.await.expect("handler should send request");
                        assert_eq!(args, expected_request);
                        completer.send(Ok(())).expect("handler should be alive");
                    }
                    None => assert_matches!(next.now_or_never(), None),
                };
            }
            if family == AF_UNSPEC || family == AF_INET6 {
                let next = v6_routes_request_stream.next();
                match expected_request {
                    Some(expected_request) => {
                        let routes::Request { args, sequence_number: _, client: _, completer } =
                            next.await.expect("handler should send request");
                        assert_eq!(args, expected_request);
                        completer.send(Ok(())).expect("handler should be alive");
                    }
                    None => assert_matches!(next.now_or_never(), None),
                };
            }
        })
        .await;

        client_sink.take_messages()
    }

    /// Test RTM_GETROUTE.
    #[test_case(
        0,
        AF_UNSPEC,
        None,
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        None,
        Some(ExpectedResponse::Ack); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        None,
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        None,
        Some(ExpectedResponse::Ack); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        None,
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        None,
        Some(ExpectedResponse::Ack); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        None,
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_route(
        flags: u16,
        family: u16,
        expected_request_args: Option<routes::GetRouteArgs>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let route_message = {
            let mut message = RouteMessage::default();
            message.header.address_family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_route_request(
                family,
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetRoute(route_message)),
                ),
                expected_request_args
                    .map(|a| routes::RequestArgs::Route(routes::RouteRequestArgs::Get(a))),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| SentMessage::unicast(match expected_response {
                    ExpectedResponse::Ack => netlink_packet::new_ack(header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(e, header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                }))
                .collect::<Vec<_>>(),
        )
    }
}
