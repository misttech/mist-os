// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{convert::Infallible as Never, pin::pin};

use anyhow::{Context as _, Error};
use assert_matches::assert_matches;
use derivative::Derivative;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_root as fnet_root;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use futures::{
    channel::{mpsc, oneshot},
    FutureExt as _, StreamExt as _,
};
use net_types::ip::{Ip, IpInvariant, Ipv4, Ipv6};

use crate::{
    client::ClientTable,
    interfaces,
    logging::{log_debug, log_info},
    messaging::Sender,
    netlink_packet::errno::Errno,
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
    routes,
    rules::{self, RuleRequestHandler as _},
};

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) enum UnifiedRequest<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    InterfacesRequest(interfaces::Request<S>),
    RoutesV4Request(routes::Request<S, Ipv4>),
    RoutesV6Request(routes::Request<S, Ipv6>),
    RuleRequest(rules::RuleRequest<S>, oneshot::Sender<Result<(), Errno>>),
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>, I: Ip> From<routes::Request<S, I>>
    for UnifiedRequest<S>
{
    fn from(request: routes::Request<S, I>) -> Self {
        let IpInvariant(request) = I::map_ip(
            request,
            |request| IpInvariant(UnifiedRequest::RoutesV4Request(request)),
            |request| IpInvariant(UnifiedRequest::RoutesV6Request(request)),
        );
        request
    }
}

pub(crate) enum UnifiedEvent {
    RoutesV4Event(fnet_routes_ext::Event<Ipv4>),
    RoutesV6Event(fnet_routes_ext::Event<Ipv6>),
    InterfacesEvent(fnet_interfaces::Event),
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) enum UnifiedPendingRequest<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    RoutesV4(crate::routes::PendingRouteRequest<S, Ipv4>),
    RoutesV6(crate::routes::PendingRouteRequest<S, Ipv6>),
    Interfaces(crate::interfaces::PendingRequest<S>),
}

/// Contains the asynchronous work related to routes and interfaces. Creates
/// routes and interface hanging get watchers and connects to route and
/// interface administration protocols in order to single-threadedly service
/// incoming `UnifiedRequest`s.
pub(crate) struct EventLoop<
    H,
    S: crate::messaging::Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
> {
    pub(crate) interfaces_proxy: fnet_root::InterfacesProxy,
    pub(crate) interfaces_state_proxy: fnet_interfaces::StateProxy,
    pub(crate) v4_routes_state: fnet_routes::StateV4Proxy,
    pub(crate) v6_routes_state: fnet_routes::StateV6Proxy,
    pub(crate) v4_routes_set_provider: fnet_routes_admin::RouteTableV4Proxy,
    pub(crate) v6_routes_set_provider: fnet_routes_admin::RouteTableV6Proxy,
    pub(crate) interfaces_handler: H,
    pub(crate) route_clients: ClientTable<NetlinkRoute, S>,
    pub(crate) unified_request_stream: mpsc::Receiver<UnifiedRequest<S>>,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum EventStreamEnded {
    #[error("routes v4 event stream ended")]
    RoutesV4,
    #[error("routes v6 event stream ended")]
    RoutesV6,
    #[error("interfaces event stream ended")]
    Interfaces,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum EventStreamError {
    #[error("error in routes v4 event stream: {0}")]
    RoutesV4(fnet_routes_ext::WatchError),
    #[error("error in routes v6 event stream: {0}")]
    RoutesV6(fnet_routes_ext::WatchError),
    #[error("error in interfaces event stream: {0}")]
    Interfaces(fidl::Error),
}

impl<
        H: interfaces::InterfacesHandler,
        S: crate::messaging::Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    > EventLoop<H, S>
{
    pub(crate) async fn run(self) -> Result<Never, Error> {
        let Self {
            interfaces_proxy,
            interfaces_state_proxy,
            v4_routes_state,
            v6_routes_state,
            v4_routes_set_provider,
            v6_routes_set_provider,
            interfaces_handler,
            route_clients,
            unified_request_stream,
        } = self;

        let (mut routes_v4_worker, v4_route_event_stream) =
            routes::RoutesWorkerState::<Ipv4>::create(&v4_routes_set_provider, &v4_routes_state)
                .await
                .context("create v4 routes worker")?;
        let (mut routes_v6_worker, v6_route_event_stream) =
            routes::RoutesWorkerState::<Ipv6>::create(&v6_routes_set_provider, &v6_routes_state)
                .await
                .context("create v6 routes worker")?;

        let (mut interfaces_worker, if_event_stream) = interfaces::InterfacesWorkerState::create(
            interfaces_handler,
            route_clients.clone(),
            interfaces_proxy.clone(),
            interfaces_state_proxy,
        )
        .await
        .context("create interfaces worker")?;

        let mut rule_table = rules::RuleTable::new_with_defaults();

        let mut unified_pending_request = None::<UnifiedPendingRequest<_>>;
        let mut unified_request_stream = unified_request_stream.chain(futures::stream::pending());

        let unified_event_stream = futures::stream::select(
            v4_route_event_stream
                .map(|res| {
                    res.map(UnifiedEvent::RoutesV4Event)
                        .map_err(|e| Error::new(EventStreamError::RoutesV4(e)))
                })
                .chain(futures::stream::once(futures::future::ready(Err(Error::new(
                    EventStreamEnded::RoutesV4,
                ))))),
            futures::stream::select(
                v6_route_event_stream
                    .map(|res| {
                        res.map(UnifiedEvent::RoutesV6Event)
                            .map_err(|e| Error::new(EventStreamError::RoutesV6(e)))
                    })
                    .chain(futures::stream::once(futures::future::ready(Err(Error::new(
                        EventStreamEnded::RoutesV6,
                    ))))),
                if_event_stream
                    .map(|res| {
                        res.map(UnifiedEvent::InterfacesEvent)
                            .map_err(|e| Error::new(EventStreamError::Interfaces(e)))
                    })
                    .chain(futures::stream::once(futures::future::ready(Err(Error::new(
                        EventStreamEnded::Interfaces,
                    ))))),
            ),
        )
        .fuse();
        let mut unified_event_stream = pin!(unified_event_stream);

        log_info!("routes and interfaces workers initialized, beginning execution");

        loop {
            let request_fut = match &unified_pending_request {
                None => unified_request_stream.next().left_future(),
                Some(pending_request) => {
                    log_debug!(
                        "not awaiting on request stream because of pending request: {:?}",
                        pending_request,
                    );
                    futures::future::pending().right_future()
                }
            }
            .fuse();
            let mut request_fut = pin!(request_fut);

            futures::select! {
                event = unified_event_stream.next() => {
                    match event.expect("event stream cannot end without error")? {
                        // The Routes worker needs access to the intended table id
                        // from the PendingRouteRequest.
                        UnifiedEvent::RoutesV4Event(event) => {
                            let pending_request_args = match unified_pending_request {
                                Some(UnifiedPendingRequest::RoutesV4(ref req)) => Some(req.args()),
                                _ => None,
                            };

                            routes_v4_worker
                            .handle_route_watcher_event(
                                &route_clients,
                                event,
                                pending_request_args.cloned())
                            .map_err(Error::new)
                            .context("handle v4 routes event")?
                        },
                        UnifiedEvent::RoutesV6Event(event) => {
                            let pending_request_args = match unified_pending_request {
                                Some(UnifiedPendingRequest::RoutesV6(ref req)) => Some(req.args()),
                                _ => None,
                            };

                            routes_v6_worker
                            .handle_route_watcher_event(
                                &route_clients,
                                event,
                                pending_request_args.cloned()
                            )
                            .map_err(Error::new)
                            .context("handle v6 routes event")?},
                        UnifiedEvent::InterfacesEvent(event) => interfaces_worker
                            .handle_interface_watcher_event(event).await
                            .map_err(Error::new)
                            .context("handle interfaces event")?,
                    }
                }
                request = request_fut => {
                    assert_matches!(
                        unified_pending_request,
                        None,
                        "should not already have pending request if handling a new request"
                    );

                    match request.expect("request stream cannot end") {
                        UnifiedRequest::InterfacesRequest(request) => {
                            let request = interfaces_worker
                                .handle_request(request).await;
                            unified_pending_request = request.map(UnifiedPendingRequest::Interfaces);
                        }
                        UnifiedRequest::RoutesV4Request(request) => {
                            let request = routes_v4_worker
                                .handle_request(&interfaces_proxy, &v4_routes_set_provider, request).await;
                            unified_pending_request = request.map(UnifiedPendingRequest::RoutesV4);
                        }
                        UnifiedRequest::RoutesV6Request(request) => {
                            let request = routes_v6_worker
                                .handle_request(&interfaces_proxy, &v6_routes_set_provider, request).await;
                            unified_pending_request = request.map(UnifiedPendingRequest::RoutesV6);
                        }
                        UnifiedRequest::RuleRequest(request, completer) => {
                            completer.send(rule_table.handle_request(request))
                                .expect("receiving end of completer should not be dropped");
                        }
                    }
                }
            }

            unified_pending_request = unified_pending_request.and_then(|pending| match pending {
                UnifiedPendingRequest::RoutesV4(pending_request) => routes_v4_worker
                    .handle_pending_request(pending_request)
                    .map(UnifiedPendingRequest::RoutesV4),
                UnifiedPendingRequest::RoutesV6(pending_request) => routes_v6_worker
                    .handle_pending_request(pending_request)
                    .map(UnifiedPendingRequest::RoutesV6),
                UnifiedPendingRequest::Interfaces(pending_request) => interfaces_worker
                    .handle_pending_request(pending_request)
                    .map(UnifiedPendingRequest::Interfaces),
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use fidl::endpoints::create_proxy;

    pub(crate) struct EventLoopServerEnd {
        pub(crate) interfaces: fidl::endpoints::ServerEnd<fnet_root::InterfacesMarker>,
        pub(crate) interfaces_state: fidl::endpoints::ServerEnd<fnet_interfaces::StateMarker>,
        pub(crate) v4_routes_state: fidl::endpoints::ServerEnd<fnet_routes::StateV4Marker>,
        pub(crate) v6_routes_state: fidl::endpoints::ServerEnd<fnet_routes::StateV6Marker>,
        pub(crate) v4_routes_set_provider:
            fidl::endpoints::ServerEnd<fnet_routes_admin::RouteTableV4Marker>,
        pub(crate) v6_routes_set_provider:
            fidl::endpoints::ServerEnd<fnet_routes_admin::RouteTableV6Marker>,
    }

    pub(crate) fn event_loop_fixture<
        H: interfaces::InterfacesHandler,
        S: crate::messaging::Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        interfaces_handler: H,
        route_clients: ClientTable<NetlinkRoute, S>,
        unified_request_stream: mpsc::Receiver<UnifiedRequest<S>>,
    ) -> (EventLoop<H, S>, EventLoopServerEnd) {
        let (interfaces_proxy, interfaces_server_end) =
            create_proxy::<fnet_root::InterfacesMarker>().unwrap();
        let (interfaces_state_proxy, interfaces_state_server_end) =
            create_proxy::<fnet_interfaces::StateMarker>().unwrap();
        let (v4_routes_state_proxy, v4_routes_state_server_end) =
            create_proxy::<fnet_routes::StateV4Marker>().unwrap();
        let (v6_routes_state_proxy, v6_routes_state_server_end) =
            create_proxy::<fnet_routes::StateV6Marker>().unwrap();
        let (v4_routes_set_provider_proxy, v4_routes_set_provider_server_end) =
            create_proxy::<fnet_routes_admin::RouteTableV4Marker>().unwrap();
        let (v6_routes_set_provider_proxy, v6_routes_set_provider_server_end) =
            create_proxy::<fnet_routes_admin::RouteTableV6Marker>().unwrap();

        (
            EventLoop {
                interfaces_proxy,
                interfaces_state_proxy,
                v4_routes_state: v4_routes_state_proxy,
                v6_routes_state: v6_routes_state_proxy,
                v4_routes_set_provider: v4_routes_set_provider_proxy,
                v6_routes_set_provider: v6_routes_set_provider_proxy,
                interfaces_handler,
                route_clients,
                unified_request_stream,
            },
            EventLoopServerEnd {
                interfaces: interfaces_server_end,
                interfaces_state: interfaces_state_server_end,
                v4_routes_state: v4_routes_state_server_end,
                v6_routes_state: v6_routes_state_server_end,
                v4_routes_set_provider: v4_routes_set_provider_server_end,
                v6_routes_set_provider: v6_routes_set_provider_server_end,
            },
        )
    }

    pub(crate) async fn serve_empty_routes<I: fnet_routes_ext::FidlRouteIpExt>(
        server_end: fidl::endpoints::ServerEnd<I::StateMarker>,
    ) {
        let stream = server_end.into_stream().expect("into stream");
        stream
            .for_each_concurrent(None, |item| async move {
                fnet_routes_ext::testutil::serve_state_request::<'_, I>(
                    item,
                    fnet_routes_ext::testutil::empty_watch_event_stream::<'_, I>(),
                )
                .await;
            })
            .await;
    }

    pub(crate) async fn serve_empty_interfaces_state(
        server_end: fidl::endpoints::ServerEnd<fnet_interfaces::StateMarker>,
    ) {
        let stream = server_end.into_stream().expect("into stream");
        stream
            .for_each_concurrent(None, |item| async move {
                let request = item.expect("interfaces state FIDL error");
                match request {
                    fnet_interfaces::StateRequest::GetWatcher {
                        options: _,
                        watcher,
                        control_handle: _,
                    } => {
                        serve_empty_interfaces_watcher(watcher).await;
                    }
                }
            })
            .await;
    }

    async fn serve_empty_interfaces_watcher(
        server_end: fidl::endpoints::ServerEnd<fnet_interfaces::WatcherMarker>,
    ) {
        let stream = server_end.into_stream().expect("into stream");
        stream
            .scan(false, |responded, item| {
                let responded = std::mem::replace(responded, true);
                let request = item.expect("interfaces watcher FIDL error");
                match request {
                    fnet_interfaces::WatcherRequest::Watch { responder } => {
                        if responded {
                            futures::future::pending().left_future()
                        } else {
                            responder
                                .send(&fnet_interfaces::Event::Idle(fnet_interfaces::Empty))
                                .expect("respond to interface watch");
                            futures::future::ready(Some(())).right_future()
                        }
                    }
                }
            })
            .for_each(|()| futures::future::ready(()))
            .await;
    }
}
