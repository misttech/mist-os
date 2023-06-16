// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests for fuchsia.net.routes.admin.
// TODO(https://fxbug.dev/129220): These tests are work-in-progress and need to
// be made more comprehensive before we can call this sufficiently tested.

#![cfg(test)]

use std::collections::HashSet;

use fidl_fuchsia_net_routes as froutes;
use fidl_fuchsia_net_routes_admin as froutes_admin;
use fidl_fuchsia_net_routes_ext as froutes_ext;
use futures::pin_mut;
use net_types::{ip::Ipv4, SpecifiedAddr};
use netstack_testing_common::realms::{Netstack, TestSandboxExt};
use netstack_testing_macros::netstack_test;

#[netstack_test]
async fn add_route<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let network =
        sandbox.create_network(format!("routes-admin-{name}")).await.expect("create network");
    let iface = realm.join_network(&network, "ep1").await.expect("join network");

    let set_provider = realm
        .connect_to_protocol::<froutes_admin::SetProviderV4Marker>()
        .expect("connect to SetProviderV4");

    let state = realm.connect_to_protocol::<froutes::StateV4Marker>().expect("connect to StateV4");
    let routes_stream =
        froutes_ext::event_stream_from_state::<Ipv4>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes = froutes_ext::collect_routes_until_idle::<Ipv4, HashSet<_>>(&mut routes_stream)
        .await
        .expect("collect routes should succeed");

    println!("initial routes = {routes:?}");

    let proxy = {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<froutes_admin::RouteSetV4Marker>()
                .expect("create proxy");
        set_provider.new_route_set(server_end).expect("new route set");
        proxy
    };

    let route_to_add = froutes_ext::Route {
        destination: net_declare::net_subnet_v4!("192.168.0.0/24"),
        action: froutes_ext::RouteAction::Forward(froutes_ext::RouteTarget {
            outbound_interface: iface.id(),
            next_hop: Some(
                SpecifiedAddr::new(net_declare::net_ip_v4!("192.168.0.1")).expect("is specified"),
            ),
        }),
        properties: froutes_ext::RouteProperties {
            specified_properties: froutes_ext::SpecifiedRouteProperties {
                metric: froutes::SpecifiedMetric::InheritedFromInterface(froutes::Empty),
            },
        },
    };

    assert!(proxy
        .add_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("add route"));

    froutes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("final routes = {routes:?}");
}
