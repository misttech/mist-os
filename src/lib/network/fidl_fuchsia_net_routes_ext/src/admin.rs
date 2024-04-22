// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for fuchsia.net.routes.admin.

use std::fmt::Debug;

use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_root as fnet_root;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use futures::future::Either;
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};
use thiserror::Error;

use crate::FidlRouteIpExt;

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

/// Admin extension for the `fuchsia.net.routes.admin` FIDL API.
pub trait FidlRouteAdminIpExt: Ip {
    /// The "set provider" protocol to use for this IP version.
    type RouteTableMarker: DiscoverableProtocolMarker;
    /// The "root set" protocol to use for this IP version.
    type GlobalRouteTableMarker: DiscoverableProtocolMarker;
    /// The "route set" protocol to use for this IP version.
    type RouteSetMarker: ProtocolMarker;
    /// The request stream for the route set protocol.
    type RouteSetRequestStream: fidl::endpoints::RequestStream;
    /// The responder for AddRoute requests.
    type AddRouteResponder: Responder<Payload = Result<bool, fnet_routes_admin::RouteSetError>>;
    /// The responder for RemoveRoute requests.
    type RemoveRouteResponder: Responder<Payload = Result<bool, fnet_routes_admin::RouteSetError>>;
    /// The responder for AuthenticateForInterface requests.
    type AuthenticateForInterfaceResponder: Responder<
        Payload = Result<(), fnet_routes_admin::AuthenticateForInterfaceError>,
    >;
}

impl FidlRouteAdminIpExt for Ipv4 {
    type RouteTableMarker = fnet_routes_admin::RouteTableV4Marker;
    type GlobalRouteTableMarker = fnet_root::RoutesV4Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV4Marker;
    type RouteSetRequestStream = fnet_routes_admin::RouteSetV4RequestStream;
    type AddRouteResponder = fnet_routes_admin::RouteSetV4AddRouteResponder;
    type RemoveRouteResponder = fnet_routes_admin::RouteSetV4RemoveRouteResponder;
    type AuthenticateForInterfaceResponder =
        fnet_routes_admin::RouteSetV4AuthenticateForInterfaceResponder;
}

impl FidlRouteAdminIpExt for Ipv6 {
    type RouteTableMarker = fnet_routes_admin::RouteTableV6Marker;
    type GlobalRouteTableMarker = fnet_root::RoutesV6Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV6Marker;
    type RouteSetRequestStream = fnet_routes_admin::RouteSetV6RequestStream;
    type AddRouteResponder = fnet_routes_admin::RouteSetV6AddRouteResponder;
    type RemoveRouteResponder = fnet_routes_admin::RouteSetV6RemoveRouteResponder;
    type AuthenticateForInterfaceResponder =
        fnet_routes_admin::RouteSetV6AuthenticateForInterfaceResponder;
}

/// Abstracts over AddRoute and RemoveRoute RouteSet method responders.
pub trait Responder: fidl::endpoints::Responder + Debug + Send {
    /// The payload of the response.
    type Payload;

    /// Sends a FIDL response.
    fn send(self, result: Self::Payload) -> Result<(), fidl::Error>;
}

macro_rules! impl_responder {
    ($resp:ty, $payload:ty $(,)?) => {
        impl Responder for $resp {
            type Payload = $payload;

            fn send(self, result: Self::Payload) -> Result<(), fidl::Error> {
                <$resp>::send(self, result)
            }
        }
    };
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

/// Dispatches `new_route_set` on either the `RouteTableV4`
/// or `RouteTableV6` proxy.
pub fn new_route_set<I: Ip + FidlRouteAdminIpExt>(
    set_provider_proxy: &<I::RouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<<I::RouteSetMarker as ProtocolMarker>::Proxy, RouteSetCreationError> {
    let (route_set_proxy, route_set_server_end) =
        fidl::endpoints::create_proxy::<I::RouteSetMarker>()
            .map_err(RouteSetCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRouteSetInput<'a, I: FidlRouteAdminIpExt> {
        route_set_server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        set_provider_proxy: &'a <I::RouteTableMarker as ProtocolMarker>::Proxy,
    }
    let IpInvariant(result) = I::map_ip::<NewRouteSetInput<'_, I>, _>(
        NewRouteSetInput::<'_, I> { route_set_server_end, set_provider_proxy },
        |NewRouteSetInput { route_set_server_end, set_provider_proxy }| {
            IpInvariant(set_provider_proxy.new_route_set(route_set_server_end))
        },
        |NewRouteSetInput { route_set_server_end, set_provider_proxy }| {
            IpInvariant(set_provider_proxy.new_route_set(route_set_server_end))
        },
    );

    result.map_err(RouteSetCreationError::RouteSet)?;
    Ok(route_set_proxy)
}

/// Dispatches `global_route_set` on either the `RoutesV4` or `RoutesV6` in
/// fuchsia.net.root.
pub fn new_global_route_set<I: Ip + FidlRouteAdminIpExt>(
    set_provider_proxy: &<I::GlobalRouteTableMarker as ProtocolMarker>::Proxy,
) -> Result<<I::RouteSetMarker as ProtocolMarker>::Proxy, RouteSetCreationError> {
    let (route_set_proxy, route_set_server_end) =
        fidl::endpoints::create_proxy::<I::RouteSetMarker>()
            .map_err(RouteSetCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRouteSetInput<'a, I: FidlRouteAdminIpExt> {
        route_set_server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        set_provider_proxy: &'a <I::GlobalRouteTableMarker as ProtocolMarker>::Proxy,
    }
    let IpInvariant(result) = I::map_ip::<NewRouteSetInput<'_, I>, _>(
        NewRouteSetInput::<'_, I> { route_set_server_end, set_provider_proxy },
        |NewRouteSetInput { route_set_server_end, set_provider_proxy }| {
            IpInvariant(set_provider_proxy.global_route_set(route_set_server_end))
        },
        |NewRouteSetInput { route_set_server_end, set_provider_proxy }| {
            IpInvariant(set_provider_proxy.global_route_set(route_set_server_end))
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

    let IpInvariant(result_fut) = I::map_ip(
        AddRouteInput { route_set, route },
        |AddRouteInput { route_set, route }| IpInvariant(Either::Left(route_set.add_route(route))),
        |AddRouteInput { route_set, route }| IpInvariant(Either::Right(route_set.add_route(route))),
    );
    result_fut.await
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

    let IpInvariant(result_fut) = I::map_ip(
        RemoveRouteInput { route_set, route },
        |RemoveRouteInput { route_set, route }| {
            IpInvariant(Either::Left(route_set.remove_route(route)))
        },
        |RemoveRouteInput { route_set, route }| {
            IpInvariant(Either::Right(route_set.remove_route(route)))
        },
    );
    result_fut.await
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

    let IpInvariant(result_fut) = I::map_ip(
        AuthenticateForInterfaceInput { route_set, credential },
        |AuthenticateForInterfaceInput { route_set, credential }| {
            IpInvariant(Either::Left(route_set.authenticate_for_interface(credential)))
        },
        |AuthenticateForInterfaceInput { route_set, credential }| {
            IpInvariant(Either::Right(route_set.authenticate_for_interface(credential)))
        },
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
        responder: I::AuthenticateForInterfaceResponder,
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
