// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing protocol-specific aspects of Netlink.

use crate::{
    client::ExternalClient,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastCapableNetlinkFamily, SingleLegacyGroup,
    },
};

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

    use netlink_packet_route::rtnl::constants::{
        RTNLGRP_DCB, RTNLGRP_DECNET_IFADDR, RTNLGRP_DECNET_ROUTE, RTNLGRP_DECNET_RULE,
        RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE, RTNLGRP_IPV4_MROUTE_R, RTNLGRP_IPV4_NETCONF,
        RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR, RTNLGRP_IPV6_IFINFO,
        RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R, RTNLGRP_IPV6_NETCONF, RTNLGRP_IPV6_PREFIX,
        RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE, RTNLGRP_LINK, RTNLGRP_MDB, RTNLGRP_MPLS_NETCONF,
        RTNLGRP_MPLS_ROUTE, RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH, RTNLGRP_NONE, RTNLGRP_NOP2,
        RTNLGRP_NOP4, RTNLGRP_NOTIFY, RTNLGRP_NSID, RTNLGRP_PHONET_IFADDR, RTNLGRP_PHONET_ROUTE,
        RTNLGRP_TC,
    };

    /// An implementation of the Netlink Route protocol family.
    pub(crate) struct NetlinkRoute {}

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

    /// A connection to the Route Netlink Protocol family.
    pub struct NetlinkRouteClient(pub(crate) ExternalClient<NetlinkRoute>);

    impl NetlinkRouteClient {
        /// Adds the given multicast group membership.
        pub fn add_membership(
            NetlinkRouteClient(client): &Self,
            group: ModernGroup,
        ) -> Result<(), InvalidModernGroupError> {
            client.add_membership(group)
        }

        /// Deletes the given multicast group membership.
        pub fn del_membership(
            NetlinkRouteClient(client): &Self,
            group: ModernGroup,
        ) -> Result<(), InvalidModernGroupError> {
            client.del_membership(group)
        }

        /// Sets the legacy multicast group memberships.
        pub fn set_legacy_memberships(
            NetlinkRouteClient(client): &Self,
            legacy_memberships: LegacyGroups,
        ) -> Result<(), InvalidLegacyGroupsError> {
            client.set_legacy_memberships(legacy_memberships)
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use std::collections::HashMap;

    pub(crate) const LEGACY_GROUP1: u32 = 0x00000001;
    pub(crate) const LEGACY_GROUP2: u32 = 0x00000010;
    pub(crate) const LEGACY_GROUP3: u32 = 0x00000100;
    pub(crate) const INVALID_LEGACY_GROUP: LegacyGroups = LegacyGroups(0x00001000);
    pub(crate) const MODERN_GROUP1: ModernGroup = ModernGroup(1);
    pub(crate) const MODERN_GROUP2: ModernGroup = ModernGroup(2);
    pub(crate) const MODERN_GROUP3: ModernGroup = ModernGroup(10);
    pub(crate) const INVALID_MODERN_GROUP: ModernGroup = ModernGroup(20);

    #[derive(Debug)]
    pub(crate) struct FakeProtocolFamily(HashMap<SingleLegacyGroup, ModernGroup>);

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
}
