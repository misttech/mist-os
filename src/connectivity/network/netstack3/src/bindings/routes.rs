// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the types of changes that can be made to the routing table, and the
//! worker responsible for executing those changes.
//!
//! Routing table changes are requested via an mpsc Sender held in BindingsCtx
//! ([`Changes`]), while the [`ChangeRunner`] is run in a separate task and is
//! responsible for ingesting those changes, updating the routing table, and
//! syncing the table to core.
//!
//! This is the source of truth for the netstack routing table, and the routing
//! table in core should be viewed as downstream of this one. This allows
//! bindings to implement routing table features without needing core to know
//! about them, such as the reference-counted RouteSets specified in
//! fuchsia.net.routes.admin.

use std::{
    collections::{HashMap, HashSet},
    pin::pin,
};

use futures::{
    channel::{mpsc, oneshot},
    Future, FutureExt as _, StreamExt as _,
};
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet},
    SpecifiedAddr,
};
use netstack3_core::routes::AddableMetric;

use crate::bindings::{util::TryIntoFidlWithContext, BindingsCtx, Ctx, IpExt};

pub(crate) mod admin;
use admin::{StrongUserRouteSet, WeakUserRouteSet};

pub(crate) mod state;
mod witness;
use witness::TableId;

type WeakDeviceId = netstack3_core::device::WeakDeviceId<crate::bindings::BindingsCtx>;
type DeviceId = netstack3_core::device::DeviceId<crate::bindings::BindingsCtx>;

#[derive(GenericOverIp, Debug)]
#[generic_over_ip(A, IpAddress)]
pub(crate) enum RouteOp<A: IpAddress> {
    Add(netstack3_core::routes::AddableEntry<A, WeakDeviceId>),
    RemoveToSubnet(Subnet<A>),
    RemoveMatching {
        subnet: Subnet<A>,
        device: WeakDeviceId,
        gateway: Option<SpecifiedAddr<A>>,
        metric: Option<AddableMetric>,
    },
}

#[derive(GenericOverIp, Debug)]
#[generic_over_ip(A, IpAddress)]
pub(crate) enum Change<A: IpAddress> {
    RouteOp(RouteOp<A>, WeakSetMembership),
    RemoveSet(WeakUserRouteSet),
    RemoveMatchingDevice(WeakDeviceId),
}

pub(crate) enum ChangeEither {
    V4(Change<Ipv4Addr>),
    V6(Change<Ipv6Addr>),
}

impl ChangeEither {
    pub(crate) fn add(
        entry: netstack3_core::routes::AddableEntryEither<WeakDeviceId>,
        set: WeakSetMembership,
    ) -> Self {
        match entry {
            netstack3_core::routes::AddableEntryEither::V4(entry) => {
                Self::V4(Change::RouteOp(RouteOp::Add(entry), set))
            }
            netstack3_core::routes::AddableEntryEither::V6(entry) => {
                Self::V6(Change::RouteOp(RouteOp::Add(entry), set))
            }
        }
    }
}

impl<A: IpAddress> From<Change<A>> for ChangeEither {
    fn from(change: Change<A>) -> Self {
        let IpInvariant(change) = A::Version::map_ip(
            change,
            |change| IpInvariant(ChangeEither::V4(change)),
            |change| IpInvariant(ChangeEither::V6(change)),
        );
        change
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("route's device no longer exists")]
    DeviceRemoved,
    #[error("routes change runner is shutting down")]
    ShuttingDown,
    #[error("route set no longer exists")]
    SetRemoved,
}

pub(crate) struct WorkItem<A: IpAddress> {
    pub(crate) change: Change<A>,
    pub(crate) responder: Option<oneshot::Sender<Result<ChangeOutcome, Error>>>,
}

/// The routing table from the perspective of bindings.
///
/// This is the source of truth for the netstack's routing table; the core
/// routing table should be viewed as downstream of this one. This allows
/// bindings to implement route-set-membership semantics without requiring
/// the concept of a route set to be implemented in core.
#[derive(Clone, Debug)]
struct Table<A: IpAddress> {
    inner: HashMap<netstack3_core::routes::AddableEntry<A, DeviceId>, EntryData>,
    /// The next [`netstack3_core::routes::Generation`] to be applied to new
    /// entries. This allows the routing table ordering to explicitly take into
    /// account the order in which routes are added to the table.
    next_generation: netstack3_core::routes::Generation,
}

#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
enum TableModifyResult<T> {
    NoChange,
    SetChanged,
    TableChanged(T),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ChangeOutcome {
    NoChange,
    Changed,
}

impl<A: IpAddress> Table<A> {
    fn new(initial_generation: netstack3_core::routes::Generation) -> Self {
        Self { inner: HashMap::new(), next_generation: initial_generation }
    }

    fn insert(
        &mut self,
        route: netstack3_core::routes::AddableEntry<A, DeviceId>,
        set: StrongSetMembership,
    ) -> TableModifyResult<(
        netstack3_core::routes::AddableEntry<A, DeviceId>,
        netstack3_core::routes::Generation,
    )> {
        let Self { inner, next_generation } = self;
        let (entry, new_to_table) = match inner.entry(route.clone()) {
            std::collections::hash_map::Entry::Occupied(occupied_entry) => {
                (occupied_entry.into_mut(), false)
            }
            std::collections::hash_map::Entry::Vacant(vacant_entry) => (
                vacant_entry.insert({
                    let gen = *next_generation;
                    *next_generation = next_generation.next();
                    EntryData::new(gen)
                }),
                true,
            ),
        };
        let new_to_set = entry.set_membership.insert(set.downgrade(), set.clone()).is_none();
        let result = if new_to_table {
            TableModifyResult::TableChanged((route.clone(), entry.generation))
        } else if new_to_set {
            TableModifyResult::SetChanged
        } else {
            TableModifyResult::NoChange
        };
        tracing::info!(
            "insert operation of route {route:?} into table with set {set:?} had result {result:?}",
        );
        result
    }

    /// Given a predicate and an indication of the route set to operate on,
    /// removes routes that match the predicate.
    ///
    /// If `set` is `SetMembership::Global`, then routes matching the predicate
    /// are removed from the table regardless of set membership. Otherwise,
    /// routes matching the predicate are removed from the indicated set, and
    /// then only removed from the overall table if that was the last reference
    /// to the route.
    fn remove(
        &mut self,
        mut should_remove: impl FnMut(&netstack3_core::routes::AddableEntry<A, DeviceId>) -> bool,
        set: WeakSetMembership,
    ) -> TableModifyResult<
        Vec<(
            netstack3_core::routes::AddableEntry<A, DeviceId>,
            netstack3_core::routes::Generation,
        )>,
    > {
        let Self { inner, next_generation: _ } = self;

        let mut removed_any_from_set = false;
        let mut removed_from_table = Vec::new();

        inner.retain(|route, data| {
            if !should_remove(route) {
                return true;
            }

            let should_remove_from_table = match &set {
                // "Global" removes mean we remove the route from the table
                // regardless of set membership.
                SetMembership::Global => true,
                SetMembership::CoreNdp
                | SetMembership::InitialDeviceRoutes
                | SetMembership::Loopback
                | SetMembership::User(_) => {
                    // Non-global named sets and user sets behave alike.
                    match data.set_membership.remove(&set) {
                        None => {
                            // Was not in the set.
                        }
                        Some(membership) => {
                            // Was in the set, this is the corresponding strong ID.
                            let _: StrongSetMembership = membership;
                            removed_any_from_set = true;
                        }
                    };
                    data.set_membership.is_empty()
                }
            };

            if should_remove_from_table {
                removed_from_table.push((route.clone(), data.generation));
                false
            } else {
                true
            }
        });

        let result = {
            if !removed_from_table.is_empty() {
                tracing::info!(
                    "remove operation on routing table resulted in removal of \
                     {} routes from the table:",
                    removed_from_table.len()
                );
                for (route, generation) in &removed_from_table {
                    tracing::info!("  removed route {route:?} (generation {generation:?})");
                }
                TableModifyResult::TableChanged(removed_from_table)
            } else if removed_any_from_set {
                tracing::info!(
                    "remove operation on routing table removed routes from set \
                    {set:?}, but not the overall table"
                );
                TableModifyResult::SetChanged
            } else {
                tracing::info!(
                    "remove operation on routing table from set {set:?} \
                     resulted in no change"
                );
                TableModifyResult::NoChange
            }
        };
        result
    }

    fn remove_user_set(
        &mut self,
        set: WeakUserRouteSet,
    ) -> Vec<(netstack3_core::routes::AddableEntry<A, DeviceId>, netstack3_core::routes::Generation)>
    {
        let Self { inner, next_generation: _ } = self;
        let set = SetMembership::User(set);
        let mut removed_from_table = Vec::new();
        inner.retain(|route, data| {
            if data.set_membership.remove(&set).is_some() && data.set_membership.is_empty() {
                removed_from_table.push((route.clone(), data.generation));
                false
            } else {
                true
            }
        });

        tracing::info!("route set removal ({set:?}) removed {} routes:", removed_from_table.len());

        for (route, generation) in &removed_from_table {
            tracing::info!("  removed route {route:?} (generation {generation:?})");
        }

        removed_from_table
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SetMembership<T> {
    /// Indicates route changes that are applied globally -- routes added
    /// globally cannot be removed by other route sets, but removing a route
    /// globally will also remove that route from other route sets.
    Global,
    /// Routes added or removed by core due to NDP belong to this route set.
    CoreNdp,
    /// Routes added as part of initial device bringup belong to this route set.
    InitialDeviceRoutes,
    /// Routes added as part of loopback device bringup belong to this route
    /// set.
    Loopback,
    /// Route sets created ephemerally (usually as part of serving FIDL
    /// protocols that involve managing route lifetimes) belong to this class
    /// of route sets.
    User(T),
}

type StrongSetMembership = SetMembership<StrongUserRouteSet>;
type WeakSetMembership = SetMembership<WeakUserRouteSet>;

impl StrongSetMembership {
    fn downgrade(&self) -> WeakSetMembership {
        match self {
            SetMembership::Global => SetMembership::Global,
            SetMembership::CoreNdp => SetMembership::CoreNdp,
            SetMembership::InitialDeviceRoutes => SetMembership::InitialDeviceRoutes,
            SetMembership::Loopback => SetMembership::Loopback,
            SetMembership::User(set) => {
                SetMembership::User(netstack3_core::sync::StrongRc::downgrade(&set))
            }
        }
    }
}

impl WeakSetMembership {
    #[cfg_attr(feature = "instrumented", track_caller)]
    fn upgrade(self) -> Option<StrongSetMembership> {
        match self {
            SetMembership::Global => Some(SetMembership::Global),
            SetMembership::CoreNdp => Some(SetMembership::CoreNdp),
            SetMembership::InitialDeviceRoutes => Some(SetMembership::InitialDeviceRoutes),
            SetMembership::Loopback => Some(SetMembership::Loopback),
            SetMembership::User(set) => set.upgrade().map(SetMembership::User),
        }
    }
}

#[derive(Clone, Debug)]
struct EntryData {
    generation: netstack3_core::routes::Generation,
    // Logically, this should be viewed as a `HashSet<StrongSetMembership>`, but
    // we use a `HashMap<WeakSetMembership, StrongSetMembership>` (where the
    // key and value set-IDs always match) in order to be able to look up using
    // only a weak set ID. We want to keep strong set memberships in the map
    // so that we can assert that we have cleaned up all references to a user
    // route set by unwrapping the primary route set ID.
    set_membership: HashMap<WeakSetMembership, StrongSetMembership>,
}

impl EntryData {
    fn new(generation: netstack3_core::routes::Generation) -> Self {
        Self { generation, set_membership: HashMap::new() }
    }
}

pub(crate) struct State<I: Ip> {
    receiver: mpsc::UnboundedReceiver<WorkItem<I::Addr>>,
    table: Table<I::Addr>,
    update_dispatcher: crate::bindings::routes::state::RouteUpdateDispatcher<I>,
}

#[derive(derivative::Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct Changes<A: IpAddress> {
    sender: mpsc::UnboundedSender<WorkItem<A>>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I> State<I>
where
    I: IpExt,
{
    pub(crate) async fn run_changes(&mut self, mut ctx: Ctx) {
        let State { receiver, table, update_dispatcher } = self;
        let mut receiver = pin!(receiver);

        while let Some(WorkItem { change, responder }) = receiver.next().await {
            let result = handle_change::<I>(table, &mut ctx, change, update_dispatcher).await;
            if let Some(responder) = responder {
                match responder.send(result) {
                    Ok(()) => (),
                    Err(result) => match result {
                        Ok(outcome) => {
                            match outcome {
                                ChangeOutcome::NoChange | ChangeOutcome::Changed => {
                                    // We don't need to log anything here;
                                    // the change succeeded.
                                }
                            }
                        }
                        Err(e) => {
                            // Since the other end dropped the receiver, no one will
                            // observe the result of this route change, so we have to
                            // log any errors ourselves.
                            tracing::error!("error while handling route change: {:?}", e);
                        }
                    },
                };
            }
        }
    }
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn to_entry<I: netstack3_core::IpExt>(
    ctx: &mut Ctx,
    addable_entry: netstack3_core::routes::AddableEntry<I::Addr, DeviceId>,
) -> netstack3_core::routes::Entry<I::Addr, DeviceId> {
    let device_metric = ctx.api().device_ip::<I>().get_routing_metric(&addable_entry.device);
    addable_entry.resolve_metric(device_metric)
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
async fn handle_change<I>(
    table: &mut Table<I::Addr>,
    ctx: &mut Ctx,
    change: Change<I::Addr>,
    route_update_dispatcher: &crate::bindings::routes::state::RouteUpdateDispatcher<I>,
) -> Result<ChangeOutcome, Error>
where
    I: IpExt,
{
    tracing::debug!("routes::handle_change {change:?}");

    enum TableChange<I: Ip, Iter> {
        Add(netstack3_core::routes::Entry<I::Addr, DeviceId>),
        Remove(Iter),
    }

    let table_change: TableChange<I, _> = match change {
        Change::RouteOp(RouteOp::Add(addable_entry), set) => {
            let set = set.upgrade().ok_or(Error::SetRemoved)?;
            let addable_entry =
                addable_entry.try_map_device_id(|d| d.upgrade().ok_or(Error::DeviceRemoved))?;
            match table.insert(addable_entry, set) {
                TableModifyResult::NoChange => return Ok(ChangeOutcome::NoChange),
                TableModifyResult::SetChanged => return Ok(ChangeOutcome::Changed),
                TableModifyResult::TableChanged((addable_entry, _generation)) => {
                    TableChange::Add(to_entry::<I>(ctx, addable_entry))
                }
            }
        }
        Change::RouteOp(RouteOp::RemoveToSubnet(subnet), set) => {
            match table.remove(|entry| &entry.subnet == &subnet, set) {
                TableModifyResult::NoChange => return Ok(ChangeOutcome::NoChange),
                TableModifyResult::SetChanged => return Ok(ChangeOutcome::Changed),
                TableModifyResult::TableChanged(entries) => {
                    TableChange::Remove(itertools::Either::Left(entries.into_iter()))
                }
            }
        }
        Change::RouteOp(RouteOp::RemoveMatching { subnet, device, gateway, metric }, set) => {
            match table.remove(
                |entry| {
                    entry.subnet == subnet
                        && entry.device == device
                        && entry.gateway == gateway
                        && metric.map(|metric| metric == entry.metric).unwrap_or(true)
                },
                set,
            ) {
                TableModifyResult::NoChange => return Ok(ChangeOutcome::NoChange),
                TableModifyResult::SetChanged => return Ok(ChangeOutcome::Changed),
                TableModifyResult::TableChanged(entries) => TableChange::Remove(
                    itertools::Either::Right(itertools::Either::Left(entries.into_iter())),
                ),
            }
        }
        Change::RemoveMatchingDevice(device) => {
            let result = table.remove(
                |entry| entry.device == device,
                // NB: we use `SetMembership::Global` here to remove routes on
                // this device from the table regardless of the sets they belong
                // to.
                SetMembership::Global,
            );
            match result {
                TableModifyResult::NoChange => return Ok(ChangeOutcome::NoChange),
                TableModifyResult::SetChanged => {
                    unreachable!(
                        "TableModifyResult::SetChanged cannot be returned \
                         when globally removing a route"
                    )
                }
                TableModifyResult::TableChanged(routes_from_table) => {
                    TableChange::Remove(itertools::Either::Right(itertools::Either::Right(
                        itertools::Either::Left(routes_from_table.into_iter()),
                    )))
                }
            }
        }
        Change::RemoveSet(set) => {
            let entries = table.remove_user_set(set);
            if entries.is_empty() {
                return Ok(ChangeOutcome::NoChange);
            }
            TableChange::Remove(itertools::Either::Right(itertools::Either::Right(
                itertools::Either::Right(entries.into_iter()),
            )))
        }
    };

    let new_routes = table
        .inner
        .iter()
        .map(|(entry, data)| {
            let device_metric = ctx.api().device_ip::<I>().get_routing_metric(&entry.device);
            entry.clone().resolve_metric(device_metric).with_generation(data.generation)
        })
        .collect::<Vec<_>>();
    ctx.api().routes::<I>().set_routes(new_routes);

    match table_change {
        TableChange::Add(entry) => {
            if entry.subnet.prefix() == 0 {
                // Only notify that we newly have a default route if this is the
                // only default route on this device.
                if table
                    .inner
                    .iter()
                    .filter(|(table_entry, _)| {
                        table_entry.subnet.prefix() == 0 && &table_entry.device == &entry.device
                    })
                    .count()
                    == 1
                {
                    ctx.bindings_ctx().notify_interface_update(
                        &entry.device,
                        crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                            version: I::VERSION,
                            has_default_route: true,
                        },
                    )
                }
            }
            let installed_route = entry
                .try_into_fidl_with_ctx(ctx.bindings_ctx())
                .expect("failed to convert route to FIDL");
            route_update_dispatcher
                .notify(crate::bindings::routes::state::RoutingTableUpdate::<I>::RouteAdded(
                    installed_route,
                ))
                .await
                .expect("failed to notify route update dispatcher");
        }
        TableChange::Remove(removed) => {
            // Clone the Ctx so we can capture it in the mapping iterator. This
            // is cheaper than collecting into a Vec to eliminate the borrow.
            let mut ctx_clone = ctx.clone();
            let removed = removed.map(|(entry, _generation)| to_entry::<I>(&mut ctx_clone, entry));
            notify_removed_routes::<I>(ctx.bindings_ctx(), route_update_dispatcher, removed, table)
                .await;
        }
    };

    Ok(ChangeOutcome::Changed)
}

async fn notify_removed_routes<I: Ip>(
    bindings_ctx: &crate::bindings::BindingsCtx,
    dispatcher: &crate::bindings::routes::state::RouteUpdateDispatcher<I>,
    removed_routes: impl IntoIterator<Item = netstack3_core::routes::Entry<I::Addr, DeviceId>>,
    table: &Table<I::Addr>,
) {
    let mut devices_with_default_routes: Option<HashSet<_>> = None;
    let mut already_notified_devices = HashSet::new();

    for entry in removed_routes {
        if entry.subnet.prefix() == 0 {
            // Check if there are now no default routes on this device.
            let devices_with_default_routes = (&mut devices_with_default_routes)
                .get_or_insert_with(|| {
                    table
                        .inner
                        .iter()
                        .filter_map(|(table_entry, _)| {
                            (table_entry.subnet.prefix() == 0).then(|| table_entry.device.clone())
                        })
                        .collect()
                });

            if !devices_with_default_routes.contains(&entry.device)
                && already_notified_devices.insert(entry.device.clone())
            {
                bindings_ctx.notify_interface_update(
                    &entry.device,
                    crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                        version: I::VERSION,
                        has_default_route: false,
                    },
                )
            }
        }
        let installed_route =
            entry.try_into_fidl_with_ctx(bindings_ctx).expect("failed to convert route to FIDL");
        dispatcher
            .notify(crate::bindings::routes::state::RoutingTableUpdate::<I>::RouteRemoved(
                installed_route,
            ))
            .await
            .expect("failed to notify route update dispatcher");
    }
}

#[derive(Clone)]
pub(crate) struct ChangeSink {
    v4: Changes<Ipv4Addr>,
    v6: Changes<Ipv6Addr>,
}

#[must_use = "route changes won't be applied without running the ChangeRunner"]
pub(crate) struct ChangeRunner {
    v4: State<Ipv4>,
    v6: State<Ipv6>,
}

impl ChangeRunner {
    pub(crate) fn route_update_dispatchers(
        &self,
    ) -> (
        crate::bindings::routes::state::RouteUpdateDispatcher<Ipv4>,
        crate::bindings::routes::state::RouteUpdateDispatcher<Ipv6>,
    ) {
        let Self { v4, v6 } = self;
        (v4.update_dispatcher.clone(), v6.update_dispatcher.clone())
    }

    pub(crate) async fn run(&mut self, ctx: Ctx) {
        let Self { v4, v6 } = self;
        let v4_fut = v4.run_changes(ctx.clone());
        let v6_fut = v6.run_changes(ctx);
        let ((), ()) = futures::future::join(v4_fut, v6_fut).await;
    }
}

pub(crate) fn create_sink_and_runner() -> (ChangeSink, ChangeRunner) {
    fn create<I: Ip>() -> (Changes<I::Addr>, State<I>) {
        let (sender, receiver) = mpsc::unbounded();
        let state = State {
            receiver,
            table: Table::new(netstack3_core::routes::Generation::initial()),
            update_dispatcher: Default::default(),
        };
        (Changes { sender }, state)
    }
    let (v4, v4_state) = create::<Ipv4>();
    let (v6, v6_state) = create::<Ipv6>();
    (ChangeSink { v4, v6 }, ChangeRunner { v4: v4_state, v6: v6_state })
}

impl ChangeSink {
    /// Closes the channels over which routes change requests are sent, causing
    /// [`ChangeRunner::run`] to exit.
    pub(crate) fn close_senders(&self) {
        let Self { v4, v6 } = self;
        v4.sender.close_channel();
        v6.sender.close_channel();
    }

    pub(crate) fn fire_change_and_forget<A: IpAddress>(&self, change: Change<A>) {
        let sender = self.change_sender::<A>();
        let item = WorkItem { change, responder: None };
        match sender.unbounded_send(item) {
            Ok(()) => (),
            Err(e) => {
                let WorkItem { change, responder: _ } = e.into_inner();
                tracing::warn!(
                    "failed to send route change {:?} because route change sink is closed",
                    change
                );
            }
        };
    }

    pub(crate) fn send_change<A: IpAddress>(
        &self,
        change: Change<A>,
    ) -> impl Future<Output = Result<ChangeOutcome, Error>> {
        let sender = self.change_sender::<A>();
        let (responder, receiver) = oneshot::channel();
        let item = WorkItem { change, responder: Some(responder) };
        match sender.unbounded_send(item) {
            Ok(()) => receiver.map(|r| r.expect("responder should not be dropped")).left_future(),
            Err(e) => {
                let _: mpsc::TrySendError<WorkItem<A>> = e;
                futures::future::ready(Err(Error::ShuttingDown)).right_future()
            }
        }
    }

    fn change_sender<A: IpAddress>(&self) -> &mpsc::UnboundedSender<WorkItem<A>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(A, IpAddress)]
        struct ChangeSender<'a, A: IpAddress> {
            sender: &'a mpsc::UnboundedSender<WorkItem<A>>,
        }

        let ChangeSender { sender } = <A::Version as Ip>::map_ip(
            IpInvariant(self),
            |IpInvariant(ChangeSink { v4, v6: _ })| ChangeSender { sender: &v4.sender },
            |IpInvariant(ChangeSink { v4: _, v6 })| ChangeSender { sender: &v6.sender },
        );
        sender
    }
}
