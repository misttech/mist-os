// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests for fuchsia.net.routes.admin.
// TODO(https://fxbug.dev/129220): These tests are work-in-progress and need to
// be made more comprehensive before we can call this sufficiently tested.

#![cfg(test)]

use std::collections::HashSet;

use assert_matches::assert_matches;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fuchsia_async::TimeoutExt as _;
use futures::{future::FutureExt, pin_mut};
use net_types::{ip::Ipv4, SpecifiedAddr};
use netstack_testing_common::{
    realms::{Netstack, TestSandboxExt},
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

struct TestSetup<'a> {
    realm: netemul::TestRealm<'a>,
    network: netemul::TestNetwork<'a>,
    interface: netemul::TestInterface<'a>,
    set_provider: fnet_routes_admin::SetProviderV4Proxy,
    state: fnet_routes::StateV4Proxy,
}

impl<'a> TestSetup<'a> {
    async fn new<N: Netstack>(sandbox: &'a netemul::TestSandbox, name: &str) -> TestSetup<'a> {
        let realm = sandbox
            .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
            .expect("create realm");
        let network =
            sandbox.create_network(format!("routes-admin-{name}")).await.expect("create network");
        let interface = realm.join_network(&network, "ep1").await.expect("join network");
        let set_provider = realm
            .connect_to_protocol::<fnet_routes_admin::SetProviderV4Marker>()
            .expect("connect to SetProviderV4");
        let state =
            realm.connect_to_protocol::<fnet_routes::StateV4Marker>().expect("connect to StateV4");
        TestSetup { realm, network, interface, set_provider, state }
    }
}

fn test_route(interface: &netemul::TestInterface<'_>) -> fnet_routes_ext::Route<Ipv4> {
    fnet_routes_ext::Route {
        destination: net_declare::net_subnet_v4!("192.0.2.0/24"),
        action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
            outbound_interface: interface.id(),
            next_hop: Some(
                SpecifiedAddr::new(net_declare::net_ip_v4!("192.0.2.1")).expect("is specified"),
            ),
        }),
        properties: fnet_routes_ext::RouteProperties {
            specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty),
            },
        },
    }
}

#[netstack_test]
#[test_case(true ; "explicitly removing the route")]
#[test_case(false ; "dropping the route set")]
async fn add_remove_route<N: Netstack>(name: &str, explicit_remove: bool) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<Ipv4>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<Ipv4, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    println!("initial routes = {routes:?}");

    let proxy = {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fnet_routes_admin::RouteSetV4Marker>()
                .expect("create proxy");
        set_provider.new_route_set(server_end).expect("new route set");
        proxy
    };

    let route_to_add = test_route(&interface);

    assert!(proxy
        .add_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("add route"));

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("routes after adding = {routes:?}");

    // Move the RouteSetV4Proxy into an Option so that we can avoid dropping it in one of the cases.
    let _maybe_proxy = if explicit_remove {
        assert!(proxy
            .remove_route(&route_to_add.try_into().expect("convert to FIDL"))
            .await
            .expect("no FIDL error")
            .expect("remove route"));
        println!("explicitly removed route");
        Some(proxy)
    } else {
        drop(proxy);
        println!("removed route by dropping RouteSet");
        None
    };

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("routes after removing = {routes:?}");
}

#[netstack_test]
async fn add_route_twice_with_same_set<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<Ipv4>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<Ipv4, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let proxy = {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fnet_routes_admin::RouteSetV4Marker>()
                .expect("create proxy");
        set_provider.new_route_set(server_end).expect("new route set");
        proxy
    };

    let route_to_add = test_route(&interface);

    for expect_newly_added in [true, false] {
        assert_eq!(
            expect_newly_added,
            proxy
                .add_route(&route_to_add.try_into().expect("convert to FIDL"))
                .await
                .expect("no FIDL error")
                .expect("add route")
        );
    }

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Even though it was added twice, removing the route once should remove it.
    assert!(proxy
        .remove_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("remove route"));

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Removing the route a second time should return false.
    assert!(!proxy
        .remove_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("remove route"));
}

// TODO(https://fxbug.dev/129220): Add test cases where one of the "multiple
// sets" is the "system route set", and/or cases where the route is implicitly
// installed by the netstack (e.g. for loopback).
#[netstack_test]
async fn add_route_with_multiple_sets<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<Ipv4>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<Ipv4, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let get_route_set = || {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fnet_routes_admin::RouteSetV4Marker>()
                .expect("create proxy");
        set_provider.new_route_set(server_end).expect("new route set");
        proxy
    };
    let proxy_a = get_route_set();
    let proxy_b = get_route_set();

    let route_to_add = test_route(&interface);

    for proxy in [&proxy_a, &proxy_b] {
        assert!(proxy
            .add_route(&route_to_add.try_into().expect("convert to FIDL"))
            .await
            .expect("no FIDL error")
            .expect("add route"));
    }

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Even if one of the route sets removes the route, it should remain until
    // the other one removes it.
    assert!(proxy_a
        .remove_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("remove route"));

    assert_matches!(
        fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
            !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
        })
        .map(Some)
        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None)
        .await,
        None
    );

    // Then removing the route from the other route set should result in the route getting removed.
    assert!(proxy_b
        .remove_route(&route_to_add.try_into().expect("convert to FIDL"))
        .await
        .expect("no FIDL error")
        .expect("remove route"));

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");
}
