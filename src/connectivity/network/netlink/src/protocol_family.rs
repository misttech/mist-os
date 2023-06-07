// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing protocol-specific aspects of Netlink.

use netlink_packet_core::{NetlinkMessage, NetlinkPayload};
use netlink_packet_utils::Emitable;

use std::fmt::Debug;

// TODO(https://github.com/rust-lang/rust/issues/91611): Replace this with
// #![feature(async_fn_in_trait)] once it supports `Send` bounds. See
// https://blog.rust-lang.org/inside-rust/2023/05/03/stabilizing-async-fn-in-trait.html.
use async_trait::async_trait;
use tracing::warn;

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
    type Message: Clone + Debug + Emitable + Send + 'static;
    /// The implementation for handling requests from this protocol family.
    type RequestHandler<S: Sender<Self::Message>>: NetlinkFamilyRequestHandler<Self, S>;

    const NAME: &'static str;
}

#[async_trait]
/// A request handler implementation for a particular Netlink protocol family.
pub(crate) trait NetlinkFamilyRequestHandler<F: ProtocolFamily, S: Sender<F::Message>>:
    Clone + Send + 'static
{
    /// Handles the given request and generates the associated response(s).
    async fn handle_request(&mut self, req: F::Message, client: &mut InternalClient<F, S>);
}

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

    use futures::channel::mpsc;

    use crate::interfaces;

    use netlink_packet_core::{NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::rtnl::{
        constants::{
            RTNLGRP_DCB, RTNLGRP_DECNET_IFADDR, RTNLGRP_DECNET_ROUTE, RTNLGRP_DECNET_RULE,
            RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE, RTNLGRP_IPV4_MROUTE_R, RTNLGRP_IPV4_NETCONF,
            RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR, RTNLGRP_IPV6_IFINFO,
            RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R, RTNLGRP_IPV6_NETCONF, RTNLGRP_IPV6_PREFIX,
            RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE, RTNLGRP_LINK, RTNLGRP_MDB, RTNLGRP_MPLS_NETCONF,
            RTNLGRP_MPLS_ROUTE, RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH, RTNLGRP_NONE, RTNLGRP_NOP2,
            RTNLGRP_NOP4, RTNLGRP_NOTIFY, RTNLGRP_NSID, RTNLGRP_PHONET_IFADDR,
            RTNLGRP_PHONET_ROUTE, RTNLGRP_TC,
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
        type Message = NetlinkMessage<RtnlMessage>;
        type RequestHandler<S: Sender<Self::Message>> = NetlinkRouteRequestHandler<S>;

        const NAME: &'static str = "NETLINK_ROUTE";
    }

    #[derive(Clone)]
    pub(crate) struct NetlinkRouteRequestHandler<
        S: Sender<<NetlinkRoute as ProtocolFamily>::Message>,
    > {
        // TODO(https://issuetracker.google.com/283827094): Handle interface/address
        // requests.
        #[allow(unused)]
        pub(crate) interfaces_request_sink: mpsc::Sender<interfaces::Request<S>>,
    }

    #[async_trait]
    impl<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>>
        NetlinkFamilyRequestHandler<NetlinkRoute, S> for NetlinkRouteRequestHandler<S>
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<RtnlMessage>,
            client: &mut InternalClient<NetlinkRoute, S>,
        ) {
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

            use RtnlMessage::*;
            match req {
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
                    if req_header.flags&NLM_F_ACK == NLM_F_ACK {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                            req,
                        );
                        client.send(crate::netlink_packet::new_ack(req_header))
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
                    if req_header.flags&NLM_F_DUMP == NLM_F_DUMP {
                        warn!(
                            "Received unsupported NETLINK_ROUTE DUMP request; responding with Done: {:?}",
                            req
                        );
                        client.send(crate::netlink_packet::new_done())
                    } else if req_header.flags&NLM_F_ACK == NLM_F_ACK {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request: responding with Ack {:?}",
                            req
                        );
                        client.send(crate::netlink_packet::new_ack(req_header))
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

    use netlink_packet_utils::Emitable;

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

    #[derive(Clone, Debug, Default, PartialEq)]
    pub(crate) struct FakeNetlinkMessage;

    impl Emitable for FakeNetlinkMessage {
        fn buffer_len(&self) -> usize {
            0
        }

        fn emit(&self, _buffer: &mut [u8]) {}
    }

    /// Handler of [`FakeNetlinkMessage`] requests.
    ///
    /// Reflects the given request back as the response.
    #[derive(Clone)]
    pub(crate) struct FakeNetlinkRequestHandler;

    #[async_trait]
    impl<S: Sender<FakeNetlinkMessage>> NetlinkFamilyRequestHandler<FakeProtocolFamily, S>
        for FakeNetlinkRequestHandler
    {
        async fn handle_request(
            &mut self,
            req: FakeNetlinkMessage,
            client: &mut InternalClient<FakeProtocolFamily, S>,
        ) {
            client.send(req)
        }
    }

    impl ProtocolFamily for FakeProtocolFamily {
        type Message = FakeNetlinkMessage;
        type RequestHandler<S: Sender<Self::Message>> = FakeNetlinkRequestHandler;

        const NAME: &'static str = "FAKE_PROTOCOL_FAMILY";
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{
        messaging::testutil::FakeSender,
        netlink_packet::{new_ack, new_done},
        protocol_family::route::{NetlinkRoute, NetlinkRouteRequestHandler},
    };
    use futures::channel::mpsc;
    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::{RtnlMessage, TcMessage};
    use test_case::test_case;

    enum ExpectedResponse {
        Ack,
        Done,
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
        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> { interfaces_request_sink };

        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let header = {
            let mut header = NetlinkHeader::default();
            header.flags = flags;
            header
        };

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
                assert_eq!(client_sink.take_messages(), [new_ack(header)])
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(client_sink.take_messages(), [new_done()])
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }
}
