// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;

use assert_matches::assert_matches;
use net_types::ip::{GenericOverIp, Ip};

use crate::routes::{NetlinkRouteMessage, UNMANAGED_ROUTE_TABLE_INDEX};

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
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

/// The index of a route table known to be managed by the netlink worker (so, not the main table).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub(crate) struct ManagedNetlinkRouteTableIndex(NetlinkRouteTableIndex);

impl ManagedNetlinkRouteTableIndex {
    pub(crate) const fn new(index: NetlinkRouteTableIndex) -> Option<Self> {
        if index.get() == UNMANAGED_ROUTE_TABLE_INDEX.get() {
            None
        } else {
            Some(Self(index))
        }
    }

    pub(crate) const fn get(self) -> NetlinkRouteTableIndex {
        let Self(index) = self;
        index
    }
}

impl From<ManagedNetlinkRouteTableIndex> for NetlinkRouteTableIndex {
    fn from(index: ManagedNetlinkRouteTableIndex) -> Self {
        index.get()
    }
}

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
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
    NetlinkManaged { table_id: ManagedNetlinkRouteTableIndex },
    // Routing tables that are not managed by Netlink, but exist on the system.
    Unmanaged,
}

impl RouteTableKey {
    pub(crate) const fn from(index: NetlinkRouteTableIndex) -> RouteTableKey {
        match ManagedNetlinkRouteTableIndex::new(index) {
            Some(table_id) => RouteTableKey::NetlinkManaged { table_id },
            None => RouteTableKey::Unmanaged,
        }
    }
}

impl From<NetlinkRouteTableIndex> for RouteTableKey {
    fn from(index: NetlinkRouteTableIndex) -> RouteTableKey {
        RouteTableKey::from(index)
    }
}

/// Returned as the `Err` variant when an operation expected to act on a managed route table.
#[derive(Debug, Copy, Clone)]
pub(crate) struct UnmanagedTableError;

/// State tracked for a route table managed by the netlink worker.
#[derive(derivative::Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct RouteTable<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    /// The route table FIDL proxy backing this route table.
    // NOTE: this field is currently never read, but must still be retained in order to keep
    // alive the corresponding route table.
    #[derivative(Debug = "ignore")]
    pub(crate) _route_table_proxy: <I::RouteTableMarker as ProtocolMarker>::Proxy,

    /// A route set derived from the corresponding FIDL route table.
    #[derivative(Debug = "ignore")]
    pub(crate) route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,

    /// A route set derived from the main route table, used to install a "backup" copy of the
    /// route in the main table to facilitate the transition to PBR rules support in netlink.
    // TODO(https://fxbug.dev/358649849): Remove this once netlink supports PBR.
    #[derivative(Debug = "ignore")]
    pub(crate) route_set_from_main_table_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,

    /// The ID of the corresponding FIDL route table.
    pub(crate) fidl_table_id: fnet_routes_ext::TableId,
}

/// The result of looking up a route table by [`NetlinkRouteTableIndex`].
#[derive(derivative::Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) enum RouteTableLookup<
    'a,
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    Managed(&'a RouteTable<I>),
    Unmanaged(#[derivative(Debug = "ignore")] &'a <I::RouteSetMarker as ProtocolMarker>::Proxy),
}

/// Contains the route tables tracked by the netlink worker.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) struct RouteTableMap<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    route_tables: HashMap<ManagedNetlinkRouteTableIndex, RouteTable<I>>,
    fidl_table_ids: HashMap<fnet_routes_ext::TableId, ManagedNetlinkRouteTableIndex>,

    route_table_provider: <I::RouteTableProviderMarker as ProtocolMarker>::Proxy,
    main_route_table_proxy: <I::RouteTableMarker as ProtocolMarker>::Proxy,
    unmanaged_route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,

    pub(crate) main_route_table_id: fnet_routes_ext::TableId,
}

impl<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    RouteTableMap<I>
{
    pub(crate) fn new(
        main_route_table_proxy: <I::RouteTableMarker as ProtocolMarker>::Proxy,
        main_route_table_id: fnet_routes_ext::TableId,
        unmanaged_route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,
        route_table_provider: <I::RouteTableProviderMarker as ProtocolMarker>::Proxy,
    ) -> Self {
        Self {
            route_tables: HashMap::new(),
            fidl_table_ids: HashMap::new(),
            main_route_table_proxy,
            main_route_table_id,
            unmanaged_route_set_proxy,
            route_table_provider,
        }
    }

    /// Maps from FIDL table IDs to their corresponding [`RouteTableKey`].
    pub(crate) fn lookup_route_table_key_from_fidl_table_id(
        &self,
        fidl_table_id: fnet_routes_ext::TableId,
    ) -> Option<RouteTableKey> {
        if fidl_table_id == self.main_route_table_id {
            Some(RouteTableKey::Unmanaged)
        } else {
            Some(RouteTableKey::NetlinkManaged {
                table_id: self.fidl_table_ids.get(&fidl_table_id).copied()?,
            })
        }
    }

    /// Looks up a route table.
    pub(crate) fn get(&self, key: &NetlinkRouteTableIndex) -> Option<RouteTableLookup<'_, I>> {
        match RouteTableKey::from(*key) {
            RouteTableKey::Unmanaged => {
                Some(RouteTableLookup::Unmanaged(&self.unmanaged_route_set_proxy))
            }
            RouteTableKey::NetlinkManaged { table_id } => {
                self.route_tables.get(&table_id).map(RouteTableLookup::Managed)
            }
        }
    }

    /// Maps from a [`NetlinkRouteTableIndex`] to the corresponding FIDL table ID.
    pub(crate) fn get_fidl_table_id(
        &self,
        key: &NetlinkRouteTableIndex,
    ) -> Option<fnet_routes_ext::TableId> {
        match self.get(key)? {
            RouteTableLookup::Unmanaged(_) => Some(self.main_route_table_id),
            RouteTableLookup::Managed(route_table) => Some(route_table.fidl_table_id),
        }
    }

    /// Inserts a new netlink-managed route table.
    pub(crate) fn insert(&mut self, key: ManagedNetlinkRouteTableIndex, value: RouteTable<I>) {
        let fidl_table_id = value.fidl_table_id;
        let prev = self.route_tables.insert(key, value);
        assert_matches!(prev, None, "{prev:?}");

        let prev = self.fidl_table_ids.insert(fidl_table_id, key);
        assert_matches!(
            prev,
            None,
            "fidl_table_id {fidl_table_id:?} already maps to netlink table {prev:?}"
        );
    }

    /// Removes a netlink-managed route table.
    fn remove(&mut self, key: ManagedNetlinkRouteTableIndex) -> Option<RouteTable<I>> {
        let Self { route_tables, fidl_table_ids, .. } = self;
        let route_table = route_tables.remove(&key)?;
        assert_eq!(fidl_table_ids.remove(&route_table.fidl_table_id), Some(key));
        Some(route_table)
    }

    /// Removes the table matching the given FIDL table ID, if there is a managed table
    /// matching it.
    pub(crate) fn remove_table_by_fidl_id(
        &mut self,
        table_id: fnet_routes_ext::TableId,
    ) -> Option<Result<RouteTable<I>, UnmanagedTableError>> {
        match self.lookup_route_table_key_from_fidl_table_id(table_id) {
            None => None,
            Some(RouteTableKey::Unmanaged) => Some(Err(UnmanagedTableError)),
            Some(RouteTableKey::NetlinkManaged { table_id }) => self.remove(table_id).map(Ok),
        }
    }

    /// If a table corresponding to `key` is not already being tracked, and `key` does not
    /// correspond to the main table, initializes a new [`RouteTable`] entry for this key.
    pub(crate) async fn create_route_table_if_managed_and_not_present(
        &mut self,
        key: NetlinkRouteTableIndex,
    ) {
        match RouteTableKey::from(key) {
            RouteTableKey::Unmanaged => {}
            RouteTableKey::NetlinkManaged { table_id } => {
                self.create_route_table_if_not_present(table_id).await;
            }
        }
    }

    /// If a table corresponding to `key` is not already being tracked, initializes a new
    /// [`RouteTable`] entry for this key.
    async fn create_route_table_if_not_present(&mut self, key: ManagedNetlinkRouteTableIndex) {
        let should_insert = self.route_tables.get(&key).is_none();

        // There's nothing graceful we can do if any of these operations fail, so we might as well
        // panic.
        if should_insert {
            let route_table_proxy = fnet_routes_ext::admin::new_route_table::<I>(
                &self.route_table_provider,
                Some(format!("netlink:{}", key.get().get())),
            )
            .expect("error creating new route table");
            let fidl_table_id = fnet_routes_ext::admin::get_table_id::<I>(&route_table_proxy)
                .await
                .expect("error getting table ID");
            let route_set_proxy = fnet_routes_ext::admin::new_route_set::<I>(&route_table_proxy)
                .expect("error creating new route set");
            let route_set_from_main_table_proxy =
                fnet_routes_ext::admin::new_route_set::<I>(&self.main_route_table_proxy)
                    .expect("error creating new route set");
            self.insert(
                key,
                RouteTable {
                    _route_table_proxy: route_table_proxy,
                    route_set_from_main_table_proxy,
                    route_set_proxy,
                    fidl_table_id,
                },
            );
        }
    }
}

impl<'a, I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    std::ops::Index<&'a ManagedNetlinkRouteTableIndex> for RouteTableMap<I>
{
    type Output = RouteTable<I>;

    fn index(&self, index: &'a ManagedNetlinkRouteTableIndex) -> &Self::Output {
        &self.route_tables[index]
    }
}

/// All of the routes known to the netlink worker, and the tables in which they are installed.
/// This is populated via the fuchsia.net.routes watcher protocol, and may include routes in tables
/// that are not known to netlink (i.e. route tables that were created by other Fuchsia components
/// via fuchsia.net.routes.admin.RouteTableProvider).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FidlRouteMap<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    routes: BTreeMap<
        fnet_routes_ext::Route<I>,
        BTreeMap<fnet_routes_ext::TableId, fnet_routes_ext::EffectiveRouteProperties>,
    >,
    by_tables: BTreeMap<
        fnet_routes_ext::TableId,
        BTreeMap<fnet_routes_ext::Route<I>, fnet_routes_ext::EffectiveRouteProperties>,
    >,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(crate) enum RouteRemoveResult {
    DidNotExist,
    RemovedButTableNotEmpty(fnet_routes_ext::EffectiveRouteProperties),
    RemovedAndTableNewlyEmpty(fnet_routes_ext::EffectiveRouteProperties),
}

impl<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    FidlRouteMap<I>
{
    /// Returns whether the given route is installed in all of the specified FIDL tables.
    pub(crate) fn route_is_installed_in_tables<'a>(
        &'a self,
        route: &'a fnet_routes_ext::Route<I>,
        tables: impl IntoIterator<Item = &'a fnet_routes_ext::TableId>,
    ) -> bool {
        match self.routes.get(route) {
            None => false,
            Some(installed_tables) => {
                tables.into_iter().all(|table| installed_tables.contains_key(table))
            }
        }
    }

    /// Returns whether the given route is uninstalled from all of the specified FIDL tables.
    pub(crate) fn route_is_uninstalled_in_tables<'a>(
        &'a self,
        route: &'a fnet_routes_ext::Route<I>,
        tables: impl IntoIterator<Item = &'a fnet_routes_ext::TableId>,
    ) -> bool {
        match self.routes.get(route) {
            None => true,
            Some(installed_tables) => {
                tables.into_iter().all(|table| !installed_tables.contains_key(table))
            }
        }
    }

    /// Adds the route, table, and properties to the set of route-table pairs tracked by this map.
    pub(crate) fn add(
        &mut self,
        route: fnet_routes_ext::Route<I>,
        table: fnet_routes_ext::TableId,
        properties: fnet_routes_ext::EffectiveRouteProperties,
    ) -> Option<fnet_routes_ext::EffectiveRouteProperties> {
        let props_by_routes = self.routes.entry(route).or_default().insert(table, properties);
        let props_by_tables = self.by_tables.entry(table).or_default().insert(route, properties);
        assert_eq!(props_by_routes, props_by_tables);
        props_by_routes
    }

    /// Removes the (route, table) combination from this map.
    ///
    /// The returned [`RouteRemoveResult`] indicates the outcome of the removal and whether it
    /// resulted in a table becoming empty.
    pub(crate) fn remove(
        &mut self,
        route: fnet_routes_ext::Route<I>,
        table: fnet_routes_ext::TableId,
    ) -> RouteRemoveResult {
        let props_by_routes = match self.routes.entry(route) {
            Entry::Vacant(_) => None,
            Entry::Occupied(mut occupied_entry) => {
                let removed = occupied_entry.get_mut().remove(&table);
                if removed.is_some() && occupied_entry.get().is_empty() {
                    let _: BTreeMap<_, _> = occupied_entry.remove();
                }
                removed
            }
        };
        let props_by_tables = match self.by_tables.entry(table) {
            Entry::Vacant(_) => None,
            Entry::Occupied(mut occupied_entry) => {
                let removed_props = occupied_entry.get_mut().remove(&route);
                match removed_props {
                    None => None,
                    Some(removed_props) => {
                        let removed_last_route_from_table = if occupied_entry.get().is_empty() {
                            let _: BTreeMap<
                                fnet_routes_ext::Route<I>,
                                fnet_routes_ext::EffectiveRouteProperties,
                            > = occupied_entry.remove();
                            true
                        } else {
                            false
                        };
                        Some((removed_props, removed_last_route_from_table))
                    }
                }
            }
        };
        assert_eq!(props_by_routes, props_by_tables.map(|(props, _removed_last)| props));

        match props_by_tables {
            None => RouteRemoveResult::DidNotExist,
            Some((props, false)) => RouteRemoveResult::RemovedButTableNotEmpty(props),
            Some((props, true)) => RouteRemoveResult::RemovedAndTableNewlyEmpty(props),
        }
    }

    pub(crate) fn iter<'a>(
        &'a self,
    ) -> impl Iterator<
        Item = (
            &'a fnet_routes_ext::Route<I>,
            &'a BTreeMap<fnet_routes_ext::TableId, fnet_routes_ext::EffectiveRouteProperties>,
        ),
    > {
        self.routes.iter()
    }

    /// Returns an iterator over routes installed within the given table.
    pub(crate) fn iter_messages<'a>(
        &'a self,
        route_table_map: &'a RouteTableMap<I>,
        table_id: NetlinkRouteTableIndex,
    ) -> impl Iterator<Item = NetlinkRouteMessage> + 'a {
        route_table_map
            .get_fidl_table_id(&table_id)
            .and_then(|fidl_table_id| {
                self.by_tables.get(&fidl_table_id).map(|routes| {
                    routes
                        .into_iter()
                        .map(move |(route, props)| fnet_routes_ext::InstalledRoute {
                            route: *route,
                            table_id: fidl_table_id,
                            effective_properties: *props,
                        })
                        .filter_map(move |installed_route| {
                            NetlinkRouteMessage::optionally_from(installed_route, table_id)
                        })
                })
            })
            .into_iter()
            .flatten()
    }

    pub(crate) fn iter_table<'a>(
        &'a self,
        table: fnet_routes_ext::TableId,
    ) -> impl Iterator<
        Item = (&'a fnet_routes_ext::Route<I>, &'a fnet_routes_ext::EffectiveRouteProperties),
    > {
        self.by_tables.get(&table).into_iter().flatten()
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use fidl_fuchsia_net_routes_ext::admin::FidlRouteAdminIpExt;
    use itertools::Itertools as _;
    use net_types::ip::Ipv6;
    use proptest::arbitrary::Arbitrary;
    use proptest::prelude::*;
    use proptest::test_runner::Config;
    use proptest_support::failed_seeds;
    use {fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_ext as fnet_routes_ext};

    use super::*;

    impl Arbitrary for NetlinkRouteTableIndex {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;

        fn arbitrary_with((): ()) -> Self::Strategy {
            // NB: use u8 to constrain the space enough to get some collisions.
            // Note that this still includes the unmanaged route table index, which is 254.
            let _: u8 = u8::try_from(crate::routes::UNMANAGED_ROUTE_TABLE_INDEX.get())
                .expect("must be valid u8");

            <u8 as Arbitrary>::arbitrary_with(())
                .prop_map(|index| NetlinkRouteTableIndex(index.into()))
                .boxed()
        }
    }

    impl Arbitrary for ManagedNetlinkRouteTableIndex {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;

        fn arbitrary_with((): ()) -> Self::Strategy {
            <NetlinkRouteTableIndex as Arbitrary>::arbitrary_with(())
                .prop_filter_map(
                    "is unmanaged route table index",
                    ManagedNetlinkRouteTableIndex::new,
                )
                .boxed()
        }
    }

    #[derive(proptest_derive::Arbitrary, Debug, PartialEq, Eq, Hash, Copy, Clone)]
    struct FidlTableId(u8);

    impl From<FidlTableId> for fnet_routes_ext::TableId {
        fn from(id: FidlTableId) -> Self {
            Self::new(id.0.into())
        }
    }

    const NEVER_GENERATED_TABLE_ID: fnet_routes_ext::TableId = fnet_routes_ext::TableId::new(
        // NB: we know we never generate this because 256 is out of bounds for a u8.
        256,
    );

    #[derive(proptest_derive::Arbitrary, Debug, PartialEq, Eq, Hash)]
    enum RouteTableMapOp {
        Insert(ManagedNetlinkRouteTableIndex),
        Remove(ManagedNetlinkRouteTableIndex),
    }

    fn collect_and_assert_all_unique(
        iter: impl Iterator<Item = fnet_routes_ext::TableId>,
    ) -> HashSet<fnet_routes_ext::TableId> {
        let vec = iter.collect::<Vec<_>>();
        let count_from_vec = vec.len();
        let set = vec.into_iter().collect::<HashSet<_>>();
        assert_eq!(set.len(), count_from_vec);
        set
    }

    #[derive(Copy, Clone, Debug)]
    struct TestRoute(fnet_routes_ext::Route<Ipv6>);

    // The contents don't really matter as the routes just need to be distinct according
    // to even 1 field (in this case the interface).
    const fn build_test_route(outbound_interface: u64) -> TestRoute {
        TestRoute(fnet_routes_ext::Route {
            destination: net_declare::net_subnet_v6!("2001:db8:1::/64"),
            action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                outbound_interface,
                next_hop: None,
            }),
            properties: fnet_routes_ext::RouteProperties {
                specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                    metric: fnet_routes::SpecifiedMetric::ExplicitMetric(1),
                },
            },
        })
    }

    const TEST_ROUTES: [TestRoute; 5] = [
        build_test_route(1),
        build_test_route(2),
        build_test_route(3),
        build_test_route(4),
        build_test_route(5),
    ];

    impl Arbitrary for TestRoute {
        type Parameters = ();
        type Strategy = proptest::sample::Select<Self>;

        fn arbitrary_with((): ()) -> Self::Strategy {
            proptest::sample::select(&TEST_ROUTES)
        }
    }

    #[derive(proptest_derive::Arbitrary, Copy, Clone, Debug)]
    struct TestEffectiveRouteProperties(u32);

    impl From<TestEffectiveRouteProperties> for fnet_routes_ext::EffectiveRouteProperties {
        fn from(TestEffectiveRouteProperties(metric): TestEffectiveRouteProperties) -> Self {
            fnet_routes_ext::EffectiveRouteProperties { metric }
        }
    }

    #[derive(proptest_derive::Arbitrary, Copy, Clone, Debug)]
    enum FidlRouteMapOp {
        Add(TestRoute, FidlTableId, TestEffectiveRouteProperties),
        Remove(TestRoute, FidlTableId),
    }

    proptest! {
        #![proptest_config(Config {
            // Add all failed seeds here.
            failure_persistence: failed_seeds!(),
            ..Config::default()
        })]

        // Test that the route table map preserves a 1:1 mapping between
        // `ManagedNetlinkRouteTableIndex` and `fnet_routes_ext::TableId` after arbitrary
        // operations.
        #[test]
        fn route_table_map_is_bijection(ops_to_try: Vec<RouteTableMapOp>) {
            // We create a bunch of proxies that go unused in this test. In order for this to
            // succeed we must have an executor.
            let _executor = fuchsia_async::TestExecutor::new();

            let (main_route_table_proxy, _server_end) =
                fidl::endpoints::create_proxy::<<Ipv6 as FidlRouteAdminIpExt>::RouteTableMarker>();
            let (unmanaged_route_set_proxy, _unmanaged_route_set_server_end) =
                fidl::endpoints::create_proxy::<<Ipv6 as FidlRouteAdminIpExt>::RouteSetMarker>();
            let (route_table_provider, _server_end) = fidl::endpoints::create_proxy::<
                <Ipv6 as FidlRouteAdminIpExt>::RouteTableProviderMarker,
            >();

            let mut route_table_map = RouteTableMap::<Ipv6>::new(
                main_route_table_proxy,
                fnet_routes_ext::TableId::new(
                    // arbitrary
                    6,
                ),
                unmanaged_route_set_proxy,
                route_table_provider,
            );

            let choose_unused_fidl_id = move |map: &RouteTableMap<Ipv6>| {
                // Assert that the existing set of FIDL ids tracked in the table is consistent.
                let used_fidl_ids_according_to_fidl_table_ids = collect_and_assert_all_unique(
                    map.fidl_table_ids
                        .keys()
                        .copied()
                        .chain(std::iter::once(route_table_map.main_route_table_id)),
                );

                let used_fidl_ids_according_to_route_tables = collect_and_assert_all_unique(
                    map.route_tables
                        .values()
                        .map(|table| table.fidl_table_id)
                        .chain(std::iter::once(route_table_map.main_route_table_id)),
                );

                assert_eq!(
                    used_fidl_ids_according_to_fidl_table_ids,
                    used_fidl_ids_according_to_route_tables
                );

                // Then just use the lowest hitherto unused one.
                (0u32..)
                    .map(fnet_routes_ext::TableId::new)
                    .find(|id| !used_fidl_ids_according_to_route_tables.contains(id))
                    .expect("should not run out of IDs")
            };

            let test_remove =
                |map: &mut RouteTableMap<Ipv6>, netlink_id: ManagedNetlinkRouteTableIndex| {
                    // Before removing, check for consistency between presence according to
                    // `get_fidl_table_id` and according to `remove`.
                    let prev_fidl_table_id = map.get_fidl_table_id(&netlink_id.get());

                    if let Some(table) = map.remove(netlink_id) {
                        assert_eq!(Some(table.fidl_table_id), prev_fidl_table_id);
                    } else {
                        assert_eq!(prev_fidl_table_id, None);
                    }

                    // Now everything should indicate absence.
                    assert_eq!(map.get_fidl_table_id(&netlink_id.get()), None);
                    assert_matches!(map.remove(netlink_id), None);
                    if let Some(prev_fidl_table_id) = prev_fidl_table_id {
                        assert_eq!(
                            map.lookup_route_table_key_from_fidl_table_id(prev_fidl_table_id),
                            None
                        );
                    }
                };

            let test_insert =
                |map: &mut RouteTableMap<Ipv6>, netlink_id: ManagedNetlinkRouteTableIndex| {
                    // Callers are expected to avoid inserting duplicate netlink IDs.
                    // To ensure this, test removal as well first.
                    test_remove(map, netlink_id);

                    // Create placeholder proxies.
                    let (route_table_proxy, _server_end) = fidl::endpoints::create_proxy::<
                        <Ipv6 as FidlRouteAdminIpExt>::RouteTableMarker,
                    >();
                    let (route_set_proxy, _server_end) = fidl::endpoints::create_proxy::<
                        <Ipv6 as FidlRouteAdminIpExt>::RouteSetMarker,
                    >();
                    let (route_set_from_main_table_proxy, _server_end) =
                        fidl::endpoints::create_proxy::<
                            <Ipv6 as FidlRouteAdminIpExt>::RouteSetMarker,
                        >();

                    // It's expected that the netstack will ensure no-FIDL-ID clashes
                    // for us as long as we keep route table proxies alive.
                    let fidl_table_id = choose_unused_fidl_id(&*map);

                    map.insert(
                        netlink_id,
                        RouteTable::<Ipv6> {
                            _route_table_proxy: route_table_proxy,
                            route_set_proxy,
                            route_set_from_main_table_proxy,
                            fidl_table_id,
                        },
                    );

                    // Now the "reverse" lookup should yield the expected item.
                    assert_eq!(
                        map.lookup_route_table_key_from_fidl_table_id(fidl_table_id),
                        Some(RouteTableKey::NetlinkManaged { table_id: netlink_id })
                    );
                    // As should the "forward" lookup.
                    assert_eq!(map.get_fidl_table_id(&netlink_id.get()), Some(fidl_table_id));
                };

            for op in ops_to_try {
                match op {
                    RouteTableMapOp::Insert(netlink_id) => {
                        test_insert(&mut route_table_map, netlink_id);
                    }
                    RouteTableMapOp::Remove(netlink_id) => {
                        test_remove(&mut route_table_map, netlink_id);
                    }
                }

                // After each op, check the invariant that the mapping between netlink IDs
                // and FIDL IDs is bidirectional.
                let by_route_table: Vec<(ManagedNetlinkRouteTableIndex, fnet_routes_ext::TableId)> =
                    route_table_map
                        .route_tables
                        .iter()
                        .map(|(netlink_id, table)| (*netlink_id, table.fidl_table_id))
                        .sorted()
                        .collect::<Vec<_>>();

                let by_fidl_id: Vec<(ManagedNetlinkRouteTableIndex, fnet_routes_ext::TableId)> =
                    route_table_map
                        .fidl_table_ids
                        .iter()
                        .map(|(fidl_table_id, netlink_id)| (*netlink_id, *fidl_table_id))
                        .sorted()
                        .collect::<Vec<_>>();

                assert_eq!(by_route_table, by_fidl_id);
            }
        }

        #[test]
        fn fidl_route_map_maintains_invariants(ops: Vec<FidlRouteMapOp>) {
            let mut fidl_route_map = FidlRouteMap::default();

            for op in ops {
                match op {
                    FidlRouteMapOp::Add(TestRoute(route), table, props) => {
                        let table = fnet_routes_ext::TableId::from(table);
                        let props = fnet_routes_ext::EffectiveRouteProperties::from(props);

                        // Save copies before and after the addition.
                        let before_add = fidl_route_map.clone();
                        let prev_props = fidl_route_map.add(route, table, props);
                        let after_add = fidl_route_map.clone();

                        // Check idempotence.
                        let repeated_props = fidl_route_map.add(route, table, props);
                        prop_assert_eq!(&fidl_route_map, &after_add);
                        prop_assert_eq!(repeated_props, Some(props));

                        // Check that removal reverses addition if the table wasn't previously
                        // associated with the route.
                        if prev_props.is_none() {
                            let removed_props = fidl_route_map.remove(route, table);
                            prop_assert_eq!(&fidl_route_map, &before_add);
                            let p = assert_matches!(removed_props,
                                RouteRemoveResult::RemovedButTableNotEmpty(p)
                                | RouteRemoveResult::RemovedAndTableNewlyEmpty(p)
                                => p
                            );
                            prop_assert_eq!(p, props);

                            // Check that addition again has the same outcome.
                            prop_assert_eq!(fidl_route_map.add(route, table, props), None);
                            prop_assert_eq!(&fidl_route_map, &after_add);
                        }

                        // The route should register as installed.
                        prop_assert!(fidl_route_map
                            .route_is_installed_in_tables(&route, std::iter::once(&table)));
                        prop_assert!(!fidl_route_map
                            .route_is_uninstalled_in_tables(&route, std::iter::once(&table)));

                        // But still not register as installed in a table that doesn't exist.
                        prop_assert!(!fidl_route_map.route_is_installed_in_tables(
                            &route,
                            [&table, &NEVER_GENERATED_TABLE_ID]
                        ));

                        // But we still fail the "uninstalled" check even if there's another
                        // table it's not installed in.
                        prop_assert!(!fidl_route_map.route_is_uninstalled_in_tables(
                            &route,
                            [&table, &NEVER_GENERATED_TABLE_ID],
                        ));
                    }
                    FidlRouteMapOp::Remove(TestRoute(route), table) => {
                        let table = fnet_routes_ext::TableId::from(table);

                        // Save copies before and after the removal.
                        let before_remove = fidl_route_map.clone();
                        let prev_props = fidl_route_map.remove(route, table);
                        let after_remove = fidl_route_map.clone();

                        // Check idempotence.
                        prop_assert_eq!(
                            fidl_route_map.remove(route, table),
                            RouteRemoveResult::DidNotExist
                        );
                        assert_eq!(fidl_route_map, after_remove);

                        match prev_props {
                            RouteRemoveResult::DidNotExist => {
                                // If no properties were yielded on removal, the map should have
                                // remained the same.
                                prop_assert_eq!(before_remove, after_remove);
                            }
                            RouteRemoveResult::RemovedButTableNotEmpty(inner_prev_props)
                            | RouteRemoveResult::RemovedAndTableNewlyEmpty(inner_prev_props) => {
                                // If properties were yielded, then re-adding them should
                                // reverse the removal.
                                prop_assert_eq!(
                                    fidl_route_map.add(route, table, inner_prev_props),
                                    None
                                );
                                prop_assert_eq!(&fidl_route_map, &before_remove);

                                // Then removing the (route, table) association again should
                                // yield the same outcome.
                                let props = fidl_route_map.remove(route, table);
                                prop_assert_eq!(props, prev_props);
                                prop_assert_eq!(&fidl_route_map, &after_remove);
                            }
                        }

                        // The route should register as uninstalled.
                        prop_assert!(!fidl_route_map
                            .route_is_installed_in_tables(&route, std::iter::once(&table)));
                        prop_assert!(fidl_route_map
                            .route_is_uninstalled_in_tables(&route, std::iter::once(&table)));
                        prop_assert!(!fidl_route_map.route_is_installed_in_tables(
                            &route,
                            [&table, &NEVER_GENERATED_TABLE_ID]
                        ));
                        prop_assert!(fidl_route_map.route_is_uninstalled_in_tables(
                            &route,
                            [&table, &NEVER_GENERATED_TABLE_ID]
                        ));
                    }
                }

                // After each op, check that the map is consistent.
                let by_routes = fidl_route_map
                    .routes
                    .iter()
                    .flat_map(|(route, tables)| {
                        tables.iter().map(|(table, props)| (*route, *table, *props))
                    })
                    .sorted()
                    .collect::<Vec<_>>();
                let by_tables = fidl_route_map
                    .by_tables
                    .iter()
                    .flat_map(|(table, routes)| {
                        routes.iter().map(|(route, props)| (*route, *table, *props))
                    })
                    .sorted()
                    .collect::<Vec<_>>();
                prop_assert_eq!(by_routes, by_tables);
            }
        }
    }
}
