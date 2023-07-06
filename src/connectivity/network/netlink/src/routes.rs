// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_ROUTE information by receiving RTM_ROUTE
//! Netlink messages and maintaining route table state from Netstack.

use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
};

use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_interfaces_admin::{
    self as fnet_interfaces_admin, ProofOfInterfaceAuthorization,
};
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_root as fnet_root;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin::RouteSetError;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;

use futures::{
    channel::{mpsc, oneshot},
    pin_mut, StreamExt as _, TryStreamExt as _,
};

use net_types::ip::{GenericOverIp, Ip, IpAddress, IpInvariant, IpVersion, Subnet};
use netlink_packet_core::{NetlinkMessage, NLM_F_MULTIPART};
use netlink_packet_route::{
    RouteHeader, RouteMessage, RtnlMessage, AF_INET, AF_INET6, RTNLGRP_IPV4_ROUTE,
    RTNLGRP_IPV6_ROUTE, RTN_UNICAST, RTPROT_UNSPEC, RT_SCOPE_UNIVERSE, RT_TABLE_MAIN,
};
use netlink_packet_utils::nla::Nla;

use crate::{
    client::{ClientTable, InternalClient},
    errors::EventLoopError,
    logging::{log_debug, log_error, log_warn},
    messaging::Sender,
    multicast_groups::ModernGroup,
    netlink_packet::{errno::Errno, UNSPECIFIED_SEQUENCE_NUMBER},
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
};

/// Arguments for an RTM_GETROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum GetRouteArgs {
    Dump,
}

/// Arguments for an RTM_NEWROUTE unicast route.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnicastRouteArgs<I: Ip> {
    // The network and prefix of the route.
    subnet: Subnet<I::Addr>,
    // The forwarding action. Unicast routes are gateway/direct routes and must
    // have a target.
    target: fnet_routes_ext::RouteTarget<I>,
    // The metric used to weigh the importance of the route.
    priority: u32,
}

/// Arguments for an RTM_NEWROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewRouteArgs<I: Ip> {
    #[allow(unused)]
    /// Direct or gateway routes.
    Unicast(UnicastRouteArgs<I>),
}

/// [`Request`] arguments associated with routes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteRequestArgs<I: Ip> {
    /// RTM_GETROUTE
    Get(GetRouteArgs),
    #[allow(unused)]
    // TODO(issuetracker.google.com/283136222): Use this message type to handle
    // `RTM_NEWROUTE` requests from clients.
    /// RTM_NEWROUTE
    New(NewRouteArgs<I>),
}

/// The argument(s) for a [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestArgs<I: Ip> {
    Route(RouteRequestArgs<I>),
}

/// An error encountered while handling a [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    /// The route already exists in the route set.
    AlreadyExists,
    /// Invalid destination subnet or next-hop.
    InvalidRequest,
    /// Interface present in request that was not recognized by Netstack.
    UnrecognizedInterface,
    /// Unspecified error.
    Unknown,
}

impl RequestError {
    #[allow(unused)]
    pub(crate) fn into_errno(self) -> Errno {
        match self {
            RequestError::AlreadyExists => Errno::EEXIST,
            RequestError::InvalidRequest => Errno::EINVAL,
            RequestError::Unknown => Errno::ENOTSUP,
            RequestError::UnrecognizedInterface => Errno::ENODEV,
        }
    }
}

fn map_route_set_error<I: Ip + fnet_routes_ext::FidlRouteIpExt>(
    e: RouteSetError,
    route: I::Route,
    interface_id: u64,
) -> RequestError {
    match e {
        RouteSetError::Unauthenticated => {
            // Authenticated with Netstack for this interface, but
            // the route set claims the interface did
            // not authenticate.
            panic!(
                "authenticated for interface {:?}, but received unauthentication error from route set for route ({:?})",
                interface_id,
                route,
            );
        }
        RouteSetError::InvalidDestinationSubnet => {
            // Subnet had an incorrect prefix length or host bits were set.
            log_debug!(
                "invalid subnet observed from route ({:?}) from interface {:?}",
                route,
                interface_id,
            );
            return RequestError::InvalidRequest;
        }
        RouteSetError::InvalidNextHop => {
            // Non-unicast next-hop found in request.
            log_debug!(
                "invalid next hop observed from route ({:?}) from interface {:?}",
                route,
                interface_id,
            );
            return RequestError::InvalidRequest;
        }
        err => {
            // `RouteSetError` is a flexible FIDL enum so we cannot
            // exhaustively match.
            //
            // We don't know what the error is but we know that the route
            // set was unmodified as a result of the operation.
            log_error!(
                "unrecognized route set error {:?} with route ({:?}) from interface {:?}",
                err,
                route,
                interface_id
            );
            return RequestError::Unknown;
        }
    }
}

/// A request associated with routes.
#[derive(Debug)]
pub(crate) struct Request<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>, I: Ip> {
    /// The resource and operation-specific argument(s) for this request.
    pub args: RequestArgs<I>,
    /// The request's sequence number.
    ///
    /// This value will be copied verbatim into any message sent as a result of
    /// this request.
    pub sequence_number: u32,
    /// The client that made the request.
    pub client: InternalClient<NetlinkRoute, S>,
    /// A completer that will have the result of the request sent over.
    pub completer: oneshot::Sender<Result<(), RequestError>>,
}

/// Contains the asynchronous work related to RTM_ROUTE messages.
///
/// Connects to the route watcher and can respond to RTM_ROUTE
/// message requests.
pub(crate) struct EventLoop<
    S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    /// TODO(issuetracker.google.com/289517742): Share the Control handles
    /// between Interfaces and Routes.
    /// An `InterfacesProxy` to authenticate the `RouteSet` to manage
    /// routes on interfaces.
    interfaces_proxy: fnet_root::InterfacesProxy,
    /// A 'StateProxy` to connect to the routes watcher.
    state_proxy: <I::StateMarker as ProtocolMarker>::Proxy,
    /// TODO(issuetracker.google.com/289582515): Create a new `RouteSet`
    /// when a request with a new table is received.
    /// A `RouteSetProxy` to provide isolated administrative access
    /// to this worker's `RouteSet`.
    route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,
    /// The current set of clients of NETLINK_ROUTE protocol family.
    route_clients: ClientTable<NetlinkRoute, S>,
    /// A stream of [`Request`]s for the event loop to handle.
    request_stream: mpsc::Receiver<Request<S, I>>,
}

/// FIDL errors from the routes worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoutesFidlError {
    /// Error connecting to marker with `connect_to_protocol`.
    #[error("connecting to marker: {0}")]
    ProtocolConnection(anyhow::Error),
    /// Error while creating new isolated managed route set.
    #[error("creating new route set: {0}")]
    RouteSetCreation(fnet_routes_ext::admin::RouteSetCreationError),
    /// Error while getting route event stream from state.
    #[error("watcher creation: {0}")]
    WatcherCreation(fnet_routes_ext::WatcherCreationError),
    /// Error while route watcher stream.
    #[error("watch: {0}")]
    Watch(fnet_routes_ext::WatchError),
}

/// Netstack errors from the routes worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoutesNetstackError<I: Ip> {
    /// Event stream ended unexpectedly.
    #[error("event stream ended")]
    EventStreamEnded,
    /// Unexpected route was received from routes watcher.
    #[error("unexpected route: {0:?}")]
    UnexpectedRoute(fnet_routes_ext::InstalledRoute<I>),
    /// Unexpected event was received from routes watcher.
    #[error("unexpected event: {0:?}")]
    UnexpectedEvent(fnet_routes_ext::Event<I>),
}

impl<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    > EventLoop<S, I>
{
    /// `new` returns an `EventLoop` instance.
    pub(crate) fn new(
        route_clients: ClientTable<NetlinkRoute, S>,
        request_stream: mpsc::Receiver<Request<S, I>>,
    ) -> Result<Self, EventLoopError<RoutesFidlError, RoutesNetstackError<I>>> {
        use fuchsia_component::client::connect_to_protocol;
        let interfaces_proxy = connect_to_protocol::<fnet_root::InterfacesMarker>()
            .map_err(|e| EventLoopError::Fidl(RoutesFidlError::ProtocolConnection(e)))?;
        let state_proxy = connect_to_protocol::<I::StateMarker>()
            .map_err(|e| EventLoopError::Fidl(RoutesFidlError::ProtocolConnection(e)))?;
        let set_provider_proxy = connect_to_protocol::<I::SetProviderMarker>()
            .map_err(|e| EventLoopError::Fidl(RoutesFidlError::ProtocolConnection(e)))?;

        let route_set_proxy = match fnet_routes_ext::admin::new_route_set::<I>(&set_provider_proxy)
        {
            Ok(proxy) => proxy,
            Err(e) => return Err(EventLoopError::Fidl(RoutesFidlError::RouteSetCreation(e))),
        };

        Ok(EventLoop {
            interfaces_proxy,
            state_proxy,
            route_set_proxy,
            route_clients,
            request_stream,
        })
    }

    /// Run the asynchronous work related to RTM_ROUTE messages.
    ///
    /// The event loop can track Ipv4 or Ipv6 routes, and is
    /// never expected to complete.
    /// Returns: `EventLoopError` that requires restarting the event
    /// loop task, for example, if the watcher stream ends or if the
    /// FIDL protocol cannot be connected.
    pub(crate) async fn run(self) -> EventLoopError<RoutesFidlError, RoutesNetstackError<I>> {
        let EventLoop {
            interfaces_proxy,
            state_proxy,
            route_set_proxy,
            route_clients,
            request_stream,
        } = self;

        let route_event_stream = {
            let stream_res = fnet_routes_ext::event_stream_from_state(&state_proxy)
                .map_err(|e| EventLoopError::Fidl(RoutesFidlError::WatcherCreation(e)));

            match stream_res {
                Ok(stream) => stream.fuse(),
                Err(e) => return e,
            }
        };
        pin_mut!(route_event_stream);

        let routes_res = fnet_routes_ext::collect_routes_until_idle::<_, HashSet<_>>(
            route_event_stream.by_ref(),
        )
        .await;

        let routes = match routes_res {
            Ok(routes) => routes,
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::ErrorInStream(e)) => {
                return EventLoopError::Fidl(RoutesFidlError::Watch(e));
            }
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::StreamEnded) => {
                return EventLoopError::Netstack(RoutesNetstackError::EventStreamEnded);
            }
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::UnexpectedEvent(event)) => {
                return EventLoopError::Netstack(RoutesNetstackError::UnexpectedEvent(event));
            }
        };

        let mut route_messages = new_set_with_existing_routes(routes);

        // Chain a pending so that the stream never ends. This is so that tests
        // can safely rely on just closing the watcher to terminate the event
        // loop. This is okay because we do not expect the request stream to
        // reasonably end and if we did want to support graceful shutdown of the
        // event loop, we can have a dedicated shutdown signal.
        let mut request_stream = request_stream.chain(futures::stream::pending());

        loop {
            futures::select! {
                stream_res = route_event_stream.try_next() => {
                    let event = match stream_res {
                        Ok(Some(event)) => event,
                        Ok(None) => return EventLoopError::Netstack(
                            RoutesNetstackError::EventStreamEnded
                        ),
                        Err(e) => {
                            return EventLoopError::Fidl(RoutesFidlError::Watch(e));
                        }
                    };

                    match handle_route_watcher_event(&mut route_messages, &route_clients, event) {
                        Ok(()) => {}
                        // These errors are severe enough to indicate a larger problem in Netstack.
                        Err(RouteEventHandlerError::AlreadyExistingRouteAddition(route))
                        | Err(RouteEventHandlerError::NonExistentRouteDeletion(route)) => {
                            return EventLoopError::Netstack(
                                RoutesNetstackError::UnexpectedRoute(route)
                            );
                        }
                        Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(event)) => {
                            return EventLoopError::Netstack(
                                RoutesNetstackError::UnexpectedEvent(event)
                            );
                        }
                    }
                }
                req = request_stream.next() => {
                    Self::handle_request(
                        &interfaces_proxy,
                        &route_set_proxy,
                        &route_messages,
                        req.expect(
                            "request stream should never end because of chained `pending`",
                        )
                    ).await
                }
            }
        }
    }

    // TODO(issuetracker.google.com/289518265): Once `authenticate_for_interface` call is
    // forwarded to `Control`, update to use `fnet_interfaces_ext::admin::Control`.
    fn get_interface_control(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        interface_id: u64,
    ) -> fnet_interfaces_admin::ControlProxy {
        let (control, server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
                .expect("create Control proxy");
        interfaces_proxy.get_admin(interface_id, server_end).expect("send get admin request");
        control
    }

    async fn authenticate_for_interface(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as fidl::endpoints::ProtocolMarker>::Proxy,
        interface_id: u64,
    ) -> Result<(), RequestError> {
        let control = Self::get_interface_control(interfaces_proxy, interface_id);

        let grant = match control.get_authorization_for_interface().await {
            Ok(grant) => grant,
            Err(fidl::Error::ClientChannelClosed { status, protocol_name }) => {
                log_debug!(
                    "{}: netstack dropped the {} channel, interface {} does not exist",
                    status,
                    protocol_name,
                    interface_id
                );
                return Err(RequestError::UnrecognizedInterface);
            }
            Err(e) => panic!("unexpected error from interface authorization request: {e:?}"),
        };
        let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);

        #[derive(GenericOverIp)]
        struct AuthorizeInputs<'a, I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt> {
            route_set_proxy: &'a <I::RouteSetMarker as fidl::endpoints::ProtocolMarker>::Proxy,
            proof: ProofOfInterfaceAuthorization,
        }

        let IpInvariant(authorize_fut) = I::map_ip(
            AuthorizeInputs::<'_, I> { route_set_proxy, proof },
            |AuthorizeInputs { route_set_proxy, proof }| {
                IpInvariant(route_set_proxy.authenticate_for_interface(proof))
            },
            |AuthorizeInputs { route_set_proxy, proof }| {
                IpInvariant(route_set_proxy.authenticate_for_interface(proof))
            },
        );

        authorize_fut.await.expect("sent authorization request").map_err(|e| {
            log_warn!("error authenticating for interface ({interface_id}): {e:?}");
            RequestError::UnrecognizedInterface
        })?;

        Ok(())
    }

    async fn handle_new_route_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
        new_route_args: NewRouteArgs<I>,
    ) -> Result<(), RequestError> {
        let interface_id = match new_route_args {
            NewRouteArgs::Unicast(args) => args.target.outbound_interface,
        };
        let route: I::Route = {
            let route: fnet_routes_ext::Route<I> = new_route_args.into();
            route.try_into().expect("route should be converted")
        };

        // Attempt to add a route without authenticating for the interface
        // first, in case the interface has already been authenticated.
        // With this approach, it is not necessary to maintain state of which
        // interfaces are authenticated.
        match fnet_routes_ext::admin::add_route::<I>(&route_set_proxy, &route)
            .await
            .expect("sent add route request")
        {
            Ok(true) => return Ok(()),
            // TODO(issuetracker.google.com/289518732): Get the key (dest, metric)
            // and check if a route with the metric currently exists in the observed
            // set. If it exists, return `AlreadyExists` early without a call to
            // `add_route`.
            // When `add_route` has an `Ok(false)` response, this indicates that the
            // route already exists, which should manifest as a hard error in Linux.
            Ok(false) => return Err(RequestError::AlreadyExists),
            Err(RouteSetError::Unauthenticated) => {}
            Err(e) => {
                log_warn!("error adding route to interface ({interface_id}): {e:?}");
                return Err(map_route_set_error::<I>(e, route, interface_id));
            }
        };

        // Authenticate for the interface if we failed to add
        // a route and received the `Unauthenticated` error.
        Self::authenticate_for_interface(&interfaces_proxy, &route_set_proxy, interface_id).await?;

        match fnet_routes_ext::admin::add_route::<I>(&route_set_proxy, &route)
            .await
            .expect("sent add route request")
        {
            Ok(true) => return Ok(()),
            Ok(false) => return Err(RequestError::AlreadyExists),
            // Only try to add a route once more after authenticating. All errors
            // are treated as hard errors after the second add route attempt.
            Err(e) => {
                log_warn!("error adding route to interface ({interface_id}): {e:?}");
                return Err(map_route_set_error::<I>(e, route, interface_id));
            }
        };
    }

    async fn handle_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
        route_messages: &HashSet<NetlinkRouteMessage>,
        Request { args, sequence_number, mut client, completer }: Request<S, I>,
    ) {
        log_debug!("handling request {args:?} from {client}");

        let result = match args {
            RequestArgs::Route(args) => match args {
                RouteRequestArgs::Get(args) => match args {
                    GetRouteArgs::Dump => {
                        route_messages.clone().into_iter().for_each(|message| {
                            client.send_unicast(message.into_rtnl_new_route(sequence_number, true))
                        });
                        Ok(())
                    }
                },
                RouteRequestArgs::New(args) => {
                    Self::handle_new_route_request(interfaces_proxy, route_set_proxy, args).await
                }
            },
        };

        log_debug!("handled request {args:?} from {client} with result = {result:?}");

        match completer.send(result) {
            Ok(()) => (),
            Err(result) => {
                // Not treated as a hard error because the socket may have been
                // closed.
                log_warn!(
                    "failed to send result ({:?}) to {} after handling request {:?}",
                    result,
                    client,
                    args
                )
            }
        }
    }
}

// Errors related to handling route events.
#[derive(Debug, PartialEq)]
enum RouteEventHandlerError<I: Ip> {
    // Route watcher event handler attempted to add a route that already existed.
    AlreadyExistingRouteAddition(fnet_routes_ext::InstalledRoute<I>),
    // Route watcher event handler attempted to remove a route that does not exist.
    NonExistentRouteDeletion(fnet_routes_ext::InstalledRoute<I>),
    // Route watcher event handler attempted to process a route event that was not add or remove.
    NonAddOrRemoveEventReceived(fnet_routes_ext::Event<I>),
}

/// Handles events observed by the route watchers by adding/removing routes
/// from the underlying `NetlinkRouteMessage` set.
///
/// Returns a `RoutesEventLoopError` when unexpected events or HashSet issues occur.
fn handle_route_watcher_event<I: Ip, S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>(
    route_messages: &mut HashSet<NetlinkRouteMessage>,
    route_clients: &ClientTable<NetlinkRoute, S>,
    event: fnet_routes_ext::Event<I>,
) -> Result<(), RouteEventHandlerError<I>> {
    let message_for_clients = match event {
        fnet_routes_ext::Event::Added(route) => {
            if let Some(route_message) = NetlinkRouteMessage::optionally_from(route) {
                if !route_messages.insert(route_message.clone()) {
                    return Err(RouteEventHandlerError::AlreadyExistingRouteAddition(route));
                }
                Some(route_message.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false))
            } else {
                None
            }
        }
        fnet_routes_ext::Event::Removed(route) => {
            if let Some(route_message) = NetlinkRouteMessage::optionally_from(route) {
                if !route_messages.remove(&route_message) {
                    return Err(RouteEventHandlerError::NonExistentRouteDeletion(route));
                }
                Some(route_message.into_rtnl_del_route())
            } else {
                None
            }
        }
        // We don't expect to observe any existing events, because the route watchers were drained
        // of existing events prior to starting the event loop.
        fnet_routes_ext::Event::Existing(_)
        | fnet_routes_ext::Event::Idle
        | fnet_routes_ext::Event::Unknown => {
            return Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(event));
        }
    };
    if let Some(message_for_clients) = message_for_clients {
        let route_group = match I::VERSION {
            IpVersion::V4 => ModernGroup(RTNLGRP_IPV4_ROUTE),
            IpVersion::V6 => ModernGroup(RTNLGRP_IPV6_ROUTE),
        };
        route_clients.send_message_to_group(message_for_clients, route_group);
    }

    Ok(())
}

/// A wrapper type for the netlink_packet_route `RouteMessage` to enable conversions
/// from [`fnet_routes_ext::InstalledRoute`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetlinkRouteMessage(RouteMessage);

// Constructs a new set of `NetlinkRouteMessage` from an
// `InstalledRoute` HashSet.
fn new_set_with_existing_routes<I: Ip>(
    routes: HashSet<fnet_routes_ext::InstalledRoute<I>>,
) -> HashSet<NetlinkRouteMessage> {
    return routes
        .iter()
        .filter_map(|route| NetlinkRouteMessage::optionally_from(*route))
        .collect::<HashSet<_>>();
}

impl NetlinkRouteMessage {
    /// Implement optional conversions from `InstalledRoute` to `NetlinkRouteMessage`.
    /// `Ok` becomes `Some`, while `Err` is logged and becomes `None`.
    fn optionally_from<I: Ip>(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) -> Option<NetlinkRouteMessage> {
        match route.try_into() {
            Ok(route) => Some(route),
            Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding) => {
                log_warn!("Unexpected non-forwarding route in routing table: {:?}", route);
                None
            }
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(id)) => {
                log_warn!("Invalid interface id found in routing table route: {:?}", id);
                None
            }
        }
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::NewRoute`].
    pub(crate) fn into_rtnl_new_route(
        self,
        sequence_number: u32,
        is_dump: bool,
    ) -> NetlinkMessage<RtnlMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::NewRoute(message).into();
        msg.header.sequence_number = sequence_number;
        if is_dump {
            msg.header.flags |= NLM_F_MULTIPART;
        }
        msg.finalize();
        msg
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::DelRoute`].
    fn into_rtnl_del_route(self) -> NetlinkMessage<RtnlMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::DelRoute(message).into();
        msg.finalize();
        msg
    }
}

impl Hash for NetlinkRouteMessage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let NetlinkRouteMessage(message) = self;
        message.header.hash(state);
        message.nlas.iter().for_each(|nla| {
            let mut buffer = vec![0u8; nla.value_len() as usize];
            nla.emit_value(&mut buffer);
            buffer.hash(state);
        });
    }
}

// NetlinkRouteMessage conversion related errors.
#[derive(Debug, PartialEq)]
pub(crate) enum NetlinkRouteMessageConversionError {
    // Route with non-forward action received from Netstack.
    RouteActionNotForwarding,
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

// Implement conversions from `InstalledRoute` to `NetlinkRouteMessage`
// which is fallible iff, the route's action is not `Forward`.
impl<I: Ip> TryFrom<fnet_routes_ext::InstalledRoute<I>> for NetlinkRouteMessage {
    type Error = NetlinkRouteMessageConversionError;
    fn try_from(
        fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route { destination, action, properties: _ },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        }: fnet_routes_ext::InstalledRoute<I>,
    ) -> Result<Self, Self::Error> {
        let fnet_routes_ext::RouteTarget { outbound_interface, next_hop } = match action {
            fnet_routes_ext::RouteAction::Unknown => {
                return Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding)
            }
            fnet_routes_ext::RouteAction::Forward(target) => target,
        };

        let mut route_header = RouteHeader::default();
        // Both possible constants are in the range of u8-accepted values, so they can be
        // safely casted to a u8.
        route_header.address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        }
        .try_into()
        .expect("should fit into u8");
        route_header.destination_prefix_length = destination.prefix();

        // The following fields are used in the header, but they do not have any
        // corresponding values in `InstalledRoute`. The fields explicitly
        // defined below  are expected to be needed at some point, but the
        // information is not currently provided by the watcher.
        //
        // length of source prefix
        // tos filter (type of service)
        route_header.table = RT_TABLE_MAIN;
        route_header.protocol = RTPROT_UNSPEC;
        // Universe for routes with next_hop. Valid as long as route action
        // is forwarding.
        route_header.scope = RT_SCOPE_UNIVERSE;
        route_header.kind = RTN_UNICAST;

        // The NLA order follows the list that attributes are listed on the
        // rtnetlink man page.
        // The following fields are used in the options in the NLA, but they
        // do not have any corresponding values in `InstalledRoute`.
        //
        // RTA_SRC (route source address)
        // RTA_IIF (input interface index)
        // RTA_PREFSRC (preferred source address)
        // RTA_METRICS (route statistics)
        // RTA_MULTIPATH
        // RTA_FLOW
        // RTA_CACHEINFO
        // RTA_MARK
        // RTA_MFC_STATS
        // RTA_VIA
        // RTA_NEWDST
        // RTA_PREF
        // RTA_ENCAP_TYPE
        // RTA_ENCAP
        // RTA_EXPIRES (can set to 'forever' if it is required)
        let mut nlas = vec![];

        // A prefix length of 0 indicates it is the default route. Specifying
        // destination NLA does not provide useful information.
        if route_header.destination_prefix_length > 0 {
            let destination_nla = netlink_packet_route::route::Nla::Destination(
                destination.network().bytes().to_vec(),
            );
            nlas.push(destination_nla);
        }

        // We expect interface ids to safely fit in the range of u32 values.
        let outbound_id: u32 = match outbound_interface.try_into() {
            Err(std::num::TryFromIntError { .. }) => {
                return Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(
                    outbound_interface,
                ))
            }
            Ok(id) => id,
        };
        let oif_nla = netlink_packet_route::route::Nla::Oif(outbound_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = next_hop.bytes().iter().cloned().collect();
            let gateway_nla = netlink_packet_route::route::Nla::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = netlink_packet_route::route::Nla::Priority(metric);
        nlas.push(priority_nla);

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.nlas = nlas;
        Ok(NetlinkRouteMessage(route_message))
    }
}

// Implement conversions from [`NewRouteArgs<I>`] to
// [`fnet_routes_ext::Route<I>`].
impl<I: Ip> From<NewRouteArgs<I>> for fnet_routes_ext::Route<I> {
    fn from(new_route_args: NewRouteArgs<I>) -> Self {
        match new_route_args {
            NewRouteArgs::Unicast(args) => {
                let UnicastRouteArgs { subnet, target, priority } = args;
                fnet_routes_ext::Route {
                    destination: subnet,
                    action: fnet_routes_ext::RouteAction::Forward(target),
                    properties: fnet_routes_ext::RouteProperties {
                        specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                            metric: fnet_routes::SpecifiedMetric::ExplicitMetric(priority),
                        },
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::HashSet;

    use fidl::endpoints::{ControlHandle, RequestStream};
    use fidl_fuchsia_net_routes as fnet_routes;
    use fidl_fuchsia_net_routes_admin as fnet_routes_admin;

    use assert_matches::assert_matches;
    use futures::{
        future::{Future, FutureExt as _},
        sink::SinkExt as _,
        Stream,
    };
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{GenericOverIp, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet},
        SpecifiedAddr,
    };
    use netlink_packet_core::NetlinkPayload;
    use netlink_packet_route::RTNLGRP_LINK;
    use test_case::test_case;

    use crate::messaging::testutil::{FakeSender, SentMessage};

    const TEST_V4_SUBNET: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/24");
    const TEST_V4_NEXTHOP: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const TEST_V4_NEXTHOP2: Ipv4Addr = net_ip_v4!("192.0.2.2");
    const TEST_V6_SUBNET: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::0/32");
    const TEST_V6_NEXTHOP: Ipv6Addr = net_ip_v6!("2001:db8::1");
    const TEST_V6_NEXTHOP2: Ipv6Addr = net_ip_v6!("2001:db8::2");

    const INTERFACE_ID1: u32 = 1;
    const INTERFACE_ID2: u32 = 2;
    const LOWER_METRIC: u32 = 0;
    const HIGHER_METRIC: u32 = 100;
    const TEST_SEQUENCE_NUMBER: u32 = 1234;

    fn create_installed_route<I: Ip>(
        subnet: Subnet<I::Addr>,
        next_hop: I::Addr,
        interface_id: u64,
        metric: u32,
    ) -> fnet_routes_ext::InstalledRoute<I> {
        fnet_routes_ext::InstalledRoute::<I> {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget::<I> {
                    outbound_interface: interface_id,
                    next_hop: SpecifiedAddr::new(next_hop),
                }),
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(metric),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        }
    }

    fn create_netlink_route_message(
        address_family: u8,
        destination_prefix_length: u8,
        nlas: Vec<netlink_packet_route::route::Nla>,
    ) -> NetlinkRouteMessage {
        let mut route_header = RouteHeader::default();
        route_header.address_family = address_family;
        route_header.destination_prefix_length = destination_prefix_length;
        route_header.kind = RTN_UNICAST;
        route_header.table = RT_TABLE_MAIN;

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.nlas = nlas;

        NetlinkRouteMessage(route_message)
    }

    fn create_nlas<I: Ip>(
        destination: Option<Subnet<I::Addr>>,
        next_hop: Option<I::Addr>,
        outgoing_interface_id: u32,
        metric: u32,
    ) -> Vec<netlink_packet_route::route::Nla> {
        let mut nlas = vec![];

        if let Some(destination) = destination {
            let destination_nla = netlink_packet_route::route::Nla::Destination(
                destination.network().bytes().to_vec(),
            );
            nlas.push(destination_nla);
        }

        let oif_nla = netlink_packet_route::route::Nla::Oif(outgoing_interface_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = next_hop.bytes().iter().cloned().collect();
            let gateway_nla = netlink_packet_route::route::Nla::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = netlink_packet_route::route::Nla::Priority(metric);
        nlas.push(priority_nla);
        nlas
    }

    #[fuchsia::test]
    fn test_handle_route_watcher_event_v4() {
        handle_route_watcher_event_helper::<Ipv4>(TEST_V4_SUBNET, TEST_V4_NEXTHOP);
    }

    #[fuchsia::test]
    fn test_handle_route_watcher_event_v6() {
        handle_route_watcher_event_helper::<Ipv6>(TEST_V6_SUBNET, TEST_V6_NEXTHOP);
    }

    fn handle_route_watcher_event_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, INTERFACE_ID1.into(), LOWER_METRIC);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, INTERFACE_ID2.into(), HIGHER_METRIC);

        let add_event1 = fnet_routes_ext::Event::Added(installed_route1);
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);
        let unknown_event: fnet_routes_ext::Event<I> = fnet_routes_ext::Event::Unknown;

        let mut route_messages: HashSet<NetlinkRouteMessage> = HashSet::new();
        let expected_route_message1: NetlinkRouteMessage = installed_route1.try_into().unwrap();
        let expected_route_message2: NetlinkRouteMessage = installed_route2.try_into().unwrap();

        // Setup two fake clients: one is a member of the route multicast group.
        let (right_group, wrong_group) = match I::VERSION {
            IpVersion::V4 => (ModernGroup(RTNLGRP_IPV4_ROUTE), ModernGroup(RTNLGRP_IPV6_ROUTE)),
            IpVersion::V6 => (ModernGroup(RTNLGRP_IPV6_ROUTE), ModernGroup(RTNLGRP_IPV4_ROUTE)),
        };
        let (mut right_sink, right_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[right_group],
        );
        let (mut wrong_sink, wrong_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[wrong_group],
        );
        let route_clients: ClientTable<NetlinkRoute, FakeSender<_>> = ClientTable::default();
        route_clients.add_client(right_client);
        route_clients.add_client(wrong_client);

        // An event that is not an add or remove should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, unknown_event),
            Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(_))
        );
        assert_eq!(route_messages.len(), 0);
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event1),
            Ok(())
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message1.clone()]));
        assert_eq!(
            &right_sink.take_messages()[..],
            &[SentMessage::multicast(
                expected_route_message1
                    .clone()
                    .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                right_group
            )]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding the same route again should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event1),
            Err(RouteEventHandlerError::AlreadyExistingRouteAddition(_))
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message1.clone()]));
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding a different route should result in an addition.
        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event2),
            Ok(())
        );
        assert_eq!(
            route_messages,
            HashSet::from_iter([expected_route_message1.clone(), expected_route_message2.clone()])
        );
        assert_eq!(
            &right_sink.take_messages()[..],
            &[SentMessage::multicast(
                expected_route_message2
                    .clone()
                    .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                right_group
            )]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, remove_event),
            Ok(())
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message2.clone()]));
        assert_eq!(
            &right_sink.take_messages()[..],
            &[SentMessage::multicast(
                expected_route_message1.clone().into_rtnl_del_route(),
                right_group
            )]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Removing a route that doesn't exist should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, remove_event),
            Err(RouteEventHandlerError::NonExistentRouteDeletion(_))
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message2.clone()]));
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }

    #[test_case(TEST_V4_SUBNET, TEST_V4_NEXTHOP)]
    #[test_case(TEST_V6_SUBNET, TEST_V6_NEXTHOP)]
    #[test_case(net_subnet_v4!("0.0.0.0/0"), net_ip_v4!("0.0.0.1"))]
    #[test_case(net_subnet_v6!("::/0"), net_ip_v6!("::1"))]
    fn test_netlink_route_message_try_from_installed_route<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: A,
    ) {
        netlink_route_message_conversion_helper::<A::Version>(subnet, next_hop);
    }

    fn netlink_route_message_conversion_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let installed_route =
            create_installed_route::<I>(subnet, next_hop, INTERFACE_ID1.into(), LOWER_METRIC);
        let prefix_length = subnet.prefix();
        let subnet = if prefix_length > 0 { Some(subnet) } else { None };
        let nlas = create_nlas::<I>(subnet, Some(next_hop), INTERFACE_ID1, LOWER_METRIC);
        let address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        }
        .try_into()
        .expect("should fit into u8");
        let expected = create_netlink_route_message(address_family, prefix_length, nlas);

        let actual: NetlinkRouteMessage = installed_route.try_into().unwrap();
        assert_eq!(actual, expected);
    }

    #[test_case(TEST_V4_SUBNET)]
    #[test_case(TEST_V6_SUBNET)]
    fn test_non_forward_route_conversion<A: IpAddress>(subnet: Subnet<A>) {
        let installed_route = fnet_routes_ext::InstalledRoute::<A::Version> {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Unknown,
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(LOWER_METRIC),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                metric: LOWER_METRIC,
            },
        };

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            installed_route.try_into();
        assert_eq!(actual, Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding));
    }

    #[fuchsia::test]
    fn test_oversized_interface_id_route_conversion() {
        let invalid_interface_id = (u32::MAX as u64) + 1;
        let installed_route: fnet_routes_ext::InstalledRoute<Ipv4> = create_installed_route(
            TEST_V4_SUBNET,
            TEST_V4_NEXTHOP,
            invalid_interface_id,
            Default::default(),
        );

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            installed_route.try_into();
        assert_eq!(
            actual,
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );
    }

    #[test]
    fn test_into_rtnl_new_route_is_serializable() {
        let route = create_netlink_route_message(0, 0, vec![]);
        let new_route_message = route.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false);
        let mut buf = vec![0; new_route_message.buffer_len()];
        // Serialize will panic if `new_route_message` is malformed.
        new_route_message.serialize(&mut buf);
    }

    #[test]
    fn test_into_rtnl_del_route_is_serializable() {
        let route = create_netlink_route_message(0, 0, vec![]);
        let del_route_message = route.into_rtnl_del_route();
        let mut buf = vec![0; del_route_message.buffer_len()];
        // Serialize will panic if `del_route_message` is malformed.
        del_route_message.serialize(&mut buf);
    }

    #[test_case(TEST_V4_SUBNET, TEST_V4_NEXTHOP)]
    #[test_case(TEST_V6_SUBNET, TEST_V6_NEXTHOP)]
    fn test_new_set_with_existing_routes<A: IpAddress>(subnet: Subnet<A>, next_hop: A) {
        new_set_with_existing_routes_helper::<A::Version>(subnet, next_hop);
    }

    fn new_set_with_existing_routes_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let interface_id = u32::MAX;

        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, interface_id as u64, LOWER_METRIC);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, (interface_id as u64) + 1, HIGHER_METRIC);
        let routes: HashSet<fnet_routes_ext::InstalledRoute<I>> =
            vec![installed_route1, installed_route2].into_iter().collect::<_>();

        // One `InstalledRoute` has an invalid interface id, so it should be removed in
        // the conversion to the `NetlinkRouteMessage` HashSet.
        let actual = new_set_with_existing_routes::<I>(routes);
        assert_eq!(actual.len(), 1);

        let nlas = create_nlas::<I>(Some(subnet), Some(next_hop), interface_id, LOWER_METRIC);
        let address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        }
        .try_into()
        .expect("should fit into u8");
        let netlink_route_message =
            create_netlink_route_message(address_family, subnet.prefix(), nlas);
        let expected: HashSet<NetlinkRouteMessage> =
            vec![netlink_route_message].into_iter().collect::<_>();
        assert_eq!(actual, expected);
    }

    struct Setup<
        W,
        R,
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    > {
        pub event_loop: EventLoop<FakeSender<RtnlMessage>, I>,
        pub watcher_stream: W,
        pub route_set_stream: R,
        pub interfaces_request_stream: fnet_root::InterfacesRequestStream,
        pub request_sink: mpsc::Sender<Request<FakeSender<RtnlMessage>, I>>,
    }

    fn setup_with_route_clients<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RtnlMessage>>,
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<Item = <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        I,
    > {
        let (state_proxy, state_stream) =
            fidl::endpoints::create_proxy_and_stream::<I::StateMarker>().unwrap();
        let (interfaces_proxy, interfaces_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_root::InterfacesMarker>().unwrap();
        let (route_set_proxy, route_set_stream) =
            fidl::endpoints::create_proxy_and_stream::<I::RouteSetMarker>().unwrap();
        let (request_sink, request_stream) = mpsc::channel(1);
        let event_loop = EventLoop::<FakeSender<_>, I> {
            interfaces_proxy,
            state_proxy,
            route_set_proxy,
            route_clients,
            request_stream,
        };

        #[derive(GenericOverIp)]
        struct StateRequestWrapper<I: Ip + fnet_routes_ext::FidlRouteIpExt> {
            request: <<I::StateMarker as ProtocolMarker>::RequestStream as futures::TryStream>::Ok,
        }

        #[derive(GenericOverIp)]
        struct WatcherRequestWrapper<I: Ip + fnet_routes_ext::FidlRouteIpExt> {
            watcher: <I::WatcherMarker as ProtocolMarker>::RequestStream,
        }

        let watcher_stream = state_stream
            .and_then(|request| {
                let wrapper = I::map_ip(
                    StateRequestWrapper { request },
                    |StateRequestWrapper { request }| match request {
                        fnet_routes::StateV4Request::GetWatcherV4 {
                            options: _,
                            watcher,
                            control_handle: _,
                        } => WatcherRequestWrapper { watcher: watcher.into_stream().unwrap() },
                    },
                    |StateRequestWrapper { request }| match request {
                        fnet_routes::StateV6Request::GetWatcherV6 {
                            options: _,
                            watcher,
                            control_handle: _,
                        } => WatcherRequestWrapper { watcher: watcher.into_stream().unwrap() },
                    },
                );
                futures::future::ok(wrapper)
            })
            .map(|res| res.expect("watcher stream error"))
            .map(|WatcherRequestWrapper { watcher }| watcher)
            // For testing, we only expect there to be a single connection to the watcher, so the
            // stream is condensed into a single `WatchRequest` stream.
            .flatten();

        Setup {
            event_loop,
            watcher_stream,
            route_set_stream,
            interfaces_request_stream,
            request_sink,
        }
    }

    fn setup<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>(
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<Item = <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        I,
    > {
        setup_with_route_clients::<I>(ClientTable::default())
    }

    async fn respond_to_watcher<
        I: fnet_routes_ext::FidlRouteIpExt,
        S: Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
    >(
        stream: S,
        updates: impl IntoIterator<Item = I::WatchEvent>,
    ) {
        #[derive(GenericOverIp)]
        struct HandleInputs<I: Ip + fnet_routes_ext::FidlRouteIpExt> {
            request: <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            update: I::WatchEvent,
        }
        stream
            .zip(futures::stream::iter(updates.into_iter()))
            .for_each(|(request, update)| async move {
                I::map_ip::<_, ()>(
                    HandleInputs { request, update },
                    |HandleInputs { request, update }| match request
                        .expect("failed to receive `Watch` request")
                    {
                        fnet_routes::WatcherV4Request::Watch { responder } => {
                            responder.send(&[update]).expect("failed to respond to `Watch`")
                        }
                    },
                    |HandleInputs { request, update }| match request
                        .expect("failed to receive `Watch` request")
                    {
                        fnet_routes::WatcherV6Request::Watch { responder } => {
                            responder.send(&[update]).expect("failed to respond to `Watch`")
                        }
                    },
                );
            })
            .await;
    }

    async fn respond_to_watcher_with_routes<
        I: fnet_routes_ext::FidlRouteIpExt,
        S: Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
    >(
        stream: S,
        existing_routes: impl IntoIterator<Item = fnet_routes_ext::InstalledRoute<I>>,
        new_event: Option<fnet_routes_ext::Event<I>>,
    ) {
        let events = existing_routes
            .into_iter()
            .map(|route| fnet_routes_ext::Event::<I>::Existing(route))
            .chain(std::iter::once(fnet_routes_ext::Event::<I>::Idle))
            .chain(new_event)
            .map(|event| event.try_into().unwrap());

        respond_to_watcher::<I, _>(stream, events).await;
    }

    #[test_case(TEST_V4_SUBNET, TEST_V4_NEXTHOP)]
    #[test_case(TEST_V6_SUBNET, TEST_V6_NEXTHOP)]
    #[fuchsia::test]
    async fn test_event_loop_event_errors<A: IpAddress>(subnet: Subnet<A>, next_hop: A)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let route = create_installed_route(subnet, next_hop, INTERFACE_ID1.into(), LOWER_METRIC);

        event_loop_errors_stream_ended_helper::<A::Version>(route).await;
        event_loop_errors_existing_after_add_helper::<A::Version>(route).await;
        event_loop_errors_duplicate_adds_helper::<A::Version>(route).await;
    }

    async fn event_loop_errors_stream_ended_helper<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) {
        let Setup {
            event_loop,
            watcher_stream,
            route_set_stream: _,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = event_loop.run();
        let watcher_fut = respond_to_watcher_with_routes(watcher_stream, [route], None);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            EventLoopError::Fidl(RoutesFidlError::Watch(fnet_routes_ext::WatchError::Fidl(
                fidl::Error::ClientChannelClosed { .. }
            )))
        );
    }

    async fn event_loop_errors_existing_after_add_helper<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) {
        let Setup {
            event_loop,
            watcher_stream,
            route_set_stream: _,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = event_loop.run();
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Existing(route.clone());
        let watcher_fut =
            respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event));

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            EventLoopError::Netstack(
                RoutesNetstackError::UnexpectedEvent(
                    fnet_routes_ext::Event::Existing(res)
                )
            ) => {
                assert_eq!(res, route)
            }
        );
    }

    async fn event_loop_errors_duplicate_adds_helper<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) {
        let Setup {
            event_loop,
            watcher_stream,
            route_set_stream: _,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = event_loop.run();
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Added(route.clone());
        let watcher_fut =
            respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event));

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            EventLoopError::Netstack(RoutesNetstackError::UnexpectedRoute(res)) => {
                assert_eq!(res, route)
            }
        );
    }

    fn get_test_route_events<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
    ) -> impl IntoIterator<Item = <A::Version as fnet_routes_ext::FidlRouteIpExt>::WatchEvent>
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt,
    {
        vec![
            fnet_routes_ext::Event::<A::Version>::Existing(create_installed_route(
                subnet,
                next_hop1,
                INTERFACE_ID1.into(),
                LOWER_METRIC,
            ))
            .try_into()
            .unwrap(),
            fnet_routes_ext::Event::<A::Version>::Existing(create_installed_route(
                subnet,
                next_hop2,
                INTERFACE_ID2.into(),
                HIGHER_METRIC,
            ))
            .try_into()
            .unwrap(),
            fnet_routes_ext::Event::<A::Version>::Idle.try_into().unwrap(),
        ]
    }

    fn create_unicast_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: A,
        interface_id: u64,
    ) -> UnicastRouteArgs<A::Version> {
        UnicastRouteArgs {
            subnet,
            target: fnet_routes_ext::RouteTarget {
                outbound_interface: interface_id,
                next_hop: SpecifiedAddr::new(next_hop),
            },
            priority: Default::default(),
        }
    }

    #[derive(Debug, PartialEq)]
    struct TestRequestResult {
        messages: Vec<SentMessage<RtnlMessage>>,
        waiter_result: Result<(), RequestError>,
    }

    /// Test helper to handle a request.
    ///
    /// `root_handler` returns a future that handles
    /// `fnet_root::InterfacesRequest`s.
    async fn test_request<
        A: IpAddress,
        Fut: Future<Output = ()>,
        F: FnOnce(fnet_root::InterfacesRequestStream) -> Fut,
    >(
        args: RequestArgs<A::Version>,
        root_handler: F,
        route_set_results: impl ExactSizeIterator<Item = RouteSetResult>,
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
    ) -> TestRequestResult
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (mut route_sink, route_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[ModernGroup(match A::Version::VERSION {
                IpVersion::V4 => RTNLGRP_IPV4_ROUTE,
                IpVersion::V6 => RTNLGRP_IPV6_ROUTE,
            })],
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[ModernGroup(RTNLGRP_LINK)],
        );
        let Setup {
            event_loop,
            mut watcher_stream,
            mut route_set_stream,
            interfaces_request_stream,
            mut request_sink,
        } = setup_with_route_clients::<A::Version>({
            let route_clients = ClientTable::default();
            route_clients.add_client(route_client.clone());
            route_clients.add_client(other_client);
            route_clients
        });
        let event_loop_fut = event_loop.run().fuse();
        futures::pin_mut!(event_loop_fut);

        let watcher_stream_fut = respond_to_watcher::<A::Version, _>(
            watcher_stream.by_ref(),
            get_test_route_events(subnet, next_hop1, next_hop2),
        );
        futures::select! {
            () = watcher_stream_fut.fuse() => {},
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }
        assert_eq!(&route_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

        let (completer, waiter) = oneshot::channel();
        let fut = request_sink
            .send(Request {
                args,
                sequence_number: TEST_SEQUENCE_NUMBER,
                client: route_client.clone(),
                completer,
            })
            .then(|res| {
                res.expect("send request");
                waiter
            });

        let route_set_fut = respond_to_route_set_modifications::<A::Version, _, _>(
            route_set_stream.by_ref(),
            watcher_stream.by_ref(),
            route_set_results,
        )
        .fuse();

        let root_interfaces_fut = root_handler(interfaces_request_stream).fuse();

        let waiter_result = futures::select! {
            res = fut.fuse() => res.unwrap(),
            res = futures::future::join3(route_set_fut, root_interfaces_fut, event_loop_fut) => {
                unreachable!("eventloop/stream handlers should not return: {res:?}")
            }
        };

        assert_eq!(&other_sink.take_messages()[..], &[]);
        TestRequestResult { messages: route_sink.take_messages(), waiter_result }
    }

    #[test_case(TEST_V4_SUBNET, TEST_V4_NEXTHOP, TEST_V4_NEXTHOP2; "v4_route_dump")]
    #[test_case(TEST_V6_SUBNET, TEST_V6_NEXTHOP, TEST_V6_NEXTHOP2; "v6_route_dump")]
    #[fuchsia::test]
    async fn test_get_route<A: IpAddress>(subnet: Subnet<A>, next_hop1: A, next_hop2: A)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let address_family: u8 = match A::Version::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        }
        .try_into()
        .expect("should fit into u8");
        pretty_assertions::assert_eq!(
            {
                let mut test_request_result = test_request(
                    RequestArgs::Route(RouteRequestArgs::Get(GetRouteArgs::Dump)),
                    |interfaces_request_stream| async {
                        interfaces_request_stream
                            .for_each(|req| async move {
                                panic!("unexpected InterfacesRequest: {req:?}")
                            })
                            .await;
                    },
                    std::iter::empty::<RouteSetResult>(),
                    subnet,
                    next_hop1,
                    next_hop2,
                )
                .await;
                test_request_result.messages.sort_by_key(|message| {
                    assert_matches!(
                        &message.message.payload,
                        NetlinkPayload::InnerMessage(RtnlMessage::NewRoute(m)) => {
                            // We expect there to be exactly one Oif NLA present
                            // for the given inputs.
                            m.nlas.clone().into_iter().filter_map(|nla|
                                match nla {
                                    netlink_packet_route::route::Nla::Oif(interface_id) =>
                                        Some((m.header.address_family, interface_id)),
                                    netlink_packet_route::route::Nla::Destination(_)
                                    | netlink_packet_route::route::Nla::Gateway(_)
                                    | netlink_packet_route::route::Nla::Priority(_) => None,
                                    _ => panic!("unexpected NLA {nla:?} present in payload"),
                                }
                            ).next()
                        }
                    )
                });
                test_request_result
            },
            TestRequestResult {
                messages: vec![
                    SentMessage::unicast(
                        create_netlink_route_message(
                            address_family,
                            subnet.prefix(),
                            create_nlas::<A::Version>(
                                Some(subnet),
                                Some(next_hop1),
                                INTERFACE_ID1,
                                LOWER_METRIC,
                            ),
                        )
                        .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
                    ),
                    SentMessage::unicast(
                        create_netlink_route_message(
                            address_family,
                            subnet.prefix(),
                            create_nlas::<A::Version>(
                                Some(subnet),
                                Some(next_hop2),
                                INTERFACE_ID2,
                                HIGHER_METRIC,
                            ),
                        )
                        .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
                    ),
                ],
                waiter_result: Ok(()),
            },
        )
    }

    #[derive(Debug)]
    enum RouteSetResult {
        AddResult(Result<bool, fnet_routes_admin::RouteSetError>),
        #[allow(unused)]
        // TODO(issuetracker.google.com/283136222): Handle `RTM_DELROUTE`
        // requests from clients.
        DelResult(Result<bool, fnet_routes_admin::RouteSetError>),
        AuthenticationResult(Result<(), fnet_routes_admin::AuthenticateForInterfaceError>),
    }

    // Handle RouteSet API requests then feed the returned
    // `fuchsia.net.routes.ext/Event`s to the routes watcher.
    async fn respond_to_route_set_modifications<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
        RS: Stream<Item = <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        WS: Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>
            + std::marker::Unpin,
    >(
        route_stream: RS,
        mut watcher_stream: WS,
        route_set_results: impl ExactSizeIterator<Item = RouteSetResult>,
    ) {
        #[derive(GenericOverIp)]
        struct RouteSetInputs<I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt> {
            request: <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            route_set_result: RouteSetResult,
        }
        #[derive(GenericOverIp)]
        struct RouteSetOutputs<I: Ip + fnet_routes_ext::FidlRouteIpExt> {
            event: Option<I::WatchEvent>,
        }

        let route_set_results_len = route_set_results.len();
        let counter = RefCell::new(0);
        route_stream
            .zip(futures::stream::iter(route_set_results))
            // Chain a pending so that the sink in the `forward` call below remains open and can be
            // used each time there is an item in the Stream.
            .chain(futures::stream::pending())
            .map(|(request, route_set_result)| {
                let RouteSetOutputs { event } = I::map_ip(
                    RouteSetInputs { request, route_set_result },
                    |RouteSetInputs { request, route_set_result }| match request
                        .expect("failed to receive request")
                    {
                        fnet_routes_admin::RouteSetV4Request::AddRoute { route, responder } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::AddResult(res) => res
                            );
                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `AddRoute`");
                            RouteSetOutputs {
                                event: match route_set_result {
                                    Ok(true) => Some(
                                        fnet_routes_ext::Event::Added(
                                            fnet_routes_ext::InstalledRoute {
                                                route: route.try_into().unwrap(),
                                                effective_properties:
                                                    fnet_routes_ext::EffectiveRouteProperties {
                                                        metric: Default::default(),
                                                    },
                                            },
                                        )
                                        .try_into()
                                        .unwrap(),
                                    ),
                                    _ => None,
                                },
                            }
                        }
                        fnet_routes_admin::RouteSetV4Request::RemoveRoute {
                            route: _,
                            responder,
                        } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::DelResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `RemoveRoute`");
                            RouteSetOutputs { event: None }
                        }
                        fnet_routes_admin::RouteSetV4Request::AuthenticateForInterface {
                            credential: _,
                            responder,
                        } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::AuthenticationResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `AuthenticateForInterface`");
                            RouteSetOutputs { event: None }
                        }
                    },
                    |RouteSetInputs { request, route_set_result }| match request
                        .expect("failed to receive request")
                    {
                        fnet_routes_admin::RouteSetV6Request::AddRoute { route, responder } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::AddResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `AddRoute`");
                            RouteSetOutputs {
                                event: match route_set_result {
                                    Ok(true) => Some(
                                        fnet_routes_ext::Event::Added(
                                            fnet_routes_ext::InstalledRoute {
                                                route: route.try_into().unwrap(),
                                                effective_properties:
                                                    fnet_routes_ext::EffectiveRouteProperties {
                                                        metric: Default::default(),
                                                    },
                                            },
                                        )
                                        .try_into()
                                        .unwrap(),
                                    ),
                                    _ => None,
                                },
                            }
                        }
                        fnet_routes_admin::RouteSetV6Request::RemoveRoute {
                            route: _,
                            responder,
                        } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::DelResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `RemoveRoute`");
                            RouteSetOutputs { event: None }
                        }
                        fnet_routes_admin::RouteSetV6Request::AuthenticateForInterface {
                            credential: _,
                            responder,
                        } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::AuthenticationResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `AuthenticateForInterface`");
                            RouteSetOutputs { event: None }
                        }
                    },
                );
                event
            })
            .map(Ok)
            .forward(futures::sink::unfold(watcher_stream.by_ref(), |st, events| async {
                // Increment the RefCell value for each batch of events handled.
                *counter.borrow_mut() += 1;
                respond_to_watcher::<I, _>(st.by_ref(), events).await;
                Ok::<_, std::convert::Infallible>(st)
            }))
            .await
            .unwrap();

        // Ensure the number of results that were provided are all sent to the routes watcher sink.
        assert_eq!(route_set_results_len, counter.into_inner());
    }

    /// A test helper to exercise a single route request.
    ///
    /// A test helper that calls the provided callback with a
    /// [`fnet_interfaces_admin::ControlRequest`] as they arrive.
    async fn test_route_request<
        A: IpAddress,
        Fut: Future<Output = ()>,
        F: FnMut(fnet_interfaces_admin::ControlRequest) -> Fut,
    >(
        route_request_args: RouteRequestArgs<A::Version>,
        mut control_request_handler: F,
        route_set_results: impl ExactSizeIterator<Item = RouteSetResult>,
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
    ) -> TestRequestResult
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        test_request(
            RequestArgs::Route(route_request_args),
            |interfaces_request_stream| async move {
                interfaces_request_stream
                    .filter_map(|req| {
                        futures::future::ready(match req.unwrap() {
                            fnet_root::InterfacesRequest::GetAdmin {
                                id,
                                control,
                                control_handle: _,
                            } => {
                                pretty_assertions::assert_eq!(id, INTERFACE_ID1 as u64);
                                Some(control.into_stream().unwrap())
                            }
                            req => unreachable!("unexpected interfaces request: {req:?}"),
                        })
                    })
                    .flatten()
                    .next()
                    .then(|req| control_request_handler(req.unwrap().unwrap()))
                    .await
            },
            route_set_results,
            subnet,
            next_hop1,
            next_hop2,
        )
        .await
    }

    // A test helper that calls `test_route_request()` with the provided
    // inputs and expected values.
    async fn test_new_route_helper<A: IpAddress>(
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (address_family, route_group) = match A::Version::VERSION {
            IpVersion::V4 => (AF_INET, ModernGroup(RTNLGRP_IPV4_ROUTE)),
            IpVersion::V6 => (AF_INET6, ModernGroup(RTNLGRP_IPV6_ROUTE)),
        };

        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (TEST_V4_NEXTHOP, TEST_V4_NEXTHOP2),
            |()| (TEST_V6_NEXTHOP, TEST_V6_NEXTHOP2),
        );

        // When the waiter result is Ok(()), then we know that the add
        // was successful and we got a message.
        let messages = match waiter_result {
            Ok(()) => Vec::from([SentMessage::multicast(
                create_netlink_route_message(
                    address_family.try_into().expect("should fit into u8"),
                    subnet.prefix(),
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop2),
                        INTERFACE_ID1,
                        LOWER_METRIC,
                    ),
                )
                .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                route_group,
            )]),
            Err(_) => Vec::new(),
        };

        // There are two pre-set routes in `test_route_request`.
        // * subnet, next_hop1, INTERFACE_ID1
        // * subnet, next_hop2, INTERFACE_ID2
        // To add a new route that does not get rejected by the handler due to it
        // already existing, we use a route that has next_hop2, and INTERFACE_ID1.
        let unicast_route_args = create_unicast_route_args(subnet, next_hop2, INTERFACE_ID1.into());
        pretty_assertions::assert_eq!(
            test_route_request(
                RouteRequestArgs::New(NewRouteArgs::Unicast(unicast_route_args)),
                |req| async {
                    match req {
                        fnet_interfaces_admin::ControlRequest::GetAuthorizationForInterface {
                            responder,
                        } => {
                            let (token, _) = fidl::EventPair::create();
                            let grant = fnet_interfaces_admin::GrantForInterfaceAuthorization {
                                interface_id: INTERFACE_ID1 as u64,
                                token,
                            };
                            responder.send(grant).unwrap();
                        }
                        req => panic!("unexpected request {req:?}"),
                    }
                },
                route_set_results.into_iter(),
                subnet,
                next_hop1,
                next_hop2
            )
            .await,
            TestRequestResult { messages, waiter_result },
        )
    }

    // Tests RTM_NEWROUTE with all interesting responses to add a route.
    #[test_case(
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        TEST_V4_SUBNET;
        "v4_success")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        TEST_V6_SUBNET;
        "v6_success")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        TEST_V4_SUBNET;
        "v4_failed_auth")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        TEST_V6_SUBNET;
        "v6_failed_auth")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        TEST_V4_SUBNET;
        "v4_failed_exists")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        TEST_V6_SUBNET;
        "v6_failed_exists")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        TEST_V4_SUBNET;
        "v4_invalid_dest")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        TEST_V6_SUBNET;
        "v6_invalid_dest")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        TEST_V4_SUBNET;
        "v4_invalid_hop")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        TEST_V6_SUBNET;
        "v6_invalid_hop")]
    #[fuchsia::test]
    async fn test_new_route<A: IpAddress>(
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        test_new_route_helper(route_set_results, waiter_result, subnet).await;
    }

    // Tests RTM_NEWROUTE when two unauthentication events are received - once prior to
    // making an attempt to authenticate and once after attempting to authenticate.
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        TEST_V4_SUBNET;
        "v4_unauthenticated")]
    #[test_case(
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        TEST_V6_SUBNET;
        "v6_unauthenticated")]
    #[should_panic(expected = "received unauthentication error from route set for route")]
    #[fuchsia::test]
    async fn test_new_route_failed<A: IpAddress>(
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        test_new_route_helper(route_set_results, waiter_result, subnet).await;
    }

    /// Tests RTM_NEWROUTE when the interface is removed,
    /// indicated by the closure of the admin Control's server-end.
    /// The specific cause of the interface removal is unimportant
    /// for this test.
    #[test_case(TEST_V4_SUBNET)]
    #[test_case(TEST_V6_SUBNET)]
    #[fuchsia::test]
    async fn test_new_route_interface_removed<A: IpAddress>(subnet: Subnet<A>)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (TEST_V4_NEXTHOP, TEST_V4_NEXTHOP2),
            |()| (TEST_V6_NEXTHOP, TEST_V6_NEXTHOP2),
        );

        // There are two pre-set routes in `test_route_request`.
        // * subnet, next_hop1, INTERFACE_ID1
        // * subnet, next_hop2, INTERFACE_ID2
        // To add a new route that does not get rejected by the handler due to it
        // already existing, we use a route that has next_hop2, and INTERFACE_ID1.
        let unicast_route_args = create_unicast_route_args(subnet, next_hop2, INTERFACE_ID1.into());

        pretty_assertions::assert_eq!(
            test_request(
                RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                    unicast_route_args
                ))),
                |interfaces_request_stream| async move {
                    interfaces_request_stream
                        .for_each(|req| {
                            futures::future::ready(match req.unwrap() {
                                fnet_root::InterfacesRequest::GetAdmin {
                                    id,
                                    control,
                                    control_handle: _,
                                } => {
                                    pretty_assertions::assert_eq!(id, INTERFACE_ID1 as u64);
                                    let control = control.into_stream().unwrap();
                                    let control = control.control_handle();
                                    control.shutdown();
                                }
                                req => unreachable!("unexpected interfaces request: {req:?}"),
                            })
                        })
                        .await
                },
                std::iter::once(RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated))),
                subnet,
                next_hop1,
                next_hop2,
            )
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_result: Err(RequestError::UnrecognizedInterface),
            },
        )
    }
}
