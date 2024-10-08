// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;

use crate::routes::NetlinkRouteMessage;

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NetlinkRouteTableIndex(u32);

impl NetlinkRouteTableIndex {
    pub(crate) const fn new(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn get(self) -> u32 {
        let Self(index) = self;
        index
    }
}

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NonZeroNetlinkRouteTableIndex(NonZeroU32);

impl NonZeroNetlinkRouteTableIndex {
    pub(crate) const fn new(index: NetlinkRouteTableIndex) -> Option<Self> {
        let NetlinkRouteTableIndex(index) = index;
        // NB: `Option::map` is not available in `const`
        match NonZeroU32::new(index) {
            None => None,
            Some(index) => Some(Self(index)),
        }
    }

    pub(crate) const fn new_non_zero(index: NonZeroU32) -> Self {
        Self(index)
    }

    pub(crate) const fn get(self) -> NonZeroU32 {
        let Self(index) = self;
        index
    }
}

impl From<NonZeroNetlinkRouteTableIndex> for NetlinkRouteTableIndex {
    fn from(index: NonZeroNetlinkRouteTableIndex) -> Self {
        Self(index.get().get())
    }
}

// A `RouteTable` identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RouteTableKey {
    // Routing tables that are created and managed by Netlink.
    NetlinkManaged { table_id: NetlinkRouteTableIndex },
    // Routing tables that are not managed by Netlink, but exist on the system.
    Unmanaged,
}

pub(crate) struct RouteTable<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    pub(crate) route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,
    pub(crate) route_messages: HashSet<NetlinkRouteMessage>,
}

pub(crate) struct RouteTableMap<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    route_tables: HashMap<RouteTableKey, RouteTable<I>>,
}

impl<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    RouteTableMap<I>
{
    pub(crate) fn new() -> Self {
        Self { route_tables: HashMap::new() }
    }

    pub(crate) fn from(route_tables: HashMap<RouteTableKey, RouteTable<I>>) -> Self {
        Self { route_tables }
    }

    pub(crate) fn get(&self, key: &RouteTableKey) -> Option<&RouteTable<I>> {
        self.route_tables.get(key)
    }

    pub(crate) fn get_mut(&mut self, key: &RouteTableKey) -> Option<&mut RouteTable<I>> {
        self.route_tables.get_mut(key)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&RouteTableKey, &RouteTable<I>)> + '_ {
        self.route_tables.iter()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &RouteTable<I>> + '_ {
        self.route_tables.values()
    }

    pub(crate) fn entry(
        &mut self,
        key: RouteTableKey,
    ) -> std::collections::hash_map::Entry<'_, RouteTableKey, RouteTable<I>> {
        self.route_tables.entry(key)
    }
}

impl<'a, I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    std::ops::Index<&'a RouteTableKey> for RouteTableMap<I>
{
    type Output = RouteTable<I>;

    fn index(&self, index: &'a RouteTableKey) -> &Self::Output {
        &self.route_tables[index]
    }
}
