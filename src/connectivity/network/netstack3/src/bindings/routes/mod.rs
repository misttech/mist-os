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

use std::collections::HashMap;

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

use crate::bindings::{util::TryIntoFidlWithContext, Ctx};

type WeakDeviceId = netstack3_core::device::WeakDeviceId<crate::bindings::BindingsNonSyncCtxImpl>;
type DeviceId = netstack3_core::device::DeviceId<crate::bindings::BindingsNonSyncCtxImpl>;

#[derive(GenericOverIp, Debug)]
pub(crate) enum Change<A: IpAddress> {
    Add(netstack3_core::ip::types::AddableEntry<A, WeakDeviceId>),
    RemoveToSubnet(Subnet<A>),
    RemoveMatching { subnet: Subnet<A>, device: WeakDeviceId, gateway: Option<SpecifiedAddr<A>> },
    DeviceRemoved(WeakDeviceId),
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

pub(crate) struct State<A: IpAddress> {
    receiver: mpsc::UnboundedReceiver<WorkItem<A>>,
    generation: netstack3_core::ip::types::Generation,
    table: HashMap<
        netstack3_core::ip::types::AddableEntry<A, WeakDeviceId>,
        netstack3_core::ip::types::Generation,
    >,
}

#[derive(derivative::Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct Changes<A: IpAddress> {
    sender: mpsc::UnboundedSender<WorkItem<A>>,
}

impl<A: IpAddress> State<A> {
    pub(crate) async fn run_changes<I: Ip<Addr = A>>(&mut self, mut ctx: Ctx) {
        let State { receiver, table, generation } = self;
        pin_mut!(receiver);

        while let Some(WorkItem { change, responder }) = receiver.next().await {
            let result = handle_change::<I>(table, generation, &mut ctx, change);
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

fn handle_change<I: Ip>(
    table: &mut HashMap<
        netstack3_core::ip::types::AddableEntry<I::Addr, WeakDeviceId>,
        netstack3_core::ip::types::Generation,
    >,
    generation: &mut netstack3_core::ip::types::Generation,
    ctx: &mut Ctx,
    change: Change<I::Addr>,
) -> Result<(), Error> {
    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();

    enum TableChange<I: Ip> {
        Add(netstack3_core::ip::types::AddableEntryAndGeneration<I::Addr, WeakDeviceId>),
        Remove(
            HashMap<
                netstack3_core::ip::types::AddableEntry<I::Addr, WeakDeviceId>,
                netstack3_core::ip::types::Generation,
            >,
        ),
    }

    let table_change = match change {
        Change::Add(storable_entry) => {
            *generation = generation.next();
            let newly_inserted = table.insert(storable_entry.clone(), *generation).is_none();
            if !newly_inserted {
                return Err(Error::AlreadyExists);
            }
            TableChange::<I>::Add(storable_entry.with_generation(*generation))
        }
        Change::RemoveToSubnet(subnet) => {
            let (removed, kept) = std::mem::take(table).into_iter().partition::<HashMap<_, _>, _>(
                |(entry, _generation): &(
                    netstack3_core::ip::types::AddableEntry<
                        I::Addr,
                        netstack3_core::device::WeakDeviceId<
                            crate::bindings::BindingsNonSyncCtxImpl,
                        >,
                    >,
                    netstack3_core::ip::types::Generation,
                )| &entry.subnet == &subnet,
            );
            *table = kept;
            TableChange::<I>::Remove(removed)
        }
        Change::RemoveMatching { subnet, device, gateway } => {
            let (removed, kept) = std::mem::take(table).into_iter().partition::<HashMap<_, _>, _>(
                |(entry, _generation)| {
                    entry.subnet == subnet && &entry.device == &device && entry.gateway == gateway
                },
            );
            *table = kept;
            TableChange::<I>::Remove(removed)
        }
        Change::DeviceRemoved(device) => {
            let (_removed, kept) = std::mem::take(table)
                .into_iter()
                .partition::<HashMap<_, _>, _>(|(entry, _generation)| &entry.device == &device);
            *table = kept;

            // This is simply keeping the bindings route table up to date with
            // routes that were removed by core as a result of removing a
            // device, so we do no additional work to commit the changes to core
            // here or notify watchers -- that work has to be done synchronously
            // in the event context before the device is removed from core.
            // TODO(https://fxbug.dev/132990): Allowing core to unilaterally
            // remove routes on device removal (as opposed to going through
            // bindings) is confusing; this should be removed once we can await
            // strong device ID cleanup.
            return Ok(());
        }
    };

    // TODO(https://fxbug.dev/132990): The two `set_routes` commit paths here
    // can be greatly simplified / deduplicated once we can await strong device
    // ID cleanup and disallow core from removing routes.
    match table_change {
        TableChange::Add(storable_entry) => {
            let netstack3_core::ip::types::EntryAndGeneration { entry, generation: _ } =
                netstack3_core::set_routes::<I, _, _>(
                    sync_ctx,
                    non_sync_ctx,
                    &mut |netstack3_core::ip::types::EntryUpgrader(upgrade_entry)| {
                        (
                            table
                                .iter()
                                .flat_map(|(entry, generation)| {
                                    upgrade_entry(entry.clone().with_generation(*generation))
                                })
                                .collect(),
                            upgrade_entry(storable_entry.clone()),
                        )
                    },
                )
                .ok_or(Error::DeviceRemoved)?;
            if entry.subnet.prefix() == 0 {
                non_sync_ctx.notify_interface_update(
                    &entry.device,
                    crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                        version: I::VERSION,
                        has_default_route: true,
                    },
                )
            }
            let installed_route = entry
                .try_into_fidl_with_ctx(non_sync_ctx)
                .expect("failed to convert route to FIDL");
            non_sync_ctx
                .route_update_dispatcher
                .lock()
                .notify(crate::bindings::routes_fidl_worker::RoutingTableUpdate::<I>::RouteAdded(
                    installed_route,
                ))
                .expect("failed to notify route update dispatcher");
        }
        TableChange::Remove(removed) => {
            let removed = netstack3_core::set_routes::<I, _, _>(
                sync_ctx,
                non_sync_ctx,
                &mut |netstack3_core::ip::types::EntryUpgrader(upgrade_entry)| {
                    (
                        table
                            .iter()
                            .flat_map(|(entry, generation)| {
                                upgrade_entry(entry.clone().with_generation(*generation))
                            })
                            .collect(),
                        removed
                            .clone()
                            .into_iter()
                            .flat_map(|(entry, generation)| {
                                upgrade_entry(entry.with_generation(generation))
                            })
                            .collect::<Vec<_>>(),
                    )
                },
            );
            if removed.is_empty() {
                return Err(Error::NotFound);
            } else {
                notify_removed_routes::<I>(
                    non_sync_ctx,
                    removed.into_iter().map(
                        |netstack3_core::ip::types::EntryAndGeneration { entry, generation: _ }| {
                            entry
                        },
                    ),
                );
            }
        }
    };

    Ok(())
}

pub(crate) fn notify_removed_routes<I: Ip>(
    non_sync_ctx: &mut crate::bindings::BindingsNonSyncCtxImpl,
    removed_routes: impl IntoIterator<Item = netstack3_core::ip::types::Entry<I::Addr, DeviceId>>,
) {
    let mut dispatcher = non_sync_ctx.route_update_dispatcher.lock();
    for entry in removed_routes {
        if entry.subnet.prefix() == 0 {
            // TODO(https://fxbug.dev/132990): This is not strictly correct, as
            // you could have two default routes on the same device with
            // different metrics. Once we can hold strong IDs in bindings, it
            // should be easy to do a proper before/after check of whether the
            // routing table has a default route.
            non_sync_ctx.notify_interface_update(
                &entry.device,
                crate::bindings::InterfaceUpdate::DefaultRouteChanged {
                    version: I::VERSION,
                    has_default_route: false,
                },
            )
        }
        let installed_route =
            entry.try_into_fidl_with_ctx(non_sync_ctx).expect("failed to convert route to FIDL");
        dispatcher
            .notify(crate::bindings::routes_fidl_worker::RoutingTableUpdate::<I>::RouteRemoved(
                installed_route,
            ))
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
    v4: State<Ipv4Addr>,
    v6: State<Ipv6Addr>,
}

impl ChangeRunner {
    pub(crate) async fn run(&mut self, ctx: Ctx) {
        let Self { v4, v6 } = self;
        let v4_fut = v4.run_changes::<Ipv4>(ctx.clone());
        let v6_fut = v6.run_changes::<Ipv6>(ctx);
        let ((), ()) = futures::future::join(v4_fut, v6_fut).await;
    }
}

pub(crate) fn create_sink_and_runner() -> (ChangeSink, ChangeRunner) {
    fn create<A: IpAddress>() -> (Changes<A>, State<A>) {
        let (sender, receiver) = mpsc::unbounded();
        let state = State {
            receiver,
            table: HashMap::new(),
            generation: netstack3_core::ip::types::Generation::initial(),
        };
        (Changes { sender }, state)
    }
    let (v4, v4_state) = create::<Ipv4Addr>();
    let (v6, v6_state) = create::<Ipv6Addr>();
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
