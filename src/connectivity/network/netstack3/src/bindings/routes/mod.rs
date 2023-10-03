// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the types of changes that can be made to the routing table, and the
//! worker responsible for executing those changes.
//!
//! Routing table changes are requested via an mpsc Sender held in NonSyncCtx
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
    sync::Arc,
};

use futures::{
    channel::{mpsc, oneshot},
    pin_mut, Future, FutureExt as _, StreamExt as _,
};
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet,
        SubnetEither,
    },
    SpecifiedAddr,
};
use netstack3_core::SyncCtx;

use crate::bindings::{util::TryIntoFidlWithContext, BindingsNonSyncCtxImpl, Ctx};

type WeakDeviceId = netstack3_core::device::WeakDeviceId<crate::bindings::BindingsNonSyncCtxImpl>;
type DeviceId = netstack3_core::device::DeviceId<crate::bindings::BindingsNonSyncCtxImpl>;

#[derive(GenericOverIp, Debug)]
#[generic_over_ip(A, IpAddress)]
pub(crate) enum Change<A: IpAddress> {
    Add(netstack3_core::ip::types::AddableEntry<A, WeakDeviceId>),
    RemoveToSubnet(Subnet<A>),
    RemoveMatching { subnet: Subnet<A>, device: WeakDeviceId, gateway: Option<SpecifiedAddr<A>> },
    RemoveMatchingDevice(WeakDeviceId),
}

pub(crate) enum ChangeEither {
    V4(Change<Ipv4Addr>),
    V6(Change<Ipv6Addr>),
}

impl ChangeEither {
    pub(crate) fn remove_matching_without_gateway(
        subnet: SubnetEither,
        device: WeakDeviceId,
    ) -> Self {
        match subnet {
            SubnetEither::V4(subnet) => {
                Self::V4(Change::RemoveMatching { subnet, device, gateway: None })
            }
            SubnetEither::V6(subnet) => {
                Self::V6(Change::RemoveMatching { subnet, device, gateway: None })
            }
        }
    }

    pub(crate) fn add(entry: netstack3_core::ip::types::AddableEntryEither<WeakDeviceId>) -> Self {
        match entry {
            netstack3_core::ip::types::AddableEntryEither::V4(entry) => {
                Self::V4(Change::Add(entry))
            }
            netstack3_core::ip::types::AddableEntryEither::V6(entry) => {
                Self::V6(Change::Add(entry))
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
    #[error("route already exists")]
    AlreadyExists,
    #[error("matching route not found")]
    NotFound,
    #[error("route's device no longer exists")]
    DeviceRemoved,
    #[error("routes change runner is shutting down")]
    ShuttingDown,
}

pub(crate) struct WorkItem<A: IpAddress> {
    pub(crate) change: Change<A>,
    pub(crate) responder: Option<oneshot::Sender<Result<(), Error>>>,
}

pub(crate) struct State<I: Ip> {
    receiver: mpsc::UnboundedReceiver<WorkItem<I::Addr>>,
    generation: netstack3_core::ip::types::Generation,
    table: HashMap<
        netstack3_core::ip::types::AddableEntry<I::Addr, DeviceId>,
        netstack3_core::ip::types::Generation,
    >,
    update_dispatcher: crate::bindings::routes_fidl_worker::RouteUpdateDispatcher<I>,
}

#[derive(derivative::Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct Changes<A: IpAddress> {
    sender: mpsc::UnboundedSender<WorkItem<A>>,
}

impl<I: Ip> State<I> {
    pub(crate) async fn run_changes(&mut self, mut ctx: Ctx) {
        let State { receiver, table, generation, update_dispatcher } = self;
        pin_mut!(receiver);

        while let Some(WorkItem { change, responder }) = receiver.next().await {
            let result =
                handle_change::<I>(table, generation, &mut ctx, change, update_dispatcher).await;
            if let Some(responder) = responder {
                match responder.send(result) {
                    Ok(()) => (),
                    Err(result) => match result {
                        Ok(()) => {}
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

fn to_entry<I: Ip>(
    sync_ctx: &Arc<SyncCtx<BindingsNonSyncCtxImpl>>,
    addable_entry: netstack3_core::ip::types::AddableEntry<I::Addr, DeviceId>,
) -> netstack3_core::ip::types::Entry<I::Addr, DeviceId> {
    let device_metric = netstack3_core::get_routing_metric(sync_ctx, &addable_entry.device);
    addable_entry.resolve_metric(device_metric)
}

async fn handle_change<I: Ip>(
    table: &mut HashMap<
        netstack3_core::ip::types::AddableEntry<I::Addr, DeviceId>,
        netstack3_core::ip::types::Generation,
    >,
    generation: &mut netstack3_core::ip::types::Generation,
    ctx: &mut Ctx,
    change: Change<I::Addr>,
    route_update_dispatcher: &crate::bindings::routes_fidl_worker::RouteUpdateDispatcher<I>,
) -> Result<(), Error> {
    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();

    enum TableChange<I: Ip> {
        Add(netstack3_core::ip::types::Entry<I::Addr, DeviceId>),
        Remove(Vec<netstack3_core::ip::types::Entry<I::Addr, DeviceId>>),
    }

    let table_change: TableChange<I> = match change {
        Change::Add(addable_entry) => {
            *generation = generation.next();
            let addable_entry = addable_entry
                .try_map_device_id(|device_id| device_id.upgrade().ok_or(Error::DeviceRemoved))?;
            let newly_inserted = table.insert(addable_entry.clone(), *generation).is_none();
            if !newly_inserted {
                return Err(Error::AlreadyExists);
            }
            TableChange::Add(to_entry::<I>(sync_ctx, addable_entry))
        }
        Change::RemoveToSubnet(subnet) => {
            let (removed, kept) = std::mem::take(table).into_iter().partition::<HashMap<_, _>, _>(
                |(entry, _generation): &(
                    netstack3_core::ip::types::AddableEntry<
                        I::Addr,
                        netstack3_core::device::DeviceId<crate::bindings::BindingsNonSyncCtxImpl>,
                    >,
                    netstack3_core::ip::types::Generation,
                )| &entry.subnet == &subnet,
            );
            *table = kept;
            if removed.is_empty() {
                return Err(Error::NotFound);
            }
            TableChange::Remove(
                removed
                    .into_iter()
                    .map(|(entry, _generation)| to_entry::<I>(sync_ctx, entry))
                    .collect(),
            )
        }
        Change::RemoveMatching { subnet, device, gateway } => {
            let (removed, kept) = std::mem::take(table).into_iter().partition::<HashMap<_, _>, _>(
                |(entry, _generation)| {
                    entry.subnet == subnet && &entry.device == &device && entry.gateway == gateway
                },
            );
            *table = kept;
            if removed.is_empty() {
                return Err(Error::NotFound);
            }
            TableChange::Remove(
                removed
                    .into_iter()
                    .map(|(entry, _generation)| to_entry::<I>(sync_ctx, entry))
                    .collect(),
            )
        }
        Change::RemoveMatchingDevice(device) => {
            let (removed, kept) = std::mem::take(table)
                .into_iter()
                .partition::<HashMap<_, _>, _>(|(entry, _generation)| &entry.device == &device);
            *table = kept;
            if removed.is_empty() {
                return Err(Error::NotFound);
            }
            TableChange::Remove(
                removed
                    .into_iter()
                    .map(|(entry, _generation)| to_entry::<I>(sync_ctx, entry))
                    .collect(),
            )
        }
    };

    netstack3_core::set_routes::<I, _>(
        sync_ctx,
        non_sync_ctx,
        table
            .iter()
            .map(|(entry, generation)| {
                let device_metric = netstack3_core::get_routing_metric(sync_ctx, &entry.device);
                entry.clone().resolve_metric(device_metric).with_generation(*generation)
            })
            .collect::<Vec<_>>(),
    );

    match table_change {
        TableChange::Add(entry) => {
            if entry.subnet.prefix() == 0 {
                // Only notify that we newly have a default route if this is the
                // only default route on this device.
                if table
                    .iter()
                    .filter(|(table_entry, _)| {
                        table_entry.subnet.prefix() == 0 && &table_entry.device == &entry.device
                    })
                    .count()
                    == 1
                {
                    non_sync_ctx.notify_interface_update(
                        &entry.device,
                        crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                            version: I::VERSION,
                            has_default_route: true,
                        },
                    )
                }
            }
            let installed_route = entry
                .try_into_fidl_with_ctx(non_sync_ctx)
                .expect("failed to convert route to FIDL");
            route_update_dispatcher
                .notify(crate::bindings::routes_fidl_worker::RoutingTableUpdate::<I>::RouteAdded(
                    installed_route,
                ))
                .await
                .expect("failed to notify route update dispatcher");
        }
        TableChange::Remove(removed) => {
            notify_removed_routes::<I>(non_sync_ctx, route_update_dispatcher, removed, table).await;
        }
    };

    Ok(())
}

async fn notify_removed_routes<I: Ip>(
    non_sync_ctx: &mut crate::bindings::BindingsNonSyncCtxImpl,
    dispatcher: &crate::bindings::routes_fidl_worker::RouteUpdateDispatcher<I>,
    removed_routes: impl IntoIterator<Item = netstack3_core::ip::types::Entry<I::Addr, DeviceId>>,
    table: &HashMap<
        netstack3_core::ip::types::AddableEntry<I::Addr, DeviceId>,
        netstack3_core::ip::types::Generation,
    >,
) {
    let mut devices_with_default_routes: Option<HashSet<_>> = None;
    let mut already_notified_devices = HashSet::new();

    for entry in removed_routes {
        if entry.subnet.prefix() == 0 {
            // Check if there are now no default routes on this device.
            let devices_with_default_routes = (&mut devices_with_default_routes)
                .get_or_insert_with(|| {
                    table
                        .iter()
                        .filter_map(|(table_entry, _)| {
                            (table_entry.subnet.prefix() == 0).then(|| table_entry.device.clone())
                        })
                        .collect()
                });

            if !devices_with_default_routes.contains(&entry.device)
                && already_notified_devices.insert(entry.device.clone())
            {
                non_sync_ctx.notify_interface_update(
                    &entry.device,
                    crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                        version: I::VERSION,
                        has_default_route: false,
                    },
                )
            }
        }
        let installed_route =
            entry.try_into_fidl_with_ctx(non_sync_ctx).expect("failed to convert route to FIDL");
        dispatcher
            .notify(crate::bindings::routes_fidl_worker::RoutingTableUpdate::<I>::RouteRemoved(
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
        crate::bindings::routes_fidl_worker::RouteUpdateDispatcher<Ipv4>,
        crate::bindings::routes_fidl_worker::RouteUpdateDispatcher<Ipv6>,
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
            table: HashMap::new(),
            generation: netstack3_core::ip::types::Generation::initial(),
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
    ) -> impl Future<Output = Result<(), Error>> {
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
