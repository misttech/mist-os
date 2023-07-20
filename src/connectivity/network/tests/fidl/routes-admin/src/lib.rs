// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests for fuchsia.net.routes.admin.
// TODO(https://fxbug.dev/129220): Add tests for authentication of interfaces.

#![cfg(test)]

use std::collections::HashSet;

use assert_matches::assert_matches;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext::{
    self as fnet_routes_ext, admin::FidlRouteAdminIpExt, FidlRouteIpExt,
};
use fidl_fuchsia_net_stack as fnet_stack;
use fuchsia_async::TimeoutExt as _;
use futures::{future::FutureExt as _, pin_mut};
use itertools::Itertools as _;
use net_declare::{fidl_ip_v4, fidl_ip_v4_with_prefix, fidl_ip_v6, fidl_ip_v6_with_prefix};
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    SpecifiedAddr,
};
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
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"), None
    => Ok(());
    "accepts with no nexthop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"), Some(fidl_ip_v4!("192.0.2.1"))
    => Ok(());
    "accepts with valid nexthop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.1/24"), None
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with set host bits"
)]
#[test_case(
    fnet::Ipv4AddressWithPrefix {
        addr: fidl_ip_v4!("192.0.2.0"),
        prefix_len: 33,
    }, None
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with invalid prefix length"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"), Some(fidl_ip_v4!("255.255.255.255")) => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects broadcast next hop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"), Some(fidl_ip_v4!("0.0.0.0")) => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects next hop set to unspecified address"
)]
async fn validates_route_v4<N: Netstack>(
    name: &str,
    destination: fnet::Ipv4AddressWithPrefix,
    next_hop: Option<fnet::Ipv4Address>,
) -> Result<(), fnet_routes_admin::RouteSetError> {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state: _ } =
        TestSetup::<Ipv4>::new::<N>(&sandbox, name).await;
    let proxy =
        fnet_routes_ext::admin::new_route_set::<Ipv4>(&set_provider).expect("new route set");
    fnet_routes_ext::admin::add_route::<Ipv4>(
        &proxy,
        &fnet_routes::RouteV4 {
            destination,
            action: fnet_routes::RouteActionV4::Forward(fnet_routes::RouteTargetV4 {
                outbound_interface: interface.id(),
                next_hop: next_hop.map(Box::new),
            }),
            properties: fnet_routes::RoutePropertiesV4::default(),
        },
    )
    .await
    .expect("no FIDL error")
    .map(|_: bool| ())
}

#[netstack_test]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), None
    => Ok(());
    "accepts with no nexthop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("2001:DB8::1"))
    => Ok(());
    "accepts with valid nexthop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::1/64"), None
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with set host bits"
)]
#[test_case(
    fnet::Ipv6AddressWithPrefix {
        addr: fidl_ip_v6!("2001:DB8::"),
        prefix_len: 129,
    }, None
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with invalid prefix length"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("ff0e:0:0:0:0:DB8::1"))
    => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects multicast next hop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("::"))
    => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects next hop set to unspecified address"
)]
async fn validates_route_v6<N: Netstack>(
    name: &str,
    destination: fnet::Ipv6AddressWithPrefix,
    next_hop: Option<fnet::Ipv6Address>,
) -> Result<(), fnet_routes_admin::RouteSetError> {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _realm, network: _network, interface, set_provider, state: _ } =
        TestSetup::<Ipv6>::new::<N>(&sandbox, name).await;
    let proxy =
        fnet_routes_ext::admin::new_route_set::<Ipv6>(&set_provider).expect("new route set");
    fnet_routes_ext::admin::add_route::<Ipv6>(
        &proxy,
        &fnet_routes::RouteV6 {
            destination,
            action: fnet_routes::RouteActionV6::Forward(fnet_routes::RouteTargetV6 {
                outbound_interface: interface.id(),
                next_hop: next_hop.map(Box::new),
            }),
            properties: fnet_routes::RoutePropertiesV6::default(),
        },
    )
    .await
    .expect("no FIDL error")
    .map(|_: bool| ())
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
            fnet_routes_ext::admin::add_route::<I>(
                &proxy,
                &route_to_add.try_into().expect("convert to FIDL")
            )
            .await
            .expect("no FIDL error")
            .expect("add route"),
            expect_newly_added
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

#[netstack_test]
async fn add_route_with_multiple_route_sets<
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

#[netstack_test]
async fn add_remove_system_route<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _network, interface, set_provider, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let route_set =
        fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");
    let fuchsia_net_stack = realm
        .connect_to_protocol::<fnet_stack::StackMarker>()
        .expect("connect to fuchsia.net.stack.Stack");

    let route_to_add = test_route::<I>(&interface);

    // Add a "system route" with fuchsia.net.stack.
    fuchsia_net_stack
        .add_forwarding_entry(&route_to_add.try_into().expect("convert to ForwardingEntry"))
        .await
        .expect("should not have FIDL error")
        .expect("should succeed");

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // Trying to remove the route via the RouteSet should return `newly_removed` = false.
    assert!(!fnet_routes_ext::admin::remove_route::<I>(
        &route_set,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));

    // Adding and removing the same route with a RouteSet should not result in it going away.
    assert!(fnet_routes_ext::admin::add_route::<I>(
        &route_set,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("add route"));

    assert!(fnet_routes_ext::admin::remove_route::<I>(
        &route_set,
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
}

#[netstack_test]
async fn system_removes_route_from_route_set<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _network, interface, set_provider, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let route_set =
        fnet_routes_ext::admin::new_route_set::<I>(&set_provider).expect("new route set");
    let fuchsia_net_stack = realm
        .connect_to_protocol::<fnet_stack::StackMarker>()
        .expect("connect to fuchsia.net.stack.Stack");

    let route_to_add = test_route::<I>(&interface);

    // Add a route with the RouteSet.
    assert!(fnet_routes_ext::admin::add_route::<I>(
        &route_set,
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

    // Have the "system" remove that route out from under the RouteSet.
    fuchsia_net_stack
        .del_forwarding_entry(&route_to_add.try_into().expect("convert to ForwardingEntry"))
        .await
        .expect("should not have FIDL error")
        .expect("should succeed");

    // The route should disappear.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    // When we "remove" the route from the local RouteSet, the RouteSet should
    // have noticed the route disappearing (and thus return false here because
    // the route had already been removed).
    assert!(!fnet_routes_ext::admin::remove_route::<I>(
        &route_set,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("remove route"));
}

// TODO(https://fxbug.dev/130864): This test case doesn't entirely make sense to
// live here as it does not involve the routes-admin FIDL API, but this is the
// best place to park it until fuchsia.net.stack.Stack/{Add, Del}ForwardingEntry
// is removed, as it's part of generally validating the correctness of the
// changes supporting the routes-admin implementation.
#[netstack_test]
async fn root_route_apis_can_remove_loopback_route<
    I: net_types::ip::Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _network, interface: _interface, set_provider: _, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    pin_mut!(routes_stream);

    let mut routes = HashSet::new();
    let loopback_dest = I::map_ip(
        (),
        |()| net_declare::net_subnet_v4!("127.0.0.0/8"),
        |()| net_declare::net_subnet_v6!("::1/128"),
    );

    let is_loopback_route =
        |fnet_routes_ext::InstalledRoute {
             route: fnet_routes_ext::Route { destination, action: _, properties: _ },
             effective_properties: _,
         }: &fnet_routes_ext::InstalledRoute<I>| { destination == &loopback_dest };

    // Wait for loopback route to be present.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(is_loopback_route)
    })
    .await
    .expect("should succeed");

    let loopback_route = routes
        .iter()
        .filter(|route| is_loopback_route(*route))
        .exactly_one()
        .expect("should have exactly one loopback route");

    // Remove the loopback route.
    let fuchsia_net_stack = realm
        .connect_to_protocol::<fnet_stack::StackMarker>()
        .expect("connect to fuchsia.net.stack.Stack");
    fuchsia_net_stack
        .del_forwarding_entry(&loopback_route.route.try_into().expect("convert to ForwardingEntry"))
        .await
        .expect("should not have FIDL error")
        .expect("should succeed");

    // Loopback route should disappear.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(is_loopback_route)
    })
    .await
    .expect("should succeed");
}
