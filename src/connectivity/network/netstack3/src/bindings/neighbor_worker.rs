// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{HashMap, VecDeque};

use fidl::endpoints::{ControlHandle as _, RequestStream as _, Responder as _};
use fidl_fuchsia_net_ext::IntoExt;
use fidl_fuchsia_net_multicast_ext::FidlMulticastAdminIpExt;
use fidl_fuchsia_net_neighbor::{
    self as fnet_neighbor, ControllerRequest, ControllerRequestStream, ViewRequest,
    ViewRequestStream,
};
use {fidl_fuchsia_net as fnet, fidl_fuchsia_net_neighbor_ext as fnet_neighbor_ext};

use assert_matches::assert_matches;
use futures::channel::mpsc;
use futures::task::Poll;
use futures::{Future, SinkExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use log::{error, info, warn};
use net_types::ethernet::Mac;
use net_types::ip::{Ip, IpAddr, IpAddress, Ipv4, Ipv6};
use net_types::{SpecifiedAddr, Witness as _};

use crate::bindings::devices::{BindingId, DeviceIdAndName};
use crate::bindings::time::StackTime;
use crate::bindings::util::IntoFidl;
use crate::bindings::{BindingsCtx, Ctx};
use netstack3_core::device::{
    DeviceId, EthernetDeviceId, EthernetLinkDevice, EthernetWeakDeviceId, WeakDeviceId,
};
use netstack3_core::error::NotFoundError;
use netstack3_core::neighbor::{NeighborRemovalError, StaticNeighborInsertionError};
use netstack3_core::routes::Entry;
use netstack3_core::{neighbor, IpExt};

#[derive(Debug)]
pub(crate) struct Event {
    pub(crate) id: EthernetWeakDeviceId<BindingsCtx>,
    pub(crate) addr: SpecifiedAddr<IpAddr>,
    pub(crate) kind: neighbor::EventKind<Mac>,
    pub(crate) at: StackTime,
}

struct EventLogger<'a> {
    event: &'a Event,
    ctx: &'a Ctx,
}

// NB: By burying this logic in a Display impl, we ensure it only gets
// evaluated if the stack's log level is sufficient to actually log the message.
impl std::fmt::Display for EventLogger<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { event: Event { id, addr, kind, at }, ctx } = self;
        let dev = WeakDeviceId::Ethernet(id.clone());
        // NB: We need a mutable reference to ctx, but the `Display` trait
        // only gives us a shared reference. Clone the Ctx to work around this.
        // Fortunately Ctx clones are cheap because it's an `Arc`.
        let mut ctx = (*ctx).clone();
        let neighbor_type = match (*addr).into() {
            IpAddr::V4(addr) => get_neighbor_type::<Ipv4>(&addr, &dev, &mut ctx),
            IpAddr::V6(addr) => get_neighbor_type::<Ipv6>(&addr, &dev, &mut ctx),
        };
        let bindings_id = id.bindings_id();
        write!(f, "neighbor event {bindings_id:?} {addr} ({neighbor_type}) {kind:?} at {at}")
    }
}

// Additional debug info from the routing table about a particular neighbor.
//
// Note the order of the variants below is important: each later variant is
// considered an upgrade of an earlier variant.
#[derive(Debug, PartialEq, PartialOrd)]
enum NeighborType {
    // No additional information is known about this neighbor.
    Unknown,
    // The neighbor is directly connected (e.g. it's reachable via an onlink
    // route).
    Onlink,
    // The neighbor is a gateway (e.g. it's listed as the gateway on a route).
    Gateway,
    // The neighbor is a gateway for the Internet (e.g. it's listed as a gateway
    // on a default route).
    InternetGateway,
}

impl std::fmt::Display for NeighborType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "Unknown"),
            Self::Onlink => write!(f, "Onlink"),
            Self::Gateway => write!(f, "Gateway"),
            Self::InternetGateway => write!(f, "Internet Gateway"),
        }
    }
}

impl NeighborType {
    fn maybe_upgrade(&mut self, new: NeighborType) {
        if &new > self {
            *self = new
        }
    }

    // Considers the given route, and modifies our current understanding
    // of this `NeighborType`.
    fn maybe_upgrade_with_route<I: Ip, D1, D2: PartialEq<D1>>(
        &mut self,
        addr: &SpecifiedAddr<I::Addr>,
        dev: &D1,
        route: &Entry<I::Addr, D2>,
    ) {
        let Entry { device, subnet, gateway, metric: _ } = route;
        // NB: Ignore routes on different devices.
        if device != dev {
            return;
        }
        match gateway {
            None => {
                if subnet.contains(&addr.get()) {
                    self.maybe_upgrade(NeighborType::Onlink)
                }
            }
            Some(gateway) => {
                if gateway == addr {
                    // Is this a default route?
                    if subnet.prefix() == 0 {
                        self.maybe_upgrade(NeighborType::InternetGateway)
                    } else {
                        self.maybe_upgrade(NeighborType::Gateway)
                    }
                }
            }
        }
    }
}

// Identify the `NeighborType` by consulting the routing table.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn get_neighbor_type<I: IpExt + FidlMulticastAdminIpExt>(
    addr: &SpecifiedAddr<I::Addr>,
    dev: &WeakDeviceId<BindingsCtx>,
    ctx: &mut Ctx,
) -> NeighborType {
    let neighbor_type = ctx.api().routes::<I>().fold_routes(
        NeighborType::Unknown,
        |mut neighbor_type, _table_id, route| {
            neighbor_type.maybe_upgrade_with_route::<I, _, _>(addr, dev, route);
            neighbor_type
        },
    );
    neighbor_type
}

fn new_fidl_entry(
    binding_id: BindingId,
    addr: SpecifiedAddr<IpAddr>,
    state: neighbor::EventState<Mac>,
    at: StackTime,
) -> fnet_neighbor::Entry {
    let (state, mac) = match state {
        neighbor::EventState::Dynamic(dynamic_state) => match dynamic_state {
            neighbor::EventDynamicState::Incomplete => {
                (fnet_neighbor::EntryState::Incomplete, None)
            }
            neighbor::EventDynamicState::Reachable(mac) => {
                (fnet_neighbor::EntryState::Reachable, Some(mac))
            }
            neighbor::EventDynamicState::Stale(mac) => {
                (fnet_neighbor::EntryState::Stale, Some(mac))
            }
            neighbor::EventDynamicState::Delay(mac) => {
                (fnet_neighbor::EntryState::Delay, Some(mac))
            }
            neighbor::EventDynamicState::Probe(mac) => {
                (fnet_neighbor::EntryState::Probe, Some(mac))
            }
            neighbor::EventDynamicState::Unreachable(mac) => {
                (fnet_neighbor::EntryState::Unreachable, Some(mac))
            }
        },
        neighbor::EventState::Static(mac) => (fnet_neighbor::EntryState::Static, Some(mac)),
    };
    fnet_neighbor_ext::Entry {
        interface: binding_id.into(),
        neighbor: addr.get().into_ext(),
        state,
        mac: mac.map(IntoExt::into_ext),
        updated_at: at.into_fidl(),
    }
    .into()
}

pub(crate) struct Worker {
    event_receiver: mpsc::UnboundedReceiver<Event>,
    watcher_receiver: mpsc::Receiver<NewWatcher>,
}

/// Arbitrarily picked constant to limit memory consumed by queued watcher requests.
const WATCHER_CHANNEL_CAPACITY: usize = 128;

pub(crate) fn new_worker() -> (Worker, mpsc::Sender<NewWatcher>, mpsc::UnboundedSender<Event>) {
    let (event_sink, event_receiver) = futures::channel::mpsc::unbounded();
    let (watcher_sink, watcher_receiver) =
        futures::channel::mpsc::channel(WATCHER_CHANNEL_CAPACITY);
    (Worker { event_receiver, watcher_receiver }, watcher_sink, event_sink)
}

fn handle_new_watcher(
    neighbor_state: &HashMap<
        BindingId,
        HashMap<SpecifiedAddr<IpAddr>, (neighbor::EventState<Mac>, StackTime)>,
    >,
    watchers: &mut futures::stream::FuturesUnordered<Watcher>,
    NewWatcher { options, stream }: NewWatcher,
) {
    let options = match options.try_into() {
        Ok(options) => options,
        Err(e) => {
            warn!("failed to initialize neighbor watcher: {:?}", e);
            stream.control_handle().shutdown_with_epitaph(zx::Status::INVALID_ARGS);
            return;
        }
    };
    let event_queue = EventQueue(
        neighbor_state
            .iter()
            .map(|(binding_id, entries)| {
                entries.iter().map(|(addr, (state, last_updated))| {
                    fnet_neighbor::EntryIteratorItem::Existing(new_fidl_entry(
                        *binding_id,
                        *addr,
                        *state,
                        *last_updated,
                    ))
                })
            })
            .flatten()
            .chain(std::iter::once(fnet_neighbor::EntryIteratorItem::Idle(
                fnet_neighbor::IdleEvent,
            )))
            .collect(),
    );
    watchers.push(Watcher { stream, options, event_queue, responder: None });
}

impl Worker {
    pub(crate) async fn run(self, ctx: Ctx) {
        let Self { event_receiver: event_stream, watcher_receiver: new_watchers } = self;
        let mut watchers = futures::stream::FuturesUnordered::<Watcher>::new();
        let mut neighbor_state: HashMap<
            _,
            HashMap<SpecifiedAddr<IpAddr>, (neighbor::EventState<Mac>, StackTime)>,
        > = HashMap::new();

        enum SinkItem {
            NewWatcher(NewWatcher),
            Event(Event),
        }
        // Always consume events before watchers. That allows external
        // observers to assume all side effects of a call are already
        // applied before a watcher observes its initial existing set of
        // properties.
        let mut stream = futures::stream::select_with_strategy(
            event_stream.map(SinkItem::Event),
            new_watchers.map(SinkItem::NewWatcher),
            |_: &mut ()| futures::stream::PollNext::Left,
        );

        enum Item {
            WatcherEnded(Result<(), fidl::Error>),
            SinkItem(Option<SinkItem>),
        }
        loop {
            let item = futures::select! {
                i = stream.next() => Item::SinkItem(i),
                w = watchers.select_next_some() => Item::WatcherEnded(w),
                complete => break,
            };
            match item {
                Item::SinkItem(None) => {
                    info!("neighbor worker shutting down, waiting for watchers to end")
                }
                Item::WatcherEnded(r) => r.unwrap_or_else(|e| {
                    if !e.is_closed() {
                        error!("error operating neighbor watcher {:?}", e);
                    }
                }),
                Item::SinkItem(Some(SinkItem::NewWatcher(new_watcher))) => {
                    handle_new_watcher(&neighbor_state, &mut watchers, new_watcher);
                }
                Item::SinkItem(Some(SinkItem::Event(
                    ref event @ Event { ref id, kind, addr, at },
                ))) => {
                    info!("{}", EventLogger { event, ctx: &ctx });

                    let DeviceIdAndName { id: binding_id, name: _ } = *id.bindings_id();
                    let entry = neighbor_state
                        .entry(binding_id)
                        .or_insert_with(|| HashMap::new())
                        .entry(addr);
                    let fidl_event = match kind {
                        neighbor::EventKind::Added(state) => match entry {
                            std::collections::hash_map::Entry::Occupied(occupied) => {
                                panic!(
                                    "neighbor added but already exists: entry={:?}, event={:?}",
                                    occupied.get(),
                                    event
                                );
                            }
                            std::collections::hash_map::Entry::Vacant(vacant) => {
                                let _ = vacant.insert((state, at));
                                fnet_neighbor::EntryIteratorItem::Added(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                        neighbor::EventKind::Removed => match entry {
                            std::collections::hash_map::Entry::Vacant(_) => {
                                panic!("neighbor removed but not found: {event:?}");
                            }
                            std::collections::hash_map::Entry::Occupied(occupied) => {
                                let (state, at) = occupied.remove();

                                let entry = assert_matches!(
                                    neighbor_state.entry(binding_id),
                                    std::collections::hash_map::Entry::Occupied(o) => o
                                );
                                if entry.get().is_empty() {
                                    let _ = entry.remove();
                                }

                                fnet_neighbor::EntryIteratorItem::Removed(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                        neighbor::EventKind::Changed(state) => match entry {
                            std::collections::hash_map::Entry::Vacant(_) => {
                                panic!("neighbor changed but not found: {event:?}");
                            }
                            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                                let (current_state, _) = *occupied.get();
                                // NS3 core guarantees to only emit changed events if state
                                // actually changed.
                                assert_ne!(
                                    current_state, state,
                                    "neighbor changed but nothing changed: {event:?}",
                                );
                                let _ = occupied.insert((state, at));
                                fnet_neighbor::EntryIteratorItem::Changed(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                    };
                    watchers.iter_mut().for_each(|watcher| {
                        watcher.push(fidl_event.clone());
                    });
                }
            }
        }
        info!("all neighbor watchers closed, neighbor worker shutdown is complete");
    }
}

#[derive(Debug)]
/// A bounded queue of [`Events`] to be returned to `fuchsia.net.neighbor/EntryIterator`.
struct EventQueue(VecDeque<fnet_neighbor::EntryIteratorItem>);

const MAX_ITEM_BATCH_SIZE: usize = fnet_neighbor::MAX_ITEM_BATCH_SIZE as usize;
// Arbitrarily-chosen maximum number of events to queue per client (4 times the
// maximum number of entries held in core per IP per interface).
const MAX_EVENTS: usize = 4 * neighbor::MAX_ENTRIES;

impl EventQueue {
    fn is_empty(&self) -> bool {
        let Self(event_queue) = self;
        event_queue.is_empty()
    }

    fn push(
        &mut self,
        event: fnet_neighbor::EntryIteratorItem,
    ) -> Result<(), fnet_neighbor::EntryIteratorItem> {
        let Self(event_queue) = self;
        if event_queue.len() >= MAX_EVENTS {
            return Err(event);
        }
        event_queue.push_back(event);
        Ok(())
    }

    fn pop_max(&mut self) -> impl IntoIterator<Item = fnet_neighbor::EntryIteratorItem> + '_ {
        let Self(event_queue) = self;
        let count = std::cmp::min(MAX_ITEM_BATCH_SIZE, event_queue.len());
        event_queue.drain(0..count)
    }
}

/// The task that serves `fuchsia.net.neighbor/EntryIterator`.
///
/// The future implementation drives `stream` and responds to the requests
/// with events from `event_queue`, and completes when `stream` is exhausted
/// (possibly with an error).
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub(crate) struct Watcher {
    stream: fnet_neighbor::EntryIteratorRequestStream,
    options: fnet_neighbor_ext::EntryIteratorOptions,
    event_queue: EventQueue,
    responder: Option<fnet_neighbor::EntryIteratorGetNextResponder>,
}

fn send_events(
    responder: fnet_neighbor::EntryIteratorGetNextResponder,
    events: &[fnet_neighbor::EntryIteratorItem],
) {
    responder.send(events).unwrap_or_else(|e| {
        if e.is_closed() {
            warn!("neighbor watcher closed when sending event");
        } else {
            error!("error sending event to neighbor watcher: {e:?}");
        }
    })
}

impl Future for Watcher {
    type Output = Result<(), fidl::Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let next_request = self.as_mut().stream.poll_next_unpin(cx)?;
            match futures::ready!(next_request) {
                Some(fnet_neighbor::EntryIteratorRequest::GetNext { responder }) => {
                    if !self.event_queue.is_empty() {
                        let events = self.event_queue.pop_max().into_iter().collect::<Vec<_>>();
                        send_events(responder, &events);
                    } else {
                        match &self.responder {
                            Some(existing) => {
                                existing
                                    .control_handle()
                                    .shutdown_with_epitaph(zx::Status::ALREADY_EXISTS);
                                return Poll::Ready(Ok(()));
                            }
                            None => {
                                self.responder = Some(responder);
                            }
                        }
                    }
                }
                None => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl Watcher {
    fn push(&mut self, event: fnet_neighbor::EntryIteratorItem) {
        let Self {
            stream,
            event_queue,
            responder,
            options: fnet_neighbor_ext::EntryIteratorOptions {},
        } = self;

        match responder.take() {
            Some(responder) => {
                debug_assert!(event_queue.is_empty());
                send_events(responder, std::slice::from_ref(&event));
            }
            None => {
                event_queue.push(event).unwrap_or_else(|_: fnet_neighbor::EntryIteratorItem| {
                    warn!("too many pending events enqueued for neighbor watcher, closing channel");
                    stream.control_handle().shutdown();
                });
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("neighbor worker no longer available")]
pub(crate) struct WorkerClosedError;

/// Possible errors when serving `fuchsia.net.neighbor/View`.
#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("failed to send a Watcher task to parent")]
    Send(#[from] WorkerClosedError),
    #[error(transparent)]
    Fidl(#[from] fidl::Error),
}

pub(crate) struct NewWatcher {
    stream: fnet_neighbor::EntryIteratorRequestStream,
    options: fnet_neighbor::EntryIteratorOptions,
}

pub(super) async fn serve_view(
    stream: ViewRequestStream,
    sink: mpsc::Sender<NewWatcher>,
) -> Result<(), Error> {
    stream
        .err_into()
        .try_fold(sink, |mut sink, request| async move {
            match request {
                ViewRequest::OpenEntryIterator { it, options, control_handle: _ } => sink
                    .send(NewWatcher { stream: it.into_stream(), options })
                    .await
                    .map_err(|_: mpsc::SendError| Error::Send(WorkerClosedError))?,
            }
            Ok(sink)
        })
        .map_ok(|_: mpsc::Sender<NewWatcher>| ())
        .await
}

fn get_ethernet_id(ctx: &Ctx, interface: u64) -> Result<EthernetDeviceId<BindingsCtx>, zx::Status> {
    match BindingId::new(interface)
        .and_then(|id| ctx.bindings_ctx().devices.get_core_id(id))
        .ok_or(zx::Status::NOT_FOUND)?
    {
        DeviceId::Ethernet(e) => Ok(e),
        // NUD is not supported for Loopback, pure IP, or blackhole devices.
        DeviceId::Loopback(_) | DeviceId::PureIp(_) | DeviceId::Blackhole(_) => {
            Err(zx::Status::NOT_SUPPORTED)
        }
    }
}

#[netstack3_core::context_ip_bounds(A::Version, BindingsCtx)]
fn add_static_entry<A: IpAddress>(
    ctx: &mut Ctx,
    interface: u64,
    neighbor: A,
    mac: fnet::MacAddress,
) -> Result<(), zx::Status>
where
    A::Version: IpExt,
{
    let device_id = get_ethernet_id(ctx, interface)?;
    let mac = mac.into_ext();
    ctx.api()
        .neighbor::<A::Version, EthernetLinkDevice>()
        .insert_static_entry(&device_id, neighbor, mac)
        .map_err(|e| match e {
            StaticNeighborInsertionError::MacAddressNotUnicast
            | StaticNeighborInsertionError::IpAddressInvalid => zx::Status::INVALID_ARGS,
        })
}

#[netstack3_core::context_ip_bounds(A::Version, BindingsCtx)]
fn remove_entry<A: IpAddress>(ctx: &mut Ctx, interface: u64, neighbor: A) -> Result<(), zx::Status>
where
    A::Version: IpExt,
{
    let device_id = get_ethernet_id(ctx, interface)?;
    ctx.api()
        .neighbor::<A::Version, EthernetLinkDevice>()
        .remove_entry(&device_id, neighbor)
        .map_err(|e| match e {
            NeighborRemovalError::IpAddressInvalid => zx::Status::INVALID_ARGS,
            NeighborRemovalError::NotFound(NotFoundError) => zx::Status::NOT_FOUND,
        })
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn clear_entries<I: IpExt>(ctx: &mut Ctx, interface: u64) -> Result<(), zx::Status> {
    let device_id = get_ethernet_id(ctx, interface)?;
    Ok(ctx.api().neighbor::<I, EthernetLinkDevice>().flush_table(&device_id))
}

pub(super) async fn serve_controller(
    ctx: Ctx,
    stream: ControllerRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each(|request| async {
            let mut ctx: Ctx = ctx.clone();
            match request {
                ControllerRequest::AddEntry { interface, neighbor, mac, responder } => {
                    let result = match neighbor.into_ext() {
                        IpAddr::V4(v4) => add_static_entry(&mut ctx, interface, v4, mac),
                        IpAddr::V6(v6) => add_static_entry(&mut ctx, interface, v6, mac),
                    };
                    responder.send(result.map_err(|e| e.into_raw()))
                }
                ControllerRequest::RemoveEntry { interface, neighbor, responder } => {
                    let result = match neighbor.into_ext() {
                        IpAddr::V4(v4) => remove_entry(&mut ctx, interface, v4),
                        IpAddr::V6(v6) => remove_entry(&mut ctx, interface, v6),
                    };
                    responder.send(result.map_err(|e| e.into_raw()))
                }
                ControllerRequest::ClearEntries { interface, ip_version, responder } => {
                    let result = match ip_version {
                        fnet::IpVersion::V4 => clear_entries::<Ipv4>(&mut ctx, interface),
                        fnet::IpVersion::V6 => clear_entries::<Ipv6>(&mut ctx, interface),
                    };
                    responder.send(result.map_err(|e| e.into_raw()))
                }
            }
        })
        .await
}

#[cfg(test)]
mod tests {
    use net_declare::{net_ip_v4, net_subnet_v4};
    use net_types::ip::{Ipv4Addr, Subnet};
    use net_types::SpecifiedAddr;
    use netstack3_core::routes::{Metric, RawMetric};
    use test_case::test_case;

    use super::*;

    const RIGHT_ADDR_RAW: Ipv4Addr = net_ip_v4!("192.168.0.1");
    const RIGHT_ADDR: SpecifiedAddr<Ipv4Addr> =
        unsafe { SpecifiedAddr::new_unchecked(RIGHT_ADDR_RAW) };
    const WRONG_ADDR: SpecifiedAddr<Ipv4Addr> =
        unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("192.168.0.2")) };

    const RIGHT_DEV: u8 = 1;
    const WRONG_DEV: u8 = 2;

    const ONLINK_ENTRY: Entry<Ipv4Addr, u8> = Entry {
        subnet: unsafe { Subnet::new_unchecked(RIGHT_ADDR_RAW, 32) },
        device: RIGHT_DEV,
        gateway: None,
        metric: Metric::ExplicitMetric(RawMetric(0)),
    };
    const GATEWAY_ENTRY: Entry<Ipv4Addr, u8> = Entry {
        subnet: net_subnet_v4!("192.168.0.0/16"),
        device: RIGHT_DEV,
        gateway: Some(RIGHT_ADDR),
        metric: Metric::ExplicitMetric(RawMetric(0)),
    };
    const INTERNET_GATEWAY_ENTRY: Entry<Ipv4Addr, u8> = Entry {
        subnet: net_subnet_v4!("0.0.0.0/0"),
        device: RIGHT_DEV,
        gateway: Some(RIGHT_ADDR),
        metric: Metric::ExplicitMetric(RawMetric(0)),
    };

    #[test_case(RIGHT_ADDR, WRONG_DEV, ONLINK_ENTRY, NeighborType::Unknown
        => NeighborType::Unknown; "wrong_dev")]
    #[test_case(WRONG_ADDR, RIGHT_DEV, ONLINK_ENTRY, NeighborType::Unknown
        => NeighborType::Unknown; "wrong_addr")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, ONLINK_ENTRY, NeighborType::Unknown
        => NeighborType::Onlink; "unknown_to_onlink")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, GATEWAY_ENTRY, NeighborType::Unknown
        => NeighborType::Gateway; "unknown_to_gateway")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, GATEWAY_ENTRY, NeighborType::Onlink
        => NeighborType::Gateway; "onlink_to_gateway")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, ONLINK_ENTRY, NeighborType::Gateway
        => NeighborType::Gateway; "gateway_downgrade_forbidden")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, INTERNET_GATEWAY_ENTRY, NeighborType::Unknown
        => NeighborType::InternetGateway; "unknown_to_internet_gateway")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, INTERNET_GATEWAY_ENTRY, NeighborType::Onlink
        => NeighborType::InternetGateway; "onlink_to_internet_gateway")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, INTERNET_GATEWAY_ENTRY, NeighborType::Gateway
        => NeighborType::InternetGateway; "gateway_to_internet_gateway")]
    #[test_case(RIGHT_ADDR, RIGHT_DEV, GATEWAY_ENTRY, NeighborType::InternetGateway
        => NeighborType::InternetGateway; "internet_gateway_downgrade_forbidden")]
    fn upgrade_neighbor_type(
        addr: SpecifiedAddr<Ipv4Addr>,
        dev: u8,
        entry: Entry<Ipv4Addr, u8>,
        mut initial: NeighborType,
    ) -> NeighborType {
        initial.maybe_upgrade_with_route::<Ipv4, u8, u8>(&addr, &dev, &entry);
        initial
    }
}
