// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for fuchsia.net.routes.admin.

use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker};
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
    type SetProviderMarker: DiscoverableProtocolMarker;
    /// The "route set" protocol to use for this IP version.
    type RouteSetMarker: ProtocolMarker;
}

impl FidlRouteAdminIpExt for Ipv4 {
    type SetProviderMarker = fnet_routes_admin::SetProviderV4Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV4Marker;
}

impl FidlRouteAdminIpExt for Ipv6 {
    type SetProviderMarker = fnet_routes_admin::SetProviderV6Marker;
    type RouteSetMarker = fnet_routes_admin::RouteSetV6Marker;
}

/// Dispatches `new_route_set` on either the `SetProviderV4`
/// or `SetProviderV6` proxy.
pub fn new_route_set<I: Ip + FidlRouteAdminIpExt>(
    set_provider_proxy: &<I::SetProviderMarker as ProtocolMarker>::Proxy,
) -> Result<<I::RouteSetMarker as ProtocolMarker>::Proxy, RouteSetCreationError> {
    let (route_set_proxy, route_set_server_end) =
        fidl::endpoints::create_proxy::<I::RouteSetMarker>()
            .map_err(RouteSetCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    struct NewRouteSetInput<'a, I: Ip + FidlRouteAdminIpExt> {
        route_set_server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
        set_provider_proxy: &'a <I::SetProviderMarker as ProtocolMarker>::Proxy,
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

/// Dispatches `add_route` on either the `RouteSetV4` or `RouteSetV6` proxy.
pub async fn add_route<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    route_set: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
    route: &I::Route,
) -> Result<Result<bool, fnet_routes_admin::RouteSetError>, fidl::Error> {
    #[derive(GenericOverIp)]
    struct AddRouteInput<'a, I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt> {
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
    struct RemoveRouteInput<'a, I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt> {
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
