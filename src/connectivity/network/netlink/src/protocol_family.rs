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
    client::ExternalClient,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastCapableNetlinkFamily, SingleLegacyGroup,
    },
};

/// A type representing a Netlink Protocol Family.
pub(crate) trait ProtocolFamily: MulticastCapableNetlinkFamily + Send + 'static {
    /// The message type associated with the protocol family.
    type Message: Clone + Debug + Emitable + Send + 'static;
    /// The implementation for handling requests from this protocol family.
    type RequestHandler: NetlinkFamilyRequestHandler<Self::Message>;

    const NAME: &'static str;
}

#[async_trait]
/// A request handler implementation for a particular Netlink protocol family.
pub(crate) trait NetlinkFamilyRequestHandler<M>: Clone + Send + 'static {
    /// Handles the given request and generates the associated response(s).
    async fn handle_request(&mut self, req: M) -> Vec<M>;
}

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

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
        fn legacy_to_modern(group: SingleLegacyGroup) -> Option<ModernGroup> {
            // Legacy NETLINK_ROUTE multicast groups.
            const RTMGRP_LINK: u32 = 0x1;
            const RTMGRP_NOTIFY: u32 = 0x2;
            const RTMGRP_NEIGH: u32 = 0x4;
            const RTMGRP_TC: u32 = 0x8;
            const RTMGRP_IPV4_IFADDR: u32 = 0x10;
            const RTMGRP_IPV4_MROUTE: u32 = 0x20;
            const RTMGRP_IPV4_ROUTE: u32 = 0x40;
            const RTMGRP_IPV4_RULE: u32 = 0x80;
            const RTMGRP_IPV6_IFADDR: u32 = 0x100;
            const RTMGRP_IPV6_MROUTE: u32 = 0x200;
            const RTMGRP_IPV6_ROUTE: u32 = 0x400;
            const RTMGRP_IPV6_RULE: u32 = 0x800;
            const RTMGRP_DECNET_IFADDR: u32 = 0x1000;
            const RTMGRP_DECNET_ROUTE: u32 = 0x2000;
            const RTMGRP_IPV6_PREFIX: u32 = 0x20000;

            match group.inner() {
                RTMGRP_LINK => Some(RTNLGRP_LINK),
                RTMGRP_NOTIFY => Some(RTNLGRP_NOTIFY),
                RTMGRP_NEIGH => Some(RTNLGRP_NEIGH),
                RTMGRP_TC => Some(RTNLGRP_TC),
                RTMGRP_IPV4_IFADDR => Some(RTNLGRP_IPV4_IFADDR),
                RTMGRP_IPV4_MROUTE => Some(RTNLGRP_IPV4_MROUTE),
                RTMGRP_IPV4_ROUTE => Some(RTNLGRP_IPV4_ROUTE),
                RTMGRP_IPV4_RULE => Some(RTNLGRP_IPV4_RULE),
                RTMGRP_IPV6_IFADDR => Some(RTNLGRP_IPV6_IFADDR),
                RTMGRP_IPV6_MROUTE => Some(RTNLGRP_IPV6_MROUTE),
                RTMGRP_IPV6_ROUTE => Some(RTNLGRP_IPV6_ROUTE),
                RTMGRP_IPV6_RULE => Some(RTNLGRP_IPV6_RULE),
                RTMGRP_DECNET_IFADDR => Some(RTNLGRP_DECNET_IFADDR),
                RTMGRP_DECNET_ROUTE => Some(RTNLGRP_DECNET_ROUTE),
                RTMGRP_IPV6_PREFIX => Some(RTNLGRP_IPV6_PREFIX),
                _ => None,
            }
            .map(ModernGroup)
        }

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
        type RequestHandler = NetlinkRouteRequestHandler;

        const NAME: &'static str = "NETLINK_ROUTE";
    }

    #[derive(Clone)]
    pub(crate) struct NetlinkRouteRequestHandler;

    #[async_trait]
    impl NetlinkFamilyRequestHandler<NetlinkMessage<RtnlMessage>> for NetlinkRouteRequestHandler {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<RtnlMessage>,
        ) -> Vec<NetlinkMessage<RtnlMessage>> {
            let (req_header, payload) = req.into_parts();
            let req = match payload {
                NetlinkPayload::InnerMessage(p) => p,
                p => panic!("Unexpected netlink payload: {:?}", p),
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
                    warn!(
                        "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                        req
                    );
                    vec![crate::netlink_packet::new_ack(req_header)]
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
                    warn!(
                        "Received unsupported NETLINK_ROUTE request; responding with Done: {:?}",
                        req
                    );
                    vec![crate::netlink_packet::new_done()]
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
    pub(crate) const LEGACY_GROUP2: u32 = 0x00000010;
    pub(crate) const LEGACY_GROUP3: u32 = 0x00000100;
    pub(crate) const INVALID_LEGACY_GROUP: LegacyGroups = LegacyGroups(0x00001000);
    pub(crate) const MODERN_GROUP1: ModernGroup = ModernGroup(1);
    pub(crate) const MODERN_GROUP2: ModernGroup = ModernGroup(2);
    pub(crate) const MODERN_GROUP3: ModernGroup = ModernGroup(10);
    pub(crate) const INVALID_MODERN_GROUP: ModernGroup = ModernGroup(20);

    #[derive(Debug)]
    pub(crate) enum FakeProtocolFamily {}

    impl MulticastCapableNetlinkFamily for FakeProtocolFamily {
        fn legacy_to_modern(legacy_group: SingleLegacyGroup) -> Option<ModernGroup> {
            match legacy_group.inner() {
                LEGACY_GROUP1 => Some(MODERN_GROUP1),
                LEGACY_GROUP2 => Some(MODERN_GROUP2),
                LEGACY_GROUP3 => Some(MODERN_GROUP3),
                _ => None,
            }
        }
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
    impl<M: Send + 'static> NetlinkFamilyRequestHandler<M> for FakeNetlinkRequestHandler {
        async fn handle_request(&mut self, req: M) -> Vec<M> {
            vec![req]
        }
    }

    impl ProtocolFamily for FakeProtocolFamily {
        type Message = FakeNetlinkMessage;
        type RequestHandler = FakeNetlinkRequestHandler;

        const NAME: &'static str = "FAKE_PROTOCOL_FAMILY";
    }
}
