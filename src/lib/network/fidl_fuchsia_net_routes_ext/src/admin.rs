// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for fuchsia.net.routes.admin.

use std::fmt::Debug;

use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker, ServerEnd};
use futures::future::Either;
use futures::TryStream;
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};
use thiserror::Error;
use {
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin, fidl_fuchsia_net_root as fnet_root,
    fidl_fuchsia_net_routes_admin as fnet_routes_admin,
};

use crate::{impl_responder, FidlRouteIpExt, Responder, TableId};

/// Route set creation errors.
#[derive(Clone, Debug, Error)]
pub enum RouteSetCreationError {
    /// Proxy creation failed.
    #[error("failed to create proxy: {0}")]
    CreateProxy(fidl::Error),
    /// Route set creation failed.
    #[error("failed to create route set: {0}")]
    RouteSet(fidl::Error),
}

/// Route table creation errors.
#[derive(Clone, Debug, Error)]
pub enum RouteTableCreationError {
    /// Proxy creation failed.
    #[error("failed to create proxy: {0}")]
    CreateProxy(fidl::Error),
    /// Route table creation failed.
    #[error("failed to create route set: {0}")]
    RouteTable(fidl::Error),
}

/// Admin extension for the `fuchsia.net.routes.admin` FIDL API.
pub trait FidlRouteAdminIpExt: Ip {
    /// The "route table" protocol to use for this IP version.
    type RouteTableMarker: DiscoverableProtocolMarker<
        RequestStream = Self::RouteTableRequestStream,
        Proxy: Clone,
    >;
    /// The "root set" protocol to use for this IP version.
    type GlobalRouteTableMarker: DiscoverableProtocolMarker;
    /// The "route set" protocol to use for this IP version.
    type RouteSetMarker: ProtocolMarker<RequestStream = Self::RouteSetRequestStream>;
    /// The "route table provider" protocol to use for this IP version.
    type RouteTableProviderMarker: DiscoverableProtocolMarker<Proxy: Clone>;
    /// The request stream for the route set protocol.
    type RouteSetRequestStream: fidl::endpoints::RequestStream<Ok: Send, ControlHandle: Send>;
    /// The request stream for the route table protocol.
    type RouteTableRequestStream: fidl::endpoints::RequestStream<Ok: Send, ControlHandle: Send>;
    /// The responder for AddRoute requests.
    type AddRouteResponder: Responder<Payload = Result<bool, fnet_routes_admin::RouteSetError>>;
    /// The responder for RemoveRoute requests.
    type RemoveRouteResponder: Responder<Payload = Result<bool, fnet_routes_admin::RouteSetError>>;
    /// The responder for AuthenticateForInterface requests.
    type RouteSetAuthenticateForInterfaceResponder: Responder<
        Payload = Result<(), fnet_routes_admin::AuthenticateForInterfaceError>,
    >;
    /// The responder for GetTableId requests.
    type RouteTableGetTableIdResponder: Responder<Payload = u32>;
    /// The responder for RemoveRequests.
    type RouteTableRemoveResponder: Responder<
        Payload = Result<(), fnet_routes_admin::BaseRouteTableRemoveError>,
    >;
    /// The responder for GetAuthorizationForRouteTable requests.
    type RouteTableGetAuthorizationResponder: Responder<
        Payload = fnet_routes_admin::GrantForRouteTableAuthorization,
    >;
    /// The control handle for RouteTable protocols.
    type RouteTableControlHandle: fidl::endpoints::ControlHandle + Debug;

    /// Turns a FIDL route set request into the extension type.
    fn into_route_set_request(
        request: fidl::endpoints::Request<Self::RouteSetMarker>,
    ) -> RouteSetRequest<Self>;

    /// Turns a FIDL route table request into the extension type.
    fn into_route_table_request(
        request: fidl::endpoints::Request<Self::RouteTableMarker>,
    ) -> RouteTableRequest<Self>;

    /// Turns a FIDL route set request stream item into a Result of the extension type.
    fn into_route_set_request_result(
        request: <Self::RouteSetRequestStream as futures::Stream>::Item,
    ) -> Result<RouteSetRequest<Self>, fidl::Error>;

    /// Turns a FIDL route table request stream item into a Result of the extension type.
    fn into_route_table_request_result(
        request: <Self::RouteTableRequestStream as futures::Stream>::Item,
    ) -> Result<RouteTableRequest<Self>, fidl::Error>;
}

impl FidlRouteAdminIpExt for Ipv4 {
    type RouteTableMarker = fnet_routes_admin::RouteTableV4Marker;
    type GlobalRouteTableMarker = fnet_root::RoutesV4Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV4Marker;
    type RouteTableProviderMarker = fnet_routes_admin::RouteTableProviderV4Marker;
    type RouteSetRequestStream = fnet_routes_admin::RouteSetV4RequestStream;
    type RouteTableRequestStream = fnet_routes_admin::RouteTableV4RequestStream;
    type AddRouteResponder = fnet_routes_admin::RouteSetV4AddRouteResponder;
    type RemoveRouteResponder = fnet_routes_admin::RouteSetV4RemoveRouteResponder;
    type RouteSetAuthenticateForInterfaceResponder =
        fnet_routes_admin::RouteSetV4AuthenticateForInterfaceResponder;
    type RouteTableGetTableIdResponder = fnet_routes_admin::RouteTableV4GetTableIdResponder;
    type RouteTableRemoveResponder = fnet_routes_admin::RouteTableV4RemoveResponder;
    type RouteTableGetAuthorizationResponder =
        fnet_routes_admin::RouteTableV4GetAuthorizationForRouteTableResponder;
    type RouteTableControlHandle = fnet_routes_admin::RouteTableV4ControlHandle;

    fn into_route_set_request(
        request: fidl::endpoints::Request<Self::RouteSetMarker>,
    ) -> RouteSetRequest<Self> {
        RouteSetRequest::from(request)
    }

    fn into_route_table_request(
        request: fidl::endpoints::Request<Self::RouteTableMarker>,
    ) -> RouteTableRequest<Self> {
        RouteTableRequest::from(request)
    }

    fn into_route_set_request_result(
        request: <Self::RouteSetRequestStream as futures::Stream>::Item,
    ) -> Result<RouteSetRequest<Self>, fidl::Error> {
        request.map(RouteSetRequest::from)
    }

    fn into_route_table_request_result(
        request: <Self::RouteTableRequestStream as futures::Stream>::Item,
    ) -> Result<RouteTableRequest<Self>, fidl::Error> {
        request.map(RouteTableRequest::from)
    }
}

impl FidlRouteAdminIpExt for Ipv6 {
    type RouteTableMarker = fnet_routes_admin::RouteTableV6Marker;
    type GlobalRouteTableMarker = fnet_root::RoutesV6Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV6Marker;
    type RouteTableProviderMarker = fnet_routes_admin::RouteTableProviderV6Marker;
    type RouteSetRequestStream = fnet_routes_admin::RouteSetV6RequestStream;
    type RouteTableRequestStream = fnet_routes_admin::RouteTableV6RequestStream;
    type AddRouteResponder = fnet_routes_admin::RouteSetV6AddRouteResponder;
    type RemoveRouteResponder = fnet_routes_admin::RouteSetV6RemoveRouteResponder;
    type RouteSetAuthenticateForInterfaceResponder =
        fnet_routes_admin::RouteSetV6AuthenticateForInterfaceResponder;
    type RouteTableGetTableIdResponder = fnet_routes_admin::RouteTableV6GetTableIdResponder;
    type RouteTableRemoveResponder = fnet_routes_admin::RouteTableV6RemoveResponder;
    type RouteTableGetAuthorizationResponder =
        fnet_routes_admin::RouteTableV6GetAuthorizationForRouteTableResponder;
    type RouteTableControlHandle = fnet_routes_admin::RouteTableV6ControlHandle;

    fn into_route_set_request(
        request: fidl::endpoints::Request<Self::RouteSetMarker>,
    ) -> RouteSetRequest<Self> {
        RouteSetRequest::from(request)
    }

    fn into_route_table_request(
        request: fidl::endpoints::Request<Self::RouteTableMarker>,
    ) -> RouteTableRequest<Self> {
        RouteTableRequest::from(request)
    }

    fn into_route_set_request_result(
        request: <Self::RouteSetRequestStream as futures::Stream>::Item,
    ) -> Result<RouteSetRequest<Self>, fidl::Error> {
        request.map(RouteSetRequest::from)
    }

    fn into_route_table_request_result(
        request: <Self::RouteTableRequestStream as futures::Stream>::Item,
    ) -> Result<RouteTableRequest<Self>, fidl::Error> {
        request.map(RouteTableRequest::from)
    }
}

impl_responder!(
    fnet_routes_admin::RouteSetV4AddRouteResponder,
    Result<bool, fnet_routes_admin::RouteSetError>,
);
impl_responder!(
    fnet_routes_admin::RouteSetV4RemoveRouteResponder,
    Result<bool, fnet_routes_admin::RouteSetError>,
);
impl_responder!(
    fnet_routes_admin::RouteSetV6AddRouteResponder,
    Result<bool, fnet_routes_admin::RouteSetError>,
);
impl_responder!(
    fnet_routes_admin::RouteSetV6RemoveRouteResponder,
    Result<bool, fnet_routes_admin::RouteSetError>,
);
impl_responder!(
    fnet_routes_admin::RouteSetV4AuthenticateForInterfaceResponder,
    Result<(), fnet_routes_admin::AuthenticateForInterfaceError>,
);
impl_responder!(
    fnet_routes_admin::RouteSetV6AuthenticateForInterfaceResponder,
    Result<(), fnet_routes_admin::AuthenticateForInterfaceError>,
);
impl_responder!(fnet_routes_admin::RouteTableV4GetTableIdResponder, u32,);
impl_responder!(fnet_routes_admin::RouteTableV6GetTableIdResponder, u32,);
impl_responder!(
    fnet_routes_admin::RouteTableV4RemoveResponder,
    Result<(), fnet_routes_admin::BaseRouteTableRemoveError>,
);
impl_responder!(
    fnet_routes_admin::RouteTableV6RemoveResponder,
    Result<(), fnet_routes_admin::BaseRouteTableRemoveError>,
);
impl_responder!(
    fnet_routes_admin::RouteTableV4GetAuthorizationForRouteTableResponder,
    fnet_routes_admin::GrantForRouteTableAuthorization,
);
impl_responder!(
    fnet_routes_admin::RouteTableV6GetAuthorizationForRouteTableResponder,
    fnet_routes_admin::GrantForRouteTableAuthorization,
);

/// The compiler often fails to infer that an item in the RouteTableProvider request stream is a
/// Result. This function helps force it to understand this so that the Result can be unwrapped
/// to get the actual RouteTableProvider request inside.
pub fn concretize_route_table_provider_request<I: Ip + FidlRouteAdminIpExt>(
    item: <<<I as FidlRouteAdminIpExt>::RouteTableProviderMarker as ProtocolMarker>::RequestStream as futures::Stream>::Item,
) -> Result<(ServerEnd<I::RouteTableMarker>, Option<String>), fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct In<I: FidlRouteAdminIpExt>(<<<I as FidlRouteAdminIpExt>::RouteTableProviderMarker as ProtocolMarker>::RequestStream as futures::Stream>::Item);

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Out<I: FidlRouteAdminIpExt>(
        Result<(ServerEnd<I::RouteTableMarker>, Option<String>), fidl::Error>,
    );

    let Out(result) = net_types::map_ip_twice!(I, In(item), |In(item)| Out(
        item.map(unpack_route_table_provider_request::<I>)
    ));
    result
}

/// Unpacks the `[ServerEnd]` and debug name from a request for a new route table.
pub fn unpack_route_table_provider_request<I: Ip + FidlRouteAdminIpExt>(
    request: <<I::RouteTableProviderMarker as ProtocolMarker>::RequestStream as TryStream>::Ok,
) -> (ServerEnd<I::RouteTableMarker>, Option<String>) {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Request<I: FidlRouteAdminIpExt>(
        <<I::RouteTableProviderMarker as ProtocolMarker>::RequestStream as TryStream>::Ok,
    );

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Contents<I: FidlRouteAdminIpExt>((ServerEnd<I::RouteTableMarker>, Option<String>));

    let Contents(contents) = I::map_ip(
        Request(request),
        |Request(request)| {
            let fnet_routes_admin::RouteTableProviderV4Request::NewRouteTable {
                provider,
                options: fnet_routes_admin::RouteTableOptionsV4 { name, __source_breaking },
                control_handle: _,
            } = request;
            Contents((provider, name))
        },
        |Request(request)| {
            let fnet_routes_admin::RouteTableProviderV6Request::NewRouteTable {
                provider,
                options: fnet_routes_admin::RouteTableOptionsV6 { name, __source_breaking },
                control_handle: _,
            } = request;
            Contents((provider, name))
        },
    );
    contents
}

/// Dispatches `new_route_table` on either the `RouteTableProviderV4`
/// or `RouteTableProviderV6` proxy.
pub fn new_route_table<I: Ip + FidlRouteAdminIpExt>(
    route_table_provider_proxy: &<I::RouteTableProviderMarker as ProtocolMarker>::Proxy,
    name: Option<String>,
) -> Result<<I::RouteTableMarker as ProtocolMarker>::Proxy, RouteTableCreationError> {
    let (route_table_proxy, route_table_server_end) =
        fidl::endpoints::create_proxy::<I::RouteTableMarker>();

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRouteTableInput<'a, I: FidlRouteAdminIpExt> {
        route_table_server_end: fidl::endpoints::ServerEnd<I::RouteTableMarker>,
        route_table_provider_proxy: &'a <I::RouteTableProviderMarker as ProtocolMarker>::Proxy,
        name: Option<String>,
    }

    let result = I::map_ip_in(
        NewRouteTableInput::<'_, I> { route_table_server_end, route_table_provider_proxy, name },
        |NewRouteTableInput { route_table_server_end, route_table_provider_proxy, name }| {
            route_table_provider_proxy.new_route_table(
                route_table_server_end,
                &fnet_routes_admin::RouteTableOptionsV4 {
                    name,
                    ..fnet_routes_admin::RouteTableOptionsV4::default()
                },
            )
        },
        |NewRouteTableInput { route_table_server_end, route_table_provider_proxy, name }| {
            route_table_provider_proxy.new_route_table(
                route_table_server_end,
                &fnet_routes_admin::RouteTableOptionsV6 {
                    name,
                    ..fnet_routes_admin::RouteTableOptionsV6::default()
                },
            )
        },
    );

    result.map_err(RouteTableCreationError::RouteTable)?;
    Ok(route_table_proxy)
}

/// Dispatches `new_route_set` on either the `RouteTableV4`
/// or `RouteTableV6` proxy.
pub fn new_route_set<I: Ip + FidlRouteAdminIpExt>(
    route_table_proxy: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<<I::RouteSetMarker as ProtocolMarker>::Proxy, RouteSetCreationError> {
    let (route_set_proxy, route_set_server_end) =
        fidl::endpoints::create_proxy::<I::RouteSetMarker>();

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRouteSetInput<'a, I: FidlRouteAdminIpExt> {
        route_set_server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        route_table_proxy: &'a <I::RouteTableMarker as ProtocolMarker>::Proxy,
    }
    let result = I::map_ip_in(
        NewRouteSetInput::<'_, I> { route_set_server_end, route_table_proxy },
        |NewRouteSetInput { route_set_server_end, route_table_proxy }| {
            route_table_proxy.new_route_set(route_set_server_end)
        },
        |NewRouteSetInput { route_set_server_end, route_table_proxy }| {
            route_table_proxy.new_route_set(route_set_server_end)
        },
    );

    result.map_err(RouteSetCreationError::RouteSet)?;
    Ok(route_set_proxy)
}

/// Dispatches `global_route_set` on either the `RoutesV4` or `RoutesV6` in
/// fuchsia.net.root.
pub fn new_global_route_set<I: Ip + FidlRouteAdminIpExt>(
    route_table_proxy: &<I::GlobalRouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<<I::RouteSetMarker as ProtocolMarker>::Proxy, RouteSetCreationError> {
    let (route_set_proxy, route_set_server_end) =
        fidl::endpoints::create_proxy::<I::RouteSetMarker>();

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRouteSetInput<'a, I: FidlRouteAdminIpExt> {
        route_set_server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        route_table_proxy: &'a <I::GlobalRouteTableMarker as ProtocolMarker>::Proxy,
    }
    let result = I::map_ip_in(
        NewRouteSetInput::<'_, I> { route_set_server_end, route_table_proxy },
        |NewRouteSetInput { route_set_server_end, route_table_proxy }| {
            route_table_proxy.global_route_set(route_set_server_end)
        },
        |NewRouteSetInput { route_set_server_end, route_table_proxy }| {
            route_table_proxy.global_route_set(route_set_server_end)
        },
    );

    result.map_err(RouteSetCreationError::RouteSet)?;
    Ok(route_set_proxy)
}

/// Dispatches `add_route` on either the `RouteSetV4` or `RouteSetV6` proxy.
pub async fn add_route<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_set: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
    route: &I::Route,
) -> Result<Result<bool, fnet_routes_admin::RouteSetError>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct AddRouteInput<'a, I: FidlRouteAdminIpExt + FidlRouteIpExt> {
        route_set: &'a <I::RouteSetMarker as ProtocolMarker>::Proxy,
        route: &'a I::Route,
    }

    I::map_ip_in(
        AddRouteInput { route_set, route },
        |AddRouteInput { route_set, route }| Either::Left(route_set.add_route(route)),
        |AddRouteInput { route_set, route }| Either::Right(route_set.add_route(route)),
    )
    .await
}

/// Dispatches `remove_route` on either the `RouteSetV4` or `RouteSetV6` proxy.
pub async fn remove_route<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_set: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
    route: &I::Route,
) -> Result<Result<bool, fnet_routes_admin::RouteSetError>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct RemoveRouteInput<'a, I: FidlRouteAdminIpExt + FidlRouteIpExt> {
        route_set: &'a <I::RouteSetMarker as ProtocolMarker>::Proxy,
        route: &'a I::Route,
    }

    I::map_ip_in(
        RemoveRouteInput { route_set, route },
        |RemoveRouteInput { route_set, route }| Either::Left(route_set.remove_route(route)),
        |RemoveRouteInput { route_set, route }| Either::Right(route_set.remove_route(route)),
    )
    .await
}

/// Dispatches `authenticate_for_interface` on either the `RouteSetV4` or
/// `RouteSetV6` proxy.
pub async fn authenticate_for_interface<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_set: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
    credential: fnet_interfaces_admin::ProofOfInterfaceAuthorization,
) -> Result<Result<(), fnet_routes_admin::AuthenticateForInterfaceError>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct AuthenticateForInterfaceInput<'a, I: FidlRouteAdminIpExt + FidlRouteIpExt> {
        route_set: &'a <I::RouteSetMarker as ProtocolMarker>::Proxy,
        credential: fnet_interfaces_admin::ProofOfInterfaceAuthorization,
    }

    I::map_ip_in(
        AuthenticateForInterfaceInput { route_set, credential },
        |AuthenticateForInterfaceInput { route_set, credential }| {
            Either::Left(route_set.authenticate_for_interface(credential))
        },
        |AuthenticateForInterfaceInput { route_set, credential }| {
            Either::Right(route_set.authenticate_for_interface(credential))
        },
    )
    .await
}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
struct RouteTableProxy<'a, I: FidlRouteAdminIpExt + FidlRouteIpExt> {
    route_table: &'a <I::RouteTableMarker as ProtocolMarker>::Proxy,
}

/// Dispatches `detach` on either the `RouteTableV4` or `RouteTableV6` proxy.
pub async fn detach_route_table<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_table: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<(), fidl::Error> {
    let IpInvariant(result) = net_types::map_ip_twice!(
        I,
        RouteTableProxy { route_table },
        |RouteTableProxy { route_table }| { IpInvariant(route_table.detach()) }
    );

    result
}

/// Dispatches `remove` on either the `RouteTableV4` or `RouteTableV6` proxy.
pub async fn remove_route_table<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_table: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<Result<(), fnet_routes_admin::BaseRouteTableRemoveError>, fidl::Error> {
    let IpInvariant(result_fut) = net_types::map_ip_twice!(
        I,
        RouteTableProxy { route_table },
        |RouteTableProxy { route_table }| { IpInvariant(route_table.remove()) }
    );

    result_fut.await
}

/// Dispatches `get_table_id` on either the `RouteTableV4` or `RouteTableV6`
/// proxy.
pub async fn get_table_id<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_table: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<TableId, fidl::Error> {
    let IpInvariant(result_fut) = net_types::map_ip_twice!(
        I,
        RouteTableProxy { route_table },
        |RouteTableProxy { route_table }| IpInvariant(route_table.get_table_id()),
    );

    result_fut.await.map(TableId::new)
}

/// Dispatches `get_authorization_for_route_table` on either the `RouteTableV4`
/// or `RouteTableV6` proxy.
pub async fn get_authorization_for_route_table<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_table: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<fnet_routes_admin::GrantForRouteTableAuthorization, fidl::Error> {
    let IpInvariant(result_fut) = net_types::map_ip_twice!(
        I,
        RouteTableProxy { route_table },
        |RouteTableProxy { route_table }| IpInvariant(
            route_table.get_authorization_for_route_table()
        ),
    );

    result_fut.await
}

/// GenericOverIp version of RouteSetV{4, 6}Request.
#[derive(GenericOverIp, Debug)]
#[generic_over_ip(I, Ip)]
pub enum RouteSetRequest<I: FidlRouteAdminIpExt> {
    /// Adds a route to the route set.
    AddRoute {
        /// The route to add.
        route: Result<
            crate::Route<I>,
            crate::FidlConversionError<crate::RoutePropertiesRequiredFields>,
        >,
        /// The responder for this request.
        responder: I::AddRouteResponder,
    },
    /// Removes a route from the route set.
    RemoveRoute {
        /// The route to add.
        route: Result<
            crate::Route<I>,
            crate::FidlConversionError<crate::RoutePropertiesRequiredFields>,
        >,
        /// The responder for this request.
        responder: I::RemoveRouteResponder,
    },
    /// Authenticates the route set for managing routes on an interface.
    AuthenticateForInterface {
        /// The credential proving authorization for this interface.
        credential: fnet_interfaces_admin::ProofOfInterfaceAuthorization,
        /// The responder for this request.
        responder: I::RouteSetAuthenticateForInterfaceResponder,
    },
}

impl From<fnet_routes_admin::RouteSetV4Request> for RouteSetRequest<Ipv4> {
    fn from(value: fnet_routes_admin::RouteSetV4Request) -> Self {
        match value {
            fnet_routes_admin::RouteSetV4Request::AddRoute { route, responder } => {
                RouteSetRequest::AddRoute { route: route.try_into(), responder }
            }
            fnet_routes_admin::RouteSetV4Request::RemoveRoute { route, responder } => {
                RouteSetRequest::RemoveRoute { route: route.try_into(), responder }
            }
            fnet_routes_admin::RouteSetV4Request::AuthenticateForInterface {
                credential,
                responder,
            } => RouteSetRequest::AuthenticateForInterface { credential, responder },
        }
    }
}

impl From<fnet_routes_admin::RouteSetV6Request> for RouteSetRequest<Ipv6> {
    fn from(value: fnet_routes_admin::RouteSetV6Request) -> Self {
        match value {
            fnet_routes_admin::RouteSetV6Request::AddRoute { route, responder } => {
                RouteSetRequest::AddRoute { route: route.try_into(), responder }
            }
            fnet_routes_admin::RouteSetV6Request::RemoveRoute { route, responder } => {
                RouteSetRequest::RemoveRoute { route: route.try_into(), responder }
            }
            fnet_routes_admin::RouteSetV6Request::AuthenticateForInterface {
                credential,
                responder,
            } => RouteSetRequest::AuthenticateForInterface { credential, responder },
        }
    }
}

/// GenericOverIp version of RouteTableV{4, 6}Request.
#[derive(GenericOverIp, derivative::Derivative)]
#[derivative(Debug(bound = ""))]
#[generic_over_ip(I, Ip)]
pub enum RouteTableRequest<I: FidlRouteAdminIpExt> {
    /// Gets the table ID for the table
    GetTableId {
        /// Responder for the request.
        responder: I::RouteTableGetTableIdResponder,
    },
    /// Detaches the table lifetime from the channel.
    Detach {
        /// Control handle to the protocol.
        control_handle: I::RouteTableControlHandle,
    },
    /// Removes the table.
    Remove {
        /// Responder to the request.
        responder: I::RouteTableRemoveResponder,
    },
    /// Gets the authorization for the route table.
    GetAuthorizationForRouteTable {
        /// Responder to the request.
        responder: I::RouteTableGetAuthorizationResponder,
    },
    /// Creates a new route set for the table.
    NewRouteSet {
        /// The server end of the route set protocol.
        route_set: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        /// Control handle to the protocol.
        control_handle: I::RouteTableControlHandle,
    },
}

impl From<fnet_routes_admin::RouteTableV4Request> for RouteTableRequest<Ipv4> {
    fn from(value: fnet_routes_admin::RouteTableV4Request) -> Self {
        match value {
            fnet_routes_admin::RouteTableV4Request::NewRouteSet { route_set, control_handle } => {
                RouteTableRequest::NewRouteSet { route_set, control_handle }
            }
            fnet_routes_admin::RouteTableV4Request::GetTableId { responder } => {
                RouteTableRequest::GetTableId { responder }
            }

            fnet_routes_admin::RouteTableV4Request::Detach { control_handle } => {
                RouteTableRequest::Detach { control_handle }
            }

            fnet_routes_admin::RouteTableV4Request::Remove { responder } => {
                RouteTableRequest::Remove { responder }
            }
            fnet_routes_admin::RouteTableV4Request::GetAuthorizationForRouteTable { responder } => {
                RouteTableRequest::GetAuthorizationForRouteTable { responder }
            }
        }
    }
}

impl From<fnet_routes_admin::RouteTableV6Request> for RouteTableRequest<Ipv6> {
    fn from(value: fnet_routes_admin::RouteTableV6Request) -> Self {
        match value {
            fnet_routes_admin::RouteTableV6Request::NewRouteSet { route_set, control_handle } => {
                RouteTableRequest::NewRouteSet { route_set, control_handle }
            }
            fnet_routes_admin::RouteTableV6Request::GetTableId { responder } => {
                RouteTableRequest::GetTableId { responder }
            }

            fnet_routes_admin::RouteTableV6Request::Detach { control_handle } => {
                RouteTableRequest::Detach { control_handle }
            }

            fnet_routes_admin::RouteTableV6Request::Remove { responder } => {
                RouteTableRequest::Remove { responder }
            }
            fnet_routes_admin::RouteTableV6Request::GetAuthorizationForRouteTable { responder } => {
                RouteTableRequest::GetAuthorizationForRouteTable { responder }
            }
        }
    }
}
