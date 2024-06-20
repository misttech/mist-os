// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL Worker for the `fuchsia.net.routes` suite of protocols.

use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::pin::pin;

use async_utils::event::Event;
use derivative::Derivative;
use either::Either;
use fidl::endpoints::{DiscoverableProtocolMarker as _, ProtocolMarker};
use futures::channel::{mpsc, oneshot};
use futures::future::FusedFuture as _;
use futures::{FutureExt, StreamExt as _, TryStream, TryStreamExt as _};
use itertools::Itertools as _;
use log::{debug, error, info, warn};
use net_types::ethernet::Mac;
use net_types::ip::{GenericOverIp, Ip, IpAddr, IpAddress, Ipv4, Ipv6};
use net_types::SpecifiedAddr;
use netstack3_core::device::{DeviceId, EthernetDeviceId, EthernetLinkDevice};
use netstack3_core::error::AddressResolutionFailed;
use netstack3_core::neighbor::{LinkResolutionContext, LinkResolutionResult};
use netstack3_core::routes::{NextHop, ResolvedRoute, WrapBroadcastMarker};
use netstack3_core::sync::Mutex;
use thiserror::Error;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_routes as fnet_routes,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext, fuchsia_zircon as zx,
};

use crate::bindings::util::{ConversionContext as _, IntoCore as _, IntoFidl as _, ResultExt as _};
use crate::bindings::{routes, BindingsCtx, Ctx, IpExt};

impl LinkResolutionContext<EthernetLinkDevice> for BindingsCtx {
    type Notifier = LinkResolutionNotifier;
}

#[derive(Debug)]
pub(crate) struct LinkResolutionNotifier(oneshot::Sender<Result<Mac, AddressResolutionFailed>>);

impl netstack3_core::neighbor::LinkResolutionNotifier<EthernetLinkDevice>
    for LinkResolutionNotifier
{
    type Observer = oneshot::Receiver<Result<Mac, AddressResolutionFailed>>;

    fn new() -> (Self, Self::Observer) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), rx)
    }

    fn notify(self, result: Result<Mac, AddressResolutionFailed>) {
        let Self(tx) = self;
        tx.send(result).unwrap_or_else(|_| {
            error!("link address observer was dropped before resolution completed")
        });
    }
}

/// Serve the `fuchsia.net.routes/State` protocol.
pub(crate) async fn serve_state(rs: fnet_routes::StateRequestStream, ctx: Ctx) {
    rs.try_for_each_concurrent(None, |req| async {
        match req {
            fnet_routes::StateRequest::Resolve { destination, responder } => {
                let result = resolve(destination, ctx.clone()).await;
                responder
                    .send(result.as_ref().map_err(|e| e.into_raw()))
                    .unwrap_or_log("failed to respond");
                Ok(())
            }
            fnet_routes::StateRequest::GetRouteTableName { table_id: _, responder: _ } => {
                todo!("TODO(https://fxbug.dev/336205291): Implement for main table");
            }
        }
    })
    .await
    .unwrap_or_else(|e| warn!("error serving {}: {:?}", fnet_routes::StateMarker::PROTOCOL_NAME, e))
}

/// Resolves the route to the given destination address.
///
/// Returns `Err` if the destination can't be resolved.
async fn resolve(
    destination: fnet::IpAddress,
    ctx: Ctx,
) -> Result<fnet_routes::Resolved, zx::Status> {
    let addr: IpAddr = destination.into_core();
    match addr {
        IpAddr::V4(addr) => resolve_inner(addr, ctx).await,
        IpAddr::V6(addr) => resolve_inner(addr, ctx).await,
    }
}

/// The inner implementation of [`resolve`] that's generic over `Ip`.
#[netstack3_core::context_ip_bounds(A::Version, BindingsCtx)]
async fn resolve_inner<A: IpAddress>(
    destination: A,
    mut ctx: Ctx,
) -> Result<fnet_routes::Resolved, zx::Status>
where
    A::Version: IpExt,
{
    let sanitized_dst = SpecifiedAddr::new(destination)
        .map(|dst| {
            netstack3_core::routes::RoutableIpAddr::try_from(dst).map_err(
                |netstack3_core::socket::AddrIsMappedError {}| zx::Status::ADDRESS_UNREACHABLE,
            )
        })
        .transpose()?;
    let ResolvedRoute { device, src_addr, local_delivery_device: _, next_hop } =
        match ctx.api().routes::<A::Version>().resolve_route(sanitized_dst) {
            Err(e) => {
                info!("Resolve failed for {}, {:?}", destination, e);
                return Err(zx::Status::ADDRESS_UNREACHABLE);
            }
            Ok(resolved_route) => resolved_route,
        };
    let (next_hop_addr, next_hop_type) = match next_hop {
        NextHop::RemoteAsNeighbor => {
            (SpecifiedAddr::new(destination), Either::Left(fnet_routes::Resolved::Direct))
        }
        NextHop::Broadcast(marker) => {
            <A::Version as Ip>::map_ip::<_, ()>(
                WrapBroadcastMarker(marker),
                |WrapBroadcastMarker(())| (),
                |WrapBroadcastMarker(never)| match never {},
            );
            (SpecifiedAddr::new(destination), Either::Left(fnet_routes::Resolved::Direct))
        }
        NextHop::Gateway(gateway) => (Some(gateway), Either::Right(fnet_routes::Resolved::Gateway)),
    };
    let remote_mac = match &device {
        DeviceId::Loopback(_device) => None,
        DeviceId::Ethernet(device) => {
            if let Some(addr) = next_hop_addr {
                Some(resolve_ethernet_link_addr(&mut ctx, device, &addr).await?)
            } else {
                warn!("Cannot attempt Ethernet link resolution for the unspecified address.");
                return Err(zx::Status::ADDRESS_UNREACHABLE);
            }
        }
        DeviceId::PureIp(_device) => None,
    };

    let destination = {
        let address =
            next_hop_addr.map_or(A::Version::UNSPECIFIED_ADDRESS, |a| *a).to_ip_addr().into_fidl();
        let source_address = src_addr.addr().to_ip_addr().into_fidl();
        let mac = remote_mac.map(|mac| mac.into_fidl());
        let interface_id = ctx.bindings_ctx().get_binding_id(device);
        fnet_routes::Destination {
            address: Some(address),
            mac,
            interface_id: Some(interface_id.get()),
            source_address: Some(source_address),
            ..Default::default()
        }
    };

    Ok(either::for_both!(next_hop_type, f => f(destination)))
}

/// Performs link-layer resolution of the remote IP Address on the given device.
#[netstack3_core::context_ip_bounds(A::Version, BindingsCtx)]
async fn resolve_ethernet_link_addr<A: IpAddress>(
    ctx: &mut Ctx,
    device: &EthernetDeviceId<BindingsCtx>,
    remote: &SpecifiedAddr<A>,
) -> Result<Mac, zx::Status>
where
    A::Version: IpExt,
{
    match ctx.api().neighbor::<A::Version, EthernetLinkDevice>().resolve_link_addr(device, remote) {
        LinkResolutionResult::Resolved(mac) => Ok(mac),
        LinkResolutionResult::Pending(observer) => observer
            .await
            .expect("core must send link resolution result before dropping notifier")
            .map_err(|AddressResolutionFailed| zx::Status::ADDRESS_UNREACHABLE),
    }
}

/// Serve the `fuchsia.net.routes/StateV4` protocol.
pub(crate) async fn serve_state_v4(
    rs: fnet_routes::StateV4RequestStream,
    dispatcher: &RouteUpdateDispatcher<Ipv4>,
) {
    rs.try_for_each_concurrent(None, |req| match req {
        fnet_routes::StateV4Request::GetWatcherV4 { options, watcher, control_handle: _ } => {
            serve_route_watcher::<Ipv4>(watcher, options.into(), dispatcher).map(|result| {
                Ok(result.unwrap_or_else(|e| {
                    warn!("error serving {}: {:?}", fnet_routes::WatcherV4Marker::DEBUG_NAME, e)
                }))
            })
        }
        fnet_routes::StateV4Request::GetRuleWatcherV4 {
            options: _,
            watcher: _,
            control_handle: _,
        } => {
            todo!("TODO(https://fxbug.dev/336204757): Implement rules watcher");
        }
    })
    .await
    .unwrap_or_else(|e| {
        warn!("error serving {}: {:?}", fnet_routes::StateV4Marker::PROTOCOL_NAME, e)
    })
}

/// Serve the `fuchsia.net.routes/StateV6` protocol.
pub(crate) async fn serve_state_v6(
    rs: fnet_routes::StateV6RequestStream,
    dispatcher: &RouteUpdateDispatcher<Ipv6>,
) {
    rs.try_for_each_concurrent(None, |req| match req {
        fnet_routes::StateV6Request::GetWatcherV6 { options, watcher, control_handle: _ } => {
            serve_route_watcher::<Ipv6>(watcher, options.into(), dispatcher).map(|result| {
                Ok(result.unwrap_or_else(|e| {
                    warn!("error serving {}: {:?}", fnet_routes::WatcherV6Marker::DEBUG_NAME, e)
                }))
            })
        }
        fnet_routes::StateV6Request::GetRuleWatcherV6 {
            options: _,
            watcher: _,
            control_handle: _,
        } => {
            todo!("TODO(https://fxbug.dev/336204757): Implement rules watcher");
        }
    })
    .await
    .unwrap_or_else(|e| {
        warn!("error serving {}: {:?}", fnet_routes::StateV6Marker::PROTOCOL_NAME, e)
    })
}

#[derive(Debug, Error)]
pub(crate) enum ServeWatcherError {
    #[error("the request stream contained a FIDL error")]
    ErrorInStream(fidl::Error),
    #[error("a FIDL error was encountered while sending the response")]
    FailedToRespond(fidl::Error),
    #[error("the client called `Watch` while a previous call was already pending")]
    PreviousPendingWatch,
    #[error("the client was canceled")]
    Canceled,
}

// Serve a single client of the `WatcherV4` or `WatcherV6` protocol.
async fn serve_route_watcher<I: fnet_routes_ext::FidlRouteIpExt>(
    server_end: fidl::endpoints::ServerEnd<I::WatcherMarker>,
    fnet_routes_ext::WatcherOptions { table_interest }: fnet_routes_ext::WatcherOptions,
    dispatcher: &RouteUpdateDispatcher<I>,
) -> Result<(), ServeWatcherError> {
    let client_interest = match table_interest {
        Some(fnet_routes::TableInterest::Main(fnet_routes::Main)) => {
            RouteTableInterest::Only { table_id: routes::main_table_id::<I>().into() }
        }
        Some(fnet_routes::TableInterest::Only(table_id)) => RouteTableInterest::Only { table_id },
        Some(fnet_routes::TableInterest::All(fnet_routes::All)) | None => RouteTableInterest::All,
        Some(fnet_routes::TableInterest::__SourceBreaking { unknown_ordinal }) => {
            return Err(ServeWatcherError::ErrorInStream(fidl::Error::UnknownOrdinal {
                ordinal: unknown_ordinal,
                protocol_name: <I::WatcherMarker as ProtocolMarker>::DEBUG_NAME,
            }))
        }
    };
    serve_watcher(server_end, client_interest, dispatcher).await
}

pub(crate) async fn serve_watcher<E: FidlWatcherEvent, WI: WatcherInterest<E>>(
    server_end: fidl::endpoints::ServerEnd<E::WatcherMarker>,
    interest: WI,
    UpdateDispatcher(dispatcher): &UpdateDispatcher<E, WI>,
) -> Result<(), ServeWatcherError> {
    let mut watcher = {
        let mut dispatcher = dispatcher.lock();
        dispatcher.connect_new_client(interest)
    };

    let request_stream =
        server_end.into_stream().expect("failed to acquire request_stream from server_end");

    let canceled_fut = watcher.canceled.wait();

    let result = {
        let mut request_stream = request_stream.map_err(ServeWatcherError::ErrorInStream).fuse();
        let mut canceled_fut = pin!(canceled_fut);
        let mut pending_watch_request = futures::future::OptionFuture::default();
        loop {
            pending_watch_request = futures::select! {
                request = request_stream.try_next() => match request {
                    Ok(Some(req)) => if pending_watch_request.is_terminated() {
                        // Convince the compiler that we're not holding on to a
                        // borrow of watcher.
                        std::mem::drop(pending_watch_request);
                        // Old request is terminated, accept this new one.
                        Some(watcher.watch().map(move |events| (req, events))).into()
                    } else {
                        break Err(ServeWatcherError::PreviousPendingWatch);
                    },
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                },
                r = pending_watch_request => {
                    let (request, events) = r.expect("OptionFuture is not selected if empty");
                    match E::respond_to_watch_request(request, events) {
                        Ok(()) => None.into(),
                        Err(e) => break Err(ServeWatcherError::FailedToRespond(e)),
                    }
                },
                () = canceled_fut => break Err(ServeWatcherError::Canceled),
            };
        }
    };
    {
        let mut dispatcher = dispatcher.lock();
        dispatcher.disconnect_client(watcher);
    }

    result
}

impl<I: fnet_routes_ext::FidlRouteIpExt> FidlWatcherEvent for fnet_routes_ext::Event<I> {
    type WatcherMarker = I::WatcherMarker;

    // Responds to a single `Watch` request with the given batch of events.
    fn respond_to_watch_request(
        req: <<Self::WatcherMarker as ProtocolMarker>::RequestStream as TryStream>::Ok,
        events: Vec<Self>,
    ) -> Result<(), fidl::Error> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Inputs<I: fnet_routes_ext::FidlRouteIpExt> {
            req: <<I::WatcherMarker as ProtocolMarker>::RequestStream as TryStream>::Ok,
            events: Vec<fnet_routes_ext::Event<I>>,
        }
        let result =
            I::map_ip_in::<Inputs<I>, _>(
                Inputs { req, events },
                |Inputs { req, events }| match req {
                    fnet_routes::WatcherV4Request::Watch { responder } => {
                        let events = events
                            .into_iter()
                            .map(|event| {
                                event.try_into().unwrap_or_else(|e| {
                                    match e {
                            fnet_routes_ext::NetTypeConversionError::UnknownUnionVariant(msg) => {
                                panic!("tried to send an event with Unknown enum variant: {}", msg)
                            }
                        }
                                })
                            })
                            .collect::<Vec<_>>();
                        responder.send(&events)
                    }
                },
                |Inputs { req, events }| match req {
                    fnet_routes::WatcherV6Request::Watch { responder } => {
                        let events = events
                            .into_iter()
                            .map(|event| {
                                event.try_into().unwrap_or_else(|e| {
                                    match e {
                            fnet_routes_ext::NetTypeConversionError::UnknownUnionVariant(msg) => {
                                panic!("tried to send an event with Unknown enum variant: {}", msg)
                            }
                        }
                                })
                            })
                            .collect::<Vec<_>>();
                        responder.send(&events)
                    }
                },
            );
        result
    }
}

/// An update to the routing/rule table.
#[derive(Clone)]
pub(crate) enum Update<E: WatcherEvent> {
    Added(E::Resource),
    Removed(E::Resource),
}

/// An event to be watched.
///
/// For example [`fnet_routes_ext::Event`] for the routes watcher. This is a
/// separate trait from [`FidlWatcherEvent`] so that we can tie this trait
/// bound to logic that is unrelated to FIDL. This gives us the benefit to not
/// mention the fidl IP extension traits everywhere.
pub(crate) trait WatcherEvent: From<Update<Self>> + Debug + Clone {
    /// The installed resource that is being tracked.
    ///
    /// E.g. [`fnet_routes_ext::InstalledRoute`] for [`fnet_routes_ext::Event`].
    type Resource: Hash + PartialEq + Eq + Clone + Debug;

    /// The maximum pending events before closing the client.
    const MAX_PENDING_EVENTS: usize;

    /// The idle event.
    const IDLE: Self;

    /// Turn the installed type into an existing event.
    fn existing(installed: Self::Resource) -> Self;
}

impl<I: Ip> WatcherEvent for fnet_routes_ext::Event<I> {
    type Resource = fnet_routes_ext::InstalledRoute<I>;

    // The maximum number of events for the `fuchsia.net.routes/Watcher` client
    // is allowed to have queued. Clients will be dropped if they exceed this
    // limit. Keep this a multiple of `fnet_routes::MAX_EVENTS` (5 is somewhat
    // arbitrary) so we don't artificially truncate the allowed batch size.
    const MAX_PENDING_EVENTS: usize = (fnet_routes::MAX_EVENTS * 5) as usize;

    const IDLE: Self = fnet_routes_ext::Event::Idle;

    fn existing(installed: Self::Resource) -> Self {
        Self::Existing(installed)
    }
}

/// The trait that abstracts the FIDL behavior of a [`WatcherEvent`].
pub(crate) trait FidlWatcherEvent: WatcherEvent {
    /// The protocol marker for the watcher protocol
    type WatcherMarker: ProtocolMarker;

    /// Responds to the FIDL `watch` request.
    fn respond_to_watch_request(
        req: <<Self::WatcherMarker as ProtocolMarker>::RequestStream as TryStream>::Ok,
        events: Vec<Self>,
    ) -> Result<(), fidl::Error>;
}

impl<I: Ip> From<Update<fnet_routes_ext::Event<I>>> for fnet_routes_ext::Event<I> {
    fn from(update: Update<fnet_routes_ext::Event<I>>) -> Self {
        match update {
            Update::Added(added) => fnet_routes_ext::Event::Added(added),
            Update::Removed(removed) => fnet_routes_ext::Event::Removed(removed),
        }
    }
}

// Consumes updates to the system routing table and dispatches them to clients
// of the `fuchsia.net.routes/{Rule}WatcherV{4,6}` protocols.
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
#[derivative(Default(bound = ""))]
pub(crate) struct UpdateDispatcher<E: WatcherEvent, WI>(
    std::sync::Arc<Mutex<UpdateDispatcherInner<E, WI>>>,
);

pub(crate) type RouteUpdateDispatcher<I> =
    UpdateDispatcher<fnet_routes_ext::Event<I>, RouteTableInterest>;

// The inner representation of a `UpdateDispatcher` holding state for the
// given IP protocol.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct UpdateDispatcherInner<E: WatcherEvent, WI> {
    // The set of currently installed routes/rules.
    installed: HashSet<E::Resource>,
    // The list of currently connected clients.
    clients: Vec<WatcherSink<E, WI>>,
}

// The error type returned by `UpdateDispatcher.notify()`.
#[derive(Debug, PartialEq)]
pub(crate) enum UpdateNotifyError<E: WatcherEvent> {
    // `notify` was called with `TableUpdate::Added` for a route
    // that already exists.
    AlreadyExists(E::Resource),
    // `notify` was called with `TableUpdate::Removed` for a route
    // that does not exist.
    NotFound(E::Resource),
}

impl<E: WatcherEvent, WI: WatcherInterest<E>> UpdateDispatcherInner<E, WI> {
    /// Notifies this `UpdateDispatcher` of an update to the routing table.
    /// The update will be dispatched to all active watcher clients.
    fn notify(&mut self, update: Update<E>) -> Result<(), UpdateNotifyError<E>> {
        let UpdateDispatcherInner { installed, clients } = self;
        match &update {
            Update::Added(added) => {
                if !installed.insert(added.clone()) {
                    return Err(UpdateNotifyError::AlreadyExists(added.clone()));
                }
            }
            Update::Removed(removed) => {
                if !installed.remove(removed) {
                    return Err(UpdateNotifyError::NotFound(removed.clone()));
                }
            }
        }
        let event = E::from(update.clone());
        for client in clients {
            client.send(event.clone())
        }
        Ok(())
    }

    /// Registers a new client with this `UpdateDispatcher`.
    fn connect_new_client(&mut self, client_interest: WI) -> Watcher<E> {
        let UpdateDispatcherInner { installed: routes, clients } = self;
        let (watcher, sink) = Watcher::new_with_existing(routes.iter().cloned(), client_interest);
        clients.push(sink);
        watcher
    }

    /// Disconnects the given watcher from this `UpdateDispatcher`.
    fn disconnect_client(&mut self, watcher: Watcher<E>) {
        let UpdateDispatcherInner { installed: _, clients } = self;
        let (idx, _): (usize, &WatcherSink<E, WI>) = clients
            .iter()
            .enumerate()
            .filter(|(_idx, client)| client.is_connected_to(&watcher))
            .exactly_one()
            .expect("expected exactly one sink");
        let _: WatcherSink<E, WI> = clients.swap_remove(idx);
    }
}

impl<E: WatcherEvent, WI: WatcherInterest<E>> UpdateDispatcher<E, WI> {
    pub(crate) fn notify(&self, update: Update<E>) -> Result<(), UpdateNotifyError<E>> {
        let Self(inner) = self;
        inner.lock().notify(update)
    }
}

/// The tables the client is interested in.
#[derive(Debug)]
pub(crate) enum RouteTableInterest {
    /// The client is interested in updates across all tables.
    All,
    /// The client only want updates on the specific table.
    ///
    /// The table ID is a scalar instead of [`TableId`] because we don't perform
    /// validation but only filtering.
    Only { table_id: u32 },
}

impl RouteTableInterest {
    fn has_interest_in<I: Ip>(&self, event: &fnet_routes_ext::Event<I>) -> bool {
        match event {
            fnet_routes_ext::Event::Unknown | fnet_routes_ext::Event::Idle => true,
            fnet_routes_ext::Event::Existing(installed_route)
            | fnet_routes_ext::Event::Added(installed_route)
            | fnet_routes_ext::Event::Removed(installed_route) => match self {
                RouteTableInterest::All => true,
                RouteTableInterest::Only { table_id } => installed_route.table_id == *table_id,
            },
        }
    }
}

/// Consumes events for a single client of the
/// `fuchsia.net.routes/{Rule}WatcherV{4,6}` protocols.
#[derive(Debug)]
struct WatcherSink<E, WI> {
    // The sink with which to send changes to this client.
    sink: mpsc::Sender<E>,
    // The mechanism with which to cancel the client.
    cancel: Event,
    // What table is this client interested in.
    interest: WI,
}

impl<E: WatcherEvent, WI: WatcherInterest<E>> WatcherSink<E, WI> {
    // Send this [`WatcherSink`] a new event.
    fn send(&mut self, event: E) {
        let interested = self.interest.has_interest_in(&event);

        if !interested {
            debug!(
                "The client for sink {:?} is not interested in this event {:?}, skipping",
                self, event
            );
            return;
        }

        self.sink.try_send(event).unwrap_or_else(|e| {
            if e.is_full() {
                if self.cancel.signal() {
                    warn!(
                        "too many unconsumed events (the client may not be \
                        calling Watch frequently enough): {}",
                        E::MAX_PENDING_EVENTS
                    )
                }
            } else {
                panic!("unexpected error trying to send: {:?}", e)
            }
        })
    }

    // Returns `true` if this sink forwards events to the given watcher.
    fn is_connected_to(&self, watcher: &Watcher<E>) -> bool {
        self.sink.is_connected_to(&watcher.receiver)
    }
}

/// An implementation of the `fuchsia.net.routes.{Rule}WatcherV{4,6}` protocols
/// for a single client.
#[derive(Debug)]
pub(crate) struct Watcher<E> {
    /// The `Existing` + `Idle` events for this client, capturing all of the
    /// routes that existed at the time it was instantiated.
    // NB: storing this as an `IntoIter` makes `watch` easier to implement.
    existing_events: std::vec::IntoIter<E>,
    /// The receiver of routing changes for this client.
    receiver: mpsc::Receiver<E>,
    /// The mechanism to observe that this client has been canceled.
    canceled: Event,
}

/// Expresses interest in certain events for each watcher client.
pub(crate) trait WatcherInterest<E>: Debug {
    /// Interested in the given event.
    fn has_interest_in(&self, event: &E) -> bool;
}

impl<I: Ip> WatcherInterest<fnet_routes_ext::Event<I>> for RouteTableInterest {
    fn has_interest_in(&self, event: &fnet_routes_ext::Event<I>) -> bool {
        self.has_interest_in(event)
    }
}

impl<E: WatcherEvent> Watcher<E> {
    /// Creates a new `Watcher` with the given existing resources.
    fn new_with_existing<R: Iterator<Item = E::Resource>, WI: WatcherInterest<E>>(
        installed: R,
        client_interest: WI,
    ) -> (Self, WatcherSink<E, WI>) {
        let (sender, receiver) = mpsc::channel(E::MAX_PENDING_EVENTS);
        let cancel = Event::new();
        (
            Watcher {
                existing_events: installed
                    .map(E::existing)
                    .filter(|event| client_interest.has_interest_in(event))
                    .chain(std::iter::once(E::IDLE))
                    .collect::<Vec<_>>()
                    .into_iter(),
                receiver,
                canceled: cancel.clone(),
            },
            WatcherSink { sink: sender, cancel, interest: client_interest },
        )
    }

    // Watch returns the currently available events (up to
    // [`fnet_routes::MAX_EVENTS`]). This call will block if there are no
    // available events.
    fn watch(&mut self) -> impl futures::Future<Output = Vec<E>> + Unpin + '_ {
        let Watcher { existing_events, receiver, canceled: _ } = self;
        futures::stream::iter(existing_events.by_ref())
            .chain(receiver)
            // Note: `ready_chunks` blocks until at least 1 event is ready.
            .ready_chunks(fnet_routes::MAX_EVENTS.into())
            .into_future()
            .map(|(r, _ready_chunks)| r.expect("underlying event stream unexpectedly ended"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_subnet_v4, net_subnet_v6};

    type RouteUpdateDispatcherInner<I> =
        UpdateDispatcherInner<fnet_routes_ext::Event<I>, RouteTableInterest>;
    type RouteUpdateNotifyError<I> = UpdateNotifyError<fnet_routes_ext::Event<I>>;

    fn arbitrary_route_on_interface<I: Ip>(interface: u64) -> fnet_routes_ext::InstalledRoute<I> {
        fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route {
                destination: I::map_ip(
                    (),
                    |()| net_subnet_v4!("192.168.0.0/24"),
                    |()| net_subnet_v6!("fe80::/64"),
                ),
                action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                    outbound_interface: interface,
                    next_hop: None,
                }),
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(0),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: 0 },
            table_id: 0,
        }
    }

    // Tests that `UpdateDispatcher` returns an error when it receives a
    // `Removed` update for a non-existent route.
    #[ip_test(I)]
    fn dispatcher_fails_to_remove_non_existent<I: Ip>() {
        let route = arbitrary_route_on_interface::<I>(1);
        assert_eq!(
            RouteUpdateDispatcherInner::default().notify(Update::Removed(route.clone())),
            Err(RouteUpdateNotifyError::NotFound(route))
        );
    }

    // Tests that `UpdateDispatcher` returns an error when it receives an
    // `Add` update for an already existing route.
    #[ip_test(I)]
    fn dispatcher_fails_to_add_existing<I: Ip>() {
        let mut dispatcher = RouteUpdateDispatcherInner::default();
        let route = arbitrary_route_on_interface::<I>(1);
        assert_eq!(dispatcher.notify(Update::Added(route)), Ok(()));
        assert_eq!(
            dispatcher.notify(Update::Added(route)),
            Err(RouteUpdateNotifyError::AlreadyExists(route))
        );
    }

    // Tests the basic functionality of the `UpdateDispatcher`,
    // `WatcherSink`, and `Watcher`.
    #[ip_test(I)]
    fn notify_dispatch_watch<I: Ip>() {
        let mut dispatcher = RouteUpdateDispatcherInner::default();

        // Add a new watcher and verify there are no existing routes.
        let mut watcher1 = dispatcher.connect_new_client(RouteTableInterest::All);
        assert_eq!(watcher1.watch().now_or_never().unwrap(), [fnet_routes_ext::Event::<I>::Idle]);

        // Add a route and verify that the watcher is notified.
        let route = arbitrary_route_on_interface(1);
        dispatcher.notify(Update::Added(route)).expect("failed to notify");
        assert_eq!(
            watcher1.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Added(route)]
        );

        // Connect a second watcher and verify it sees the route as `Existing`.
        let mut watcher2 = dispatcher.connect_new_client(RouteTableInterest::All);
        assert_eq!(
            watcher2.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Existing(route), fnet_routes_ext::Event::<I>::Idle]
        );

        // Remove the route and verify both watchers are notified.
        dispatcher.notify(Update::Removed(route)).expect("failed to notify");
        assert_eq!(
            watcher1.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Removed(route)]
        );
        assert_eq!(
            watcher2.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Removed(route)]
        );

        // Disconnect the first client, and verify the second client is still
        // able to be notified.
        dispatcher.disconnect_client(watcher1);
        dispatcher.notify(Update::Added(route)).expect("failed to notify");
        assert_eq!(
            watcher2.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Added(route)]
        );
    }

    // Tests that a `RouteWatcher` is canceled if it exceeds
    // `MAX_PENDING_EVENTS` in its queue.
    #[ip_test(I)]
    fn cancel_watcher_with_too_many_pending_events<I: Ip>() {
        // Helper function to drain the watcher of a specific number of events,
        // which may be spread across multiple batches of size
        // `fnet_routes::MAX_EVENTS`.
        fn drain_watcher<I: Ip>(
            watcher: &mut Watcher<fnet_routes_ext::Event<I>>,
            num_required_events: usize,
        ) {
            let mut num_observed_events = 0;
            while num_observed_events < num_required_events {
                num_observed_events += watcher.watch().now_or_never().unwrap().len()
            }
            assert_eq!(num_observed_events, num_required_events);
        }

        let mut dispatcher = RouteUpdateDispatcherInner::default();
        // `Existing` routes shouldn't count against the client's quota.
        // Exceed the quota, and then verify new clients can still connect.
        // Note that `EXCESS` is 2, because mpsc::channel implicitly adds +1 to
        // the buffer size for every connected sender (and the dispatcher holds
        // a sender).
        const EXCESS: usize = 2;
        const TOO_MANY_EVENTS: usize =
            <fnet_routes_ext::Event<Ipv4> as WatcherEvent>::MAX_PENDING_EVENTS + EXCESS;
        for i in 0..TOO_MANY_EVENTS {
            let route = arbitrary_route_on_interface::<I>(i.try_into().unwrap());
            dispatcher.notify(Update::Added(route)).expect("failed to notify");
        }
        let mut watcher1 = dispatcher.connect_new_client(RouteTableInterest::All);
        let mut watcher2 = dispatcher.connect_new_client(RouteTableInterest::All);
        assert_eq!(watcher1.canceled.wait().now_or_never(), None);
        assert_eq!(watcher2.canceled.wait().now_or_never(), None);
        // Drain all of the `Existing` events (and +1 for the `Idle` event).
        drain_watcher(&mut watcher1, TOO_MANY_EVENTS + 1);
        drain_watcher(&mut watcher2, TOO_MANY_EVENTS + 1);

        // Generate `TOO_MANY_EVENTS`, consuming the excess on `watcher1` but
        // not on `watcher2`; `watcher2` should be canceled.
        for i in 0..EXCESS {
            assert_eq!(watcher1.canceled.wait().now_or_never(), None);
            assert_eq!(watcher2.canceled.wait().now_or_never(), None);
            let route = arbitrary_route_on_interface::<I>(i.try_into().unwrap());
            dispatcher.notify(Update::Removed(route)).expect("failed to notify");
        }
        drain_watcher(&mut watcher1, EXCESS);
        for i in EXCESS..TOO_MANY_EVENTS {
            assert_eq!(watcher1.canceled.wait().now_or_never(), None);
            assert_eq!(watcher2.canceled.wait().now_or_never(), None);
            let route = arbitrary_route_on_interface::<I>(i.try_into().unwrap());
            dispatcher.notify(Update::Removed(route)).expect("failed to notify");
        }
        assert_eq!(watcher1.canceled.wait().now_or_never(), None);
        assert_eq!(watcher2.canceled.wait().now_or_never(), Some(()));
    }

    #[ip_test(I)]
    fn watcher_respects_interest<I: Ip>() {
        let mut dispatcher = RouteUpdateDispatcherInner::default();
        let main_table_id = routes::main_table_id::<I>();
        let other_table_id = main_table_id.next().expect("no next ID");
        let main_route = fnet_routes_ext::InstalledRoute {
            table_id: main_table_id.into(),
            ..arbitrary_route_on_interface::<I>(0)
        };
        dispatcher.notify(Update::Added(main_route)).expect("failed to notify");
        let other_route = fnet_routes_ext::InstalledRoute {
            table_id: other_table_id.into(),
            ..arbitrary_route_on_interface::<I>(0)
        };
        dispatcher.notify(Update::Added(other_route)).expect("failed to notify");
        let mut all_watcher = dispatcher.connect_new_client(RouteTableInterest::All);
        let mut main_watcher = dispatcher.connect_new_client(RouteTableInterest::Only {
            table_id: routes::main_table_id::<I>().into(),
        });
        let mut other_watcher = dispatcher
            .connect_new_client(RouteTableInterest::Only { table_id: other_table_id.into() });
        // They can get out of order because installed routes are stored in
        // HashSet.
        assert_matches!(
            &all_watcher.watch().now_or_never().unwrap()[..],
            [
                fnet_routes_ext::Event::Existing(installed_1),
                fnet_routes_ext::Event::Existing(installed_2),
                fnet_routes_ext::Event::<I>::Idle
            ] => {
                assert_eq!(
                    HashSet::<fnet_routes_ext::InstalledRoute<I>>::from_iter(
                        [*installed_1, *installed_2]
                    ), HashSet::from_iter([main_route, other_route]));
            }
        );
        assert_eq!(
            main_watcher.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Existing(main_route), fnet_routes_ext::Event::<I>::Idle]
        );
        assert_eq!(
            other_watcher.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Existing(other_route), fnet_routes_ext::Event::<I>::Idle]
        );
        dispatcher.notify(Update::Removed(main_route)).expect("failed to notify");
        dispatcher.notify(Update::Removed(other_route)).expect("failed to notify");
        // For a watcher interested in all tables, it should observe all route
        // changes in all tables.
        assert_eq!(
            all_watcher.watch().now_or_never().unwrap(),
            [
                fnet_routes_ext::Event::Removed(main_route),
                fnet_routes_ext::Event::Removed(other_route),
            ]
        );
        // For a watcher interested in only the main table, it should observe
        // route changes in only the main table.
        assert_eq!(
            main_watcher.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Removed(main_route)]
        );
        // For a watcher interested in only the other table, it should observe
        // route changes in only the other table.
        assert_eq!(
            other_watcher.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Removed(other_route)]
        );
    }
}
