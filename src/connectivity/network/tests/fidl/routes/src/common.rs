// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext::{
    self as fnet_routes_ext, admin::FidlRouteAdminIpExt, FidlRouteIpExt,
};
use net_types::{ip::Ip, SpecifiedAddr};
use netstack_testing_common::realms::{Netstack, TestSandboxExt as _};

/// Common test setup that can be shared by all routes tests.
pub struct TestSetup<'a, I: Ip + FidlRouteIpExt + FidlRouteAdminIpExt> {
    pub realm: netemul::TestRealm<'a>,
    pub network: netemul::TestNetwork<'a>,
    pub interface: netemul::TestInterface<'a>,
    pub route_table: <I::RouteTableMarker as ProtocolMarker>::Proxy,
    pub global_route_table: <I::GlobalRouteTableMarker as ProtocolMarker>::Proxy,
    pub state: <I::StateMarker as ProtocolMarker>::Proxy,
}

impl<'a, I: Ip + FidlRouteIpExt + FidlRouteAdminIpExt> TestSetup<'a, I> {
    /// Creates a new test setup.
    pub async fn new<N: Netstack>(
        sandbox: &'a netemul::TestSandbox,
        name: &str,
    ) -> TestSetup<'a, I> {
        let realm = sandbox
            .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
            .expect("create realm");
        let network =
            sandbox.create_network(format!("routes-admin-{name}")).await.expect("create network");
        let interface = realm.join_network(&network, "ep1").await.expect("join network");
        let route_table = realm
            .connect_to_protocol::<I::RouteTableMarker>()
            .expect("connect to routes-admin RouteTable");
        let global_route_table = realm
            .connect_to_protocol::<I::GlobalRouteTableMarker>()
            .expect("connect to global route set provider");

        let state = realm.connect_to_protocol::<I::StateMarker>().expect("connect to routes State");
        TestSetup { realm, network, interface, route_table, global_route_table, state }
    }
}

/// A route for testing.
pub fn test_route<I: Ip>(
    interface: &netemul::TestInterface<'_>,
    metric: fnet_routes::SpecifiedMetric,
) -> fnet_routes_ext::Route<I> {
    let destination = I::map_ip(
        (),
        |()| net_declare::net_subnet_v4!("192.0.2.0/24"),
        |()| net_declare::net_subnet_v6!("2001:DB8::/64"),
    );
    let next_hop_addr = I::map_ip(
        (),
        |()| net_declare::net_ip_v4!("192.0.2.1"),
        |()| net_declare::net_ip_v6!("2001:DB8::1"),
    );

    fnet_routes_ext::Route {
        destination,
        action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
            outbound_interface: interface.id(),
            next_hop: Some(SpecifiedAddr::new(next_hop_addr).expect("is specified")),
        }),
        properties: fnet_routes_ext::RouteProperties {
            specified_properties: fnet_routes_ext::SpecifiedRouteProperties { metric },
        },
    }
}
