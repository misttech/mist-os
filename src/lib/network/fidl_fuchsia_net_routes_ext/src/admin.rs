// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for fuchsia.net.routes.admin.

use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker};
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};
use thiserror::Error;

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

/// Dispatches `new_route_set` on either the `RouteSetV4`
/// or `RouteSetV6` proxy.
pub fn new_route_set<I: Ip + FidlRouteAdminIpExt>(
    set_provider_proxy: <I::SetProviderMarker as ProtocolMarker>::Proxy,
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
        NewRouteSetInput::<'_, I> { route_set_server_end, set_provider_proxy: &set_provider_proxy },
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
