// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL Worker for the `fuchsia.net.routes` suite of protocols.

use std::collections::HashSet;

use async_utils::event::Event;
use either::Either;
use fidl::endpoints::{DiscoverableProtocolMarker as _, ProtocolMarker as _};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fuchsia_zircon::sys::ZX_ERR_ADDRESS_UNREACHABLE;
use futures::{
    channel::mpsc, lock::Mutex as AsyncMutex, FutureExt, StreamExt as _, TryStream,
    TryStreamExt as _,
};
use itertools::Itertools as _;
use net_types::{
    ethernet::Mac,
    ip::{GenericOverIp, Ip, IpAddr, IpAddress, IpInvariant, Ipv4, Ipv6},
    SpecifiedAddr,
};
use netstack3_core::{
    device::{DeviceId, EthernetDeviceId},
    ip::{
        device::nud::NeighborLinkAddr,
        types::{NextHop, ResolvedRoute},
    },
};
use thiserror::Error;
use tracing::{info, warn};

use crate::bindings::{
    util::{ConversionContext as _, IntoCore as _, IntoFidl as _},
    BindingsNonSyncCtxImpl, Ctx, Netstack, SyncCtx,
};

// The maximum number of events a client for the `fuchsia.net.routes/Watcher`
// is allowed to have queued. Clients will be dropped if they exceed this limit.
// Keep this a multiple of `fnet_routes::MAX_EVENTS` (5 is somewhat arbitrary)
// so that we don't artificially truncate the allowed batch size.
const MAX_PENDING_EVENTS: usize = (fnet_routes::MAX_EVENTS * 5) as usize;

/// Serve the `fuchsia.net.routes/State` protocol.
pub(crate) async fn serve_state(rs: fnet_routes::StateRequestStream, ns: Netstack) {
    rs.try_for_each_concurrent(None, |req| match req {
        fnet_routes::StateRequest::Resolve { destination, responder } => futures::future::ready(
            responder.send(&mut resolve(destination, ns.clone()).ok_or(ZX_ERR_ADDRESS_UNREACHABLE)),
        ),
    })
    .await
    .unwrap_or_else(|e| warn!("error serving {}: {:?}", fnet_routes::StateMarker::PROTOCOL_NAME, e))
}

/// Resolves the route to the given destination address.
///
/// Returns `None` if the destination can't be resolved.
fn resolve(destination: fnet::IpAddress, ns: Netstack) -> Option<fnet_routes::Resolved> {
    let addr: IpAddr = destination.into_core();
    match addr {
        IpAddr::V4(addr) => resolve_inner(addr, ns),
        IpAddr::V6(addr) => resolve_inner(addr, ns),
    }
}

/// The inner implementation of [`resolve`] that's generic over `Ip`.
fn resolve_inner<A: IpAddress>(destination: A, ns: Netstack) -> Option<fnet_routes::Resolved> {
    let mut ctx = ns.ctx.clone();
    let Ctx { sync_ctx, ref mut non_sync_ctx } = &mut ctx;
    let ResolvedRoute { device, src_addr, next_hop } =
        match netstack3_core::ip::resolve_route::<A::Version, _>(sync_ctx, destination) {
            Err(e) => {
                info!("Resolve failed for {}, {:?}", destination, e);
                return None;
            }
            Ok(resolved_route) => resolved_route,
        };
    let (next_hop_addr, next_hop_type) = match next_hop {
        NextHop::RemoteAsNeighbor => {
            (SpecifiedAddr::new(destination), Either::Left(fnet_routes::Resolved::Direct))
        }
        NextHop::Gateway(gateway) => (Some(gateway), Either::Right(fnet_routes::Resolved::Gateway)),
    };
    let remote_mac = match &device {
        DeviceId::Loopback(_device) => None,
        DeviceId::Ethernet(device) => {
            if let Some(addr) = next_hop_addr {
                resolve_ethernet_link_addr(sync_ctx, device, &addr)
            } else {
                warn!("Cannot attempt Ethernet link resolution for the unspecified address.");
                return None;
            }
        }
    };

    let destination = {
        let address =
            next_hop_addr.map_or(A::Version::UNSPECIFIED_ADDRESS, |a| *a).to_ip_addr().into_fidl();
        let source_address = src_addr.to_ip_addr().into_fidl();
        let mac = remote_mac.map(|mac| mac.into_fidl());
        let interface_id = non_sync_ctx.get_binding_id(device);
        fnet_routes::Destination {
            address: Some(address),
            mac,
            interface_id: Some(interface_id.get()),
            source_address: Some(source_address),
            ..Default::default()
        }
    };

    Some(either::for_both!(next_hop_type, f => f(destination)))
}

/// Performs link-layer resolution of the remote IP Address on the given device.
fn resolve_ethernet_link_addr<A: IpAddress>(
    sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
    device: &EthernetDeviceId<BindingsNonSyncCtxImpl>,
    remote: &SpecifiedAddr<A>,
) -> Option<Mac> {
    match netstack3_core::device::ethernet::resolve_ethernet_link_addr::<A::Version, _>(
        sync_ctx, device, remote,
    ) {
        NeighborLinkAddr::Resolved(mac) => Some(mac),
        NeighborLinkAddr::PendingNeighborResolution => {
            // TODO(https://fxbug.dev/120878): Properly wait for neighbor
            // resolution.
            warn!("Neighbor resolution unimplemented: https://fxbug.dev/120878. Omitting MAC.");
            None
        }
    }
}

/// Serve the `fuchsia.net.routes/StateV4` protocol.
pub(crate) async fn serve_state_v4(rs: fnet_routes::StateV4RequestStream, ns: Netstack) {
    rs.try_for_each_concurrent(None, |req| match req {
        fnet_routes::StateV4Request::GetWatcherV4 { options: _, watcher, control_handle: _ } => {
            serve_watcher::<Ipv4>(watcher, ns.clone()).map(|result| {
                Ok(result.unwrap_or_else(|e| {
                    warn!("error serving {}: {:?}", fnet_routes::WatcherV4Marker::DEBUG_NAME, e)
                }))
            })
        }
    })
    .await
    .unwrap_or_else(|e| {
        warn!("error serving {}: {:?}", fnet_routes::StateV4Marker::PROTOCOL_NAME, e)
    })
}

/// Serve the `fuchsia.net.routes/StateV6` protocol.
pub(crate) async fn serve_state_v6(rs: fnet_routes::StateV6RequestStream, ns: Netstack) {
    rs.try_for_each_concurrent(None, |req| match req {
        fnet_routes::StateV6Request::GetWatcherV6 { options: _, watcher, control_handle: _ } => {
            serve_watcher::<Ipv6>(watcher, ns.clone()).map(|result| {
                Ok(result.unwrap_or_else(|e| {
                    warn!("error serving {}: {:?}", fnet_routes::WatcherV6Marker::DEBUG_NAME, e)
                }))
            })
        }
    })
    .await
    .unwrap_or_else(|e| {
        warn!("error serving {}: {:?}", fnet_routes::StateV6Marker::PROTOCOL_NAME, e)
    })
}

#[derive(Debug, Error)]
enum ServeWatcherError {
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
async fn serve_watcher<I: fnet_routes_ext::FidlRouteIpExt>(
    server_end: fidl::endpoints::ServerEnd<I::WatcherMarker>,
    ns: Netstack,
) -> Result<(), ServeWatcherError> {
    let request_stream =
        server_end.into_stream().expect("failed to acquire request_stream from server_end");
    let watcher = {
        let mut ctx = ns.ctx.clone();
        let Ctx { sync_ctx: _, ref mut non_sync_ctx } = &mut ctx;
        let x = non_sync_ctx.route_update_dispatcher.lock().connect_new_client::<I>();
        x
    };

    let canceled_fut = watcher.canceled.wait();
    // NB: `watcher` needs to be an `AsyncMutex` so that it can be borrowed from
    // "all" futures in the following `try_for_each_concurrent`.
    let watcher = AsyncMutex::new(watcher);

    let result = {
        let watch_fut = request_stream
            .map_err(ServeWatcherError::ErrorInStream)
            .try_for_each_concurrent(None, |request| async {
                let mut watcher = watcher
                    .try_lock()
                    // If the watcher is already borrowed, then we're still
                    // handling a previous call to watch. This is erroneous
                    // usage by the client, and we're expected to hangup.
                    .ok_or(ServeWatcherError::PreviousPendingWatch)?;
                let result = respond_to_watch_request(request, watcher.watch().await)
                    .map_err(ServeWatcherError::FailedToRespond);
                result
            });

        futures::pin_mut!(canceled_fut, watch_fut);
        futures::select! {
            result = watch_fut => result,
            () = canceled_fut => Err(ServeWatcherError::Canceled),
        }
    };

    {
        let mut ctx = ns.ctx.clone();
        let Ctx { sync_ctx: _, ref mut non_sync_ctx } = &mut ctx;
        non_sync_ctx.route_update_dispatcher.lock().disconnect_client::<I>(watcher.into_inner());
    }

    result
}

// Responds to a single `Watch` request with the given batch of events.
fn respond_to_watch_request<I: fnet_routes_ext::FidlRouteIpExt>(
    req: <<I::WatcherMarker as fidl::endpoints::ProtocolMarker>::RequestStream as TryStream>::Ok,
    events: Vec<fnet_routes_ext::Event<I>>,
) -> Result<(), fidl::Error> {
    #[derive(GenericOverIp)]
    struct Inputs<I: Ip + fnet_routes_ext::FidlRouteIpExt> {
        req:
            <<I::WatcherMarker as fidl::endpoints::ProtocolMarker>::RequestStream as TryStream>::Ok,
        events: Vec<fnet_routes_ext::Event<I>>,
    }
    let IpInvariant(result) = I::map_ip::<Inputs<I>, _>(
        Inputs { req, events },
        |Inputs { req, events }| match req {
            fnet_routes::WatcherV4Request::Watch { responder } => {
                let events = events
                    .into_iter()
                    .map(|event| {
                        event.try_into().unwrap_or_else(|e| match e {
                            fnet_routes_ext::NetTypeConversionError::UnknownUnionVariant(msg) => {
                                panic!("tried to send an event with Unknown enum variant: {}", msg)
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                IpInvariant(responder.send(&events))
            }
        },
        |Inputs { req, events }| match req {
            fnet_routes::WatcherV6Request::Watch { responder } => {
                let events = events
                    .into_iter()
                    .map(|event| {
                        event.try_into().unwrap_or_else(|e| match e {
                            fnet_routes_ext::NetTypeConversionError::UnknownUnionVariant(msg) => {
                                panic!("tried to send an event with Unknown enum variant: {}", msg)
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                IpInvariant(responder.send(&events))
            }
        },
    );
    result
}

// An update to the routing table.
pub(crate) enum RoutingTableUpdate<I: Ip> {
    RouteAdded(fnet_routes_ext::InstalledRoute<I>),
    RouteRemoved(fnet_routes_ext::InstalledRoute<I>),
}

// Consumes updates to the system routing table and dispatches them to clients
// of the `fuchsia.net.routes/WatcherV{4,6}` protocols.
#[derive(Default)]
pub(crate) struct RouteUpdateDispatcher {
    inner_v4: RouteUpdateDispatcherInner<Ipv4>,
    inner_v6: RouteUpdateDispatcherInner<Ipv6>,
}

// The inner representation of a `RouteUpdateDispatcher` holding state for the
// given IP protocol.
#[derive(Default)]
struct RouteUpdateDispatcherInner<I: Ip> {
    // The set of currently installed routes.
    routes: HashSet<fnet_routes_ext::InstalledRoute<I>>,
    // The list of currently connected clients.
    clients: Vec<RoutesWatcherSink<I>>,
}

// The error type returned by `RouteUpdateDispatcher.notify()`.
#[derive(Debug, PartialEq)]
pub(crate) enum RouteUpdateNotifyError<I: Ip> {
    // `notify` was called with `RoutingTableUpdate::RouteAdded` for a route
    // that already exists.
    AlreadyExists(fnet_routes_ext::InstalledRoute<I>),
    // `notify` was called with `RoutingTableUpdate::RouteRemoved` for a route
    // that does not exist.
    NotFound(fnet_routes_ext::InstalledRoute<I>),
}

impl RouteUpdateDispatcher {
    // Returns the associated inner state for the given IP protocol.
    fn as_inner_mut<I: Ip>(&mut self) -> &mut RouteUpdateDispatcherInner<I> {
        #[derive(GenericOverIp)]
        struct Holder<'a, I: Ip>(&'a mut RouteUpdateDispatcherInner<I>);
        let RouteUpdateDispatcher { inner_v4, inner_v6 } = self;
        let Holder(inner) = I::map_ip(
            IpInvariant((inner_v4, inner_v6)),
            |IpInvariant((inner_v4, _inner_v6))| Holder(inner_v4),
            |IpInvariant((_inner_v4, inner_v6))| Holder(inner_v6),
        );
        inner
    }

    // Notify this `RouteUpdateDispatcher` of an update to the routing table.
    // The update will be dispatched to all active watcher clients.
    pub(crate) fn notify<I: Ip>(
        &mut self,
        update: RoutingTableUpdate<I>,
    ) -> Result<(), RouteUpdateNotifyError<I>> {
        let RouteUpdateDispatcherInner { routes, clients } = self.as_inner_mut::<I>();
        let event = match update {
            RoutingTableUpdate::RouteAdded(route) => {
                if routes.insert(route.clone()) {
                    fnet_routes_ext::Event::Added(route)
                } else {
                    return Err(RouteUpdateNotifyError::AlreadyExists(route));
                }
            }
            RoutingTableUpdate::RouteRemoved(route) => {
                if routes.remove(&route) {
                    fnet_routes_ext::Event::Removed(route)
                } else {
                    return Err(RouteUpdateNotifyError::NotFound(route));
                }
            }
        };
        for client in clients {
            client.send(event)
        }
        Ok(())
    }

    // Register a new client with this `RouteUpdateDispatcher`.
    fn connect_new_client<I: Ip>(&mut self) -> RoutesWatcher<I> {
        let RouteUpdateDispatcherInner { routes, clients } = self.as_inner_mut::<I>();
        let (watcher, sink) = RoutesWatcher::new_with_existing_routes(routes.iter().cloned());
        clients.push(sink);
        watcher
    }

    // Disconnect the given watcher from this `RouteUpdateDispatcher`.
    fn disconnect_client<I: Ip>(&mut self, watcher: RoutesWatcher<I>) {
        let RouteUpdateDispatcherInner { routes: _, clients } = self.as_inner_mut::<I>();
        let (idx, _): (usize, &RoutesWatcherSink<I>) = clients
            .iter()
            .enumerate()
            .filter(|(_idx, client)| client.is_connected_to(&watcher))
            .exactly_one()
            .expect("expected exactly one sink");
        let _: RoutesWatcherSink<I> = clients.swap_remove(idx);
    }
}

// Consumes events for a single client of the
// `fuchsia.net.routes/WatcherV{4,6}` protocols.
#[derive(Debug)]
struct RoutesWatcherSink<I: Ip> {
    // The sink with which to send routing changes to this client.
    sink: mpsc::Sender<fnet_routes_ext::Event<I>>,
    // The mechanism with which to cancel the client.
    cancel: Event,
}

impl<I: Ip> RoutesWatcherSink<I> {
    // Send this [`RoutesWatcherSink`] a new event.
    fn send(&mut self, event: fnet_routes_ext::Event<I>) {
        self.sink.try_send(event).unwrap_or_else(|e| {
            if e.is_full() {
                if self.cancel.signal() {
                    warn!(
                        "too many unconsumed events (the client may not be \
                        calling Watch frequently enough): {}",
                        MAX_PENDING_EVENTS
                    )
                }
            } else {
                panic!("unexpected error trying to send: {:?}", e)
            }
        })
    }

    // Returns `true` if this sink forwards events to the given watcher.
    fn is_connected_to(&self, watcher: &RoutesWatcher<I>) -> bool {
        self.sink.is_connected_to(&watcher.receiver)
    }
}

#[derive(Debug)]
// An implementation of the `fuchsia.net.routes.WatcherV{4,6}` protocols for
// a single client.
struct RoutesWatcher<I: Ip> {
    // The `Existing` + `Idle` events for this client, capturing all of the
    // routes that existed at the time it was instantiated.
    // NB: storing this as an `IntoIter` makes `watch` easier to implement.
    existing_events:
        <std::vec::Vec<fnet_routes_ext::Event<I>> as std::iter::IntoIterator>::IntoIter,
    // The receiver of routing changes for this client.
    receiver: mpsc::Receiver<fnet_routes_ext::Event<I>>,
    // The mechanism to observe that this client has been canceled.
    canceled: Event,
}

impl<I: Ip> RoutesWatcher<I> {
    // Creates a new `RoutesWatcher` with the given existing routes.
    fn new_with_existing_routes<R: Iterator<Item = fnet_routes_ext::InstalledRoute<I>>>(
        routes: R,
    ) -> (Self, RoutesWatcherSink<I>) {
        let (sender, receiver) = mpsc::channel(MAX_PENDING_EVENTS);
        let cancel = Event::new();
        (
            RoutesWatcher {
                existing_events: routes
                    .map(fnet_routes_ext::Event::Existing)
                    .chain(std::iter::once(fnet_routes_ext::Event::Idle))
                    .collect::<Vec<_>>()
                    .into_iter(),
                receiver: receiver,
                canceled: cancel.clone(),
            },
            RoutesWatcherSink { sink: sender, cancel },
        )
    }

    // Watch returns the currently available events (up to
    // [`fnet_routes::MAX_EVENTS`]). This call will block if there are no
    // available events.
    async fn watch(&mut self) -> Vec<fnet_routes_ext::Event<I>> {
        let RoutesWatcher { existing_events, receiver, canceled: _ } = self;
        futures::stream::iter(existing_events.by_ref())
            .chain(receiver)
            // Note: `ready_chunks` blocks until at least 1 event is ready.
            .ready_chunks(fnet_routes::MAX_EVENTS.into())
            .next()
            .await
            .expect("underlying event stream unexpectedly ended")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ip_test_macro::ip_test;
    use net_declare::{net_subnet_v4, net_subnet_v6};

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
        }
    }

    // Tests that `RouteUpdateDispatcher` returns an error when it receives a
    // `RemoveRoute` update for a non-existent route.
    #[ip_test]
    fn dispatcher_fails_to_remove_non_existent<I: Ip>() {
        let route = arbitrary_route_on_interface::<I>(1);
        assert_eq!(
            RouteUpdateDispatcher::default()
                .notify(RoutingTableUpdate::RouteRemoved(route.clone())),
            Err(RouteUpdateNotifyError::NotFound(route))
        );
    }

    // Tests that `RouteUpdateDispatcher` returns an error when it receives an
    // `AddRoute` update for an already existing route.
    #[ip_test]
    fn dispatcher_fails_to_add_existing<I: Ip>() {
        let mut dispatcher = RouteUpdateDispatcher::default();
        let route = arbitrary_route_on_interface::<I>(1);
        assert_eq!(dispatcher.notify(RoutingTableUpdate::RouteAdded(route)), Ok(()));
        assert_eq!(
            dispatcher.notify(RoutingTableUpdate::RouteAdded(route)),
            Err(RouteUpdateNotifyError::AlreadyExists(route))
        );
    }

    // Tests the basic functionality of the `RouteUpdateDispatcher`,
    // `RouteWatcherSink`, and `RouteWatcher`.
    #[ip_test]
    fn notify_dispatch_watch<I: Ip>() {
        let mut dispatcher = RouteUpdateDispatcher::default();

        // Add a new watcher and verify there are no existing routes.
        let mut watcher1 = dispatcher.connect_new_client::<I>();
        assert_eq!(watcher1.watch().now_or_never().unwrap(), [fnet_routes_ext::Event::<I>::Idle]);

        // Add a route and verify that the watcher is notified.
        let route = arbitrary_route_on_interface(1);
        dispatcher.notify(RoutingTableUpdate::RouteAdded(route)).expect("failed to notify");
        assert_eq!(
            watcher1.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Added(route)]
        );

        // Connect a second watcher and verify it sees the route as `Existing`.
        let mut watcher2 = dispatcher.connect_new_client::<I>();
        assert_eq!(
            watcher2.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Existing(route), fnet_routes_ext::Event::<I>::Idle]
        );

        // Remove the route and verify both watchers are notified.
        dispatcher.notify(RoutingTableUpdate::RouteRemoved(route)).expect("failed to notify");
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
        dispatcher.notify(RoutingTableUpdate::RouteAdded(route)).expect("failed to notify");
        assert_eq!(
            watcher2.watch().now_or_never().unwrap(),
            [fnet_routes_ext::Event::Added(route)]
        );
    }

    // Tests that a `RouteWatcher` is canceled if it exceeds
    // `MAX_PENDING_EVENTS` in its queue.
    #[ip_test]
    fn cancel_watcher_with_too_many_pending_events<I: Ip>() {
        // Helper function to drain the watcher of a specific number of events,
        // which may be spread across multiple batches of size
        // `fnet_routes::MAX_EVENTS`.
        fn drain_watcher<I: Ip>(watcher: &mut RoutesWatcher<I>, num_required_events: usize) {
            let mut num_observed_events = 0;
            while num_observed_events < num_required_events {
                num_observed_events += watcher.watch().now_or_never().unwrap().len()
            }
            assert_eq!(num_observed_events, num_required_events);
        }

        let mut dispatcher = RouteUpdateDispatcher::default();
        // `Existing` routes shouldn't count against the client's quota.
        // Exceed the quota, and then verify new clients can still connect.
        // Note that `EXCESS` is 2, because mpsc::channel implicitly adds +1 to
        // the buffer size for every connected sender (and the dispatcher holds
        // a sender).
        const EXCESS: usize = 2;
        const TOO_MANY_EVENTS: usize = MAX_PENDING_EVENTS + EXCESS;
        for i in 0..TOO_MANY_EVENTS {
            let route = arbitrary_route_on_interface::<I>(i.try_into().unwrap());
            dispatcher.notify(RoutingTableUpdate::RouteAdded(route)).expect("failed to notify");
        }
        let mut watcher1 = dispatcher.connect_new_client::<I>();
        let mut watcher2 = dispatcher.connect_new_client::<I>();
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
            dispatcher.notify(RoutingTableUpdate::RouteRemoved(route)).expect("failed to notify");
        }
        drain_watcher(&mut watcher1, EXCESS);
        for i in EXCESS..TOO_MANY_EVENTS {
            assert_eq!(watcher1.canceled.wait().now_or_never(), None);
            assert_eq!(watcher2.canceled.wait().now_or_never(), None);
            let route = arbitrary_route_on_interface::<I>(i.try_into().unwrap());
            dispatcher.notify(RoutingTableUpdate::RouteRemoved(route)).expect("failed to notify");
        }
        assert_eq!(watcher1.canceled.wait().now_or_never(), None);
        assert_eq!(watcher2.canceled.wait().now_or_never(), Some(()));
    }
}
