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

    use std::num::NonZeroU32;

    use futures::{
        channel::{mpsc, oneshot},
        sink::SinkExt as _,
    };
    use net_types::ip::IpVersion;

    use crate::{
        interfaces,
        netlink_packet::{new_ack, new_done, AckErrorCode, NackErrorCode},
        routes,
    };

    use netlink_packet_core::{NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::rtnl::{
        constants::{
            AF_INET, AF_INET6, AF_UNSPEC, RTNLGRP_DCB, RTNLGRP_DECNET_IFADDR, RTNLGRP_DECNET_ROUTE,
            RTNLGRP_DECNET_RULE, RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE, RTNLGRP_IPV4_MROUTE_R,
            RTNLGRP_IPV4_NETCONF, RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR,
            RTNLGRP_IPV6_IFINFO, RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R, RTNLGRP_IPV6_NETCONF,
            RTNLGRP_IPV6_PREFIX, RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE, RTNLGRP_LINK, RTNLGRP_MDB,
            RTNLGRP_MPLS_NETCONF, RTNLGRP_MPLS_ROUTE, RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH,
            RTNLGRP_NONE, RTNLGRP_NOP2, RTNLGRP_NOP4, RTNLGRP_NOTIFY, RTNLGRP_NSID,
            RTNLGRP_PHONET_IFADDR, RTNLGRP_PHONET_ROUTE, RTNLGRP_TC,
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
        #[allow(unused)]
        pub(crate) v4_routes_request_sink: mpsc::Sender<routes::Request<S>>,
        #[allow(unused)]
        pub(crate) v6_routes_request_sink: mpsc::Sender<routes::Request<S>>,
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
            let Self {
                interfaces_request_sink,
                v4_routes_request_sink: _v4_routes_request_sink,
                v6_routes_request_sink: _v6_routes_request_sink,
            } = self;

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
            let is_ack = req_header.flags & NLM_F_ACK == NLM_F_ACK;

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
                    client.send(new_done(req_header))
                }
                GetAddress(ref message) if is_dump => {
                    let ip_version_filter = match message.header.family.into() {
                        AF_UNSPEC => None,
                        AF_INET => Some(IpVersion::V4),
                        AF_INET6 => Some(IpVersion::V6),
                        family => {
                            debug!("invalid address family ({}) in address dump request: {:?}", family, req);
                            client.send(new_ack(NackErrorCode::INVALID, req_header));
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
                    client.send(new_done(req_header))
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
                // TODO(https://issuetracker.google.com/283134942): Implement NewAddress.
                | NewAddress(_)
                // TODO(https://issuetracker.google.com/283134942): Implement DelAddress.
                | DelAddress(_)
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
                    if is_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                            req,
                        );
                        client.send(new_ack(AckErrorCode, req_header))
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
                // TODO(https://issuetracker.google.com/283137647): Implement GetRoute.
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
                        client.send(new_done(req_header))
                    } else if is_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request: responding with Ack {:?}",
                            req
                        );
                        client.send(new_ack(AckErrorCode, req_header))
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
            client.send(req)
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

    use fidl_fuchsia_net_interfaces as fnet_interfaces;

    use assert_matches::assert_matches;
    use futures::{channel::mpsc, future::FutureExt as _, stream::StreamExt as _};
    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::{
        AddressMessage, LinkMessage, RtnlMessage, TcMessage, AF_INET, AF_INET6, AF_PACKET,
        AF_UNSPEC, ARPHRD_LOOPBACK, ARPHRD_PPP, IFA_F_PERMANENT, IFF_LOOPBACK, IFF_RUNNING, IFF_UP,
    };
    use test_case::test_case;

    use crate::{
        client::ClientTable,
        interfaces,
        messaging::testutil::FakeSender,
        netlink_packet::{
            new_ack, new_done, AckErrorCode, NackErrorCode, UNSPECIFIED_SEQUENCE_NUMBER,
        },
        protocol_family::route::{NetlinkRoute, NetlinkRouteRequestHandler},
    };

    enum ExpectedResponse<C> {
        Ack(C),
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
        Some(ExpectedResponse::Ack(AckErrorCode)); "get_with_ack_flag")]
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
        Some(ExpectedResponse::Ack(AckErrorCode)); "new_with_ack_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack(AckErrorCode)); "new_with_ack_and_dump_flags")]
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
        Some(ExpectedResponse::Ack(AckErrorCode)); "del_with_ack_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack(AckErrorCode)); "del_with_ack_and_dump_flags")]
    #[fuchsia::test]
    async fn test_handle_unsupported_request_response(
        tc_fn: fn(TcMessage) -> RtnlMessage,
        flags: u16,
        expected_response: Option<ExpectedResponse<AckErrorCode>>,
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
            Some(ExpectedResponse::Ack(code)) => {
                assert_eq!(client_sink.take_messages(), [new_ack(code, header)])
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(client_sink.take_messages(), [new_done(header)])
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }

    async fn test_request(
        request: NetlinkMessage<RtnlMessage>,
    ) -> Vec<NetlinkMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let interfaces::testutil::Setup {
            event_loop,
            mut watcher_stream,
            request_sink: interfaces_request_sink,
            interfaces_request_stream: _,
        } = interfaces::testutil::setup_with_route_clients({
            let route_clients = ClientTable::default();
            route_clients.add_client(client.clone());
            route_clients
        });
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };

        let event_loop_fut = event_loop.run().fuse();
        futures::pin_mut!(event_loop_fut);

        let fut = interfaces::testutil::respond_to_watcher(
            watcher_stream.by_ref(),
            [
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(interfaces::testutil::LO_INTERFACE_ID),
                    name: Some(interfaces::testutil::LO_NAME.to_string()),
                    device_class: Some(interfaces::testutil::LOOPBACK),
                    online: Some(true),
                    addresses: Some(vec![
                        interfaces::testutil::test_addr(interfaces::testutil::TEST_V4_ADDR),
                        interfaces::testutil::test_addr(interfaces::testutil::TEST_V6_ADDR),
                    ]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(interfaces::testutil::PPP_INTERFACE_ID),
                    name: Some(interfaces::testutil::PPP_NAME.to_string()),
                    device_class: Some(interfaces::testutil::PPP),
                    online: Some(false),
                    addresses: Some(vec![
                        interfaces::testutil::test_addr(interfaces::testutil::TEST_V6_ADDR),
                        interfaces::testutil::test_addr(interfaces::testutil::TEST_V4_ADDR),
                    ]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
            ],
        )
        .then(|()| handler.handle_request(request, &mut client));

        futures::select! {
            () = fut.fuse() => {},
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }

        client_sink.take_messages()
    }

    fn test_get_link_dump_messages() -> impl IntoIterator<Item = interfaces::NetlinkLinkMessage> {
        [
            interfaces::testutil::create_netlink_link_message(
                interfaces::testutil::LO_INTERFACE_ID,
                ARPHRD_LOOPBACK,
                IFF_UP | IFF_RUNNING | IFF_LOOPBACK,
                interfaces::testutil::create_nlas(
                    interfaces::testutil::LO_NAME.to_string(),
                    ARPHRD_LOOPBACK,
                    true,
                ),
            ),
            interfaces::testutil::create_netlink_link_message(
                interfaces::testutil::PPP_INTERFACE_ID,
                ARPHRD_PPP,
                0,
                interfaces::testutil::create_nlas(
                    interfaces::testutil::PPP_NAME.to_string(),
                    ARPHRD_PPP,
                    false,
                ),
            ),
        ]
    }

    /// Test RTM_GETLINK.
    #[test_case(
        0,
        [],
        None; "no_flags")]
    #[test_case(
        NLM_F_ACK,
        [],
        Some(ExpectedResponse::Ack(AckErrorCode)); "ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        test_get_link_dump_messages(),
        Some(ExpectedResponse::Done); "dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        test_get_link_dump_messages(),
        Some(ExpectedResponse::Done); "dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_link(
        flags: u16,
        messages: impl IntoIterator<Item = interfaces::NetlinkLinkMessage>,
        expected_response: Option<ExpectedResponse<AckErrorCode>>,
    ) {
        let header = header_with_flags(flags);

        pretty_assertions::assert_eq!(
            {
                let mut messages = test_request(NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetLink(LinkMessage::default())),
                ))
                .await;
                if flags & NLM_F_DUMP == NLM_F_DUMP {
                    // Ignore the last message in a dump when sorting since its
                    // a done message.
                    let len = messages.len();
                    interfaces::testutil::sort_link_messages(&mut messages[..len - 1]);
                }
                messages
            },
            messages
                .into_iter()
                .map(|l| l.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER))
                .chain(expected_response.map(|expected_response| match expected_response {
                    ExpectedResponse::Ack(code) => new_ack(code, header),
                    ExpectedResponse::Done => new_done(header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    fn test_get_v4_addr_dump_messages(
    ) -> impl IntoIterator<Item = interfaces::NetlinkAddressMessage> {
        [
            interfaces::testutil::create_address_message(
                interfaces::testutil::LO_INTERFACE_ID.try_into().unwrap(),
                interfaces::testutil::TEST_V4_ADDR,
                interfaces::testutil::LO_NAME.to_string(),
                IFA_F_PERMANENT,
            ),
            interfaces::testutil::create_address_message(
                interfaces::testutil::PPP_INTERFACE_ID.try_into().unwrap(),
                interfaces::testutil::TEST_V4_ADDR,
                interfaces::testutil::PPP_NAME.to_string(),
                IFA_F_PERMANENT,
            ),
        ]
    }

    fn test_get_v6_addr_dump_messages(
    ) -> impl IntoIterator<Item = interfaces::NetlinkAddressMessage> {
        [
            interfaces::testutil::create_address_message(
                interfaces::testutil::LO_INTERFACE_ID.try_into().unwrap(),
                interfaces::testutil::TEST_V6_ADDR,
                interfaces::testutil::LO_NAME.to_string(),
                IFA_F_PERMANENT,
            ),
            interfaces::testutil::create_address_message(
                interfaces::testutil::PPP_INTERFACE_ID.try_into().unwrap(),
                interfaces::testutil::TEST_V6_ADDR,
                interfaces::testutil::PPP_NAME.to_string(),
                IFA_F_PERMANENT,
            ),
        ]
    }

    /// Test RTM_GETADDR.
    #[test_case(
        0,
        AF_UNSPEC,
        [],
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        [],
        Some(ExpectedResponse::Ack(Ok(()))); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        test_get_v4_addr_dump_messages().into_iter().chain(test_get_v6_addr_dump_messages()),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        test_get_v4_addr_dump_messages().into_iter().chain(test_get_v6_addr_dump_messages()),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        [],
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        [],
        Some(ExpectedResponse::Ack(Ok(()))); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        test_get_v4_addr_dump_messages(),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        test_get_v4_addr_dump_messages(),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        [],
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        [],
        Some(ExpectedResponse::Ack(Ok(()))); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        test_get_v6_addr_dump_messages(),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        test_get_v6_addr_dump_messages(),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        [],
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        [],
        Some(ExpectedResponse::Ack(Ok(()))); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        [],
        Some(ExpectedResponse::Ack(Err(NackErrorCode::INVALID))); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        [],
        Some(ExpectedResponse::Ack(Err(NackErrorCode::INVALID))); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_addr(
        flags: u16,
        family: u16,
        messages: impl IntoIterator<Item = interfaces::NetlinkAddressMessage>,
        expected_response: Option<ExpectedResponse<Result<(), NackErrorCode>>>,
    ) {
        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_request(NetlinkMessage::new(
                header,
                NetlinkPayload::InnerMessage(RtnlMessage::GetAddress(address_message)),
            ))
            .await,
            {
                let mut messages = messages
                    .into_iter()
                    .map(|a| a.to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER))
                    .collect::<Vec<_>>();
                messages.sort_by_key(|message| {
                    assert_matches!(
                        &message.payload,
                        NetlinkPayload::InnerMessage(RtnlMessage::NewAddress(m)) => {
                            (m.header.index, m.header.family)
                        }
                    )
                });
                messages
                    .into_iter()
                    .chain(expected_response.map(|expected_response| match expected_response {
                        ExpectedResponse::Ack(code) => new_ack(code, header),
                        ExpectedResponse::Done => new_done(header),
                    }))
                    .collect::<Vec<_>>()
            },
        )
    }
}
