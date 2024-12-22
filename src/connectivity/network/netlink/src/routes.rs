// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_ROUTE information by receiving RTM_ROUTE
//! Netlink messages and maintaining route table state from Netstack.

use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::num::{NonZeroU32, NonZeroU64};

use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_interfaces_admin::{
    self as fnet_interfaces_admin, ProofOfInterfaceAuthorization,
};
use fidl_fuchsia_net_routes_admin::RouteSetError;
use {
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_root as fnet_root,
    fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_ext as fnet_routes_ext,
};

use derivative::Derivative;
use futures::channel::oneshot;
use futures::StreamExt as _;
use linux_uapi::{
    rt_class_t_RT_TABLE_COMPAT, rt_class_t_RT_TABLE_MAIN, rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
    rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
};
use net_types::ip::{GenericOverIp, Ip, IpAddress, IpVersion, Subnet};
use net_types::{SpecifiedAddr, SpecifiedAddress, Witness as _};
use netlink_packet_core::{NetlinkMessage, NLM_F_MULTIPART};
use netlink_packet_route::route::{
    RouteAddress, RouteAttribute, RouteHeader, RouteMessage, RouteProtocol, RouteScope, RouteType,
};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_packet_utils::nla::Nla;
use netlink_packet_utils::DecodeError;

use crate::client::{ClientTable, InternalClient};
use crate::errors::WorkerInitializationError;
use crate::logging::{log_debug, log_error, log_warn};
use crate::messaging::Sender;
use crate::multicast_groups::ModernGroup;
use crate::netlink_packet::errno::Errno;
use crate::netlink_packet::UNSPECIFIED_SEQUENCE_NUMBER;
use crate::protocol_family::route::NetlinkRoute;
use crate::protocol_family::ProtocolFamily;
use crate::route_tables::{
    FidlRouteMap, MainRouteTable, NetlinkRouteTableIndex, NonZeroNetlinkRouteTableIndex,
    RouteRemoveResult, RouteTable, RouteTableKey, RouteTableLookup, RouteTableMap,
    TableNeedsCleanup,
};
use crate::util::respond_to_completer;

const UNMANAGED_ROUTE_TABLE: u32 = rt_class_t_RT_TABLE_MAIN;
pub(crate) const UNMANAGED_ROUTE_TABLE_INDEX: NetlinkRouteTableIndex =
    NetlinkRouteTableIndex::new(UNMANAGED_ROUTE_TABLE);

/// Arguments for an RTM_GETROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum GetRouteArgs {
    Dump,
}

/// Arguments for an RTM_NEWROUTE unicast route.
#[derive(Copy, Clone, Debug, PartialEq, Eq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) struct UnicastNewRouteArgs<I: Ip> {
    // The network and prefix of the route.
    pub subnet: Subnet<I::Addr>,
    // The forwarding action. Unicast routes are gateway/direct routes and must
    // have a target.
    pub target: fnet_routes_ext::RouteTarget<I>,
    // The metric used to weigh the importance of the route.
    pub priority: u32,
    // The routing table.
    pub table: NetlinkRouteTableIndex,
}

/// Arguments for an RTM_NEWROUTE [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewRouteArgs<I: Ip> {
    /// Direct or gateway routes.
    Unicast(UnicastNewRouteArgs<I>),
}

/// Arguments for an RTM_DELROUTE unicast route.
/// Only the subnet and table field are required. All other fields are optional.
#[derive(Copy, Clone, Debug, PartialEq, Eq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
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
    pub(crate) table: NonZeroNetlinkRouteTableIndex,
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
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) struct RoutesWorker<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
> {
    fidl_route_map: FidlRouteMap<I>,
}

fn get_table_u8_and_nla_from_key(table: RouteTableKey) -> (u8, Option<RouteAttribute>) {
    let table_id: u32 = match table {
        RouteTableKey::Unmanaged => UNMANAGED_ROUTE_TABLE,
        RouteTableKey::NetlinkManaged { table_id } => table_id.get().get(),
    };
    // When the table's value is >255, the value should be specified
    // by an NLA and the header value should be RT_TABLE_COMPAT.
    match u8::try_from(table_id) {
        Ok(t) => (t, None),
        // RT_TABLE_COMPAT (252) can be downcasted without loss into u8.
        Err(_) => (rt_class_t_RT_TABLE_COMPAT as u8, Some(RouteAttribute::Table(table_id))),
    }
}

/// FIDL errors from the routes worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoutesFidlError {
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
    Del((NetlinkRouteMessage, NonZeroNetlinkRouteTableIndex)),
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

impl<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>
    RoutesWorker<I>
{
    pub(crate) async fn create(
        main_route_table: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
        routes_state_proxy: &<I::StateMarker as ProtocolMarker>::Proxy,
        route_table_provider: <I::RouteTableProviderMarker as ProtocolMarker>::Proxy,
    ) -> Result<
        (
            Self,
            RouteTableMap<I>,
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

        let mut fidl_route_map = FidlRouteMap::<I>::default();
        for fnet_routes_ext::InstalledRoute { route, effective_properties, table_id } in
            installed_routes
        {
            let _: Option<fnet_routes_ext::EffectiveRouteProperties> =
                fidl_route_map.add(route, table_id, effective_properties);
        }

        // There's nothing we can do to gracefully recover from failing to get the main route
        // table's ID, so we might as well crash.
        let main_route_table_id = fnet_routes_ext::admin::get_table_id::<I>(main_route_table)
            .await
            .expect("getting main route table ID should succeed");
        // Same for getting a route set proxy.
        let unmanaged_route_set_proxy =
            fnet_routes_ext::admin::new_route_set::<I>(main_route_table)
                .expect("getting unmanaged route set should succeed");
        let route_table_map = RouteTableMap::new(
            main_route_table.clone(),
            main_route_table_id,
            unmanaged_route_set_proxy,
            route_table_provider,
        );
        Ok((Self { fidl_route_map }, route_table_map, route_event_stream))
    }

    /// Handles events observed by the route watchers by adding/removing routes
    /// from the underlying `NetlinkRouteMessage` set.
    ///
    /// Returns a `RouteEventHandlerError` when unexpected events or HashSet issues occur.
    pub(crate) fn handle_route_watcher_event<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        &mut self,
        route_table_map: &mut RouteTableMap<I>,
        route_clients: &ClientTable<NetlinkRoute, S>,
        event: fnet_routes_ext::Event<I>,
    ) -> Result<Option<TableNeedsCleanup>, RouteEventHandlerError<I>> {
        handle_route_watcher_event::<I, S>(
            route_table_map,
            &mut self.fidl_route_map,
            route_clients,
            event,
        )
    }

    fn get_interface_control(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        interface_id: u64,
    ) -> fnet_interfaces_ext::admin::Control {
        let (control, server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>();
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

        let authorize_fut = I::map_ip_in(
            AuthorizeInputs::<'_, I> { route_set_proxy, proof },
            |AuthorizeInputs { route_set_proxy, proof }| {
                route_set_proxy.authenticate_for_interface(proof)
            },
            |AuthorizeInputs { route_set_proxy, proof }| {
                route_set_proxy.authenticate_for_interface(proof)
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
        &self,
        route_tables: &mut RouteTableMap<I>,
        interfaces_proxy: &fnet_root::InterfacesProxy,
        args: NewRouteArgs<I>,
    ) -> Result<NewRouteArgs<I>, RequestError> {
        let (interface_id, table) = match args {
            NewRouteArgs::Unicast(args) => (args.target.outbound_interface, args.table),
        };
        let route: fnet_routes_ext::Route<I> = args.into();

        // Ideally we'd combine the two following operations with some form of
        // Entry API in order to avoid the panic, but this is difficult to pull
        // off with async.
        route_tables.create_route_table_if_managed_and_not_present(table).await;

        let table_id = route_tables.get_fidl_table_id(&table).expect("should be populated");

        // Check if the new route conflicts with an existing route.
        //
        // Note that Linux and Fuchsia differ on what constitutes a conflicting route.
        // Linux is stricter than Fuchsia and requires that all routes have a unique
        // (destination subnet, metric, table) tuple. Here we replicate the check that Linux
        // performs, so that Netlink can reject requests before handing them off to the
        // more flexible Netstack routing APIs.
        let new_route_conflicts_with_existing = self
            .fidl_route_map
            .iter_table(route_tables.get_fidl_table_id(&table).expect("should be populated"))
            .any(|(stored_route, stored_props)| {
                routes_conflict::<I>(
                    fnet_routes_ext::InstalledRoute {
                        route: *stored_route,
                        effective_properties: *stored_props,
                        table_id,
                    },
                    route,
                    table_id,
                )
            });

        if new_route_conflicts_with_existing {
            return Err(RequestError::AlreadyExists);
        }

        let (real_route_set, backup_route_set) =
            match route_tables.get(&table).expect("should have just been populated") {
                RouteTableLookup::Managed(RouteTable {
                    route_set_proxy,
                    route_set_from_main_table_proxy,
                    ..
                }) => (route_set_proxy, Some(route_set_from_main_table_proxy)),
                RouteTableLookup::Unmanaged(MainRouteTable { route_set_proxy, .. }) => {
                    (route_set_proxy, None)
                }
            };

        let route: I::Route =
            route.try_into().expect("should not have constructed unknown route action");

        // TODO(https://fxbug.dev/358649849): Until rules are fully supported in netlink and
        // netstack, routes are installed into BOTH the main table and the "real" table in order to
        // be able to exercise the install-routes-in-separate-tables path as part of transitioning
        // to full PBR support while preserving existing functionality.
        if let Some(backup_route_set) = backup_route_set {
            let _added_to_main_table_as_backup: bool = Self::dispatch_route_proxy_fn(
                &route,
                interface_id,
                &interfaces_proxy,
                backup_route_set,
                fnet_routes_ext::admin::add_route::<I>,
            )
            .await?;
        }

        let added_to_real_table: bool = Self::dispatch_route_proxy_fn(
            &route,
            interface_id,
            &interfaces_proxy,
            real_route_set,
            fnet_routes_ext::admin::add_route::<I>,
        )
        .await?;

        // When `add_route` has an `Ok(false)` response, this indicates that the
        // route already exists, which should manifest as a hard error in Linux.
        if !added_to_real_table {
            return Err(RequestError::AlreadyExists);
        };

        Ok(args)
    }

    /// Handles a delete route request.
    ///
    /// Returns the `NetlinkRouteMessage` along with its corresponding table index if the route was
    /// successfully removed so that the caller can make sure their local state (from the routes
    /// watcher) has sent a removal event for the removed route.
    async fn handle_del_route_request(
        &self,
        interfaces_proxy: &fnet_root::InterfacesProxy,
        route_tables: &mut RouteTableMap<I>,
        del_route_args: DelRouteArgs<I>,
    ) -> Result<(NetlinkRouteMessage, NonZeroNetlinkRouteTableIndex), RequestError> {
        let table = match del_route_args {
            DelRouteArgs::Unicast(args) => args.table,
        };

        let route_to_delete = &self
            .select_route_for_deletion(route_tables, del_route_args)
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

        let (real_route_set, backup_route_set) = match route_tables.get(&table.into()) {
            None => return Err(RequestError::NotFound),
            Some(lookup) => match lookup {
                RouteTableLookup::Managed(RouteTable {
                    route_set_proxy,
                    route_set_from_main_table_proxy,
                    ..
                }) => (route_set_proxy, Some(route_set_from_main_table_proxy)),
                RouteTableLookup::Unmanaged(MainRouteTable { route_set_proxy, .. }) => {
                    (route_set_proxy, None)
                }
            },
        };

        let route: fnet_routes_ext::Route<I> = route_to_delete.to_owned().into();
        let route: I::Route = route.try_into().expect("route should be converted");

        // TODO(https://fxbug.dev/358649849): Until rules are fully supported in netlink and
        // netstack, routes are installed into BOTH the main table and the "real" table in order to
        // be able to exercise both.
        if let Some(route_set) = backup_route_set {
            let _backup_copy_removed_from_main_table: bool = Self::dispatch_route_proxy_fn(
                &route,
                interface_id,
                &interfaces_proxy,
                route_set,
                fnet_routes_ext::admin::remove_route::<I>,
            )
            .await?;
        }

        let real_instance_removed: bool = Self::dispatch_route_proxy_fn(
            &route,
            interface_id,
            &interfaces_proxy,
            real_route_set,
            fnet_routes_ext::admin::remove_route::<I>,
        )
        .await?;

        if !real_instance_removed {
            log_error!(
                "Route was not removed as a result of this call. Likely Linux wanted \
                to remove a route from the global route set which is not supported  \
                by this API, route: {:?}",
                route_to_delete
            );
            return Err(RequestError::DeletionNotAllowed);
        }

        Ok((route_to_delete.to_owned(), table))
    }

    /// Select a route for deletion, based on the given deletion arguments.
    ///
    /// Note that Linux and Fuchsia differ on how to specify a route for deletion.
    /// Linux is more flexible and allows you specify matchers as arguments, where
    /// Fuchsia requires that you exactly specify the route. Here, Linux's matchers
    /// are provided in `deletion_args`; Many of the matchers are optional, and an
    /// existing route matches the arguments if all provided arguments are equal to
    /// the values held by the route. If multiple routes match the arguments, the
    /// route with the lowest metric is selected.
    fn select_route_for_deletion(
        &self,
        route_tables: &RouteTableMap<I>,
        deletion_args: DelRouteArgs<I>,
    ) -> Option<NetlinkRouteMessage> {
        select_route_for_deletion(&self.fidl_route_map, route_tables, deletion_args)
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
        route_tables: &mut RouteTableMap<I>,
        interfaces_proxy: &fnet_root::InterfacesProxy,
        Request { args, sequence_number, mut client, completer }: Request<S, I>,
    ) -> Option<PendingRouteRequest<S, I>> {
        log_debug!("handling request {args:?} from {client}");

        #[derive(Derivative)]
        #[derivative(Debug(bound = ""))]
        enum RequestHandled<S, I>
        where
            S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
            I: Ip,
        {
            Pending(PendingRouteRequest<S, I>),
            Done(
                Result<(), RequestError>,
                InternalClient<NetlinkRoute, S>,
                oneshot::Sender<Result<(), RequestError>>,
            ),
        }

        let request_handled = match args {
            RequestArgs::Route(args) => match args {
                RouteRequestArgs::Get(args) => match args {
                    GetRouteArgs::Dump => {
                        let main_route_table_id = route_tables.main_route_table.fidl_table_id;
                        self.fidl_route_map
                            .iter()
                            .flat_map(|(route, tables)| {
                                // If the table is both in the main table and in another table,
                                // suppress the main table version from the dump, as we installed it
                                // there in order to support the transition to netlink supporting
                                // rules-based routing.
                                // TODO(https://fxbug.dev/358649849): Remove this hack once netlink
                                // supports PBR rules.
                                let suppress_main_table = tables.len() >= 2;
                                tables
                                    .iter()
                                    .filter(move |(fidl_table_id, _)| {
                                        if suppress_main_table {
                                            **fidl_table_id != main_route_table_id
                                        } else {
                                            true
                                        }
                                    })
                                    .map(move |(fidl_table_id, props)| {
                                        fnet_routes_ext::InstalledRoute {
                                            route: *route,
                                            table_id: *fidl_table_id,
                                            effective_properties: *props,
                                        }
                                    })
                            })
                            .filter_map(|installed_route| {
                                let table_index = match route_tables
                                    .lookup_route_table_key_from_fidl_table_id(
                                        installed_route.table_id,
                                    ) {
                                    None => {
                                        // This FIDL table ID is not managed by netlink, so ignore
                                        // it in the dump.
                                        return None;
                                    }
                                    Some(RouteTableKey::Unmanaged) => UNMANAGED_ROUTE_TABLE_INDEX,
                                    Some(RouteTableKey::NetlinkManaged { table_id }) => {
                                        table_id.get()
                                    }
                                };
                                NetlinkRouteMessage::optionally_from(installed_route, table_index)
                            })
                            .for_each(|message| {
                                client.send_unicast(
                                    message.into_rtnl_new_route(sequence_number, true),
                                )
                            });
                        RequestHandled::Done(Ok(()), client, completer)
                    }
                },
                RouteRequestArgs::New(args) => {
                    match self.handle_new_route_request(route_tables, interfaces_proxy, args).await
                    {
                        Ok(args) => {
                            // Route additions must be confirmed via observing routes-watcher events
                            // that indicate the route has been installed.
                            RequestHandled::Pending(PendingRouteRequest {
                                request_args: PendingRouteRequestArgs::New(args),
                                client,
                                completer,
                            })
                        }
                        Err(err) => RequestHandled::Done(Err(err), client, completer),
                    }
                }
                RouteRequestArgs::Del(args) => {
                    match self.handle_del_route_request(interfaces_proxy, route_tables, args).await
                    {
                        Ok(del_route) => {
                            // Route deletions must be confirmed via a message from the Routes
                            // watcher with the same Route struct - using the route
                            // matched for deletion.
                            RequestHandled::Pending(PendingRouteRequest {
                                request_args: PendingRouteRequestArgs::Del(del_route),
                                client,
                                completer,
                            })
                        }
                        Err(e) => RequestHandled::Done(Err(e), client, completer),
                    }
                }
            },
        };

        match request_handled {
            RequestHandled::Done(result, client, completer) => {
                log_debug!("handled request {args:?} from {client} with result = {result:?}");

                respond_to_completer(client, completer, result, args);
                None
            }
            RequestHandled::Pending(pending) => Some(pending),
        }
    }

    /// Checks whether a `PendingRequest` can be marked completed given the current state of the
    /// worker. If so, notifies the request's completer and returns `None`. If not, returns
    /// the `PendingRequest` as `Some`.
    pub(crate) fn handle_pending_request<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    >(
        &self,
        route_tables: &mut RouteTableMap<I>,
        pending_route_request: PendingRouteRequest<S, I>,
    ) -> Option<PendingRouteRequest<S, I>> {
        let PendingRouteRequest { request_args, client: _, completer: _ } = &pending_route_request;

        let done = match request_args {
            PendingRouteRequestArgs::New(args) => {
                let netlink_table_id = match args {
                    NewRouteArgs::Unicast(args) => &args.table,
                };

                let own_fidl_table_id = route_tables
                    .get_fidl_table_id(netlink_table_id)
                    .expect("should recognize table referenced in pending new route request");

                self.fidl_route_map
                    .route_is_installed_in_tables(&(*args).into(), [&own_fidl_table_id])
            }
            // For `Del` messages, we expect the exact `NetlinkRouteMessage` to match,
            // which was received as part of the `select_route_for_deletion` flow.
            PendingRouteRequestArgs::Del((route_msg, pending_table)) => {
                let netlink_table_id: NetlinkRouteTableIndex = (*pending_table).into();
                // It's okay for this to be `None`, as we may have garbage collected the
                // corresponding entry in `route_tables` if this was the last route in that table.
                let own_fidl_table_id: Option<fnet_routes_ext::TableId> =
                    route_tables.get_fidl_table_id(&netlink_table_id);

                if let Some(own_fidl_table_id) = own_fidl_table_id {
                    self.fidl_route_map.route_is_uninstalled_in_tables(
                        &route_msg.clone().into(),
                        [&own_fidl_table_id],
                    )
                } else {
                    true
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

    pub(crate) fn any_routes_reference_table(
        &self,
        TableNeedsCleanup(table_id, _table_index): TableNeedsCleanup,
    ) -> bool {
        self.fidl_route_map.table_is_present(table_id)
    }
}

/// Returns `true` if the new route conflicts with an existing route.
///
/// Note that Linux and Fuchsia differ on what constitutes a conflicting route.
/// Linux is stricter than Fuchsia and requires that all routes have a unique
/// (destination subnet, metric, table) tuple. Here we replicate the check that Linux
/// performs, so that Netlink can reject requests before handing them off to the
/// more flexible Netstack routing APIs.
fn routes_conflict<I: Ip>(
    stored_route: fnet_routes_ext::InstalledRoute<I>,
    incoming_route: fnet_routes_ext::Route<I>,
    incoming_table: fnet_routes_ext::TableId,
) -> bool {
    let fnet_routes_ext::InstalledRoute {
        route: fnet_routes_ext::Route { destination: stored_destination, action: _, properties: _ },
        effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: stored_metric },
        table_id: stored_table_id,
    } = stored_route;

    let destinations_match = stored_destination == incoming_route.destination;
    // NB: Netlink requests always specify an explicit metric.
    let metrics_match = fnet_routes::SpecifiedMetric::ExplicitMetric(stored_metric)
        == incoming_route.properties.specified_properties.metric;
    let tables_match = stored_table_id == incoming_table;

    destinations_match && metrics_match && tables_match
}

// Errors related to handling route events.
#[derive(Debug, PartialEq, thiserror::Error)]
pub(crate) enum RouteEventHandlerError<I: Ip> {
    #[error("route watcher event handler attempted to add a route that already existed: {0:?}")]
    AlreadyExistingRouteAddition(fnet_routes_ext::InstalledRoute<I>),
    #[error("route watcher event handler attempted to remove a route that does not exist: {0:?}")]
    NonExistentRouteDeletion(fnet_routes_ext::InstalledRoute<I>),
    #[error(
        "route watcher event handler attempted to process \
         a route event that was not add or remove: {0:?}"
    )]
    NonAddOrRemoveEventReceived(fnet_routes_ext::Event<I>),
}

fn handle_route_watcher_event<
    I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
>(
    route_table_map: &mut RouteTableMap<I>,
    fidl_route_map: &mut FidlRouteMap<I>,
    route_clients: &ClientTable<NetlinkRoute, S>,
    event: fnet_routes_ext::Event<I>,
) -> Result<Option<TableNeedsCleanup>, RouteEventHandlerError<I>> {
    let (message_for_clients, table_no_routes) = match event {
        fnet_routes_ext::Event::Added(added_installed_route) => {
            let fnet_routes_ext::InstalledRoute { route, table_id, effective_properties } =
                added_installed_route;

            match fidl_route_map.add(route, table_id, effective_properties) {
                None => (),
                Some(_properties) => {
                    return Err(RouteEventHandlerError::AlreadyExistingRouteAddition(
                        added_installed_route,
                    ));
                }
            }

            match route_table_map.lookup_route_table_key_from_fidl_table_id(table_id) {
                None => {
                    // This is a FIDL table ID that the netlink worker didn't know about, and is
                    // not the main table ID.
                    crate::logging::log_debug!(
                        "Observed an added route via the routes watcher that is installed in a \
                        non-main FIDL table not managed by netlink: {added_installed_route:?}"
                    );
                    // Because we'll never be able to map this FIDL table ID to a netlink table
                    // index, we have no choice but to avoid notifying netlink clients about this.
                    (None, None)
                }
                Some(table) => {
                    let table = match table {
                        RouteTableKey::Unmanaged => UNMANAGED_ROUTE_TABLE_INDEX,
                        RouteTableKey::NetlinkManaged { table_id } => table_id.get(),
                    };

                    (
                        NetlinkRouteMessage::optionally_from(added_installed_route, table).map(
                            |route_message| {
                                route_message
                                    .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false)
                            },
                        ),
                        None,
                    )
                }
            }
        }
        fnet_routes_ext::Event::Removed(removed_installed_route) => {
            let fnet_routes_ext::InstalledRoute { route, table_id, effective_properties: _ } =
                removed_installed_route;

            let need_clean_up_empty_table = match fidl_route_map.remove(route, table_id) {
                RouteRemoveResult::DidNotExist => {
                    return Err(RouteEventHandlerError::NonExistentRouteDeletion(
                        removed_installed_route,
                    ));
                }
                RouteRemoveResult::RemovedButTableNotEmpty(_properties) => false,
                RouteRemoveResult::RemovedAndTableNewlyEmpty(_properties) => true,
            };

            let (notify_message, table_index) =
                match route_table_map.lookup_route_table_key_from_fidl_table_id(table_id) {
                    None => {
                        // This is a FIDL table ID that the netlink worker didn't know about, and is
                        // not the main table ID.
                        crate::logging::log_debug!(
                        "Observed an added route via the routes watcher that is installed in a \
                        non-main FIDL table not managed by netlink: {removed_installed_route:?}"
                    );
                        // Because we'll never be able to map this FIDL table ID to a netlink table
                        // index, we have no choice but to avoid notifying netlink clients about this.
                        (None, None)
                    }
                    Some(table) => {
                        let table = match table {
                            RouteTableKey::Unmanaged => UNMANAGED_ROUTE_TABLE_INDEX,
                            RouteTableKey::NetlinkManaged { table_id } => table_id.get(),
                        };

                        (
                            NetlinkRouteMessage::optionally_from(removed_installed_route, table)
                                .map(|route_message| route_message.into_rtnl_del_route()),
                            Some(table),
                        )
                    }
                };

            (
                notify_message,
                if need_clean_up_empty_table {
                    Some(TableNeedsCleanup(
                        table_id,
                        table_index.expect("must have known about table if we're cleaning it up"),
                    ))
                } else {
                    None
                },
            )
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

    Ok(table_no_routes)
}

/// A wrapper type for the netlink_packet_route `RouteMessage` to enable conversions
/// from [`fnet_routes_ext::InstalledRoute`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetlinkRouteMessage(pub(crate) RouteMessage);

impl NetlinkRouteMessage {
    /// Implement optional conversions from `InstalledRoute` and `table`
    /// to `NetlinkRouteMessage`. `Ok` becomes `Some`, while `Err` is
    /// logged and becomes `None`.
    pub(crate) fn optionally_from<I: Ip>(
        route: fnet_routes_ext::InstalledRoute<I>,
        table: NetlinkRouteTableIndex,
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
            // TODO(https://fxbug.dev/336382905): Use the table ID.
            table_id: _,
        }: fnet_routes_ext::InstalledRoute<I>,
        table: NetlinkRouteTableIndex,
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

        let (table_u8, table_nla) = get_table_u8_and_nla_from_key(RouteTableKey::from(table));
        route_header.table = table_u8;

        // The following fields are used in the header, but they do not have any
        // corresponding values in `InstalledRoute`. The fields explicitly
        // defined below  are expected to be needed at some point, but the
        // information is not currently provided by the watcher.
        //
        // length of source prefix
        // tos filter (type of service)
        route_header.protocol = RouteProtocol::Kernel;
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
        let RouteNlaView { subnet, metric, interface_id, next_hop } =
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
    }
}

/// Select a route for deletion, based on the given deletion arguments.
///
/// Note that Linux and Fuchsia differ on how to specify a route for deletion.
/// Linux is more flexible and allows you specify matchers as arguments, where
/// Fuchsia requires that you exactly specify the route. Here, Linux's matchers
/// are provided in `deletion_args`; Many of the matchers are optional, and an
/// existing route matches the arguments if all provided arguments are equal to
/// the values held by the route. If multiple routes match the arguments, the
/// route with the lowest metric is selected.
fn select_route_for_deletion<
    I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
>(
    fidl_route_map: &FidlRouteMap<I>,
    route_tables: &RouteTableMap<I>,
    deletion_args: DelRouteArgs<I>,
) -> Option<NetlinkRouteMessage> {
    // Find the set of candidate routes, mapping them to tuples (route, metric).
    fidl_route_map
        .iter_messages(
            route_tables,
            match deletion_args {
                DelRouteArgs::Unicast(args) => args.table.into(),
            },
        )
        .filter_map(|route: NetlinkRouteMessage| {
            let NetlinkRouteMessage(existing_route) = &route;
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

            let existing_metric = *existing_metric;

            if subnet_matches && metric_matches && interface_matches && next_hop_matches {
                Some((route, existing_metric))
            } else {
                None
            }
        })
        // Select the route with the lowest metric
        .min_by(|(_route1, metric1), (_route2, metric2)| metric1.cmp(metric2))
        .map(|(route, _metric)| route)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{HashMap, VecDeque};
    use std::convert::Infallible as Never;
    use std::pin::pin;
    use std::sync::atomic::{AtomicU32, Ordering};

    use fidl::endpoints::{ControlHandle, RequestStream, ServerEnd};
    use fidl_fuchsia_net_routes_ext::admin::{RouteSetRequest, RouteTableRequest};
    use fidl_fuchsia_net_routes_ext::Responder as _;
    use {
        fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
        fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    };

    use anyhow::Error;
    use assert_matches::assert_matches;
    use futures::channel::mpsc;
    use futures::future::{Future, FutureExt as _};
    use futures::{SinkExt as _, Stream};
    use ip_test_macro::ip_test;
    use linux_uapi::rtnetlink_groups_RTNLGRP_LINK;
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::ip::{GenericOverIp, IpInvariant, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
    use net_types::SpecifiedAddr;
    use netlink_packet_core::NetlinkPayload;
    use test_case::test_case;

    use crate::eventloop::{EventLoopComponent, Optional, Required};
    use crate::interfaces::testutil::FakeInterfacesHandler;
    use crate::messaging::testutil::{FakeSender, SentMessage};
    use crate::route_tables::ManagedNetlinkRouteTableIndex;

    const V4_SUB1: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/32");
    const V4_SUB2: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.1/32");
    const V4_SUB3: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/24");
    const V4_DFLT: Subnet<Ipv4Addr> = net_subnet_v4!("0.0.0.0/0");
    const V4_NEXTHOP1: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const V4_NEXTHOP2: Ipv4Addr = net_ip_v4!("192.0.2.2");

    const V6_SUB1: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::/128");
    const V6_SUB2: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::1/128");
    const V6_SUB3: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::/64");
    const V6_DFLT: Subnet<Ipv6Addr> = net_subnet_v6!("::/0");
    const V6_NEXTHOP1: Ipv6Addr = net_ip_v6!("2001:db8::1");
    const V6_NEXTHOP2: Ipv6Addr = net_ip_v6!("2001:db8::2");

    const DEV1: u32 = 1;
    const DEV2: u32 = 2;

    const METRIC1: u32 = 1;
    const METRIC2: u32 = 100;
    const METRIC3: u32 = 9999;
    const TEST_SEQUENCE_NUMBER: u32 = 1234;
    const MANAGED_ROUTE_TABLE_ID: u32 = 5678;
    const MANAGED_ROUTE_TABLE_INDEX: NetlinkRouteTableIndex =
        NetlinkRouteTableIndex::new(MANAGED_ROUTE_TABLE_ID);
    const MANAGED_ROUTE_TABLE: RouteTableKey = RouteTableKey::from(MANAGED_ROUTE_TABLE_INDEX);
    const MAIN_FIDL_TABLE_ID: fnet_routes_ext::TableId = fnet_routes_ext::TableId::new(0);
    const OTHER_FIDL_TABLE_ID: fnet_routes_ext::TableId = fnet_routes_ext::TableId::new(1);

    fn create_installed_route<I: Ip>(
        subnet: Subnet<I::Addr>,
        next_hop: Option<I::Addr>,
        interface_id: u64,
        metric: u32,
        table_id: fnet_routes_ext::TableId,
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
            table_id,
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
        route_header.protocol = RouteProtocol::Kernel;

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

    #[ip_test(I)]
    #[test_case(RouteTableKey::Unmanaged)]
    #[test_case(MANAGED_ROUTE_TABLE)]
    #[fuchsia::test]
    async fn handles_route_watcher_event<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        table: RouteTableKey,
    ) {
        let (subnet, next_hop) =
            I::map_ip((), |()| (V4_SUB1, V4_NEXTHOP1), |()| (V6_SUB1, V6_NEXTHOP1));
        let table_id = match table {
            RouteTableKey::Unmanaged => MAIN_FIDL_TABLE_ID,
            RouteTableKey::NetlinkManaged { table_id: _ } => OTHER_FIDL_TABLE_ID,
        };
        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV1.into(), METRIC1, table_id);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, Some(next_hop), DEV2.into(), METRIC2, table_id);

        let add_event1 = fnet_routes_ext::Event::Added(installed_route1);
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);
        let unknown_event: fnet_routes_ext::Event<I> = fnet_routes_ext::Event::Unknown;

        let table_index = match table {
            RouteTableKey::Unmanaged => UNMANAGED_ROUTE_TABLE_INDEX,
            RouteTableKey::NetlinkManaged { table_id } => table_id.get(),
        };

        let expected_route_message1: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(installed_route1, table_index).unwrap();
        let expected_route_message2: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(installed_route2, table_index).unwrap();

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

        let (route_set_from_main_table_proxy, _route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_set_proxy, _route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_table_proxy, _route_table_server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableMarker>();
        let (unmanaged_route_set_proxy, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_table_provider, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableProviderMarker>();

        let mut route_table = RouteTableMap::new(
            route_table_proxy.clone(),
            MAIN_FIDL_TABLE_ID,
            unmanaged_route_set_proxy,
            route_table_provider,
        );
        let mut fidl_route_map = FidlRouteMap::<I>::default();

        match table {
            RouteTableKey::Unmanaged => {}
            RouteTableKey::NetlinkManaged { table_id } => {
                route_table.insert(
                    table_id,
                    RouteTable {
                        route_table_proxy,
                        route_set_proxy,
                        route_set_from_main_table_proxy,
                        fidl_table_id: OTHER_FIDL_TABLE_ID,
                        rule_set_authenticated: false,
                    },
                );
            }
        }

        // An event that is not an add or remove should result in an error.
        assert_matches!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                unknown_event,
            ),
            Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(_))
        );
        assert_eq!(fidl_route_map.iter_messages(&route_table, table_index).count(), 0);
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                add_event1,
            ),
            Ok(None)
        );
        assert_eq!(
            fidl_route_map.iter_messages(&route_table, table_index).collect::<HashSet<_>>(),
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
                &mut fidl_route_map,
                &route_clients,
                add_event1,
            ),
            Err(RouteEventHandlerError::AlreadyExistingRouteAddition(_))
        );
        assert_eq!(
            fidl_route_map.iter_messages(&route_table, table_index).collect::<HashSet<_>>(),
            HashSet::from_iter([expected_route_message1.clone()])
        );
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding a different route should result in an addition.
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                add_event2,
            ),
            Ok(None)
        );
        assert_eq!(
            fidl_route_map.iter_messages(&route_table, table_index).collect::<HashSet<_>>(),
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
                &mut fidl_route_map,
                &route_clients,
                remove_event,
            ),
            Ok(None)
        );
        assert_eq!(
            fidl_route_map.iter_messages(&route_table, table_index).collect::<HashSet<_>>(),
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
                &mut fidl_route_map,
                &route_clients,
                remove_event,
            ),
            Err(RouteEventHandlerError::NonExistentRouteDeletion(_))
        );
        assert_eq!(
            fidl_route_map.iter_messages(&route_table, table_index).collect::<HashSet<_>>(),
            HashSet::from_iter([expected_route_message2.clone()])
        );
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }

    #[ip_test(I, test = false)]
    #[fuchsia::test]
    async fn handle_route_watcher_event_two_routesets<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >() {
        let (subnet, next_hop) =
            I::map_ip((), |()| (V4_SUB1, V4_NEXTHOP1), |()| (V6_SUB1, V6_NEXTHOP1));

        let installed_route1: fnet_routes_ext::InstalledRoute<I> = create_installed_route(
            subnet,
            Some(next_hop),
            DEV1.into(),
            METRIC1,
            OTHER_FIDL_TABLE_ID,
        );
        let installed_route2: fnet_routes_ext::InstalledRoute<I> = create_installed_route(
            subnet,
            Some(next_hop),
            DEV2.into(),
            METRIC2,
            MAIN_FIDL_TABLE_ID,
        );

        let add_events1 = [
            fnet_routes_ext::Event::Added(fnet_routes_ext::InstalledRoute {
                table_id: MAIN_FIDL_TABLE_ID,
                ..installed_route1
            }),
            fnet_routes_ext::Event::Added(installed_route1),
        ];
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);

        // Due to the double-writing of routes into managed tables and into the main tables, we need
        // to account for notifications for both routes being added.
        let expected_route_message1_unmanaged = NetlinkRouteMessage::try_from_installed_route(
            installed_route1,
            UNMANAGED_ROUTE_TABLE_INDEX,
        )
        .unwrap();
        let expected_route_message1_managed = NetlinkRouteMessage::try_from_installed_route(
            installed_route1,
            MANAGED_ROUTE_TABLE_INDEX,
        )
        .unwrap();
        let expected_route_message2: NetlinkRouteMessage =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route2,
                UNMANAGED_ROUTE_TABLE_INDEX,
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

        let (main_route_table_proxy, _route_table_server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableMarker>();
        let (unmanaged_route_set_proxy, _unmanaged_route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_set_from_main_table_proxy, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_table_proxy, _route_table_server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableMarker>();
        let (route_set_proxy, _server_end) = fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_table_provider, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableProviderMarker>();

        let mut route_table = RouteTableMap::new(
            main_route_table_proxy,
            MAIN_FIDL_TABLE_ID,
            unmanaged_route_set_proxy,
            route_table_provider,
        );
        route_table.insert(
            ManagedNetlinkRouteTableIndex::new(MANAGED_ROUTE_TABLE_INDEX).unwrap(),
            RouteTable {
                route_set_from_main_table_proxy,
                route_set_proxy,
                route_table_proxy,
                fidl_table_id: OTHER_FIDL_TABLE_ID,
                rule_set_authenticated: false,
            },
        );

        let mut fidl_route_map = FidlRouteMap::<I>::default();

        // Send the first of the added-route events (corresponding to the route having been added
        // to the main FIDL table).
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                add_events1[0],
            ),
            Ok(None)
        );

        // Shouldn't be counted yet, as we haven't seen the route added to its own table yet.
        assert_eq!(
            &fidl_route_map
                .iter_messages(&route_table, MANAGED_ROUTE_TABLE_INDEX)
                .collect::<HashSet<_>>(),
            &HashSet::new()
        );

        // Now send the other event (corresponding to the route having been also added to the real
        // FIDL table).
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                add_events1[1],
            ),
            Ok(None)
        );

        // Now the route should have been added.
        assert_eq!(
            &fidl_route_map
                .iter_messages(&route_table, MANAGED_ROUTE_TABLE_INDEX)
                .chain(fidl_route_map.iter_messages(&route_table, UNMANAGED_ROUTE_TABLE_INDEX))
                .collect::<HashSet<_>>(),
            &HashSet::from_iter([
                expected_route_message1_unmanaged.clone(),
                expected_route_message1_managed.clone()
            ])
        );
        assert_eq!(
            &right_sink.take_messages()[..],
            &[expected_route_message1_unmanaged.clone(), expected_route_message1_managed.clone()]
                .map(|message| SentMessage::multicast(
                    message.clone().into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                    right_group
                ))
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Ensure that an unmanaged Route can be observed and added to the
        // unmanaged route set (signified by no pending request).
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                add_event2,
            ),
            Ok(None)
        );

        // Should also contain the route from before.
        assert_eq!(
            &fidl_route_map
                .iter_messages(&route_table, MANAGED_ROUTE_TABLE_INDEX)
                .chain(fidl_route_map.iter_messages(&route_table, UNMANAGED_ROUTE_TABLE_INDEX))
                .collect::<HashSet<_>>(),
            &HashSet::from_iter([
                expected_route_message1_unmanaged.clone(),
                expected_route_message1_managed.clone(),
                expected_route_message2.clone()
            ])
        );

        // However, netlink won't send any notifications about unmanaged routes.
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

        // Notify of the route being removed from the managed table.
        assert_eq!(
            handle_route_watcher_event(
                &mut route_table,
                &mut fidl_route_map,
                &route_clients,
                remove_event,
            ),
            Ok(Some(TableNeedsCleanup(OTHER_FIDL_TABLE_ID, MANAGED_ROUTE_TABLE_INDEX)))
        );
        assert_eq!(
            &fidl_route_map
                .iter_messages(&route_table, UNMANAGED_ROUTE_TABLE_INDEX)
                .collect::<HashSet<_>>(),
            &HashSet::from_iter([
                expected_route_message1_unmanaged.clone(),
                expected_route_message2.clone()
            ])
        );
        assert_eq!(
            fidl_route_map
                .iter_messages(&route_table, MANAGED_ROUTE_TABLE_INDEX)
                .collect::<HashSet<_>>(),
            HashSet::new()
        );
        assert_eq!(
            &right_sink.take_messages()[..],
            &[SentMessage::multicast(
                expected_route_message1_managed.clone().into_rtnl_del_route(),
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
        let installed_route = create_installed_route::<I>(
            subnet,
            Some(next_hop),
            DEV1.into(),
            METRIC1,
            MAIN_FIDL_TABLE_ID,
        );
        let prefix_length = subnet.prefix();
        let subnet = if prefix_length > 0 { Some(subnet) } else { None };
        let nlas = create_nlas::<I>(subnet, Some(next_hop), DEV1, METRIC1, None);
        let table = RouteTableKey::Unmanaged;
        let expected = create_netlink_route_message::<I>(prefix_length, table, nlas);

        let actual = NetlinkRouteMessage::try_from_installed_route(
            installed_route,
            UNMANAGED_ROUTE_TABLE_INDEX,
        )
        .unwrap();
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
            // TODO(https://fxbug.dev/336382905): The tests should use the ID.
            table_id: MAIN_FIDL_TABLE_ID,
        };

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route,
                UNMANAGED_ROUTE_TABLE_INDEX,
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
            MAIN_FIDL_TABLE_ID,
        );

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            NetlinkRouteMessage::try_from_installed_route(
                installed_route,
                UNMANAGED_ROUTE_TABLE_INDEX,
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

    enum OnlyRoutes {}
    impl crate::eventloop::EventLoopSpec for OnlyRoutes {
        type InterfacesProxy = Required;
        type InterfacesHandler = Required;
        type RouteClients = Required;

        // To avoid needing a different spec for V4 and V6 tests, just make both routes optional --
        // we're fine with panicking in tests anyway.
        type V4RoutesState = Optional;
        type V6RoutesState = Optional;
        type V4RoutesSetProvider = Optional;
        type V6RoutesSetProvider = Optional;
        type V4RouteTableProvider = Optional;
        type V6RouteTableProvider = Optional;
        type InterfacesStateProxy = Optional;

        type InterfacesWorker = Optional;
        type RoutesV4Worker = Optional;
        type RoutesV6Worker = Optional;
        type RuleV4Worker = Optional;
        type RuleV6Worker = Optional;
    }

    struct Setup<W, R> {
        pub event_loop_inputs: crate::eventloop::EventLoopInputs<
            FakeInterfacesHandler,
            FakeSender<RouteNetlinkMessage>,
            OnlyRoutes,
        >,
        pub watcher_stream: W,
        pub route_sets: R,
        pub interfaces_request_stream: fnet_root::InterfacesRequestStream,
        pub request_sink:
            mpsc::Sender<crate::eventloop::UnifiedRequest<FakeSender<RouteNetlinkMessage>>>,
    }

    fn setup_with_route_clients_yielding_admin_server_ends<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RouteNetlinkMessage>>,
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        (ServerEnd<I::RouteTableMarker>, ServerEnd<I::RouteTableProviderMarker>),
    > {
        let (interfaces_handler, _interfaces_handler_sink) = FakeInterfacesHandler::new();
        let (request_sink, request_stream) = mpsc::channel(1);
        let (interfaces_proxy, interfaces) =
            fidl::endpoints::create_proxy::<fnet_root::InterfacesMarker>();

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct ServerEnds<
            I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
        > {
            routes_state: ServerEnd<I::StateMarker>,
            routes_set_provider: ServerEnd<I::RouteTableMarker>,
            route_table_provider: ServerEnd<I::RouteTableProviderMarker>,
        }

        let base_inputs = crate::eventloop::EventLoopInputs {
            interfaces_handler: EventLoopComponent::Present(interfaces_handler),
            route_clients: EventLoopComponent::Present(route_clients),
            interfaces_proxy: EventLoopComponent::Present(interfaces_proxy),

            interfaces_state_proxy: EventLoopComponent::Absent(Optional),
            v4_routes_state: EventLoopComponent::Absent(Optional),
            v6_routes_state: EventLoopComponent::Absent(Optional),
            v4_main_route_table: EventLoopComponent::Absent(Optional),
            v6_main_route_table: EventLoopComponent::Absent(Optional),
            v4_route_table_provider: EventLoopComponent::Absent(Optional),
            v6_route_table_provider: EventLoopComponent::Absent(Optional),
            v4_rule_table: EventLoopComponent::Absent(Optional),
            v6_rule_table: EventLoopComponent::Absent(Optional),

            unified_request_stream: request_stream,
        };

        let (IpInvariant(inputs), server_ends) = I::map_ip_out(
            base_inputs,
            |base_inputs| {
                let (v4_routes_state, routes_state) =
                    fidl::endpoints::create_proxy::<fnet_routes::StateV4Marker>();
                let (v4_main_route_table, routes_set_provider) =
                    fidl::endpoints::create_proxy::<fnet_routes_admin::RouteTableV4Marker>();
                let (v4_route_table_provider, route_table_provider) = fidl::endpoints::create_proxy::<
                    fnet_routes_admin::RouteTableProviderV4Marker,
                >();
                let inputs = crate::eventloop::EventLoopInputs {
                    v4_routes_state: EventLoopComponent::Present(v4_routes_state),
                    v4_main_route_table: EventLoopComponent::Present(v4_main_route_table),
                    v4_route_table_provider: EventLoopComponent::Present(v4_route_table_provider),
                    ..base_inputs
                };
                let server_ends =
                    ServerEnds::<Ipv4> { routes_state, routes_set_provider, route_table_provider };
                (IpInvariant(inputs), server_ends)
            },
            |base_inputs| {
                let (v6_routes_state, routes_state) =
                    fidl::endpoints::create_proxy::<fnet_routes::StateV6Marker>();
                let (v6_main_route_table, routes_set_provider) =
                    fidl::endpoints::create_proxy::<fnet_routes_admin::RouteTableV6Marker>();
                let (v6_route_table_provider, route_table_provider) = fidl::endpoints::create_proxy::<
                    fnet_routes_admin::RouteTableProviderV6Marker,
                >();
                let inputs = crate::eventloop::EventLoopInputs {
                    v6_routes_state: EventLoopComponent::Present(v6_routes_state),
                    v6_main_route_table: EventLoopComponent::Present(v6_main_route_table),
                    v6_route_table_provider: EventLoopComponent::Present(v6_route_table_provider),
                    ..base_inputs
                };
                let server_ends =
                    ServerEnds::<Ipv6> { routes_state, routes_set_provider, route_table_provider };
                (IpInvariant(inputs), server_ends)
            },
        );

        let ServerEnds { routes_state, routes_set_provider, route_table_provider } = server_ends;

        let state_stream = routes_state.into_stream().boxed_local();

        let interfaces_request_stream = interfaces.into_stream();

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
                        } => WatcherRequestWrapper { watcher: watcher.into_stream() },
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
                        } => WatcherRequestWrapper { watcher: watcher.into_stream() },
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
            .flatten()
            .fuse();

        Setup {
            event_loop_inputs: inputs,
            watcher_stream,
            route_sets: (routes_set_provider, route_table_provider),
            interfaces_request_stream,
            request_sink,
        }
    }

    fn setup_with_route_clients<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RouteNetlinkMessage>>,
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<
            Item = (
                fnet_routes_ext::TableId,
                <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            ),
        >,
    > {
        let Setup {
            event_loop_inputs,
            watcher_stream,
            route_sets: (routes_set_provider, route_table_provider),
            interfaces_request_stream,
            request_sink,
        } = setup_with_route_clients_yielding_admin_server_ends::<I>(route_clients);
        let route_set_stream =
            fnet_routes_ext::testutil::admin::serve_all_route_sets_with_table_id::<I>(
                routes_set_provider,
                Some(MAIN_FIDL_TABLE_ID),
            )
            .map(|item| (MAIN_FIDL_TABLE_ID, item));

        let route_table_provider_request_stream = route_table_provider.into_stream();

        let table_id = AtomicU32::new(OTHER_FIDL_TABLE_ID.get());

        let route_sets_from_route_table_provider =
            futures::TryStreamExt::map_ok(route_table_provider_request_stream, move |request| {
                let (server_end, _name) =
                    fnet_routes_ext::admin::unpack_route_table_provider_request::<I>(request);
                let table_id =
                    fnet_routes_ext::TableId::new(table_id.fetch_add(1, Ordering::SeqCst));
                fnet_routes_ext::testutil::admin::serve_all_route_sets_with_table_id::<I>(
                    server_end,
                    Some(table_id),
                )
                .map(move |route_set_request| (table_id, route_set_request))
            })
            .map(|result| result.expect("should not get FIDL error"))
            .flatten_unordered(None)
            .fuse();
        let route_set_stream = futures::stream::select_all([
            route_set_stream.left_stream(),
            route_sets_from_route_table_provider.right_stream(),
        ])
        .fuse();

        Setup {
            event_loop_inputs,
            watcher_stream,
            route_sets: route_set_stream,
            interfaces_request_stream,
            request_sink,
        }
    }

    fn setup<I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt>(
    ) -> Setup<
        impl Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>,
        impl Stream<
            Item = (
                fnet_routes_ext::TableId,
                <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            ),
        >,
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
                I::map_ip_in(
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
        let route = create_installed_route(
            subnet,
            Some(next_hop),
            DEV1.into(),
            METRIC1,
            MAIN_FIDL_TABLE_ID,
        );

        event_loop_errors_stream_ended_helper::<A::Version>(route).await;
        event_loop_errors_existing_after_add_helper::<A::Version>(route).await;
        event_loop_errors_duplicate_adds_helper::<A::Version>(route).await;
    }

    async fn run_event_loop<I: Ip>(
        inputs: crate::eventloop::EventLoopInputs<
            FakeInterfacesHandler,
            FakeSender<RouteNetlinkMessage>,
            OnlyRoutes,
        >,
    ) -> Result<Never, Error> {
        let included_workers = match I::VERSION {
            IpVersion::V4 => crate::eventloop::IncludedWorkers {
                routes_v4: EventLoopComponent::Present(()),
                routes_v6: EventLoopComponent::Absent(Optional),
                interfaces: EventLoopComponent::Absent(Optional),
                rules_v4: EventLoopComponent::Absent(Optional),
                rules_v6: EventLoopComponent::Absent(Optional),
            },
            IpVersion::V6 => crate::eventloop::IncludedWorkers {
                routes_v4: EventLoopComponent::Absent(Optional),
                routes_v6: EventLoopComponent::Present(()),
                interfaces: EventLoopComponent::Absent(Optional),
                rules_v4: EventLoopComponent::Absent(Optional),
                rules_v6: EventLoopComponent::Absent(Optional),
            },
        };

        let event_loop = inputs.initialize(included_workers).await?;
        event_loop.run().await
    }

    async fn event_loop_errors_stream_ended_helper<
        I: fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
    >(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) {
        let Setup {
            event_loop_inputs,
            watcher_stream,
            route_sets: route_set_stream,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = pin!(run_event_loop::<I>(event_loop_inputs));
        let watcher_fut = pin!(respond_to_watcher_with_routes(watcher_stream, [route], None));
        // We don't expect to handle any route set requests, but we still need to drain this stream
        // so that we handle GetTableId requests.
        let drain_route_sets_fut =
            pin!(route_set_stream.for_each(|(_table_id, _route_set_request)| async move {
                panic!("not actually handling any route set requests")
            }));

        let (err, (), ()) =
            futures::future::join3(event_loop_fut, watcher_fut, drain_route_sets_fut).await;

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
            event_loop_inputs,
            watcher_stream,
            route_sets: route_set_stream,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = pin!(run_event_loop::<I>(event_loop_inputs));
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Existing(route.clone());
        let watcher_fut =
            pin!(respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event)));
        // We don't expect to handle any route set requests, but we still need to drain this stream
        // so that we handle GetTableId requests.
        let drain_route_sets_fut =
            pin!(route_set_stream.for_each(|(_table_id, _route_set_request)| async move {
                panic!("not actually handling any route set requests")
            }));

        let (err, (), ()) =
            futures::future::join3(event_loop_fut, watcher_fut, drain_route_sets_fut).await;

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
            event_loop_inputs,
            watcher_stream,
            route_sets: route_set_stream,
            interfaces_request_stream: _,
            request_sink: _,
        } = setup::<I>();
        let event_loop_fut = pin!(run_event_loop::<I>(event_loop_inputs));
        let routes_existing = [route.clone()];
        let new_event = fnet_routes_ext::Event::Added(route.clone());
        let watcher_fut =
            pin!(respond_to_watcher_with_routes(watcher_stream, routes_existing, Some(new_event)));
        // We don't expect to handle any route set requests, but we still need to drain this stream
        // so that we handle GetTableId requests.
        let drain_route_sets_fut =
            pin!(route_set_stream.for_each(|(_table_id, _route_set_request)| async move {
                panic!("not actually handling any route set requests")
            }));

        let (err, (), ()) =
            futures::future::join3(event_loop_fut, watcher_fut, drain_route_sets_fut).await;

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
    ) -> [RequestArgs<A::Version>; 2]
    where
        A::Version: fnet_routes_ext::FidlRouteIpExt,
    {
        [
            RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                create_unicast_new_route_args(
                    subnet,
                    next_hop1,
                    DEV1.into(),
                    METRIC1,
                    MANAGED_ROUTE_TABLE_INDEX,
                ),
            ))),
            RequestArgs::Route(RouteRequestArgs::New(NewRouteArgs::Unicast(
                create_unicast_new_route_args(
                    subnet,
                    next_hop2,
                    DEV2.into(),
                    METRIC2,
                    MANAGED_ROUTE_TABLE_INDEX,
                ),
            ))),
        ]
    }

    fn create_unicast_new_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: A,
        interface_id: u64,
        priority: u32,
        table: NetlinkRouteTableIndex,
    ) -> UnicastNewRouteArgs<A::Version> {
        UnicastNewRouteArgs {
            subnet,
            target: fnet_routes_ext::RouteTarget {
                outbound_interface: interface_id,
                next_hop: SpecifiedAddr::new(next_hop),
            },
            priority,
            table,
        }
    }

    fn create_unicast_del_route_args<A: IpAddress>(
        subnet: Subnet<A>,
        next_hop: Option<A>,
        interface_id: Option<u64>,
        priority: Option<u32>,
        table: NetlinkRouteTableIndex,
    ) -> UnicastDelRouteArgs<A::Version> {
        UnicastDelRouteArgs {
            subnet,
            outbound_interface: interface_id.map(NonZeroU64::new).flatten(),
            next_hop: next_hop.map(SpecifiedAddr::new).flatten(),
            priority: priority.map(NonZeroU32::new).flatten(),
            table: NonZeroNetlinkRouteTableIndex::new(table).unwrap(),
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
        route_set_results: HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>>,
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
            event_loop_inputs,
            mut watcher_stream,
            route_sets: mut route_set_stream,
            interfaces_request_stream,
            request_sink,
        } = setup_with_route_clients::<A::Version>({
            let route_clients = ClientTable::default();
            route_clients.add_client(route_client.clone());
            route_clients.add_client(other_client);
            route_clients
        });

        let mut event_loop_fut = pin!(run_event_loop::<A::Version>(event_loop_inputs)
            .map(|res| match res {
                Err(e) => {
                    log_debug!("event_loop_fut exiting with error {:?}", e);
                    Err::<std::convert::Infallible, _>(e)
                }
            })
            .fuse());

        let watcher_stream_fut = respond_to_watcher::<A::Version, _>(
            watcher_stream.by_ref(),
            std::iter::once(fnet_routes_ext::Event::<A::Version>::Idle.try_into().unwrap()),
        );
        futures::select! {
            () = watcher_stream_fut.fuse() => {},
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }
        assert_eq!(&route_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

        let route_client = &route_client;
        let fut = async {
            // Add some initial route state by sending through PendingRequests.
            let initial_new_routes =
                get_test_route_events_new_route_args(subnet, next_hop1, next_hop2);
            let count_initial_new_routes = initial_new_routes.len();

            let request_sink = futures::stream::iter(initial_new_routes)
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
            for _ in 0..count_initial_new_routes {
                // Drop two messages: once for adding the route to the managed table,
                // and another for the double-writing of the route to the main table.
                let _ = route_sink.next_message().await;
                let _ = route_sink.next_message().await;
            }
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
            res = futures::future::join3(
                    route_set_fut,
                    root_interfaces_fut,
                    event_loop_fut,
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
                    HashMap::new(),
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

    #[derive(Debug, Clone, Copy)]
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
        table_id: fnet_routes_ext::TableId,
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
            // TODO(https://fxbug.dev/336382905): The tests should use the ID.
            table_id,
        })
        .try_into()
        .unwrap()
    }

    // Handle RouteSet API requests then feed the returned
    // `fuchsia.net.routes.ext/Event`s to the routes watcher.
    async fn respond_to_route_set_modifications<
        I: Ip + fnet_routes_ext::FidlRouteIpExt + fnet_routes_ext::admin::FidlRouteAdminIpExt,
        RS: Stream<
            Item = (
                fnet_routes_ext::TableId,
                <<I::RouteSetMarker as ProtocolMarker>::RequestStream as Stream>::Item,
            ),
        >,
        WS: Stream<Item = <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item>
            + std::marker::Unpin,
    >(
        route_stream: RS,
        mut watcher_stream: WS,
        mut route_set_results: HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>>,
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

        let mut route_stream = std::pin::pin!(route_stream);
        let mut watcher_stream = std::pin::pin!(watcher_stream);

        {
            // TODO(https://fxbug.dev/337297829): Cleanup - add these `RouteSetResult`s to the
            // `RouteSetResult` iterator instead of prepending them here.
            // Handle the add requests for the initial two routes added to the route stream
            // from `get_test_route_events_new_route_args`.
            let queue = route_set_results.entry(MAIN_FIDL_TABLE_ID).or_default();
            queue.push_front(RouteSetResult::AddResult(Ok(true)));
            queue.push_front(RouteSetResult::AddResult(Ok(true)));

            let queue = route_set_results.entry(OTHER_FIDL_TABLE_ID).or_default();
            queue.push_front(RouteSetResult::AddResult(Ok(true)));
            queue.push_front(RouteSetResult::AddResult(Ok(true)));
        }

        while let Some((table_id, request)) = route_stream.next().await {
            let route_set_result = route_set_results
                .get_mut(&table_id)
                .unwrap_or_else(|| panic!("missing result for {table_id:?}"))
                .pop_front()
                .unwrap_or_else(|| panic!("missing result for {table_id:?}"));
            let RouteSetOutputs { event } = I::map_ip(
                RouteSetInputs { request, route_set_result },
                |RouteSetInputs { request, route_set_result }| {
                    let request = request.expect("failed to receive request");
                    crate::logging::log_debug!(
                        "responding on {table_id:?} to route set request {request:?} \
                        with result {route_set_result:?}"
                    );
                    match request {
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
                                        table_id,
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
                                        table_id,
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
                    }
                },
                |RouteSetInputs { request, route_set_result }| {
                    let request = request.expect("failed to receive request");
                    crate::logging::log_debug!(
                        "responding on {table_id:?} to route set request {request:?} \
                        with result {route_set_result:?}"
                    );
                    match request {
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
                                        table_id,
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
                                        table_id,
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
                    }
                },
            );

            if let Some(update) = event {
                let request = watcher_stream.next().await.expect("watcher stream should not end");

                #[derive(GenericOverIp)]
                #[generic_over_ip(I, Ip)]
                struct HandleInputs<I: fnet_routes_ext::FidlRouteIpExt> {
                    request: <<I::WatcherMarker as ProtocolMarker>::RequestStream as Stream>::Item,
                    update: I::WatchEvent,
                }

                I::map_ip_in(
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
            }
        }

        if route_set_results.values().any(|value| !value.is_empty()) {
            panic!("unused route_set_results entries: {route_set_results:?}");
        }
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
        route_set_results: HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>>,
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
                                Some(control.into_stream())
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
        route_set_results: HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>>,
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
                    route_set_results,
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

    fn both_main_table_and_other_table(
        results: Vec<RouteSetResult>,
        table_id: fnet_routes_ext::TableId,
    ) -> HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>> {
        HashMap::from_iter([
            (MAIN_FIDL_TABLE_ID, results.clone().into()),
            (table_id, results.into()),
        ])
    }

    fn both_main_table_and_first_new_table(
        results: Vec<RouteSetResult>,
    ) -> HashMap<fnet_routes_ext::TableId, VecDeque<RouteSetResult>> {
        both_main_table_and_other_table(results, OTHER_FIDL_TABLE_ID)
    }

    // Tests RTM_NEWROUTE with all interesting responses to add a route.
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        Some(METRIC3),
        DEV1;
        "v4_new_success")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        Some(METRIC3),
        DEV1;
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
        Some(METRIC3),
        DEV1;
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
        Some(METRIC3),
        DEV1;
        "v6_new_failed_auth")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        V4_SUB1,
        Some(METRIC3),
        DEV1;
        "v4_new_failed_netstack_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Ok(false))
        ],
        Err(RequestError::AlreadyExists),
        V6_SUB1,
        Some(METRIC3),
        DEV1;
        "v6_new_failed_netstack_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V4_SUB1,
        Some(METRIC1),
        DEV1;
        "v4_new_failed_netlink_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V4_SUB1,
        Some(METRIC1),
        DEV2;
        "v4_new_failed_netlink_reports_exists_different_interface")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V6_SUB1,
        Some(METRIC1),
        DEV1;
        "v6_new_failed_netlink_reports_exists")]
    #[test_case(
        RouteRequestKind::New,
        vec![],
        Err(RequestError::AlreadyExists),
        V6_SUB1,
        Some(METRIC1),
        DEV2;
        "v6_new_failed_netlink_reports_exists_different_interface")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        Some(METRIC3),
        DEV1;
        "v4_new_invalid_dest")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        Some(METRIC3),
        DEV1;
        "v6_new_invalid_dest")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        Some(METRIC3),
        DEV1;
        "v4_new_invalid_hop")]
    #[test_case(
        RouteRequestKind::New,
        vec![
            RouteSetResult::AddResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        Some(METRIC3),
        DEV1;
        "v6_new_invalid_hop")]
    // Tests RTM_DELROUTE with all interesting responses to remove a route.
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        None,
        DEV1;
        "v4_del_success_only_subnet")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V4_SUB1,
        Some(METRIC1),
        DEV1;
        "v4_del_success_only_subnet_metric")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        None,
        DEV1;
        "v6_del_success_only_subnet")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(true))
        ],
        Ok(()),
        V6_SUB1,
        Some(METRIC1),
        DEV1;
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
        None,
        DEV1;
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
        None,
        DEV1;
        "v6_del_failed_auth")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(false))
        ],
        Err(RequestError::DeletionNotAllowed),
        V4_SUB1,
        None,
        DEV1;
        "v4_del_failed_attempt_to_delete_route_from_global_set")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Ok(false))
        ],
        Err(RequestError::DeletionNotAllowed),
        V6_SUB1,
        None,
        DEV1;
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
        Some(METRIC3),
        DEV1;
        "v4_del_no_matching_route")]
    #[test_case(
        RouteRequestKind::Del,
        vec![],
        Err(RequestError::NotFound),
        V6_SUB1,
        Some(METRIC3),
        DEV1;
        "v6_del_no_matching_route")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        None,
        DEV1;
        "v4_del_invalid_dest")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidDestinationSubnet))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        None,
        DEV1;
        "v6_del_invalid_dest")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V4_SUB1,
        None,
        DEV1;
        "v4_del_invalid_hop")]
    #[test_case(
        RouteRequestKind::Del,
        vec![
            RouteSetResult::DelResult(Err(RouteSetError::InvalidNextHop))
        ],
        Err(RequestError::InvalidRequest),
        V6_SUB1,
        None,
        DEV1;
        "v6_del_invalid_hop")]
    #[fuchsia::test]
    async fn test_new_del_route<A: IpAddress>(
        kind: RouteRequestKind,
        route_set_results: Vec<RouteSetResult>,
        waiter_result: Result<(), RequestError>,
        subnet: Subnet<A>,
        metric: Option<u32>,
        interface_id: u32,
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
                    interface_id.into(),
                    metric.expect("add cases should be Some"),
                    MANAGED_ROUTE_TABLE_INDEX,
                )))
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                    subnet,
                    None,
                    None,
                    metric,
                    MANAGED_ROUTE_TABLE_INDEX,
                )))
            }
        };

        // When the waiter result is Ok(()), then we know that the add or delete
        // was successful and we got a message.
        let messages = match waiter_result {
            Ok(()) => {
                let build_message = |table| {
                    let table = RouteTableKey::from(NetlinkRouteTableIndex::new(table));
                    let route_message = create_netlink_route_message::<A::Version>(
                        subnet.prefix(),
                        table,
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
                            match table {
                                RouteTableKey::Unmanaged => None,
                                RouteTableKey::NetlinkManaged { table_id } => {
                                    Some(table_id.get().get())
                                }
                            },
                        ),
                    );
                    let netlink_message = match kind {
                        RouteRequestKind::New => {
                            route_message.into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false)
                        }
                        RouteRequestKind::Del => route_message.into_rtnl_del_route(),
                    };
                    SentMessage::multicast(netlink_message, route_group)
                };

                let route_message_in_managed_table = build_message(MANAGED_ROUTE_TABLE_ID);

                // TODO(https://fxbug.dev/358649849): Remove this "double-written" route once rules
                // are properly supported.
                let route_message_in_unmanaged_table =
                    build_message(UNMANAGED_ROUTE_TABLE_INDEX.get());
                vec![route_message_in_unmanaged_table, route_message_in_managed_table]
            }
            Err(_) => Vec::new(),
        };

        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            messages,
            both_main_table_and_first_new_table(route_set_results),
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
                    MANAGED_ROUTE_TABLE_INDEX,
                )))
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                    subnet,
                    None,
                    None,
                    None,
                    MANAGED_ROUTE_TABLE_INDEX,
                )))
            }
        };
        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            Vec::new(),
            both_main_table_and_first_new_table(route_set_results),
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
                NetlinkRouteTableIndex::new(1234),
            )));
        test_route_requests_helper(
            [RequestArgs::Route(route_req_args)],
            Vec::new(),
            HashMap::new(),
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
        MANAGED_ROUTE_TABLE_INDEX;
        "v4_new_same_table_dump")]
    #[test_case(
        V6_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        MANAGED_ROUTE_TABLE_INDEX;
        "v6_new_same_table_dump")]
    #[test_case(
        V4_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE),
        NetlinkRouteTableIndex::new(1234);
        "v4_new_different_table_dump")]
    #[test_case(
        V6_SUB1,
        ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE),
        NetlinkRouteTableIndex::new(1234);
        "v6_new_different_table_dump")]
    #[fuchsia::test]
    async fn test_new_then_get_dump_request<A: IpAddress>(
        subnet: Subnet<A>,
        group: ModernGroup,
        table: NetlinkRouteTableIndex,
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

        // We expect to see two multicast message, representing the route that was added to
        // a managed table and double-written to the main table.
        // Then, three unicast messages, representing the two routes that existed already in the
        // route set, and the one new route that was added.
        let messages = vec![
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    RouteTableKey::Unmanaged,
                    create_nlas::<A::Version>(Some(subnet), Some(next_hop1), DEV1, METRIC3, None),
                )
                .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                group,
            ),
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    RouteTableKey::from(table),
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(table.get()),
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
                    RouteTableKey::from(table),
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC3,
                        Some(table.get()),
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
            both_main_table_and_other_table(
                vec![RouteSetResult::AddResult(Ok(true))],
                if table == MANAGED_ROUTE_TABLE_INDEX {
                    OTHER_FIDL_TABLE_ID
                } else {
                    fnet_routes_ext::TableId::new(OTHER_FIDL_TABLE_ID.get() + 1)
                },
            ),
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
        let (next_hop1, next_hop2, IpInvariant(group)): (A, A, IpInvariant<ModernGroup>) =
            A::Version::map_ip(
                (),
                |()| {
                    (
                        V4_NEXTHOP1,
                        V4_NEXTHOP2,
                        IpInvariant(ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE)),
                    )
                },
                |()| {
                    (
                        V6_NEXTHOP1,
                        V6_NEXTHOP2,
                        IpInvariant(ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_ROUTE)),
                    )
                },
            );

        const ALTERNATIVE_ROUTE_TABLE: NetlinkRouteTableIndex = NetlinkRouteTableIndex::new(1337);

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
            ALTERNATIVE_ROUTE_TABLE,
        );

        // We expect to see one multicast message for having added the new route, then three unicast
        // messages, for dumping the two routes that existed already in the route set, plus the new
        // one we added.
        let messages = vec![
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    RouteTableKey::from(ALTERNATIVE_ROUTE_TABLE),
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(ALTERNATIVE_ROUTE_TABLE.get()),
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
                    RouteTableKey::from(ALTERNATIVE_ROUTE_TABLE),
                    create_nlas::<A::Version>(
                        Some(subnet),
                        Some(next_hop1),
                        DEV1,
                        METRIC1,
                        Some(ALTERNATIVE_ROUTE_TABLE.get()),
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
            HashMap::from_iter([
                // The added route already existed in the main table.
                (MAIN_FIDL_TABLE_ID, vec![RouteSetResult::AddResult(Ok(false))].into()),
                // But it is new to the other table.
                (
                    fnet_routes_ext::TableId::new(OTHER_FIDL_TABLE_ID.get() + 1),
                    vec![RouteSetResult::AddResult(Ok(true))].into(),
                ),
            ]),
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
            MANAGED_ROUTE_TABLE_INDEX,
        );

        // The subnet and metric are enough to uniquely identify the above route.
        let del_route_args = create_unicast_del_route_args(
            subnet,
            None,
            None,
            Some(METRIC3),
            MANAGED_ROUTE_TABLE_INDEX,
        );

        // We expect to see four multicast messages, the first two representing the route that was
        // added (primarily to the managed table and double-written to the main table), and the
        // other two representing the same route being removed. Then, two unicast messages,
        // representing the two routes that existed already in the route set.
        let messages = vec![
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    RouteTableKey::Unmanaged,
                    create_nlas::<A::Version>(Some(subnet), Some(next_hop1), DEV1, METRIC3, None),
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
                .into_rtnl_new_route(UNSPECIFIED_SEQUENCE_NUMBER, false),
                group,
            ),
            SentMessage::multicast(
                create_netlink_route_message::<A::Version>(
                    subnet.prefix(),
                    RouteTableKey::Unmanaged,
                    create_nlas::<A::Version>(Some(subnet), Some(next_hop1), DEV1, METRIC3, None),
                )
                .into_rtnl_del_route(),
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
            both_main_table_and_first_new_table(vec![
                RouteSetResult::AddResult(Ok(true)),
                RouteSetResult::DelResult(Ok(true)),
            ]),
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
                        MANAGED_ROUTE_TABLE_INDEX,
                    )));
                let res = RouteSetResult::AddResult(Err(RouteSetError::Unauthenticated));
                (args, res)
            }
            RouteRequestKind::Del => {
                // Remove an existing route.
                let args =
                    RouteRequestArgs::Del(DelRouteArgs::Unicast(create_unicast_del_route_args(
                        subnet,
                        None,
                        None,
                        None,
                        MANAGED_ROUTE_TABLE_INDEX,
                    )));
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
                                    let control = control.into_stream();
                                    let control = control.control_handle();
                                    control.shutdown();
                                }
                                req => unreachable!("unexpected interfaces request: {req:?}"),
                            })
                        })
                        .await
                },
                both_main_table_and_first_new_table(vec![route_set_result]),
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

    impl<I: Ip> Route<I> {
        fn to_installed_route(
            self,
            table_id: fnet_routes_ext::TableId,
        ) -> fnet_routes_ext::InstalledRoute<I> {
            let Self { subnet, device, nexthop, metric } = self;
            fnet_routes_ext::InstalledRoute {
                route: fnet_routes_ext::Route {
                    destination: subnet,
                    action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                        outbound_interface: device.into(),
                        next_hop: nexthop
                            .map(|a| SpecifiedAddr::new(a).expect("nexthop should be specified")),
                    }),
                    properties: fnet_routes_ext::RouteProperties::from_explicit_metric(metric),
                },
                effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
                table_id,
            }
        }
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
        true; "different_device_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "different_device_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        true; "different_nexthop_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        true; "different_nexthop_v6_should_match")]
    #[test_case(
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        true; "different_device_and_nexthop_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        true; "different_device_and_nexthop_v6_should_match")]
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
        true; "nexthop_previously_unset_v4_should_match")]
    #[test_case(
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        true; "nexthop_previously_unset_v6_should_match")]
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
    fn test_new_route_matcher<I: Ip>(
        route1: Route<I>,
        route2: Route<I>,
        expected_to_conflict: bool,
    ) {
        let route1 = route1.to_installed_route(MAIN_FIDL_TABLE_ID);
        let route2 = route2.to_installed_route(MAIN_FIDL_TABLE_ID);

        let got_conflict = routes_conflict::<I>(route1, route2.route, route2.table_id);
        assert_eq!(got_conflict, expected_to_conflict);

        let got_conflict = routes_conflict::<I>(route2, route1.route, route1.table_id);
        assert_eq!(got_conflict, expected_to_conflict);
    }

    // Calls `select_route_for_deletion` with the given args & existing_routes.
    //
    // Asserts that the return route matches the route in `existing_routes` at
    // `expected_index`.
    fn test_select_route_for_deletion_helper<
        I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    >(
        args: UnicastDelRouteArgs<I>,
        existing_routes: &[Route<I>],
        // The index into `existing_routes` of the route that should be selected.
        expected_index: Option<usize>,
    ) {
        let mut fidl_route_map = FidlRouteMap::<I>::default();

        // We create a bunch of proxies that go unused in this test. In order for this to succeed
        // we must have an executor.
        let _executor = fuchsia_async::TestExecutor::new();

        let (main_route_table_proxy, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableMarker>();
        let (own_route_table_proxy, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableMarker>();
        let (route_set_proxy, _server_end) = fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_set_from_main_table_proxy, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (unmanaged_route_set_proxy, _unmanaged_route_set_server_end) =
            fidl::endpoints::create_proxy::<I::RouteSetMarker>();
        let (route_table_provider, _server_end) =
            fidl::endpoints::create_proxy::<I::RouteTableProviderMarker>();

        let mut route_table_map = RouteTableMap::<I>::new(
            main_route_table_proxy,
            MAIN_FIDL_TABLE_ID,
            unmanaged_route_set_proxy,
            route_table_provider,
        );

        route_table_map.insert(
            ManagedNetlinkRouteTableIndex::new(MANAGED_ROUTE_TABLE_INDEX).unwrap(),
            RouteTable {
                route_table_proxy: own_route_table_proxy,
                route_set_proxy,
                route_set_from_main_table_proxy,
                fidl_table_id: OTHER_FIDL_TABLE_ID,
                rule_set_authenticated: false,
            },
        );

        for Route { subnet, device, nexthop, metric } in existing_routes {
            let fnet_routes_ext::InstalledRoute { route, effective_properties, table_id } =
                create_installed_route::<I>(
                    *subnet,
                    *nexthop,
                    (*device).into(),
                    *metric,
                    OTHER_FIDL_TABLE_ID,
                );
            assert_matches!(fidl_route_map.add(route, table_id, effective_properties), None);
        }

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
                &fidl_route_map,
                &route_table_map,
                DelRouteArgs::Unicast(args).try_into().unwrap(),
            ),
            expected_route
        )
    }

    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB2, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "subnet_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB3, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "subnet_prefix_len_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "subnet_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV2, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        false; "interface_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "interface_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_absent_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP2), metric: METRIC1, },
        false; "nexthop_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V4_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: Some(V4_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC2, },
        false; "metric_does_not_match_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv4>{subnet: V4_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        true; "metric_matches_v4")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB2, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "subnet_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB3, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "subnet_prefix_len_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "subnet_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV2, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        false; "interface_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: Some(NonZeroU64::new(DEV1.into()).unwrap()),
            next_hop: None, priority: None, table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "interface_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        false; "nexthop_absent_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP2), metric: METRIC1, },
        false; "nexthop_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: Some(SpecifiedAddr::new(V6_NEXTHOP1).unwrap()), priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        true; "nexthop_matches_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC2, },
        false; "metric_does_not_match_v6")]
    #[test_case(
        UnicastDelRouteArgs::<Ipv6> {
            subnet: V6_SUB1, outbound_interface: None,
            next_hop: None, priority: Some(NonZeroU32::new(METRIC1).unwrap()),
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(
                NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()
            ),
        },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: None, metric: METRIC1, },
        true; "metric_matches_v6")]
    fn test_select_route_for_deletion<
        I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    >(
        args: UnicastDelRouteArgs<I>,
        existing_route: Route<I>,
        expect_match: bool,
    ) {
        test_select_route_for_deletion_helper(args, &[existing_route], expect_match.then_some(0))
    }

    #[test_case(
        UnicastDelRouteArgs::<Ipv4> {
            subnet: V4_SUB1, outbound_interface: None, next_hop: None, priority: None,
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()),
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
            table: NonZeroNetlinkRouteTableIndex::new_non_zero(NonZeroU32::new(MANAGED_ROUTE_TABLE_INDEX.get()).unwrap()),
        },
        &[
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC2, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC1, },
        Route::<Ipv6>{subnet: V6_SUB1, device: DEV1, nexthop: Some(V6_NEXTHOP1), metric: METRIC3, },
        ],
        Some(1); "multiple_matches_prefers_lowest_metric_v6")]
    fn test_select_route_for_deletion_multiple_matches<
        I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    >(
        args: UnicastDelRouteArgs<I>,
        existing_routes: &[Route<I>],
        expected_index: Option<usize>,
    ) {
        test_select_route_for_deletion_helper(args, existing_routes, expected_index);
    }

    #[ip_test(I)]
    #[fuchsia::test]
    async fn garbage_collects_empty_table<
        I: Ip + fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    >() {
        let (_route_sink, route_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[ModernGroup(match I::VERSION {
                IpVersion::V4 => rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
                IpVersion::V6 => rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
            })],
        );
        let route_clients = ClientTable::default();
        route_clients.add_client(route_client.clone());

        let Setup {
            event_loop_inputs,
            watcher_stream,
            route_sets: (main_route_table_server_end, route_table_provider_server_end),
            interfaces_request_stream: _,
            mut request_sink,
        } = setup_with_route_clients_yielding_admin_server_ends::<I>(route_clients);

        let mut main_route_table_fut =
            pin!(fnet_routes_ext::testutil::admin::serve_noop_route_sets_with_table_id::<I>(
                main_route_table_server_end,
                MAIN_FIDL_TABLE_ID
            )
            .fuse());

        let mut watcher_stream = pin!(watcher_stream.fuse());
        let mut route_table_provider_stream =
            pin!(route_table_provider_server_end.into_stream().fuse());

        let mut event_loop = {
            let included_workers = match I::VERSION {
                IpVersion::V4 => crate::eventloop::IncludedWorkers {
                    routes_v4: EventLoopComponent::Present(()),
                    routes_v6: EventLoopComponent::Absent(Optional),
                    interfaces: EventLoopComponent::Absent(Optional),
                    rules_v4: EventLoopComponent::Absent(Optional),
                    rules_v6: EventLoopComponent::Absent(Optional),
                },
                IpVersion::V6 => crate::eventloop::IncludedWorkers {
                    routes_v4: EventLoopComponent::Absent(Optional),
                    routes_v6: EventLoopComponent::Present(()),
                    interfaces: EventLoopComponent::Absent(Optional),
                    rules_v4: EventLoopComponent::Absent(Optional),
                    rules_v6: EventLoopComponent::Absent(Optional),
                },
            };

            let event_loop_fut = event_loop_inputs.initialize(included_workers).fuse();
            let watcher_fut = async {
                let watch_req =
                    watcher_stream.by_ref().next().await.expect("should not have ended");
                // Start with no routes.
                fnet_routes_ext::testutil::handle_watch::<I>(
                    watch_req,
                    vec![fnet_routes_ext::Event::<I>::Idle.try_into().unwrap()],
                )
            }
            .fuse();

            futures::select! {
                () = main_route_table_fut => unreachable!(),
                (event_loop_result, ()) = futures::future::join(event_loop_fut, watcher_fut) => {
                    event_loop_result.expect("should not get error")
                }
            }
        };

        let (completer, mut initial_add_request_waiter) = oneshot::channel();

        let new_route_args = NewRouteArgs::Unicast(I::map_ip_out(
            (),
            |()| {
                create_unicast_new_route_args(
                    V4_SUB1,
                    V4_NEXTHOP1,
                    DEV1.into(),
                    METRIC1,
                    MANAGED_ROUTE_TABLE_INDEX,
                )
            },
            |()| {
                create_unicast_new_route_args(
                    V6_SUB1,
                    V6_NEXTHOP1,
                    DEV1.into(),
                    METRIC1,
                    MANAGED_ROUTE_TABLE_INDEX,
                )
            },
        ));
        let expected_route = fnet_routes_ext::Route::<I>::from(new_route_args);

        // Request that a route is installed in a new table.
        request_sink
            .try_send(
                Request {
                    args: RequestArgs::Route(RouteRequestArgs::New(new_route_args)),
                    sequence_number: TEST_SEQUENCE_NUMBER,
                    client: route_client.clone(),
                    completer,
                }
                .into(),
            )
            .expect("should succeed");

        // Run the event loop and observe the new table get created and the route set requests go
        // out.
        let (mut route_table_stream, mut route_set_stream) = {
            let event_loop_fut = event_loop
                .run_one_step_in_tests()
                .map(|result| result.expect("event loop should not hit error"))
                .fuse();
            let route_table_fut = async {
                let (server_end, _name) =
                    fnet_routes_ext::admin::concretize_route_table_provider_request::<I>(
                        route_table_provider_stream.next().await.expect("should not have ended"),
                    )
                    .expect("should not get error");
                let mut route_table_stream = server_end.into_stream().boxed().fuse();

                let request = I::into_route_table_request_result(
                    route_table_stream.by_ref().next().await.expect("should not have ended"),
                )
                .expect("should not get error");

                let responder = match request {
                    RouteTableRequest::GetTableId { responder } => responder,
                    _ => panic!("should be GetTableId"),
                };
                responder.send(OTHER_FIDL_TABLE_ID.get()).expect("should succeed");

                let request = I::into_route_table_request_result(
                    route_table_stream.by_ref().next().await.expect("should not have ended"),
                )
                .expect("should not get error");

                let server_end = match request {
                    RouteTableRequest::NewRouteSet { route_set, control_handle: _ } => route_set,
                    _ => panic!("should be NewRouteSet"),
                };
                let mut route_set_stream = server_end.into_stream().boxed().fuse();

                let request = I::into_route_set_request_result(
                    route_set_stream.by_ref().next().await.expect("should not have ended"),
                )
                .expect("should not get error");

                let (route, responder) = match request {
                    RouteSetRequest::AddRoute { route, responder } => (route, responder),
                    _ => panic!("should be AddRoute"),
                };
                let route = route.expect("should successfully convert FIDl");
                assert_eq!(route, expected_route);

                responder.send(Ok(true)).expect("sending response should succeed");
                (route_table_stream, route_set_stream)
            }
            .fuse();
            futures::select! {
                () = main_route_table_fut => unreachable!(),
                ((), streams) = futures::future::join(event_loop_fut, route_table_fut) => streams
            }
        };

        {
            let (routes_worker, route_table_map) = event_loop.route_table_state::<I>();
            // The new route table should be present in the map.
            let table = match route_table_map.get(&MANAGED_ROUTE_TABLE_INDEX) {
                Some(RouteTableLookup::Managed(table)) => table,
                _ => panic!("table should be present"),
            };
            assert_eq!(table.fidl_table_id, OTHER_FIDL_TABLE_ID);

            // But the new route won't be tracked because we haven't confirmed it via the watcher
            // yet.
            assert!(routes_worker.fidl_route_map.route_is_uninstalled_in_tables(
                &expected_route,
                [&OTHER_FIDL_TABLE_ID, &MAIN_FIDL_TABLE_ID]
            ));
        }

        // The request won't be complete until we've confirmed addition via the watcher.
        assert_matches!(initial_add_request_waiter.try_recv(), Ok(None));

        // Run the event loop while yielding the new route via the watcher.
        {
            let event_loop_fut = async {
                // Handling two events, so run two steps.
                event_loop.run_one_step_in_tests().await.expect("should not hit error");
                event_loop.run_one_step_in_tests().await.expect("should not hit error");
            }
            .fuse();
            let watcher_fut = async {
                let watch_req =
                    watcher_stream.by_ref().next().await.expect("should not have ended");
                // Show that the route was added for both the main and the owned FIDL table.
                fnet_routes_ext::testutil::handle_watch::<I>(
                    watch_req,
                    vec![
                        fnet_routes_ext::Event::<I>::Added(fnet_routes_ext::InstalledRoute {
                            route: expected_route,
                            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                                metric: METRIC1,
                            },
                            table_id: MAIN_FIDL_TABLE_ID,
                        })
                        .try_into()
                        .unwrap(),
                        fnet_routes_ext::Event::<I>::Added(fnet_routes_ext::InstalledRoute {
                            route: expected_route,
                            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                                metric: METRIC1,
                            },
                            table_id: OTHER_FIDL_TABLE_ID,
                        })
                        .try_into()
                        .unwrap(),
                    ],
                );
            };
            let ((), ()) = futures::join!(event_loop_fut, watcher_fut);
        }

        {
            let (routes_worker, _route_table_map) = event_loop.route_table_state::<I>();

            // The route should be noted as stored in both tables now.
            assert!(routes_worker.fidl_route_map.route_is_installed_in_tables(
                &expected_route,
                [&OTHER_FIDL_TABLE_ID, &MAIN_FIDL_TABLE_ID]
            ));
        }

        assert_matches!(initial_add_request_waiter.try_recv(), Ok(Some(Ok(()))));

        let (completer, mut del_request_waiter) = oneshot::channel();

        let del_route_args = I::map_ip_out(
            (),
            |()| {
                create_unicast_del_route_args(
                    V4_SUB1,
                    Some(V4_NEXTHOP1),
                    Some(DEV1.into()),
                    Some(METRIC1),
                    MANAGED_ROUTE_TABLE_INDEX,
                )
            },
            |()| {
                create_unicast_del_route_args(
                    V6_SUB1,
                    Some(V6_NEXTHOP1),
                    Some(DEV1.into()),
                    Some(METRIC1),
                    MANAGED_ROUTE_TABLE_INDEX,
                )
            },
        );

        // Request the route's removal.
        request_sink
            .try_send(
                Request {
                    args: RequestArgs::Route(RouteRequestArgs::Del(DelRouteArgs::Unicast(
                        del_route_args,
                    ))),
                    sequence_number: TEST_SEQUENCE_NUMBER,
                    client: route_client.clone(),
                    completer,
                }
                .into(),
            )
            .expect("should succeed");

        // Observe and handle the removal requests.
        {
            let event_loop_fut = event_loop
                .run_one_step_in_tests()
                .map(|result| result.expect("event loop should not hit error"))
                .fuse();
            let route_set_fut = async {
                let request = I::into_route_set_request_result(
                    route_set_stream.next().await.expect("should not have ended"),
                )
                .expect("should not get error");
                let (route, responder) = match request {
                    RouteSetRequest::RemoveRoute { route, responder } => (route, responder),
                    _ => panic!("should be DelRoute"),
                };
                let route = route.expect("should successfully convert FIDl");
                assert_eq!(route, expected_route);

                responder.send(Ok(true)).expect("sending response should succeed");
            }
            .fuse();

            futures::select! {
                () = main_route_table_fut => unreachable!(),
                ((), ()) = futures::future::join(event_loop_fut, route_set_fut) => (),
            }
        }

        // We still haven't confirmed the removal via the watcher.
        {
            let (routes_worker, route_table_map) = event_loop.route_table_state::<I>();
            // The route table should still be present in the map.
            let table = match route_table_map.get(&MANAGED_ROUTE_TABLE_INDEX) {
                Some(RouteTableLookup::Managed(table)) => table,
                _ => panic!("table should be present"),
            };
            assert_eq!(table.fidl_table_id, OTHER_FIDL_TABLE_ID);

            assert!(routes_worker.fidl_route_map.route_is_installed_in_tables(
                &expected_route,
                [&OTHER_FIDL_TABLE_ID, &MAIN_FIDL_TABLE_ID]
            ));
        }
        assert_matches!(del_request_waiter.try_recv(), Ok(None));

        // Run the event loop while yielding the deleted route via the watcher.
        {
            let event_loop_fut = async {
                // Handling two events, so run two steps.
                event_loop.run_one_step_in_tests().await.expect("should not hit error");
                event_loop.run_one_step_in_tests().await.expect("should not hit error");
            }
            .fuse();
            let watcher_fut = async {
                let watch_req =
                    watcher_stream.by_ref().next().await.expect("should not have ended");
                // Show that the route was removed for both the main and the owned FIDL table.
                fnet_routes_ext::testutil::handle_watch::<I>(
                    watch_req,
                    vec![
                        fnet_routes_ext::Event::<I>::Removed(fnet_routes_ext::InstalledRoute {
                            route: expected_route,
                            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                                metric: 0,
                            },
                            table_id: MAIN_FIDL_TABLE_ID,
                        })
                        .try_into()
                        .unwrap(),
                        fnet_routes_ext::Event::<I>::Removed(fnet_routes_ext::InstalledRoute {
                            route: expected_route,
                            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                                metric: 0,
                            },
                            table_id: OTHER_FIDL_TABLE_ID,
                        })
                        .try_into()
                        .unwrap(),
                    ],
                );
            };
            let ((), ()) = futures::join!(event_loop_fut, watcher_fut);
        }

        {
            let (routes_worker, route_table_map) = event_loop.route_table_state::<I>();

            // The route should be noted as being removed from both tables now.
            assert!(routes_worker.fidl_route_map.route_is_uninstalled_in_tables(
                &expected_route,
                [&OTHER_FIDL_TABLE_ID, &MAIN_FIDL_TABLE_ID]
            ));

            // And the table should now be cleaned up from the map.
            assert_matches!(route_table_map.get(&MANAGED_ROUTE_TABLE_INDEX), None);
        }
        assert_matches!(del_request_waiter.try_recv(), Ok(Some(Ok(()))));

        // Because the table was dropped from the map, the route table request stream should close.
        let route_table_request = route_table_stream.next().await;
        assert!(route_table_request.is_none());
    }
}
