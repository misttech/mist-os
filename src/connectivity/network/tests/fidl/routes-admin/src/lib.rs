// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests for fuchsia.net.routes.admin.
// TODO(https://fxbug.dev/129220): These tests are work-in-progress and need to
// be made more comprehensive before we can call this sufficiently tested.

#![cfg(test)]

use std::collections::HashSet;

use assert_matches::assert_matches;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext::{
    self as fnet_routes_ext, admin::FidlRouteAdminIpExt, FidlRouteIpExt,
};
use fuchsia_async::TimeoutExt as _;
use futures::{future::FutureExt as _, pin_mut};
use net_types::{ip::Ip, SpecifiedAddr};
use netstack_testing_common::{
    realms::{Netstack, TestSandboxExt},
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

struct TestSetup<'a, I: Ip + FidlRouteIpExt + FidlRouteAdminIpExt> {
    realm: netemul::TestRealm<'a>,
    network: netemul::TestNetwork<'a>,
    interface: netemul::TestInterface<'a>,
    set_provider: <I::SetProviderMarker as ProtocolMarker>::Proxy,
    state: <I::StateMarker as ProtocolMarker>::Proxy,
}

impl<'a, I: Ip + FidlRouteIpExt + FidlRouteAdminIpExt> TestSetup<'a, I> {
    async fn new<N: Netstack>(sandbox: &'a netemul::TestSandbox, name: &str) -> TestSetup<'a, I> {
        let realm = sandbox
            .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
            .expect("create realm");
        let network =
            sandbox.create_network(format!("routes-admin-{name}")).await.expect("create network");
        let interface = realm.join_network(&network, "ep1").await.expect("join network");
        let set_provider = realm
            .connect_to_protocol::<I::SetProviderMarker>()
            .expect("connect to routes-admin SetProvider");
        let state = realm.connect_to_protocol::<I::StateMarker>().expect("connect to routes State");
        TestSetup { realm, network, interface, set_provider, state }
    }
}

fn test_route<I: Ip>(interface: &netemul::TestInterface<'_>) -> fnet_routes_ext::Route<I> {
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
            specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty),
            },
        },
    }
}

#[netstack_test]
#[test_case(true ; "explicitly removing the route")]
#[test_case(false ; "dropping the route set")]
async fn add_remove_route<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    explicit_remove: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    println!("initial routes = {routes:?}");

    let proxy = fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");

    let route_to_add = test_route(&interface);

    assert!(fnet_routes_ext::admin::add_route::<I>(
        &proxy,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("add route"));

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("routes after adding = {routes:?}");

    // Move the RouteSet Proxy into an Option so that we can avoid dropping it in one of the cases.
    let _maybe_proxy = if explicit_remove {
        assert!(fnet_routes_ext::admin::remove_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
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

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("routes after removing = {routes:?}");
}

#[netstack_test]
async fn add_route_twice_with_same_set<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let proxy = fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");

    let route_to_add = test_route(&interface);

    for expect_newly_added in [true, false] {
        assert_eq!(
            expect_newly_added,
            fnet_routes_ext::admin::add_route::<I>(
                &proxy,
                &route_to_add.try_into().expect("convert to FIDL")
            )
            .await
            .expect("no FIDL error")
            .expect("add route")
        );
    }

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Even though it was added twice, removing the route once should remove it.
    assert!(fnet_routes_ext::admin::remove_route::<I>(
        &proxy,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Removing the route a second time should return false.
    assert!(!fnet_routes_ext::admin::remove_route::<I>(
        &proxy,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));
}

// TODO(https://fxbug.dev/129220): Add test cases where one of the "multiple
// sets" is the "system route set", and/or cases where the route is implicitly
// installed by the netstack (e.g. for loopback).
#[netstack_test]
async fn add_route_with_multiple_sets<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let get_route_set =
        || fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");
    let proxy_a = get_route_set();
    let proxy_b = get_route_set();

    let route_to_add = test_route(&interface);

    for proxy in [&proxy_a, &proxy_b] {
        assert!(fnet_routes_ext::admin::add_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error")
        .expect("add route"));
    }

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Even if one of the route sets removes the route, it should remain until
    // the other one removes it.
    assert!(fnet_routes_ext::admin::remove_route::<I>(
        &proxy_a,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));

    assert_matches!(
        fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
            !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
        })
        .map(Some)
        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None)
        .await,
        None
    );

    // Then removing the route from the other route set should result in the route getting removed.
    assert!(fnet_routes_ext::admin::remove_route::<I>(
        &proxy_b,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");
}
