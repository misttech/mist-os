// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_ROUTE information by receiving RTM_ROUTE
//! Netlink messages and maintaining route table state from Netstack.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fmt::Debug,
    hash::{Hash, Hasher},
    num::{NonZeroU32, NonZeroU64},
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

use derivative::Derivative;
use futures::{channel::oneshot, StreamExt as _};
use itertools::Itertools as _;
use linux_uapi::{
    rt_class_t_RT_TABLE_COMPAT, rt_class_t_RT_TABLE_MAIN, rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
    rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
};
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpInvariant, IpVersion, Subnet},
    SpecifiedAddr, SpecifiedAddress as _, Witness as _,
};
use netlink_packet_core::{NetlinkMessage, NLM_F_MULTIPART};
use netlink_packet_route::{
    route::{
        RouteAddress, RouteAttribute, RouteHeader, RouteMessage, RouteProtocol, RouteScope,
        RouteType,
    },
    AddressFamily, RouteNetlinkMessage,
};
use netlink_packet_utils::{nla::Nla, DecodeError};

use crate::{
    client::{ClientTable, InternalClient},
    errors::WorkerInitializationError,
    logging::{log_debug, log_error, log_warn},
    messaging::Sender,
    multicast_groups::ModernGroup,
    netlink_packet::{errno::Errno, UNSPECIFIED_SEQUENCE_NUMBER},
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
    util::respond_to_completer,
};

// TODO(https://fxbug.dev/336382905): Remove special constant and rely on table
// ids provided by the Netstack.
/// The table id that holds routes not managed by Netlink.
const UNMANAGED_ROUTE_TABLE: u32 = rt_class_t_RT_TABLE_MAIN;

/// Arguments for an RTM_GETROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum GetRouteArgs {
    Dump,
}

/// Arguments for an RTM_NEWROUTE unicast route.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnicastNewRouteArgs<I: Ip> {
    // The network and prefix of the route.
    pub subnet: Subnet<I::Addr>,
    // The forwarding action. Unicast routes are gateway/direct routes and must
    // have a target.
    pub target: fnet_routes_ext::RouteTarget<I>,
    // The metric used to weigh the importance of the route.
    pub priority: u32,
    // The routing table.
    pub table: u32,
}

/// Arguments for an RTM_NEWROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewRouteArgs<I: Ip> {
    /// Direct or gateway routes.
    Unicast(UnicastNewRouteArgs<I>),
}

/// Arguments for an RTM_DELROUTE unicast route.
/// Only the subnet and table field are required. All other fields are optional.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnicastDelRouteArgs<I: Ip> {
    // The network and prefix of the route.
    pub(crate) subnet: Subnet<I::Addr>,
    // The outbound interface to use when forwarding packets.
    pub(crate) outbound_interface: Option<NonZeroU64>,
    // The next-hop IP address of the route.
    pub(crate) next_hop: Option<SpecifiedAddr<I::Addr>>,
    // The metric used to weigh the importance of the route.
    pub(crate) priority: Option<NonZeroU32>,
    // The routing table.
    pub(crate) table: NonZeroU32,
}

/// Arguments for an RTM_DELROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum DelRouteArgs<I: Ip> {
    /// Direct or gateway routes.
    Unicast(UnicastDelRouteArgs<I>),
}

/// [`Request`] arguments associated with routes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteRequestArgs<I: Ip> {
    /// RTM_GETROUTE
    Get(GetRouteArgs),
    /// RTM_NEWROUTE
    New(NewRouteArgs<I>),
    /// RTM_DELROUTE
    Del(DelRouteArgs<I>),
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
    /// Netstack failed to delete the route due to the route not being
    /// installed by Netlink.
    DeletionNotAllowed,
    /// Invalid destination subnet or next-hop.
    InvalidRequest,
    /// No routes in the route set matched the route query.
    NotFound,
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
            RequestError::NotFound => Errno::ESRCH,
            RequestError::DeletionNotAllowed | RequestError::Unknown => Errno::ENOTSUP,
            RequestError::UnrecognizedInterface => Errno::ENODEV,
        }
    }
}

fn map_route_set_error<I: Ip + fnet_routes_ext::FidlRouteIpExt>(
    e: RouteSetError,
    route: &I::Route,
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
#[derive(Derivative, GenericOverIp)]
#[derivative(Debug(bound = ""))]
#[generic_over_ip(I, Ip)]
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

/// Handles asynchronous work related to RTM_ROUTE messages.
///
/// Can respond to RTM_ROUTE message requests.
pub(crate) struct RoutesWorkerState<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    route_tables: HashMap<RouteTableKey, RouteTable<I>>,
}

// A `RouteTable` identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RouteTableKey {
    // Routing tables that are created and managed by Netlink.
    NetlinkManaged { table_id: u32 },
    // Routing tables that are not managed by Netlink, but exist on the system.
    Unmanaged,
}

// TODO(https://fxbug.dev/336382905): Remove converter below once table
// ids are provided by Netstack.
impl From<RouteTableKey> for u32 {
    fn from(key: RouteTableKey) -> Self {
        match key {
            RouteTableKey::NetlinkManaged { table_id } => table_id,
            RouteTableKey::Unmanaged => UNMANAGED_ROUTE_TABLE,
        }
    }
}

fn get_table_u8_and_nla_from_key(table: RouteTableKey) -> (u8, Option<RouteAttribute>) {
    let table_id: u32 = table.into();
    // When the table's value is >255, the value should be specified
    // by an NLA and the header value should be RT_TABLE_COMPAT.
    match u8::try_from(table_id) {
        Ok(t) => (t, None),
        // RT_TABLE_COMPAT (252) can be downcasted without loss into u8.
        Err(_) => (rt_class_t_RT_TABLE_COMPAT as u8, Some(RouteAttribute::Table(table_id))),
    }
}

pub(crate) struct RouteTable<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    route_set_proxy: <I::RouteSetMarker as ProtocolMarker>::Proxy,
    route_messages: HashSet<NetlinkRouteMessage>,
}

/// FIDL errors from the routes worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoutesFidlError {
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
    /// Unexpected event was received from routes watcher.
    #[error("unexpected event: {0:?}")]
    UnexpectedEvent(fnet_routes_ext::Event<I>),
}

/// A subset of `RouteRequestArgs`, containing only `Request` types that can be pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PendingRouteRequestArgs<I: Ip> {
    /// RTM_NEWROUTE
    New(NewRouteArgs<I>),
    /// RTM_DELROUTE
    Del(NetlinkRouteMessage),
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct PendingRouteRequest<
    S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    I: Ip,
> {
    request_args: PendingRouteRequestArgs<I>,
    client: InternalClient<NetlinkRoute, S>,
    completer: oneshot::Sender<Result<(), RequestError>>,
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>, I: Ip> PendingRouteRequest<S, I> {
    pub(crate) fn args(&self) -> &PendingRouteRequestArgs<I> {
        &self.request_args
    }
}

impl<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    RoutesWorkerState<I>
{
    pub(crate) async fn create(
        set_provider_proxy: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
        routes_state_proxy: &<I::StateMarker as ProtocolMarker>::Proxy,
    ) -> Result<
        (
            Self,
            impl futures::Stream<
                    Item = Result<fnet_routes_ext::Event<I>, fnet_routes_ext::WatchError>,
                > + Unpin,
        ),
        WorkerInitializationError<RoutesFidlError, RoutesNetstackError<I>>,
    > {
        let mut route_event_stream =
            Box::pin(fnet_routes_ext::event_stream_from_state(routes_state_proxy).map_err(
                |e| WorkerInitializationError::Fidl(RoutesFidlError::WatcherCreation(e)),
            )?);
        let installed_routes = fnet_routes_ext::collect_routes_until_idle::<_, HashSet<_>>(
            route_event_stream.by_ref(),
        )
        .await
        .map_err(|e| match e {
            fnet_routes_ext::CollectRoutesUntilIdleError::ErrorInStream(e) => {
                WorkerInitializationError::Fidl(RoutesFidlError::Watch(e))
            }
            fnet_routes_ext::CollectRoutesUntilIdleError::StreamEnded => {
                WorkerInitializationError::Netstack(RoutesNetstackError::EventStreamEnded)
            }
            fnet_routes_ext::CollectRoutesUntilIdleError::UnexpectedEvent(event) => {
                WorkerInitializationError::Netstack(RoutesNetstackError::UnexpectedEvent(event))
            }
        })?;
        let route_messages = new_set_with_existing_routes(installed_routes);
        let route_set_proxy = create_route_set_proxy(set_provider_proxy)?;
        Ok((
            Self {
                route_tables: HashMap::from([(RouteTableKey::Unmanaged, {
                    RouteTable { route_set_proxy, route_messages }
                })]),
            },
            route_event_stream,
        ))
    }

    /// Handles events observed by the route watchers by adding/removing routes
    /// from the underlying `NetlinkRouteMessage` set.
    ///
    /// Returns a `RouteEventHandlerError` when unexpected events or HashSet issues occur.
    pub(crate) fn handle_route_watcher_event<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        &mut self,
        route_clients: &ClientTable<NetlinkRoute, S>,
        event: fnet_routes_ext::Event<I>,
        pending_request_args: Option<PendingRouteRequestArgs<I>>,
    ) -> Result<(), RouteEventHandlerError<I>> {
        handle_route_watcher_event::<I, S>(
            &mut self.route_tables,
            route_clients,
            event,
            pending_request_args,
        )
    }

    fn get_interface_control(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        interface_id: u64,
    ) -> fnet_interfaces_ext::admin::Control {
        let (control, server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
                .expect("create Control proxy");
        interfaces_proxy.get_admin(interface_id, server_end).expect("send get admin request");
        fnet_interfaces_ext::admin::Control::new(control)
    }

    async fn authenticate_for_interface(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as fidl::endpoints::ProtocolMarker>::Proxy,
        interface_id: u64,
    ) -> Result<(), RequestError> {
        let control = Self::get_interface_control(interfaces_proxy, interface_id);

        let grant = match control.get_authorization_for_interface().await {
            Ok(grant) => grant,
            Err(fnet_interfaces_ext::admin::TerminalError::Fidl(
                fidl::Error::ClientChannelClosed { status, protocol_name },
            )) => {
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
        #[generic_over_ip(I, Ip)]
        struct AuthorizeInputs<'a, I: fnet_routes_ext::admin::FidlRouteAdminIpExt> {
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

    /// Handles a new route request.
    ///
    /// Returns the `RouteRequestArgs` if the route was successfully
    /// added so that the caller can make sure their local state (from the
    /// routes watcher) has sent an event holding the added route.
    async fn handle_new_route_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
        new_route_args: NewRouteArgs<I>,
    ) -> Result<NewRouteArgs<I>, RequestError> {
        let interface_id = match new_route_args {
            NewRouteArgs::Unicast(args) => args.target.outbound_interface,
        };
        let route: I::Route = {
            let route: fnet_routes_ext::Route<I> = new_route_args.into();
            route.try_into().expect("route should be converted")
        };

        Self::dispatch_route_proxy_fn(
            &route,
            interface_id,
            &interfaces_proxy,
            &route_set_proxy,
            fnet_routes_ext::admin::add_route::<I>,
        )
        .await
        .map(|did_add| {
            if did_add {
                Ok(new_route_args)
            } else {
                // When `add_route` has an `Ok(false)` response, this indicates that the
                // route already exists, which should manifest as a hard error in Linux.
                Err(RequestError::AlreadyExists)
            }
        })?
    }

    /// Handles a delete route request.
    ///
    /// Returns the `NetlinkRouteMessage` if the route was successfully
    /// removed so that the caller can make sure their local state (from the
    /// routes watcher) has sent a removal event for the removed route.
    async fn handle_del_route_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_set_proxy: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
        del_route_args: DelRouteArgs<I>,
        existing_routes: &HashSet<NetlinkRouteMessage>,
    ) -> Result<NetlinkRouteMessage, RequestError> {
        let route_to_delete = select_route_for_deletion(del_route_args, existing_routes)
            .ok_or(RequestError::NotFound)?;

        let NetlinkRouteMessage(route) = route_to_delete;
        let interface_id = route
            .attributes
            .iter()
            .filter_map(|nla| match nla {
                RouteAttribute::Oif(interface) => Some(*interface as u64),
                _nla => None,
            })
            .next()
            .expect("there should be exactly one Oif NLA present");

        let route: I::Route = {
            let route: fnet_routes_ext::Route<I> = route_to_delete.to_owned().into();
            route.try_into().expect("route should be converted")
        };

        Self::dispatch_route_proxy_fn(
            &route,
            interface_id,
            &interfaces_proxy,
            &route_set_proxy,
            fnet_routes_ext::admin::remove_route::<I>,
        )
        .await
        .map(|did_remove| {
            if did_remove {
                Ok(route_to_delete.to_owned())
            } else {
                log_error!(
                    "Route was not removed as a result of this call. Likely Linux wanted \
                    to remove a route from the global route set which is not supported  \
                    by this API, route: {:?}",
                    route_to_delete
                );
                Err(RequestError::DeletionNotAllowed)
            }
        })?
    }

    // Dispatch a function to the RouteSetProxy.
    //
    // Attempt to dispatch the function without authenticating first. If the call is
    // unsuccessful due to an Unauthenticated error, try again after authenticating
    // for the interface.
    // Returns: whether the RouteSetProxy function made a change in the Netstack
    // (an add or delete), or `RequestError` if unsuccessful.
    async fn dispatch_route_proxy_fn<'a, Fut>(
        route: &'a I::Route,
        interface_id: u64,
        interfaces_proxy: &'a fnet_root::InterfacesProxy,
        route_set_proxy: &'a <I::RouteSetMarker as ProtocolMarker>::Proxy,
        dispatch_fn: impl Fn(&'a <I::RouteSetMarker as ProtocolMarker>::Proxy, &'a I::Route) -> Fut,
    ) -> Result<bool, RequestError>
    where
        Fut: futures::Future<Output = Result<Result<bool, RouteSetError>, fidl::Error>>,
    {
        match dispatch_fn(route_set_proxy, &route).await.expect("sent route proxy request") {
            Ok(made_change) => return Ok(made_change),
            Err(RouteSetError::Unauthenticated) => {}
            Err(e) => {
                log_warn!("error altering route on interface ({interface_id}): {e:?}");
                return Err(map_route_set_error::<I>(e, route, interface_id));
            }
        };

        // Authenticate for the interface if we received the `Unauthenticated`
        // error from the function that was dispatched.
        Self::authenticate_for_interface(interfaces_proxy, route_set_proxy, interface_id).await?;

        // Dispatch the function once more after authenticating. All errors are
        // treated as hard errors after the second dispatch attempt. Further
        // attempts are not expected to yield differing results.
        dispatch_fn(route_set_proxy, &route).await.expect("sent route proxy request").map_err(|e| {
            log_warn!(
                "error altering route after authenticating for \
                    interface ({interface_id}): {e:?}"
            );
            map_route_set_error::<I>(e, route, interface_id)
        })
    }

    /// Handles a [`Request`].
    ///
    /// Returns a [`PendingRouteRequest`] if a route was updated and the caller
    /// needs to make sure the update has been propagated to the local state
    /// (the routes watcher has sent an event for our update).
    pub(crate) async fn handle_request<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        &mut self,
        interfaces_proxy: &fnet_root::InterfacesProxy,
        set_provider_proxy: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
        Request { args, sequence_number, mut client, completer }: Request<S, I>,
    ) -> Option<PendingRouteRequest<S, I>> {
        let Self { route_tables } = self;
        log_debug!("handling request {args:?} from {client}");

        let result = match args {
            RequestArgs::Route(args) => match args {
                RouteRequestArgs::Get(args) => match args {
                    GetRouteArgs::Dump => {
                        route_tables.values().for_each(
                            |RouteTable { route_set_proxy: _, route_messages }| {
                                route_messages.iter().for_each(|message| {
                                    client.send_unicast(
                                        message.clone().into_rtnl_new_route(sequence_number, true),
                                    )
                                })
                            },
                        );
                        Ok(())
                    }
                },
                RouteRequestArgs::New(args) => {
                    let table: RouteTableKey = match args {
                        NewRouteArgs::Unicast(args) => {
                            RouteTableKey::NetlinkManaged { table_id: args.table }
                        }
                    };

                    // Populate and insert the RouteTable if not already present.
                    if let Entry::Vacant(entry) = route_tables.entry(table) {
                        let route_set_proxy = create_route_set_proxy::<I>(set_provider_proxy)
                            .expect("create RouteSet proxy");
                        let _ = entry
                            .insert(RouteTable { route_set_proxy, route_messages: HashSet::new() });
                    }

                    // Take a new reference to the fields.
                    let RouteTable { route_set_proxy, route_messages: _ } =
                        match route_tables.get(&table) {
                            Some(route_table) => route_table,
                            None => unreachable!("table should be present as it was just inserted"),
                        };

                    // TODO(https://fxbug.dev/336382905): Once Netstack can
                    // support the same routes in differing tables, check
                    // for conflicts against routes in the current table
                    // instead of checking for conflicts against
                    // all observed routes.
                    // NB: A subnet route for the interface will be added in
                    // its own route set. If the caller attempts to add it
                    // separately, we will fail to see that it already exists,
                    // so that's why we need to collect all of the existing
                    // routing state across route tables.
                    let all_route_messages = get_all_route_messages(route_tables.values());
                    match new_route_matches_existing(&args, all_route_messages) {
                        NewRouteConflict::ConflictInSameTable => Err(RequestError::AlreadyExists),
                        // TODO(https://fxbug.dev/336382905): Add the route to the Netstack
                        // once it can support installing otherwise-identical routes via
                        // Policy-Based routing.
                        NewRouteConflict::ConflictInDifferentTable => Ok(()),
                        NewRouteConflict::NoConflict => {
                            match Self::handle_new_route_request(
                                interfaces_proxy,
                                route_set_proxy,
                                args,
                            )
                            .await
                            {
                                Ok(new_route_args) => {
                                    // Route additions must be confirmed via a message from
                                    // the Routes watcher with the same Route struct.
                                    return Some(PendingRouteRequest {
                                        request_args: PendingRouteRequestArgs::New(new_route_args),
                                        client,
                                        completer,
                                    });
                                }
                                Err(e) => Err(e),
                            }
                        }
                    }
                }
                RouteRequestArgs::Del(args) => {
                    let table: RouteTableKey = match args {
                        DelRouteArgs::Unicast(args) => {
                            RouteTableKey::NetlinkManaged { table_id: args.table.get() }
                        }
                    };

                    if let Some(RouteTable { route_set_proxy, route_messages }) =
                        route_tables.get(&table)
                    {
                        match Self::handle_del_route_request(
                            interfaces_proxy,
                            &route_set_proxy,
                            args,
                            &route_messages,
                        )
                        .await
                        {
                            Ok(del_route) => {
                                // Route deletions must be confirmed via a message from the Routes
                                // watcher with the same Route struct - using the route
                                // matched for deletion.
                                return Some(PendingRouteRequest {
                                    request_args: PendingRouteRequestArgs::Del(del_route),
                                    client,
                                    completer,
                                });
                            }
                            Err(e) => Err(e),
                        }
                    } else {
                        // RouteSet did not exist for the provided route. This can occur if a
                        // Netlink client learns about the existence of a Route in the
                        // `UNMANAGED_ROUTE_TABLE` and attempts to remove it using its table id.
                        // Netlink clients are not currently expected to need to remove routes
                        // installed by Fuchsia components such as Netstack.
                        Err(RequestError::NotFound)
                    }
                }
            },
        };

        log_debug!("handled request {args:?} from {client} with result = {result:?}");

        respond_to_completer(client, completer, result, args);
        None
    }

    /// Checks whether a `PendingRequest` can be marked completed given the current state of the
    /// worker. If so, notifies the request's completer and returns `None`. If not, returns
    /// the `PendingRequest` as `Some`.
    pub(crate) fn handle_pending_request<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        &self,
        pending_route_request: PendingRouteRequest<S, I>,
    ) -> Option<PendingRouteRequest<S, I>> {
        let PendingRouteRequest { request_args, client: _, completer: _ } = &pending_route_request;

        let done = match request_args {
            PendingRouteRequestArgs::New(route) => {
                // TODO(https://fxbug.dev/336382905): Once Netstack can
                // support the same routes in differing tables, check
                // for conflicts against routes in the current table
                // instead of checking for conflicts against
                // all observed routes.
                let all_route_messages = get_all_route_messages(self.route_tables.values());
                let conflict = new_route_matches_existing(&route, all_route_messages);
                match conflict {
                    NewRouteConflict::ConflictInSameTable
                    | NewRouteConflict::ConflictInDifferentTable => true,
                    NewRouteConflict::NoConflict => false,
                }
            }
            // For `Del` messages, we expect the exact `NetlinkRouteMessage` to match,
            // which was received as part of the `select_route_for_deletion` flow.
            PendingRouteRequestArgs::Del(route_msg) => {
                // Get the table from the NLA (if present), otherwise choose
                // the value from the message header.
                let table_id = {
                    let NetlinkRouteMessage(inner_route_msg) = route_msg;
                    let RouteNlaView { table, .. } = view_existing_route_nlas(inner_route_msg);
                    match table {
                        Some(t) => *t,
                        None => inner_route_msg.header.table.into(),
                    }
                };
                let table = RouteTableKey::NetlinkManaged { table_id };
                match &self.route_tables.get(&table) {
                    Some(RouteTable { route_set_proxy: _, route_messages }) => {
                        !route_messages.contains(route_msg)
                    }
                    None => {
                        log_error!(
                            "unreachable: if there is a pending del request, \
                            the route table should be present for table {table:?}"
                        );
                        false
                    }
                }
            }
        };

        if done {
            log_debug!("completed pending request; req = {pending_route_request:?}");
            let PendingRouteRequest { request_args, client, completer } = pending_route_request;

            respond_to_completer(client, completer, Ok(()), request_args);
            None
        } else {
            // Put the pending request back so that it can be handled later.
            log_debug!("pending request not done yet; req = {pending_route_request:?}");
            Some(pending_route_request)
        }
    }
}

// Errors related to handling route events.
#[derive(Debug, PartialEq, thiserror::Error)]
pub(crate) enum RouteEventHandlerError<I: Ip> {
    #[error("route watcher event handler attempted to add a route that already existed: {0:?}")]
    AlreadyExistingRouteAddition(fnet_routes_ext::InstalledRoute<I>),
    #[error("route watcher event handler attempted to remove a route that does not exist: {0:?}")]
    NonExistentRouteDeletion(fnet_routes_ext::InstalledRoute<I>),
    #[error("route watcher event handler attempted to process a route event that was not add or remove: {0:?}")]
    NonAddOrRemoveEventReceived(fnet_routes_ext::Event<I>),
}

fn create_route_set_proxy<I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt>(
    set_provider_proxy: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<
    <I::RouteSetMarker as ProtocolMarker>::Proxy,
    WorkerInitializationError<RoutesFidlError, RoutesNetstackError<I>>,
> {
    fnet_routes_ext::admin::new_route_set::<I>(&set_provider_proxy)
        .map_err(|e| WorkerInitializationError::Fidl(RoutesFidlError::RouteSetCreation(e)))
}

fn handle_route_watcher_event<
    I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
>(
    route_tables: &mut HashMap<RouteTableKey, RouteTable<I>>,
    route_clients: &ClientTable<NetlinkRoute, S>,
    event: fnet_routes_ext::Event<I>,
    pending_request: Option<PendingRouteRequestArgs<I>>,
) -> Result<(), RouteEventHandlerError<I>> {
    // Determine if the observed Route is the same Route that
    // exists in the NewRouteArgs, extracting the table number.
    // If the observed Route is not known, it must be from an
    // Unmanaged routing table.
    fn get_table_key_from_pending_request<I: Ip>(
        route: fnet_routes_ext::InstalledRoute<I>,
        new_route_args: &NewRouteArgs<I>,
    ) -> RouteTableKey {
        if new_route_matches_new_route_args(&route, &new_route_args) {
            match new_route_args {
                NewRouteArgs::Unicast(route) => {
                    return RouteTableKey::NetlinkManaged { table_id: route.table }
                }
            };
        }
        RouteTableKey::Unmanaged
    }

    let message_for_clients = match event {
        fnet_routes_ext::Event::Added(route) => {
            let table = match pending_request {
                Some(PendingRouteRequestArgs::New(new_route_args)) => {
                    get_table_key_from_pending_request(route, &new_route_args)
                }
                _ => RouteTableKey::Unmanaged,
            };
            if let Some(route_message) = NetlinkRouteMessage::optionally_from(route, table) {
                // The Unmanaged table is initialized eagerly, so even
                // without a `PendingRouteRequest` it will be present.
                let RouteTable { route_set_proxy: _, route_messages } = route_tables
                    .get_mut(&table)
                    .expect("route table should have been initialized when handling request");
                if !route_messages.insert(route_message.clone()) {
                    return Err(RouteEventHandlerError::AlreadyExistingRouteAddition(route));
                }

                Some(route_message.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false))
            } else {
                None
            }
        }
        fnet_routes_ext::Event::Removed(route) => {
            // TODO(336382905): Update once tables are included in `Route`s.
            //
            // When a route deletion is initiated from the Fuchsia-side, the
            // table is not known. Search through the present routes across
            // `RouteSet`s to see which route matches all fields and
            // remove the route. Only a single route is expected to match
            // `installed_route_matches_netlink_message` due to the matching
            // fn checking against all identifiable fields.
            let table_with_route = route_tables
                .iter()
                .filter_map(|(table, RouteTable { route_set_proxy: _, route_messages })| {
                    route_messages
                        .iter()
                        .any(|msg| installed_route_matches_netlink_message(&route, msg))
                        .then_some(table)
                })
                .at_most_one();

            let table_with_route = match table_with_route {
                Ok(table) => table.copied(),
                Err(_) => {
                    unreachable!("two routes cannot exist in the Netstack with the same fields")
                }
            };

            if let Some(table) = table_with_route {
                if let Some(route_message) = NetlinkRouteMessage::optionally_from(route, table) {
                    // The Unmanaged table is initialized eagerly, so even
                    // without a `PendingRouteRequest` it will be present.
                    let RouteTable { route_set_proxy: _, route_messages } = route_tables
                        .get_mut(&table)
                        .expect("route table should have been initialized when handling request");
                    if !route_messages.remove(&route_message) {
                        return Err(RouteEventHandlerError::NonExistentRouteDeletion(route));
                    }
                    Some(route_message.into_rtnl_del_route())
                } else {
                    None
                }
            } else {
                return Err(RouteEventHandlerError::NonExistentRouteDeletion(route));
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
            IpVersion::V4 => ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
            IpVersion::V6 => ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        };
        route_clients.send_message_to_group(message_for_clients, route_group);
    }

    Ok(())
}

// Check if the new route matches the `NewRouteArgs`.
//
// Similar to `new_route_matches_existing`, but checks
// new routes observed from the Routes Watcher against
// a `NewRouteArgs`.
fn new_route_matches_new_route_args<I: Ip>(
    new_route: &fnet_routes_ext::InstalledRoute<I>,
    pending_route: &NewRouteArgs<I>,
) -> bool {
    let UnicastNewRouteArgs { subnet, target: _, priority, table: _ } = match pending_route {
        NewRouteArgs::Unicast(args) => args,
    };

    let fnet_routes_ext::InstalledRoute {
        route: fnet_routes_ext::Route { destination, action: _, properties: _ },
        effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
    } = new_route;

    let subnet_matches = subnet == destination;
    let metric_matches = priority == metric;

    subnet_matches && metric_matches
}

// Check if the deleted route matches a `NetlinkRouteMessage`.
//
// Similar to `select_route_for_deletion`, but checks that all fields
// in `InstalledRoute` match `NetlinkRouteMessage`.
fn installed_route_matches_netlink_message<I: Ip>(
    del_route: &fnet_routes_ext::InstalledRoute<I>,
    pending_route: &NetlinkRouteMessage,
) -> bool {
    let NetlinkRouteMessage(route_message) = pending_route;
    let fnet_routes_ext::InstalledRoute {
        route: fnet_routes_ext::Route { destination: subnet, action, properties: _ },
        effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: priority },
    } = del_route;

    let (outbound_interface, next_hop) = match action {
        fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
            outbound_interface,
            next_hop,
        }) => (outbound_interface, next_hop),
        // We don't support non-forwarding `RouteAction`s.
        fnet_routes_ext::RouteAction::Unknown => return false,
    };

    if subnet.prefix() != route_message.header.destination_prefix_length {
        return false;
    }
    let RouteNlaView {
        subnet: pending_subnet,
        metric: pending_metric,
        interface_id: pending_interface,
        next_hop: pending_next_hop,
        table: _,
    } = view_existing_route_nlas(route_message);
    let subnet_matches = pending_subnet
        .map(|dst| crate::netlink_packet::ip_addr_from_route::<I>(&dst))
        .unwrap_or(Ok(I::UNSPECIFIED_ADDRESS))
        .is_ok_and(|dst: I::Addr| dst == subnet.network());
    let metric_matches = priority == pending_metric;
    let interface_matches = *outbound_interface == (*pending_interface).into();

    // We consider the next hop to match if it is not present, because it
    // indicates the unspecified address.
    let next_hop_matches = next_hop.map_or(true, |next_hop| {
        pending_next_hop
            .clone()
            .map(|e| crate::netlink_packet::ip_addr_from_route::<I>(&e))
            .unwrap_or(Ok(I::UNSPECIFIED_ADDRESS))
            .is_ok_and(|e: I::Addr| e == next_hop.get())
    });

    subnet_matches && metric_matches && interface_matches && next_hop_matches
}

/// A wrapper type for the netlink_packet_route `RouteMessage` to enable conversions
/// from [`fnet_routes_ext::InstalledRoute`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetlinkRouteMessage(RouteMessage);

// Constructs a new set of `NetlinkRouteMessage` from an
// `InstalledRoute` HashSet.
// TODO(https://issues.fuchsia.dev/294273363): Store a HashSet of Route<I>
// instead of NetlinkRouteMessage.
fn new_set_with_existing_routes<I: Ip>(
    routes: HashSet<fnet_routes_ext::InstalledRoute<I>>,
) -> HashSet<NetlinkRouteMessage> {
    return routes
        .iter()
        .filter_map(|route| NetlinkRouteMessage::optionally_from(*route, RouteTableKey::Unmanaged))
        .collect::<HashSet<_>>();
}

impl NetlinkRouteMessage {
    /// Implement optional conversions from `InstalledRoute` and `table`
    /// to `NetlinkRouteMessage`. `Ok` becomes `Some`, while `Err` is
    /// logged and becomes `None`.
    fn optionally_from<I: Ip>(
        route: fnet_routes_ext::InstalledRoute<I>,
        table: RouteTableKey,
    ) -> Option<NetlinkRouteMessage> {
        match NetlinkRouteMessage::try_from_installed_route::<I>(route, table) {
            Ok(route) => Some(route),
            Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding) => {
                log_warn!("Unexpected non-forwarding route in routing table: {:?}", route);
                None
            }
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(id)) => {
                log_warn!("Invalid interface id found in routing table route: {:?}", id);
                None
            }
            Err(NetlinkRouteMessageConversionError::FailedToDecode(err)) => {
                log_warn!("Unable to decode route address: {:?}", err);
                None
            }
        }
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::NewRoute`].
    pub(crate) fn into_rtnl_new_route(
        self,
        sequence_number: u32,
        is_dump: bool,
    ) -> NetlinkMessage<RouteNetlinkMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::NewRoute(message).into();
        msg.header.sequence_number = sequence_number;
        if is_dump {
            msg.header.flags |= NLM_F_MULTIPART;
        }
        msg.finalize();
        msg
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::DelRoute`].
    fn into_rtnl_del_route(self) -> NetlinkMessage<RouteNetlinkMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::DelRoute(message).into();
        msg.finalize();
        msg
    }

    // TODO(https://fxbug.dev/336382905): Refactor this as a TryFrom
    // impl once tables are present in `InstalledRoute`.
    // Implement conversions from `InstalledRoute` to `NetlinkRouteMessage`
    // which is fallible iff, the route's action is not `Forward`.
    fn try_from_installed_route<I: Ip>(
        fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route { destination, action, properties: _ },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        }: fnet_routes_ext::InstalledRoute<I>,
        table: RouteTableKey,
    ) -> Result<Self, NetlinkRouteMessageConversionError> {
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
            IpVersion::V4 => AddressFamily::Inet,
            IpVersion::V6 => AddressFamily::Inet6,
        }
        .try_into()
        .expect("should fit into u8");
        route_header.destination_prefix_length = destination.prefix();

        let (table_u8, table_nla) = get_table_u8_and_nla_from_key(table);
        route_header.table = table_u8;

        // The following fields are used in the header, but they do not have any
        // corresponding values in `InstalledRoute`. The fields explicitly
        // defined below  are expected to be needed at some point, but the
        // information is not currently provided by the watcher.
        //
        // length of source prefix
        // tos filter (type of service)
        route_header.protocol = RouteProtocol::Unspec;
        // Universe for routes with next_hop. Valid as long as route action
        // is forwarding.
        route_header.scope = RouteScope::Universe;
        route_header.kind = RouteType::Unicast;

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
            let destination_nla = RouteAttribute::Destination(RouteAddress::parse(
                route_header.address_family,
                destination.network().bytes(),
            )?);
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
        let oif_nla = RouteAttribute::Oif(outbound_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = RouteAddress::parse(route_header.address_family, next_hop.bytes())?;
            let gateway_nla = RouteAttribute::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = RouteAttribute::Priority(metric);
        nlas.push(priority_nla);

        // Only include the table NLA when `table` does not fit into the u8 range.
        if let Some(nla) = table_nla {
            nlas.push(nla);
        }

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.attributes = nlas;
        Ok(NetlinkRouteMessage(route_message))
    }
}

impl Hash for NetlinkRouteMessage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let NetlinkRouteMessage(message) = self;
        message.header.hash(state);

        let mut buffer = vec![];
        message.attributes.iter().for_each(|nla| {
            buffer.resize(nla.value_len(), 0u8);
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
    // Failed to decode route address.
    FailedToDecode(DecodeErrorWrapper),
}

#[derive(Debug)]
pub(crate) struct DecodeErrorWrapper(DecodeError);

impl PartialEq for DecodeErrorWrapper {
    fn eq(&self, other: &Self) -> bool {
        // DecodeError contains anyhow::Error which unfortunately
        // can't be compared without a call to format!;
        return format!("{:?}", self.0) == format!("{:?}", other.0);
    }
}

impl From<DecodeError> for NetlinkRouteMessageConversionError {
    fn from(err: DecodeError) -> Self {
        NetlinkRouteMessageConversionError::FailedToDecode(DecodeErrorWrapper(err))
    }
}

// Implement conversions from [`NewRouteArgs<I>`] to
// [`fnet_routes_ext::Route<I>`].
impl<I: Ip> From<NewRouteArgs<I>> for fnet_routes_ext::Route<I> {
    fn from(new_route_args: NewRouteArgs<I>) -> Self {
        match new_route_args {
            NewRouteArgs::Unicast(args) => {
                let UnicastNewRouteArgs { subnet, target, priority, table: _ } = args;
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

// Implement conversions from [`NetlinkRouteMessage`] to
// [`fnet_routes_ext::Route<I>`]. This is infallible, as all
// [`NetlinkRouteMessage`]s in this module are created
// with the expected NLAs and proper formatting.
impl<I: Ip> From<NetlinkRouteMessage> for fnet_routes_ext::Route<I> {
    fn from(netlink_route_message: NetlinkRouteMessage) -> Self {
        let NetlinkRouteMessage(route_message) = netlink_route_message;
        let RouteNlaView { subnet, metric, interface_id, next_hop, table: _ } =
            view_existing_route_nlas(&route_message);
        let subnet = match subnet {
            Some(subnet) => crate::netlink_packet::ip_addr_from_route::<I>(&subnet)
                .expect("should be valid addr"),
            None => I::UNSPECIFIED_ADDRESS,
        };

        let subnet = Subnet::new(subnet, route_message.header.destination_prefix_length)
            .expect("should be valid subnet");

        let next_hop = match next_hop {
            Some(next_hop) => crate::netlink_packet::ip_addr_from_route::<I>(&next_hop)
                .map(SpecifiedAddr::new)
                .expect("should be valid addr"),
            None => None,
        };

        fnet_routes_ext::Route {
            destination: subnet,
            action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                outbound_interface: *interface_id as u64,
                next_hop,
            }),
            properties: fnet_routes_ext::RouteProperties {
                specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                    metric: fnet_routes::SpecifiedMetric::ExplicitMetric(*metric),
                },
            },
        }
    }
}

/// A view into the NLA's held by a `NetlinkRouteMessage`.
struct RouteNlaView<'a> {
    subnet: Option<&'a RouteAddress>,
    metric: &'a u32,
    interface_id: &'a u32,
    next_hop: Option<&'a RouteAddress>,
    table: Option<&'a u32>,
}

/// Extract and return a view of the Nlas from the given route.
///
/// # Panics
///
/// Panics if:
///   * The route is missing any of the following Nlas: `Oif`, `Priority`,
///     or `Destination` (only when the destination_prefix_len is non-zero).
///   * Any Nla besides `Oif`, `Priority`, `Gateway`, `Destination`, `Table`
///     is provided.
///   * Any Nla is provided multiple times.
/// Note that this fn is so opinionated about the provided NLAs because it is
/// intended to be used on existing routes, which are constructed by the module
/// meaning the exact set of NLAs is known.
fn view_existing_route_nlas(route: &RouteMessage) -> RouteNlaView<'_> {
    let mut subnet = None;
    let mut metric = None;
    let mut interface_id = None;
    let mut next_hop = None;
    let mut table = None;
    route.attributes.iter().for_each(|nla| match nla {
        RouteAttribute::Destination(dst) => {
            assert_eq!(subnet, None, "existing route has multiple `Destination` NLAs");
            subnet = Some(dst)
        }
        RouteAttribute::Priority(p) => {
            assert_eq!(metric, None, "existing route has multiple `Priority` NLAs");
            metric = Some(p)
        }
        RouteAttribute::Oif(interface) => {
            assert_eq!(interface_id, None, "existing route has multiple `Oif` NLAs");
            interface_id = Some(interface)
        }
        RouteAttribute::Gateway(gateway) => {
            assert_eq!(next_hop, None, "existing route has multiple `Gateway` NLAs");
            next_hop = Some(gateway)
        }
        RouteAttribute::Table(t) => {
            assert_eq!(table, None, "existing route has multiple `Table` NLAs");
            table = Some(t)
        }
        nla => panic!("existing route has unexpected NLA: {:?}", nla),
    });
    if subnet.is_none() {
        assert_eq!(
            route.header.destination_prefix_length, 0,
            "existing route without `Destination` NLA must be a default route"
        );
    }

    RouteNlaView {
        subnet,
        metric: metric.expect("existing routes must have a `Priority` NLA"),
        interface_id: interface_id.expect("existing routes must have an `Oif` NLA"),
        next_hop,
        table,
    }
}

#[derive(Debug, PartialEq)]
enum NewRouteConflict {
    // The new route does not match any existing routes.
    NoConflict,
    // The new route matches an existing route with all fields.
    ConflictInSameTable,
    // The new route matches an existing route with all fields, except
    // for its table id.
    ConflictInDifferentTable,
}

// Check if the new route conflicts with an existing route.
//
// Note that Linux and Fuchsia differ on what constitutes a conflicting route.
// Linux is stricter than Fuchsia and requires that all routes have a unique
// (destination subnet, metric, table) tuple. Here we replicate the check that Linux
// performs, so that Netlink can reject requests before handing them off to the
// more flexible Netstack routing APIs.
fn new_route_matches_existing<'a, I: Ip>(
    route: &NewRouteArgs<I>,
    existing_routes: impl Iterator<Item = &'a NetlinkRouteMessage>,
) -> NewRouteConflict {
    existing_routes.fold(
        NewRouteConflict::NoConflict,
        |conflict, NetlinkRouteMessage(existing_route)| {
            let UnicastNewRouteArgs { subnet, target: _, priority, table } = match route {
                NewRouteArgs::Unicast(args) => args,
            };
            let RouteNlaView {
                subnet: existing_subnet,
                metric: existing_metric,
                interface_id: _,
                next_hop: _,
                table: existing_table,
            } = view_existing_route_nlas(&existing_route);
            let table_matches =
                existing_table.unwrap_or(&(existing_route.header.table.into())) == table;
            let subnet_matches = {
                if subnet.prefix() != existing_route.header.destination_prefix_length {
                    false
                } else {
                    existing_subnet
                        .map(|dst| crate::netlink_packet::ip_addr_from_route::<I>(&dst))
                        .unwrap_or(Ok(I::UNSPECIFIED_ADDRESS))
                        .is_ok_and(|dst: I::Addr| dst == subnet.network())
                }
            };
            let metric_matches = existing_metric == priority;

            let current_conflict = match (table_matches, subnet_matches, metric_matches) {
                (true, true, true) => NewRouteConflict::ConflictInSameTable,
                (false, true, true) => NewRouteConflict::ConflictInDifferentTable,
                (_, _, _) => NewRouteConflict::NoConflict,
            };
            // Prioritize returning the NewRouteConflict with the most conflicts.
            match (conflict, current_conflict) {
                (NewRouteConflict::ConflictInSameTable, _)
                | (_, NewRouteConflict::ConflictInSameTable) => {
                    NewRouteConflict::ConflictInSameTable
                }
                (NewRouteConflict::ConflictInDifferentTable, _)
                | (_, NewRouteConflict::ConflictInDifferentTable) => {
                    NewRouteConflict::ConflictInDifferentTable
                }
                (NewRouteConflict::NoConflict, NewRouteConflict::NoConflict) => {
                    NewRouteConflict::NoConflict
                }
            }
        },
    )
}

/// TODO(https://fxbug.dev/336382905): Use table as a condition for deleting
/// existing routes once Netstack can support installing otherwise-identical
/// routes via Policy-Based Routing.
/// Select a route for deletion, based on the given deletion arguments.
///
/// Note that Linux and Fuchsia differ on how to specify a route for deletion.
/// Linux is more flexible and allows you specify matchers as arguments, where
/// Fuchsia requires that you exactly specify the route. Here, Linux's matchers
/// are provided in `deletion_args`; Many of the matchers are optional, and an
/// existing route matches the arguments if all provided arguments are equal to
/// the values held by the route. If multiple routes match the arguments, the
/// route with the lowest metric is selected.
fn select_route_for_deletion<I: Ip>(
    deletion_args: DelRouteArgs<I>,
    existing_routes: &HashSet<NetlinkRouteMessage>,
) -> Option<&NetlinkRouteMessage> {
    // Find the set of candidate routes, mapping them to tuples (route, metric).
    existing_routes
        .iter()
        .filter_map(|route| {
            let NetlinkRouteMessage(existing_route) = route;
            let UnicastDelRouteArgs { subnet, outbound_interface, next_hop, priority, table: _ } =
                match deletion_args {
                    DelRouteArgs::Unicast(args) => args,
                };
            if subnet.prefix() != existing_route.header.destination_prefix_length {
                return None;
            }
            let RouteNlaView {
                subnet: existing_subnet,
                metric: existing_metric,
                interface_id: existing_interface,
                next_hop: existing_next_hop,
                table: _,
            } = view_existing_route_nlas(existing_route);
            let subnet_matches = existing_subnet.map_or(!subnet.network().is_specified(), |dst| {
                crate::netlink_packet::ip_addr_from_route::<I>(&dst)
                    .is_ok_and(|dst: I::Addr| dst == subnet.network())
            });
            let metric_matches = priority.map_or(true, |p| p.get() == *existing_metric);
            let interface_matches =
                outbound_interface.map_or(true, |i| i.get() == (*existing_interface).into());
            let next_hop_matches = next_hop.map_or(true, |n| {
                existing_next_hop.map_or(false, |e| {
                    crate::netlink_packet::ip_addr_from_route::<I>(&e)
                        .is_ok_and(|e: I::Addr| e == n.get())
                })
            });
            if subnet_matches && metric_matches && interface_matches && next_hop_matches {
                Some((route, *existing_metric))
            } else {
                None
            }
        })
        // Select the route with the lowest metric
        .min_by(|(_route1, metric1), (_route2, metric2)| metric1.cmp(metric2))
        .map(|(route, _metric)| route)
}

fn get_all_route_messages<
    'a,
    I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
>(
    route_tables: impl Iterator<Item = &'a RouteTable<I>>,
) -> impl Iterator<Item = &'a NetlinkRouteMessage> {
    route_tables
        .map(|RouteTable { route_set_proxy: _, route_messages }| route_messages.iter())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl::endpoints::{ControlHandle, RequestStream};
    use fidl_fuchsia_net_routes as fnet_routes;
    use fidl_fuchsia_net_routes_admin as fnet_routes_admin;

    use assert_matches::assert_matches;
    use futures::{
        channel::mpsc,
        future::{Future, FutureExt as _},
        stream::BoxStream,
        SinkExt as _, Stream,
    };
    use linux_uapi::rtnetlink_groups_RTNLGRP_LINK;
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{GenericOverIp, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
        SpecifiedAddr,
    };
    use netlink_packet_core::NetlinkPayload;
    use std::pin::pin;
    use test_case::test_case;

    use crate::{
        interfaces::testutil::FakeInterfacesHandler,
        messaging::testutil::{FakeSender, SentMessage},
    };

    const V4_DFLT: Subnet<Ipv4Addr> = net_subnet_v4!("0.0.0.0/0");
    const V4_SUB1: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/32");
    const V4_SUB2: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.1/32");
    const V4_SUB3: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/24");
    const V4_NEXTHOP1: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const V4_NEXTHOP2: Ipv4Addr = net_ip_v4!("192.0.2.2");

    const V6_DFLT: Subnet<Ipv6Addr> = net_subnet_v6!("::/0");
    const V6_SUB1: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::/128");
    const V6_SUB2: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::1/128");
    const V6_SUB3: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::/64");
    const V6_NEXTHOP1: Ipv6Addr = net_ip_v6!("2001:db8::1");
    const V6_NEXTHOP2: Ipv6Addr = net_ip_v6!("2001:db8::2");

    const DEV1: u32 = 1;
    const DEV2: u32 = 2;

    const METRIC1: u32 = 1;
    const METRIC2: u32 = 100;
    const METRIC3: u32 = 9999;
    const TEST_SEQUENCE_NUMBER: u32 = 1234;
    const MANAGED_ROUTE_TABLE_ID: u32 = 5678;
    const MANAGED_ROUTE_TABLE: RouteTableKey =
        RouteTableKey::NetlinkManaged { table_id: MANAGED_ROUTE_TABLE_ID };

    fn create_installed_route<I: Ip>(
        subnet: Subnet<I::Addr>,
        next_hop: Option<I::Addr>,
        interface_id: u64,
        metric: u32,
    ) -> fnet_routes_ext::InstalledRoute<I> {
        fnet_routes_ext::InstalledRoute::<I> {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget::<I> {
                    outbound_interface: interface_id,
                    next_hop: next_hop.map(|next_hop| SpecifiedAddr::new(next_hop)).flatten(),
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

    fn create_netlink_route_message<I: Ip>(
        destination_prefix_length: u8,
        table: RouteTableKey,
        nlas: Vec<RouteAttribute>,
    ) -> NetlinkRouteMessage {
        let mut route_header = RouteHeader::default();
        let address_family = match I::VERSION {
            IpVersion::V4 => AddressFamily::Inet,
            IpVersion::V6 => AddressFamily::Inet6,
        }
        .try_into()
        .expect("should fit into u8");
        route_header.address_family = address_family;
        route_header.destination_prefix_length = destination_prefix_length;
        route_header.kind = RouteType::Unicast;

        let (table_u8, _) = get_table_u8_and_nla_from_key(table);
        route_header.table = table_u8;

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.attributes = nlas;

        NetlinkRouteMessage(route_message)
    }

    fn create_nlas<I: Ip>(
        destination: Option<Subnet<I::Addr>>,
        next_hop: Option<I::Addr>,
        outgoing_interface_id: u32,
        metric: u32,
        table: Option<u32>,
    ) -> Vec<RouteAttribute> {
        let mut nlas = vec![];

        let family = match I::VERSION {
            IpVersion::V4 => AddressFamily::Inet,
            IpVersion::V6 => AddressFamily::Inet6,
        };

        if let Some(destination) = destination {
            let destination_nla = RouteAttribute::Destination(
                RouteAddress::parse(family, destination.network().bytes()).unwrap(),
            );
            nlas.push(destination_nla);
        }

        let oif_nla = RouteAttribute::Oif(outgoing_interface_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = RouteAddress::parse(family, next_hop.bytes()).unwrap();
            let gateway_nla = RouteAttribute::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = RouteAttribute::Priority(metric);
        nlas.push(priority_nla);

        if let Some(t) = table {
            let table_nla = RouteAttribute::Table(t);
            nlas.push(table_nla);
        }
        nlas
    }

    // TODO(https://g-issues.fuchsia.dev/issues/42084902): Update to use
    // `ip_test` when async is supported.
    #[test_case(RouteTableKey::Unmanaged)]
    #[test_case(MANAGED_ROUTE_TABLE)]
    #[fuchsia::test]
    async fn test_handle_route_watcher_event_v4(table: RouteTableKey) {
        handle_route_watcher_event_helper::<Ipv4>(V4_SUB1, V4_NEXTHOP1, table).await;
    }

    #[test_case(RouteTableKey::Unmanaged)]
    #[test_case(MANAGED_ROUTE_TABLE)]
    #[fuchsia::test]
    async fn test_handle_route_watcher_event_v6(table: RouteTableKey) {
        handle_route_watcher_event_helper::<Ipv6>(V6_SUB1, V6_NEXTHOP1, table).await;
    }

    async fn handle_route_watcher_event_helper<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        subnet: Subnet<I::Addr>,
        next_hop: I::Addr,
        table: RouteTableKey,
    ) {
        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV1.into(), METRIC1);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV2.into(), METRIC2);

        let add_event1 = fnet_routes_ext::Event::Added(installed_route1);
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);
        let unknown_event: fnet_routes_ext::Event<I> = fnet_routes_ext::Event::Unknown;

        let expected_route_message1: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(installed_route1, table).unwrap();
        let expected_route_message2: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(installed_route2, table).unwrap();

        // An Unmanaged table should never be used in a PendingRouteRequest, because
        // any routes inserted through Netlink will be NetlinkManaged.
        let (add_event1_pending_request, add_event2_pending_request, remove_event_pending_request) = {
            match table {
                RouteTableKey::Unmanaged => (None, None, None),
                RouteTableKey::NetlinkManaged { .. } => (
                    Some(PendingRouteRequestArgs::New(NewRouteArgs::Unicast(
                        create_unicast_new_route_args(
                            subnet,
                            next_hop,
                            DEV1.into(),
                            METRIC1,
                            table,
                        ),
                    ))),
                    Some(PendingRouteRequestArgs::New(NewRouteArgs::Unicast(
                        create_unicast_new_route_args(
                            subnet,
                            next_hop,
                            DEV2.into(),
                            METRIC2,
                            table,
                        ),
                    ))),
                    Some(PendingRouteRequestArgs::<I>::Del(expected_route_message2.clone())),
                ),
            }
        };

        // Set up two fake clients: one is a member of the route multicast group.
        let (right_group, wrong_group) = match I::VERSION {
            IpVersion::V4 => (
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
            ),
            IpVersion::V6 => (
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
            ),
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

        let (route_set_proxy, _route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>().unwrap();

        let mut route_table = HashMap::from([(
            table,
            RouteTable { route_set_proxy, route_messages: HashSet::new() },
        )]);

        // An event that is not an add or remove should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_table, &route_clients, unknown_event, None),
            Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(_))
        );
        assert_eq!(route_table[&table].route_messages.len(), 0);
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                add_event1,
                add_event1_pending_request.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            route_table[&table].route_messages,
            HashSet::from_iter([expected_route_message1.clone()])
        );
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
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                add_event1,
                add_event1_pending_request.clone(),
            ),
            Err(RouteEventHandlerError::AlreadyExistingRouteAddition(_))
        );
        assert_eq!(
            route_table[&table].route_messages,
            HashSet::from_iter([expected_route_message1.clone()])
        );
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding a different route should result in an addition.
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                add_event2,
                add_event2_pending_request.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            route_table[&table].route_messages,
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
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                remove_event,
                remove_event_pending_request.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            route_table[&table].route_messages,
            HashSet::from_iter([expected_route_message2.clone()])
        );
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
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                remove_event,
                remove_event_pending_request,
            ),
            Err(RouteEventHandlerError::NonExistentRouteDeletion(_))
        );
        assert_eq!(
            route_table[&table].route_messages,
            HashSet::from_iter([expected_route_message2.clone()])
        );
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }

    // TODO(https://g-issues.fuchsia.dev/issues/42084902): Update to use
    // `ip_test` when async is supported.
    #[fuchsia::test]
    async fn test_handle_route_watcher_event_two_routesets_v4() {
        handle_route_watcher_event_two_routesets_helper::<Ipv4>(V4_SUB1, V4_NEXTHOP1).await;
    }

    #[fuchsia::test]
    async fn test_handle_route_watcher_event_two_routesets_v6() {
        handle_route_watcher_event_two_routesets_helper::<Ipv6>(V6_SUB1, V6_NEXTHOP1).await;
    }

    async fn handle_route_watcher_event_two_routesets_helper<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        subnet: Subnet<I::Addr>,
        next_hop: I::Addr,
    ) {
        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV1.into(), METRIC1);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV2.into(), METRIC2);

        let add_event1 = fnet_routes_ext::Event::Added(installed_route1);
        let add_event1_pending_request =
            PendingRouteRequestArgs::New(NewRouteArgs::Unicast(create_unicast_new_route_args(
                subnet,
                next_hop,
                DEV1.into(),
                METRIC1,
                MANAGED_ROUTE_TABLE,
            )));
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);

        let expected_route_message1: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(installed_route1, MANAGED_ROUTE_TABLE)
                .unwrap();
        let expected_route_message2: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route2,
                RouteTableKey::Unmanaged,
            )
            .unwrap();

        // Set up two fake clients: one is a member of the route multicast group.
        let (right_group, wrong_group) = match I::VERSION {
            IpVersion::V4 => (
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
            ),
            IpVersion::V6 => (
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
            ),
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

        let (unmanaged_route_set_proxy, _unmanaged_route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>().unwrap();
        let (managed_route_set_proxy, _managed_route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>().unwrap();

        let mut route_table = HashMap::from([
            (
                RouteTableKey::Unmanaged,
                RouteTable {
                    route_set_proxy: unmanaged_route_set_proxy,
                    route_messages: HashSet::new(),
                },
            ),
            (
                MANAGED_ROUTE_TABLE,
                RouteTable {
                    route_set_proxy: managed_route_set_proxy,
                    route_messages: HashSet::new(),
                },
            ),
        ]);

        // Ensure that the route gets added to the table managed by Netlink.
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &route_clients,
                add_event1,
                Some(add_event1_pending_request.clone())
            ),
            Ok(())
        );
        assert_eq!(
            route_table[&MANAGED_ROUTE_TABLE].route_messages,
            HashSet::from_iter([expected_route_message1.clone()])
        );
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

        // Ensure that an unmanaged Route can be observed and added to the
        // unmanaged route set (signified by no pending request).
        assert_eq!(
            handle_route_watcher_event(&mut route_table, &route_clients, add_event2, None),
            Ok(())
        );
        assert_eq!(
            route_table[&RouteTableKey::Unmanaged].route_messages,
            HashSet::from_iter([expected_route_message2.clone()])
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

        // TODO(https://fxbug.dev/336382905): Update this assertion to enable specifying
        // the table to select a route to delete between two otherwise equivalent routes.
        // Ensure that the route gets removed properly from the managed route set
        // even when there is no pending request.
        assert_eq!(
            handle_route_watcher_event(&mut route_table, &route_clients, remove_event, None),
            Ok(())
        );
        assert_eq!(
            route_table[&RouteTableKey::Unmanaged].route_messages,
            HashSet::from_iter([expected_route_message2.clone()])
        );
        assert!(route_table[&MANAGED_ROUTE_TABLE].route_messages.is_empty());
        assert_eq!(
            &right_sink.take_messages()[..],
            &[SentMessage::multicast(
                expected_route_message1.clone().into_rtnl_del_route(),
                right_group
            )]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }

    #[test_case(V4_SUB1, V4_NEXTHOP1)]
    #[test_case(V6_SUB1, V6_NEXTHOP1)]
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
            create_installed_route::<I>(subnet, Some(next_hop), DEV1.into(), METRIC1);
        let prefix_length = subnet.prefix();
        let subnet = if prefix_length > 0 { Some(subnet) } else { None };
        let nlas = create_nlas::<I>(subnet, Some(next_hop), DEV1, METRIC1, None);
        let table = RouteTableKey::Unmanaged;
        let expected = create_netlink_route_message::<I>(prefix_length, table, nlas);

        let actual = NetlinkRouteMessage::try_from_installed_route(installed_route, table).unwrap();
        assert_eq!(actual, expected);
    }

    #[test_case(V4_SUB1)]
    #[test_case(V6_SUB1)]
    fn test_non_forward_route_conversion<A: IpAddress>(subnet: Subnet<A>) {
        let installed_route = fnet_routes_ext::InstalledRoute::<A::Version> {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Unknown,
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(METRIC1),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: METRIC1 },
        };

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route,
                RouteTableKey::Unmanaged,
            );
        assert_eq!(actual, Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding));
    }

    #[fuchsia::test]
    fn test_oversized_interface_id_route_conversion() {
        let invalid_interface_id = (u32::MAX as u64) + 1;
        let installed_route: fnet_routes_ext::InstalledRoute<Ipv4> = create_installed_route(
            V4_SUB1,
            Some(V4_NEXTHOP1),
            invalid_interface_id,
            Default::default(),
        );

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route,
                RouteTableKey::Unmanaged,
            );
        assert_eq!(
            actual,
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );
    }

    #[test]
    fn test_into_rtnl_new_route_is_serializable() {
        let route = create_netlink_route_message::<Ipv4>(0, RouteTableKey::Unmanaged, vec![]);
        let new_route_message = route.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false);
        let mut buf = vec![0; new_route_message.buffer_len()];
        // Serialize will panic if `new_route_message` is malformed.
        new_route_message.serialize(&mut buf);
    }

    #[test]
    fn test_into_rtnl_del_route_is_serializable() {
        let route = create_netlink_route_message::<Ipv6>(0, RouteTableKey::Unmanaged, vec![]);
        let del_route_message = route.into_rtnl_del_route();
        let mut buf = vec![0; del_route_message.buffer_len()];
        // Serialize will panic if `del_route_message` is malformed.
        del_route_message.serialize(&mut buf);
    }

    #[test_case(V4_SUB1, V4_NEXTHOP1)]
    #[test_case(V6_SUB1, V6_NEXTHOP1)]
    fn test_new_set_with_existing_routes<A: IpAddress>(subnet: Subnet<A>, next_hop: A) {
        new_set_with_existing_routes_helper::<A::Version>(subnet, next_hop);
    }

    fn new_set_with_existing_routes_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let interface_id = u32::MAX;

        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), interface_id as u64, METRIC1);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), (interface_id as u64) + 1, METRIC2);
        let routes: HashSet<fnet_routes_ext::InstalledRoute<I>> =
            vec![installed_route1, installed_route2].into_iter().collect::<_>();

        // One `InstalledRoute` has an invalid interface id, so it should be removed in
        // the conversion to the `NetlinkRouteMessage` HashSet.
        let actual = new_set_with_existing_routes::<I>(routes);
        assert_eq!(actual.len(), 1);

        let nlas = create_nlas::<I>(Some(subnet), Some(next_hop), interface_id, METRIC1, None);
        let netlink_route_message =
            create_netlink_route_message::<I>(subnet.prefix(), RouteTableKey::Unmanaged, nlas);
        let expected: HashSet<NetlinkRouteMessage> =
            vec![netlink_route_message].into_iter().collect::<_>();
        assert_eq!(actual, expected);
    }

    struct Setup<W, R, B> {
        pub event_loop:
            crate::eventloop::EventLoop<FakeInterfacesHandler, FakeSender<RouteNetlinkMessage>>,
        pub watcher_stream: W,
        pub route_set_stream: R,
        pub interfaces_request_stream: fnet_root::InterfacesRequestStream,
        pub request_sink:
            mpsc::Sender<crate::eventloop::UnifiedRequest<FakeSender<RouteNetlinkMessage>>>,
        pub background_work: B,
    }

    fn setup_with_route_clients<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RouteNetlinkMessage>>,
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<Item = <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Future<Output = ()>,
    > {
        let (interfaces_handler, _interfaces_handler_sink) = FakeInterfacesHandler::new();
        let (request_sink, request_stream) = mpsc::channel(1);
        let (
            event_loop,
            crate::eventloop::testutil::EventLoopServerEnd {
                interfaces,
                interfaces_state,
                v4_routes_state,
                v6_routes_state,
                v4_routes_set_provider,
                v6_routes_set_provider,
            },
        ) = crate::eventloop::testutil::event_loop_fixture(
            interfaces_handler,
            route_clients,
            request_stream,
        );

        let interfaces_state_background_work =
            crate::eventloop::testutil::serve_empty_interfaces_state(interfaces_state);

        let interfaces_request_stream = interfaces.into_stream().expect("into stream");

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct StateStreamWrapper<'a, I: fnet_routes_ext::FidlRouteIpExt>(
            futures::stream::LocalBoxStream<
                'a,
                <<I::StateMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            >,
        );

        let (StateStreamWrapper(state_stream), IpInvariant(routes_state_background_work)) =
            I::map_ip(
                IpInvariant((v4_routes_state, v6_routes_state)),
                |IpInvariant((main_state, other_state))| {
                    let main_stream = main_state.into_stream().expect("into stream");
                    (
                        StateStreamWrapper(main_stream.boxed_local()),
                        IpInvariant(
                            crate::eventloop::testutil::serve_empty_routes::<Ipv6>(other_state)
                                .boxed_local(),
                        ),
                    )
                },
                |IpInvariant((other_state, main_state))| {
                    let main_stream = main_state.into_stream().expect("into stream");
                    (
                        StateStreamWrapper(main_stream.boxed_local()),
                        IpInvariant(
                            crate::eventloop::testutil::serve_empty_routes::<Ipv4>(other_state)
                                .boxed_local(),
                        ),
                    )
                },
            );

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct RouteSetStreamWrapper<'a, I: fnet_routes_ext::admin::FidlRouteAdminIpExt>(
            BoxStream<'a, <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        );

        let (RouteSetStreamWrapper(route_set_stream), IpInvariant(route_set_background_work)) =
            I::map_ip(
                IpInvariant((v4_routes_set_provider, v6_routes_set_provider)),
                |IpInvariant((main_set_provider, other_set_provider))| {
                    let main_stream = fnet_routes_ext::testutil::admin::serve_all_route_sets::<Ipv4>(
                        main_set_provider,
                    );
                    (
                        RouteSetStreamWrapper(main_stream.boxed()),
                        IpInvariant(
                            fnet_routes_ext::testutil::admin::serve_noop_route_sets::<Ipv6>(
                                other_set_provider,
                            )
                            .boxed(),
                        ),
                    )
                },
                |IpInvariant((other_set_provider, main_set_provider))| {
                    let main_stream = fnet_routes_ext::testutil::admin::serve_all_route_sets::<Ipv6>(
                        main_set_provider,
                    );
                    (
                        RouteSetStreamWrapper(main_stream.boxed()),
                        IpInvariant(
                            fnet_routes_ext::testutil::admin::serve_noop_route_sets::<Ipv4>(
                                other_set_provider,
                            )
                            .boxed(),
                        ),
                    )
                },
            );

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct StateRequestWrapper<I: fnet_routes_ext::FidlRouteIpExt> {
            request: <<I::StateMarker as ProtocolMarker>::RequestStream as futures::Stream>::Item,
        }

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct WatcherRequestWrapper<I: fnet_routes_ext::FidlRouteIpExt> {
            watcher: <I::WatcherMarker as ProtocolMarker>::RequestStream,
        }

        let watcher_stream = state_stream
            .map(|request| {
                let wrapper = I::map_ip(
                    StateRequestWrapper { request },
                    |StateRequestWrapper { request }| match request.expect("watcher stream error") {
                        fnet_routes::StateV4Request::GetWatcherV4 {
                            options: _,
                            watcher,
                            control_handle: _,
                        } => WatcherRequestWrapper { watcher: watcher.into_stream().unwrap() },
                        fnet_routes::StateV4Request::GetRuleWatcherV4 {
                            options: _,
                            watcher: _,
                            control_handle: _,
                        } => todo!("TODO(https://fxbug.dev/336204757): Implement rules watcher"),
                    },
                    |StateRequestWrapper { request }| match request.expect("watcher stream error") {
                        fnet_routes::StateV6Request::GetWatcherV6 {
                            options: _,
                            watcher,
                            control_handle: _,
                        } => WatcherRequestWrapper { watcher: watcher.into_stream().unwrap() },
                        fnet_routes::StateV6Request::GetRuleWatcherV6 {
                            options: _,
                            watcher: _,
                            control_handle: _,
                        } => todo!("TODO(https://fxbug.dev/336204757): Implement rules watcher"),
                    },
                );
                wrapper
            })
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
            background_work: async move {
                let ((), (), ()) = futures::join!(
                    interfaces_state_background_work,
                    routes_state_background_work,
                    route_set_background_work,
                );
            },
        }
    }

    fn setup<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>(
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<Item = <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Future<Output = ()>,
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
        #[generic_over_ip(I, Ip)]
        struct HandleInputs<I: fnet_routes_ext::FidlRouteIpExt> {
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

    #[test_case(V4_SUB1, V4_NEXTHOP1)]
    #[test_case(V6_SUB1, V6_NEXTHOP1)]
    #[fuchsia::test]
    async fn test_event_loop_event_errors<A: IpAddress>(subnet: Subnet<A>, next_hop: A)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let route = create_installed_route(subnet, Some(next_hop), DEV1.into(), METRIC1);

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
            background_work,
        } = setup::<I>();
        let event_loop_fut = pin!(event_loop.run());
        let watcher_fut = pin!(respond_to_watcher_with_routes(watcher_stream, [route], None));

        let ((err, ()), _incomplete_background_work) = futures::future::select(
            futures::future::join(event_loop_fut, watcher_fut),
            pin!(background_work.map(|_output| unreachable!())),
        )
        .await
        .factor_first();

        match I::VERSION {
            IpVersion::V4 => {
                assert_matches!(
                    err.unwrap_err().downcast::<crate::eventloop::EventStreamError>().unwrap(),
                    crate::eventloop::EventStreamError::RoutesV4(
                        fnet_routes_ext::WatchError::Fidl(fidl::Error::ClientChannelClosed { .. })
                    )
                );
            }
            IpVersion::V6 => {
                assert_matches!(
                    err.unwrap_err().downcast::<crate::eventloop::EventStreamError>().unwrap(),
                    crate::eventloop::EventStreamError::RoutesV6(
                        fnet_routes_ext::WatchError::Fidl(fidl::Error::ClientChannelClosed { .. })
                    )
                );
            }
        }
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
            background_work,
        } = setup::<I>();
        let event_loop_fut = pin!(event_loop.run());
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Existing(route.clone());
        let watcher_fut =
            pin!(respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event)));

        let ((err, ()), _incomplete_background_work) = futures::future::select(
            futures::future::join(event_loop_fut, watcher_fut),
            pin!(background_work.map(|_output| unreachable!())),
        )
        .await
        .factor_first();

        assert_matches!(
            err.unwrap_err().downcast::<RouteEventHandlerError<I>>().unwrap(),
            RouteEventHandlerError::NonAddOrRemoveEventReceived(
                fnet_routes_ext::Event::Existing(res)
            ) => {
                assert_eq!(res, route);
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
            background_work,
        } = setup::<I>();
        let event_loop_fut = pin!(event_loop.run());
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Added(route.clone());
        let watcher_fut =
            pin!(respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event)));

        let ((err, ()), _incomplete_background_work) = futures::future::select(
            futures::future::join(event_loop_fut, watcher_fut),
            pin!(background_work.map(|_output| unreachable!())),
        )
        .await
        .factor_first();

        assert_matches!(
            err.unwrap_err().downcast::<RouteEventHandlerError<I>>().unwrap(),
            RouteEventHandlerError::AlreadyExistingRouteAddition(
                res
            ) => {
                assert_eq!(res, route);
            }
        );
    }

    fn get_test_route_events_new_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
    ) -> impl IntoIterator<Item = RequestArgs<A::Version>>
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt,
    {
        vec![
            RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                create_unicast_new_route_args(
                    subnet,
                    next_hop1,
                    DEV1.into(),
                    METRIC1,
                    MANAGED_ROUTE_TABLE,
                ),
            ))),
            RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                create_unicast_new_route_args(
                    subnet,
                    next_hop2,
                    DEV2.into(),
                    METRIC2,
                    MANAGED_ROUTE_TABLE,
                ),
            ))),
        ]
    }

    fn create_unicast_new_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: A,
        interface_id: u64,
        priority: u32,
        table: RouteTableKey,
    ) -> UnicastNewRouteArgs<A::Version> {
        UnicastNewRouteArgs {
            subnet,
            target: fnet_routes_ext::RouteTarget {
                outbound_interface: interface_id,
                next_hop: SpecifiedAddr::new(next_hop),
            },
            priority,
            table: table.into(),
        }
    }

    fn create_unicast_del_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: Option<A>,
        interface_id: Option<u64>,
        priority: Option<u32>,
        table: RouteTableKey,
    ) -> UnicastDelRouteArgs<A::Version> {
        UnicastDelRouteArgs {
            subnet,
            outbound_interface: interface_id.map(NonZeroU64::new).flatten(),
            next_hop: next_hop.map(SpecifiedAddr::new).flatten(),
            priority: priority.map(NonZeroU32::new).flatten(),
            table: NonZeroU32::new(table.into()).unwrap(),
        }
    }

    #[derive(Debug, PartialEq)]
    struct TestRequestResult {
        messages: Vec<SentMessage<RouteNetlinkMessage>>,
        waiter_results: Vec<Result<(), RequestError>>,
    }

    /// Test helper to handle an iterator of route requests
    /// using the same clients and event loop.
    ///
    /// `root_handler` returns a future that handles
    /// `fnet_root::InterfacesRequest`s.
    async fn test_requests<
        A: IpAddress,
        Fut: Future<Output = ()>,
        F: FnOnce(fnet_root::InterfacesRequestStream) -> Fut,
    >(
        args: impl IntoIterator<Item = RequestArgs<A::Version>>,
        root_handler: F,
        route_set_results: impl ExactSizeIterator<Item = RouteSetResult>,
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
        num_sink_messages: usize,
    ) -> TestRequestResult
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (mut route_sink, route_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[ModernGroup(match A::Version::VERSION {
                IpVersion::V4 => rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
                IpVersion::V6 => rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
            })],
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[ModernGroup(rtnetlink_groups_RTNLGRP_LINK)],
        );
        let Setup {
            event_loop,
            mut watcher_stream,
            mut route_set_stream,
            interfaces_request_stream,
            request_sink,
            background_work,
        } = setup_with_route_clients::<A::Version>({
            let route_clients = ClientTable::default();
            route_clients.add_client(route_client.clone());
            route_clients.add_client(other_client);
            route_clients
        });
        let mut event_loop_fut = pin!(event_loop
            .run()
            .map(|res| match res {
                Ok(never) => match never {},
                Err(e) => {
                    log_debug!("event_loop_fut exiting with error {:?}", e);
                    Err::<std::convert::Infallible, _>(e)
                }
            })
            .fuse());
        let mut background_work = pin!(background_work.fuse());

        let watcher_stream_fut = respond_to_watcher::<A::Version, _>(
            watcher_stream.by_ref(),
            std::iter::once(fnet_routes_ext::Event::<A::Version>::Idle.try_into().unwrap()),
        );
        futures::select! {
            () = watcher_stream_fut.fuse() => {},
            () = background_work => {},
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }
        assert_eq!(&route_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

        let route_client = &route_client;
        let fut = async {
            // Add some initial route state by sending through PendingRequests.
            let request_sink = futures::stream::iter(get_test_route_events_new_route_args(
                subnet, next_hop1, next_hop2,
            ))
            .fold(request_sink, |mut request_sink, args| async move {
                let (completer, waiter) = oneshot::channel();
                request_sink
                    .send(
                        Request {
                            args,
                            sequence_number: TEST_SEQUENCE_NUMBER,
                            client: route_client.clone(),
                            completer,
                        }
                        .into(),
                    )
                    .await
                    .unwrap();
                assert_matches!(waiter.await.unwrap(), Ok(()));
                request_sink
            })
            .await;

            // Ensure these messages to load the initial route state are
            // received prior to handling the next requests. The messages for
            // these requests are not needed by the callers, so drop them.
            let _ = route_sink.next_message().await;
            let _ = route_sink.next_message().await;
            assert_eq!(route_sink.next_message().now_or_never(), None);

            let (results, _request_sink) = futures::stream::iter(args)
                .fold(
                    (Vec::new(), request_sink),
                    |(mut results, mut request_sink), args| async move {
                        let (completer, waiter) = oneshot::channel();
                        request_sink
                            .send(
                                Request {
                                    args,
                                    sequence_number: TEST_SEQUENCE_NUMBER,
                                    client: route_client.clone(),
                                    completer,
                                }
                                .into(),
                            )
                            .await
                            .unwrap();
                        results.push(waiter.await.unwrap());
                        (results, request_sink)
                    },
                )
                .await;

            let messages = {
                assert_eq!(&other_sink.take_messages()[..], &[]);
                let mut messages = Vec::new();
                while messages.len() < num_sink_messages {
                    messages.push(route_sink.next_message().await);
                }
                assert_eq!(route_sink.next_message().now_or_never(), None);
                messages
            };

            (messages, results)
        };

        let route_set_fut = respond_to_route_set_modifications::<A::Version, _, _>(
            route_set_stream.by_ref(),
            watcher_stream.by_ref(),
            route_set_results,
        )
        .fuse();

        let root_interfaces_fut = root_handler(interfaces_request_stream).fuse();

        let (messages, results) = futures::select! {
            (messages, results) = fut.fuse() => (messages, results),
            res = futures::future::join4(
                    route_set_fut,
                    root_interfaces_fut,
                    event_loop_fut,
                    background_work,
                ) => {
                unreachable!("eventloop/stream handlers should not return: {res:?}")
            }
        };

        TestRequestResult { messages, waiter_results: results }
    }

    #[test_case(V4_SUB1, V4_NEXTHOP1, V4_NEXTHOP2; "v4_route_dump")]
    #[test_case(V6_SUB1, V6_NEXTHOP1, V6_NEXTHOP2; "v6_route_dump")]
    #[fuchsia::test]
    async fn test_get_route<A: IpAddress>(subnet: Subnet<A>, next_hop1: A, next_hop2: A)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let expected_messages = vec![
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop2),
                        DEV2,
                        METRIC2,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
        ];

        pretty_assertions::assert_eq!(
            {
                let mut test_request_result = test_requests(
                    [RequestArgs::Route(RouteRequestArgs::Get(GetRouteArgs::Dump))],
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
                    expected_messages.len(),
                )
                .await;
                test_request_result.messages.sort_by_key(|message| {
                    assert_matches!(
                        &message.message.payload,
                        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(m)) => {
                            // We expect there to be exactly one Oif NLA present
                            // for the given inputs.
                            m.attributes.clone().into_iter().filter_map(|nla|
                                match nla {
                                    RouteAttribute::Oif(interface_id) =>
                                        Some((m.header.address_family, interface_id)),
                                    RouteAttribute::Destination(_)
                                    | RouteAttribute::Gateway(_)
                                    | RouteAttribute::Priority(_)
                                    | RouteAttribute::Table(_) => None,
                                    _ => panic!("unexpected NLA {nla:?} present in payload"),
                                }
                            ).next()
                        }
                    )
                });
                test_request_result
            },
            TestRequestResult { messages: expected_messages, waiter_results: vec![Ok(())] },
        )
    }

    #[derive(Debug)]
    enum RouteSetResult {
        AddResult(Result<bool, fnet_routes_admin::RouteSetError>),
        DelResult(Result<bool, fnet_routes_admin::RouteSetError>),
        AuthenticationResult(Result<(), fnet_routes_admin::AuthenticateForInterfaceError>),
    }

    fn route_event_from_route<
        I: Ip + fnet_routes_ext::FidlRouteIpExt,
        F: FnOnce(fnet_routes_ext::InstalledRoute<I>) -> fnet_routes_ext::Event<I>,
    >(
        route: I::Route,
        event_fn: F,
    ) -> I::WatchEvent {
        let route: fnet_routes_ext::Route<I> = route.try_into().unwrap();

        let metric = match route.properties.specified_properties.metric {
            fnet_routes::SpecifiedMetric::ExplicitMetric(metric) => metric,
            fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty) => {
                panic!("metric should be explicit")
            }
        };

        event_fn(fnet_routes_ext::InstalledRoute {
            route,
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        })
        .try_into()
        .unwrap()
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
        #[generic_over_ip(I, Ip)]
        struct RouteSetInputs<I: fnet_routes_ext::admin::FidlRouteAdminIpExt> {
            request: <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            route_set_result: RouteSetResult,
        }
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct RouteSetOutputs<I: fnet_routes_ext::FidlRouteIpExt> {
            event: Option<I::WatchEvent>,
        }

        route_stream
            .zip(futures::stream::iter(
                // TODO(https://fxbug.dev/337297829): Cleanup - add these `RouteSetResult`s to the
                // `RouteSetResult` iterator instead of prepending them here.
                // Handle the add requests for the initial two routes added to the route stream
                // from `get_test_route_events_new_route_args`.
                vec![RouteSetResult::AddResult(Ok(true)), RouteSetResult::AddResult(Ok(true))]
                    .into_iter()
                    .chain(route_set_results),
            ))
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
                                    Ok(true) => Some(route_event_from_route::<Ipv4, _>(
                                        route,
                                        fnet_routes_ext::Event::<Ipv4>::Added,
                                    )),
                                    _ => None,
                                },
                            }
                        }
                        fnet_routes_admin::RouteSetV4Request::RemoveRoute { route, responder } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::DelResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `RemoveRoute`");

                            RouteSetOutputs {
                                event: match route_set_result {
                                    Ok(true) => Some(route_event_from_route::<Ipv4, _>(
                                        route,
                                        fnet_routes_ext::Event::<Ipv4>::Removed,
                                    )),
                                    _ => None,
                                },
                            }
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
                                    Ok(true) => Some(route_event_from_route::<Ipv6, _>(
                                        route,
                                        fnet_routes_ext::Event::<Ipv6>::Added,
                                    )),
                                    _ => None,
                                },
                            }
                        }
                        fnet_routes_admin::RouteSetV6Request::RemoveRoute { route, responder } => {
                            let route_set_result = assert_matches!(
                                route_set_result,
                                RouteSetResult::DelResult(res) => res
                            );

                            responder
                                .send(route_set_result)
                                .expect("failed to respond to `RemoveRoute`");

                            RouteSetOutputs {
                                event: match route_set_result {
                                    Ok(true) => Some(route_event_from_route::<Ipv6, _>(
                                        route,
                                        fnet_routes_ext::Event::<Ipv6>::Removed,
                                    )),
                                    _ => None,
                                },
                            }
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
                respond_to_watcher::<I, _>(st.by_ref(), events).await;
                Ok::<_, std::convert::Infallible>(st)
            }))
            .await
            .unwrap();
    }

    /// A test helper to exercise multiple route requests.
    ///
    /// A test helper that calls the provided callback with a
    /// [`fnet_interfaces_admin::ControlRequest`] as they arrive.
    async fn test_route_requests<
        A: IpAddress,
        Fut: Future<Output = ()>,
        F: FnMut(fnet_interfaces_admin::ControlRequest) -> Fut,
    >(
        args: impl IntoIterator<Item = RequestArgs<A::Version>>,
        mut control_request_handler: F,
        route_set_results: impl ExactSizeIterator<Item = RouteSetResult>,
        subnet: Subnet<A>,
        next_hop1: A,
        next_hop2: A,
        num_sink_messages: usize,
    ) -> TestRequestResult
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        test_requests(
            args,
            |interfaces_request_stream| async move {
                interfaces_request_stream
                    .filter_map(|req| {
                        futures::future::ready(match req.unwrap() {
                            fnet_root::InterfacesRequest::GetAdmin {
                                id,
                                control,
                                control_handle: _,
                            } => {
                                pretty_assertions::assert_eq!(id, DEV1 as u64);
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
            num_sink_messages,
        )
        .await
    }

    // A test helper that calls `test_route_requests()` with the provided
    // inputs and expected values.
    async fn test_route_requests_helper<A: IpAddress>(
        args: impl IntoIterator<Item = RequestArgs<A::Version>>,
        expected_messages: Vec<SentMessage<RouteNetlinkMessage>>,
        route_set_results: Vec<RouteSetResult>,
        waiter_results: Vec<Result<(), RequestError>>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (V4_NEXTHOP1, V4_NEXTHOP2),
            |()| (V6_NEXTHOP1, V6_NEXTHOP2),
        );

        pretty_assertions::assert_eq!(
            {
                let mut test_request_result = test_route_requests(
                    args,
                    |req| async {
                        match req {
                            fnet_interfaces_admin::ControlRequest::GetAuthorizationForInterface {
                                responder,
                            } => {
                                let token = fidl::Event::create();
                                let grant = fnet_interfaces_admin::GrantForInterfaceAuthorization {
                                    interface_id: DEV1 as u64,
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
                    next_hop2,
                    expected_messages.len(),
                )
                .await;
                test_request_result.messages.sort_by_key(|message| {
                    // The sequence number sorts multicast messages prior to
                    // unicast messages.
                    let sequence_number = message.message.header.sequence_number;
                    assert_matches!(
                        &message.message.payload,
                        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(m))
                        | NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelRoute(m)) => {
                            // We expect there to be exactly one Priority NLA present
                            // for the given inputs.
                            m.attributes.clone().into_iter().filter_map(|nla|
                                match nla {
                                    RouteAttribute::Priority(priority) =>
                                        Some((sequence_number, priority)),
                                    RouteAttribute::Destination(_)
                                    | RouteAttribute::Gateway(_)
                                    | RouteAttribute::Oif(_)
                                    | RouteAttribute::Table(_) => None,
                                    _ => panic!("unexpected NLA {nla:?} present in payload"),
                                }
                            ).next()
                        }
                    )
                });
                test_request_result
            },
            TestRequestResult { messages: expected_messages, waiter_results },
        )
    }

    enum RouteRequestKind {
        New,
        Del,
    }

    // Tests RTM_NEWROUTE with all interesting responses to add a route.
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        Some(METRIC3);
        "v4_new_success")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        Some(METRIC3);
        "v6_new_success")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        V4_SUB1,
        Some(METRIC3);
        "v4_new_failed_auth")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        V6_SUB1,
        Some(METRIC3);
        "v6_new_failed_auth")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        V4_SUB1,
        Some(METRIC3);
        "v4_new_failed_netstack_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        V6_SUB1,
        Some(METRIC3);
        "v6_new_failed_netstack_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V4_SUB1,
        Some(METRIC1);
        "v4_new_failed_netlink_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V6_SUB1,
        Some(METRIC1);
        "v6_new_failed_netlink_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        Some(METRIC3);
        "v4_new_invalid_dest")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        Some(METRIC3);
        "v6_new_invalid_dest")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        Some(METRIC3);
        "v4_new_invalid_hop")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        Some(METRIC3);
        "v6_new_invalid_hop")]
    // Tests RTM_DELROUTE with all interesting responses to remove a route.
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        None;
        "v4_del_success_only_subnet")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        Some(METRIC1);
        "v4_del_success_only_subnet_metric")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        None;
        "v6_del_success_only_subnet")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        Some(METRIC1);
        "v6_del_success_only_subnet_metric")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        V4_SUB1,
        None;
        "v4_del_failed_auth")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Err(
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            )),
        ],
        Err(RequestError::UnrecognizedInterface),
        V6_SUB1,
        None;
        "v6_del_failed_auth")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(false))
        ],
        Err(RequestError::DeletionNotAllowed),
        V4_SUB1,
        None;
        "v4_del_failed_attempt_to_delete_route_from_global_set")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(false))
        ],
        Err(RequestError::DeletionNotAllowed),
        V6_SUB1,
        None;
        "v6_del_failed_attempt_to_delete_route_from_global_set")]
    // This deliberately only includes one case where a route is
    // not selected for deletion, `test_select_route_for_deletion`
    // covers these cases.
    // No route with `METRIC3` exists, so this extra selector causes the
    // `NotFound` result.
    #[test_case(
        RouteRequestKind::Del,
        vec![],
        Err(RequestError::NotFound),
        V4_SUB1,
        Some(METRIC3);
        "v4_del_no_matching_route")]
    #[test_case(
        RouteRequestKind::Del,
        vec![],
        Err(RequestError::NotFound),
        V6_SUB1,
        Some(METRIC3);
        "v6_del_no_matching_route")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        None;
        "v4_del_invalid_dest")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        None;
        "v6_del_invalid_dest")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        None;
        "v4_del_invalid_hop")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        None;
        "v6_del_invalid_hop")]
    #[fuchsia::test]
    async fn test_new_del_route<A: IpAddress>(
        kind: RouteRequestKind,
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
        metric: Option<u32>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let route_group = match A::Version::VERSION {
            IpVersion::V4 => ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
            IpVersion::V6 => ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        };

        let next_hop: A = A::Version::map_ip((), |()| V4_NEXTHOP1, |()| V6_NEXTHOP1);

        // There are two pre-set routes in `test_route_requests`.
        // * subnet, next_hop1, DEV1, METRIC1, MANAGED_ROUTE_TABLE
        // * subnet, next_hop2, DEV2, METRIC2, MANAGED_ROUTE_TABLE
        let route_req_args = match kind {
            RouteRequestKind::New => {
                // Add a route that is not already present.
                RouteRequestArgs::New(NewRouteArgs::Unicast(create_unicast_new_route_args(
                    subnet,
                    next_hop,
                    DEV1.into(),
                    metric.expect("add cases should be Some"),
                    MANAGED_ROUTE_TABLE,
                )))
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                    subnet,
                    None,
                    None,
                    metric,
                    MANAGED_ROUTE_TABLE,
                )))
            }
        };

        // When the waiter result is Ok(()), then we know that the add or delete
        // was successful and we got a message.
        let messages = match waiter_result {
            Ok(()) => {
                let route_message = create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop),
                        DEV1,
                        match kind {
                            RouteRequestKind::New => metric.expect("add cases should be some"),
                            // When a route is found for deletion, we expect that route to have
                            // a metric value of `METRIC1`. Even though there are two different
                            // routes with `subnet`, deletion prefers to select the route with
                            // the lowest metric.
                            RouteRequestKind::Del => METRIC1,
                        },
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                );

                let netlink_message = match kind {
                    RouteRequestKind::New => {
                        route_message.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false)
                    }
                    RouteRequestKind::Del => route_message.into_rtnl_del_route(),
                };

                Vec::from([SentMessage::multicast(netlink_message, route_group)])
            }
            Err(_) => Vec::new(),
        };

        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            messages,
            route_set_results,
            vec![waiter_result],
            subnet,
        )
        .await;
    }

    // Tests RTM_NEWROUTE and RTM_DELROUTE when two unauthentication events are received - once
    // prior to making an attempt to authenticate and once after attempting to authenticate.
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1;
        "v4_new_unauthenticated")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1;
        "v6_new_unauthenticated")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1;
        "v4_del_unauthenticated")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
            RouteSetResult::AuthenticationResult(Ok(())),
            RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated)),
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1;
        "v6_del_unauthenticated")]
    #[should_panic(expected = "received unauthentication error from route set for route")]
    #[fuchsia::test]
    async fn test_new_del_route_failed<A: IpAddress>(
        kind: RouteRequestKind,
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let route_req_args = match kind {
            RouteRequestKind::New => {
                let next_hop: A = A::Version::map_ip((), |()| V4_NEXTHOP1, |()| V6_NEXTHOP1);
                // Add a route that is not already present.
                RouteRequestArgs::New(NewRouteArgs::Unicast(create_unicast_new_route_args(
                    subnet,
                    next_hop,
                    DEV1.into(),
                    METRIC3,
                    MANAGED_ROUTE_TABLE,
                )))
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                    subnet,
                    None,
                    None,
                    None,
                    MANAGED_ROUTE_TABLE,
                )))
            }
        };
        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            Vec::new(),
            route_set_results,
            vec![waiter_result],
            subnet,
        )
        .await;
    }

    #[test_case(
        Err(RequestError::NotFound),
        V4_SUB1; "v4_del")]
    #[test_case(
        Err(RequestError::NotFound),
        V6_SUB1; "v6_del")]
    #[fuchsia::test]
    async fn test_del_route_nonexistent_table<A: IpAddress>(
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        // Remove a route from a table that doesn't exist yet.
        let route_req_args =
            RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                subnet,
                None,
                None,
                None,
                RouteTableKey::NetlinkManaged { table_id: 1234 },
            )));
        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            Vec::new(),
            vec![],
            vec![waiter_result],
            subnet,
        )
        .await;
    }

    /// A test to exercise a `RTM_NEWROUTE` followed by a `RTM_GETROUTE`
    /// route request, ensuring that the new route is included in the
    /// dump request.
    #[test_case(
        V4_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
        MANAGED_ROUTE_TABLE;
        "v4_new_same_table_dump")]
    #[test_case(
        V6_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        MANAGED_ROUTE_TABLE;
        "v6_new_same_table_dump")]
    #[test_case(
        V4_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
        RouteTableKey::NetlinkManaged { table_id: 1234 };
        "v4_new_different_table_dump")]
    #[test_case(
        V6_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        RouteTableKey::NetlinkManaged { table_id: 1234 };
        "v6_new_different_table_dump")]
    #[fuchsia::test]
    async fn test_new_then_get_dump_request<A: IpAddress>(
        subnet: Subnet<A>,
        group: ModernGroup,
        table: RouteTableKey,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (V4_NEXTHOP1, V4_NEXTHOP2),
            |()| (V6_NEXTHOP1, V6_NEXTHOP2),
        );

        // There are two pre-set routes in `test_route_requests`.
        // * subnet, next_hop1, DEV1, METRIC1, MANAGED_ROUTE_TABLE
        // * subnet, next_hop2, DEV2, METRIC2, MANAGED_ROUTE_TABLE
        // To add a new route that does not get rejected by the handler due to it
        // already existing, we use a route that has METRIC3.
        let unicast_route_args =
            create_unicast_new_route_args(subnet, next_hop1, DEV1.into(), METRIC3, table);

        // We expect to see one multicast message, representing the route that was added.
        // Then, three unicast messages, representing the two routes that existed already in the
        // route set, and the one new route that was added.
        let messages = vec![
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    table,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(table.into()),
                    ),
                )
                .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                group,
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop2),
                        DEV2,
                        METRIC2,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    table,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(table.into()),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
        ];

        test_route_requests_helper(
            [
                RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                    unicast_route_args,
                ))),
                RequestArgs::Route(RouteRequestArgs::Get(GetRouteArgs::Dump)),
            ],
            messages,
            vec![RouteSetResult::AddResult(Ok(true))],
            vec![Ok(()), Ok(())],
            subnet,
        )
        .await;
    }

    /// TODO(https://fxbug.dev/336382905): Once otherwise equivalent
    /// routes can be inserted into different tables, update the
    /// assertions to recognize the route as being added successfully.
    ///
    /// A test to exercise a `RTM_NEWROUTE` with a route that already
    /// exists, but in a different routing table, followed by a `RTM_GETROUTE`
    /// route request, ensuring that the new route does not initiate a
    /// multicast message and is not included in the dump request.
    #[test_case(V4_SUB1; "v4_new_dump")]
    #[test_case(V6_SUB1; "v6_new_dump")]
    #[fuchsia::test]
    async fn test_new_route_different_table_then_get_dump_request<A: IpAddress>(subnet: Subnet<A>)
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (V4_NEXTHOP1, V4_NEXTHOP2),
            |()| (V6_NEXTHOP1, V6_NEXTHOP2),
        );

        // There are two pre-set routes in `test_route_requests`.
        // * subnet, next_hop1, DEV1, METRIC1, MANAGED_ROUTE_TABLE
        // * subnet, next_hop2, DEV2, METRIC2, MANAGED_ROUTE_TABLE
        // Attempt to install the same first route, but with a different table.
        // Table id isn't important, as long as it is different
        // than MANAGED_ROUTE_TABLE.
        let unicast_route_args = create_unicast_new_route_args(
            subnet,
            next_hop1,
            DEV1.into(),
            METRIC1,
            RouteTableKey::NetlinkManaged { table_id: 1 },
        );

        // We expect to see two unicast messages, representing the routes that
        // existed already in the route set.
        let messages = vec![
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop2),
                        DEV2,
                        METRIC2,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
        ];

        test_route_requests_helper(
            [
                RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                    unicast_route_args,
                ))),
                RequestArgs::Route(RouteRequestArgs::Get(GetRouteArgs::Dump)),
            ],
            messages,
            vec![],
            vec![Ok(()), Ok(())],
            subnet,
        )
        .await;
    }

    /// A test to exercise a `RTM_NEWROUTE` followed by a `RTM_DELROUTE` for the same route, then a
    /// `RTM_GETROUTE` request, ensuring that the route added created a multicast message, but does
    /// not appear in the dump.
    #[test_case(V4_SUB1, ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE); "v4_new_del_dump")]
    #[test_case(V6_SUB1, ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE); "v6_new_del_dump")]
    #[fuchsia::test]
    async fn test_new_then_del_then_get_dump_request<A: IpAddress>(
        subnet: Subnet<A>,
        group: ModernGroup,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (V4_NEXTHOP1, V4_NEXTHOP2),
            |()| (V6_NEXTHOP1, V6_NEXTHOP2),
        );

        // There are two pre-set routes in `test_route_requests`.
        // * subnet, next_hop1, DEV1, METRIC1, MANAGED_ROUTE_TABLE
        // * subnet, next_hop2, DEV2, METRIC2, MANAGED_ROUTE_TABLE
        // To add a new route that does not get rejected by the handler due to it
        // already existing, we use a route that has METRIC3.
        let new_route_args = create_unicast_new_route_args(
            subnet,
            next_hop1,
            DEV1.into(),
            METRIC3,
            MANAGED_ROUTE_TABLE,
        );

        // The subnet and metric are enough to uniquely identify the above route.
        let del_route_args =
            create_unicast_del_route_args(subnet, None, None, Some(METRIC3), MANAGED_ROUTE_TABLE);

        // We expect to see two multicast messages, one representing the route that was added,
        // and the other representing the same route being removed. Then, two unicast messages,
        // representing the two routes that existed already in the route set.
        let messages = vec![
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                group,
            ),
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_del_route(),
                group,
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
            SentMessage::unicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop2),
                        DEV2,
                        METRIC2,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
                .into_rtnl_new_route(TEST_SEQUENCE_NUMBER, true),
            ),
        ];

        test_route_requests_helper(
            [
                RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(new_route_args))),
                RequestArgs::Route(RouteRequestArgs::Del(DelRouteArgs::Unicast(del_route_args))),
                RequestArgs::Route(RouteRequestArgs::Get(GetRouteArgs::Dump)),
            ],
            messages,
            vec![RouteSetResult::AddResult(Ok(true)), RouteSetResult::DelResult(Ok(true))],
            vec![Ok(()), Ok(()), Ok(())],
            subnet,
        )
        .await;
    }

    /// Tests RTM_NEWROUTE and RTM_DELROUTE when the interface is removed,
    /// indicated by the closure of the admin Control's server-end.
    /// The specific cause of the interface removal is unimportant
    /// for this test.
    #[test_case(RouteRequestKind::New, V4_SUB1; "v4_new_if_removed")]
    #[test_case(RouteRequestKind::New, V6_SUB1; "v6_new_if_removed")]
    #[test_case(RouteRequestKind::Del, V4_SUB1; "v4_del_if_removed")]
    #[test_case(RouteRequestKind::Del, V6_SUB1; "v6_del_if_removed")]
    #[fuchsia::test]
    async fn test_new_del_route_interface_removed<A: IpAddress>(
        kind: RouteRequestKind,
        subnet: Subnet<A>,
    ) where
        A::Version: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    {
        let (next_hop1, next_hop2): (A, A) = A::Version::map_ip(
            (),
            |()| (V4_NEXTHOP1, V4_NEXTHOP2),
            |()| (V6_NEXTHOP1, V6_NEXTHOP2),
        );

        // There are two pre-set routes in `test_route_requests`.
        // * subnet, next_hop1, DEV1, METRIC1, MANAGED_ROUTE_TABLE
        // * subnet, next_hop2, DEV2, METRIC2, MANAGED_ROUTE_TABLE
        let (route_req_args, route_set_result) = match kind {
            RouteRequestKind::New => {
                // Add a route that is not already present.
                let args =
                    RouteRequestArgs::New(NewRouteArgs::Unicast(create_unicast_new_route_args(
                        subnet,
                        next_hop1,
                        DEV1.into(),
                        METRIC3,
                        MANAGED_ROUTE_TABLE,
                    )));
                let res = RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated));
                (args, res)
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                let args = RouteRequestArgs::Del(DelRouteArgs::Unicast(
                    create_unicast_del_route_args(subnet, None, None, None, MANAGED_ROUTE_TABLE),
                ));
                let res = RouteSetResult::DelResult(Err(RouteSetError::Unauthenticated));
                (args, res)
            }
        };

        // No routes will be added or removed successfully, so there are no expected messages.
        let expected_messages = Vec::new();

        pretty_assertions::assert_eq!(
            test_requests(
                [RequestArgs::Route(route_req_args)],
                |interfaces_request_stream| async move {
                    interfaces_request_stream
                        .for_each(|req| {
                            futures::future::ready(match req.unwrap() {
                                fnet_root::InterfacesRequest::GetAdmin {
                                    id,
                                    control,
                                    control_handle: _,
                                } => {
                                    pretty_assertions::assert_eq!(id, DEV1 as u64);
                                    let control = control.into_stream().unwrap();
                                    let control = control.control_handle();
                                    control.shutdown();
                                }
                                req => unreachable!("unexpected interfaces request: {req:?}"),
                            })
                        })
                        .await
                },
                std::iter::once(route_set_result),
                subnet,
                next_hop1,
                next_hop2,
                expected_messages.len(),
            )
            .await,
            TestRequestResult {
                messages: expected_messages,
                waiter_results: vec![Err(RequestError::UnrecognizedInterface)],
            },
        )
    }

    // A flattened view of Route, convenient for holding testdata.
    #[derive(Clone)]
    struct Route<I: Ip> {
        subnet: Subnet<I::Addr>,
        device: u32,
        nexthop: Option<I::Addr>,
        metric: u32,
    }

    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "all_fields_the_same_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "all_fields_the_same_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_DFLT, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_DFLT, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "default_route_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_DFLT, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_DFLT, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "default_route_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_device_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_device_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_nexthop_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_nexthop_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_device_and_nexthop_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "different_device_and_nexthop_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "nexthop_newly_unset_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "nexthop_newly_unset_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "nexthop_previously_unset_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        NewRouteConflict::ConflictInSameTable; "nexthop_previously_unset_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC2, },
        NewRouteConflict::NoConflict; "different_metric_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC2, },
        NewRouteConflict::NoConflict; "different_metric_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB2, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::NoConflict; "different_subnet_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB2, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::NoConflict; "different_subnet_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB3, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::NoConflict; "different_subnet_prefixlen_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB3, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        NewRouteConflict::NoConflict; "different_subnet_prefixlen_v6_should_not_match")]
    fn test_new_route_matchers<I: Ip>(
        route1: Route<I>,
        route2: Route<I>,
        expected_conflict: NewRouteConflict,
    ) {
        // Route1 is converted into a NewRouteArgs, which is used as:
        // * a pending route request to compare against in `new_route_matches_new_route_args`, and
        // * an existing route to compare against in `route_matches_exsting`
        let new_route_args = {
            let Route { subnet, device, nexthop, metric } = route1;
            NewRouteArgs::Unicast(UnicastNewRouteArgs {
                subnet,
                target: fnet_routes_ext::RouteTarget::<I> {
                    outbound_interface: device.into(),
                    next_hop: nexthop
                        .map(|a| SpecifiedAddr::new(a).expect("nexthop should be specified")),
                },
                priority: metric,
                table: MANAGED_ROUTE_TABLE_ID,
            })
        };

        test_new_route_matches_new_route_args(
            route2.clone(),
            new_route_args,
            match expected_conflict {
                NewRouteConflict::NoConflict => false,
                NewRouteConflict::ConflictInSameTable
                | NewRouteConflict::ConflictInDifferentTable => true,
            },
        );
        test_new_route_matches_existing(new_route_args, route2, expected_conflict);
    }

    fn test_new_route_matches_new_route_args<I: Ip>(
        new: Route<I>,
        pending_route: NewRouteArgs<I>,
        expect_match: bool,
    ) {
        let new_route = {
            let Route { subnet, device, nexthop, metric } = new;
            create_installed_route::<I>(subnet, nexthop, device.into(), metric)
        };

        assert_eq!(new_route_matches_new_route_args(&new_route, &pending_route), expect_match)
    }

    fn test_new_route_matches_existing<I: Ip>(
        new_route_args: NewRouteArgs<I>,
        existing: Route<I>,
        expected_conflict: NewRouteConflict,
    ) {
        let Route { subnet, device, nexthop, metric } = existing;
        // Don't populate the Destination NLA if this is the default route.
        let destination = (subnet.prefix() != 0).then_some(subnet);
        let existing_route = create_netlink_route_message::<I>(
            subnet.prefix(),
            MANAGED_ROUTE_TABLE,
            create_nlas::<I>(destination, nexthop, device, metric, Some(MANAGED_ROUTE_TABLE_ID)),
        );
        let existing_routes = std::iter::once(&existing_route);
        assert_eq!(
            new_route_matches_existing(&new_route_args, existing_routes.into_iter()),
            expected_conflict
        );
    }

    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "all_fields_the_same_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "all_fields_the_same_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_DFLT, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_DFLT, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "default_route_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_DFLT, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_DFLT, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "default_route_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "different_device_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "different_device_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        false; "different_nexthop_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        false; "different_nexthop_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        false; "different_device_and_nexthop_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        false; "different_device_and_nexthop_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_newly_unset_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_newly_unset_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_previously_unset_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_previously_unset_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC2, },
        false; "different_metric_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC2, },
        false; "different_metric_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB2, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "different_subnet_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB2, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "different_subnet_v6_should_not_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB3, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "different_subnet_prefixlen_v4_should_not_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB3, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "different_subnet_prefixlen_v6_should_not_match")]
    fn test_installed_route_matches_netlink_message<I: Ip>(
        route1: Route<I>,
        route2: Route<I>,
        expect_match: bool,
    ) {
        let installed_route = {
            let Route { subnet, device, nexthop, metric } = route1;
            create_installed_route::<I>(subnet, nexthop, device.into(), metric)
        };
        let netlink_message = {
            let Route { subnet, device, nexthop, metric } = route2;
            let installed_route =
                create_installed_route::<I>(subnet, nexthop, device.into(), metric);
            // TODO(https://fxbug.dev/336382905): Pass in table as a
            // parameter once tables are provided by Netstack routes.
            NetlinkRouteMessage::try_from_installed_route(installed_route, RouteTableKey::Unmanaged)
                .unwrap()
        };

        assert_eq!(
            installed_route_matches_netlink_message(&installed_route, &netlink_message),
            expect_match
        );
    }

    // Calls `select_route_for_deletion` with the given args & existing_routes.
    //
    // Asserts that the return route matches the route in `existing_routes` at
    // `expected_index`.
    fn test_select_route_for_deletion_helper<I: Ip>(
        args: UnicastDelRouteArgs<I>,
        existing_routes: &[Route<I>],
        // The index into `existing_routes` of the route that should be selected.
        expected_index: Option<usize>,
    ) {
        let existing_routes = existing_routes
            .iter()
            .map(|Route { subnet, device, nexthop, metric }| {
                // Don't populate the Destination NLA if this is the default route.
                let destination = (subnet.prefix() != 0).then_some(*subnet);
                create_netlink_route_message::<I>(
                    subnet.prefix(),
                    MANAGED_ROUTE_TABLE,
                    create_nlas::<I>(
                        destination,
                        nexthop.to_owned(),
                        *device,
                        *metric,
                        Some(MANAGED_ROUTE_TABLE_ID),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let expected_route = expected_index.map(|index| {
            existing_routes
                .get(index)
                .expect("index should be within the bounds of `existing_routes`")
                .clone()
        });

        assert_eq!(
            select_route_for_deletion(
                DelRouteArgs::Unicast(args),
                &HashSet::from_iter(existing_routes)
            ),
            expected_route.as_ref()
        )
    }

    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB2, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "subnet_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB3, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "subnet_prefix_len_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "subnet_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "interface_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "interface_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_absent_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        false; "nexthop_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC2, },
        false; "metric_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        true; "metric_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB2, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "subnet_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB3, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "subnet_prefix_len_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "subnet_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "interface_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "interface_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_absent_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        false; "nexthop_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC2, },
        false; "metric_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        true; "metric_matches_v6")]
    fn test_select_route_for_deletion<I: Ip>(
        args: UnicastDelRouteArgs<I>,
        existing_route: Route<I>,
        expect_match: bool,
    ) {
        test_select_route_for_deletion_helper(args, &[existing_route], expect_match.then_some(0))
    }

    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        &[
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC2, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC3, },
        ],
        Some(1); "multiple_matches_prefers_lowest_metric_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroU32::new(MANAGED_ROUTE_TABLE.into()).unwrap(),
        },
        &[
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC2, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC3, },
        ],
        Some(1); "multiple_matches_prefers_lowest_metric_v6")]
    fn test_select_route_for_deletion_multiple_matches<I: Ip>(
        args: UnicastDelRouteArgs<I>,
        existing_routes: &[Route<I>],
        expected_index: Option<usize>,
    ) {
        test_select_route_for_deletion_helper(args, existing_routes, expected_index);
    }
}
