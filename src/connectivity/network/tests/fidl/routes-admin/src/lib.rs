// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests for fuchsia.net.routes.admin.
// TODO(https://fxbug.dev/42079627): Add tests for authentication of interfaces.

#![cfg(test)]

mod rules;

use std::collections::HashSet;

use assert_matches::assert_matches;
use fidl::endpoints::{ProtocolMarker, Proxy as _};
use fidl_fuchsia_net_routes_ext::admin::FidlRouteAdminIpExt;
use fidl_fuchsia_net_routes_ext::{self as fnet_routes_ext, FidlRouteIpExt, RouteAction};
use fuchsia_async::TimeoutExt as _;
use futures::future::FutureExt as _;
use futures::StreamExt;
use itertools::Itertools as _;
use net_declare::{fidl_ip_v4, fidl_ip_v4_with_prefix, fidl_ip_v6, fidl_ip_v6_with_prefix};
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6, Subnet};
use netstack_testing_common::realms::{Netstack, Netstack2, Netstack3, TestSandboxExt};
use netstack_testing_common::ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT;
use netstack_testing_macros::netstack_test;
use routes_common::{test_route, TestSetup};
use std::pin::pin;
use test_case::{test_case, test_matrix};
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_routes as fnet_routes,
    fidl_fuchsia_net_routes_admin as fnet_routes_admin, fidl_fuchsia_net_stack as fnet_stack,
};

const METRIC_TRACKS_INTERFACE: fnet_routes::SpecifiedMetric =
    fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty);

enum SystemRouteProtocol {
    NetRootRoutes,
    NetStack,
}

enum RouteSet {
    Global,
    User,
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(true, METRIC_TRACKS_INTERFACE, RouteSet::User; "explicitly removing the route")]
#[test_case(
    true,
    METRIC_TRACKS_INTERFACE,
    RouteSet::Global;
    "explicitly removing the route, global"
)]
#[test_case(false, METRIC_TRACKS_INTERFACE, RouteSet::User; "dropping the route set")]
#[test_case(
    true,
    fnet_routes::SpecifiedMetric::ExplicitMetric(0),
    RouteSet::User;
    "explicit zero metric"
)]
#[test_case(
    true,
    fnet_routes::SpecifiedMetric::ExplicitMetric(0),
    RouteSet::Global;
    "explicit zero metric, global"
)]
#[test_case(
    true,
    fnet_routes::SpecifiedMetric::ExplicitMetric(12345),
    RouteSet::User;
    "explicit non-zero metric"
)]
#[test_case(
    true,
    fnet_routes::SpecifiedMetric::ExplicitMetric(12345),
    RouteSet::Global;
    "explicit non-zero metric, global"
)]
async fn add_remove_route<I: FidlRouteAdminIpExt + FidlRouteIpExt, N: Netstack>(
    name: &str,
    explicit_remove: bool,
    metric: fnet_routes::SpecifiedMetric,
    route_set_type: RouteSet,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _realm,
        network: _network,
        interface,
        route_table,
        global_route_table,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    println!("initial routes = {routes:?}");

    let proxy = match route_set_type {
        RouteSet::Global => fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
            .expect("new global route set"),
        RouteSet::User => {
            fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set")
        }
    };

    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route_to_add = test_route(&interface, metric);

    let add_route_and_assert_added = |proxy| async {
        assert!(fnet_routes_ext::admin::add_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error")
        .expect("add route"));
        proxy
    };

    let proxy = add_route_and_assert_added(proxy).await;

    // Dropping the channel to the main route table should not remove the table
    // nor any routes in it.
    std::mem::drop(route_table);

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
    })
    .await
    .expect("should succeed");

    println!("routes after adding = {routes:?}");

    // Move the RouteSet Proxy into an Option so that we can avoid dropping it in one of the cases.
    let maybe_proxy = if explicit_remove {
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

    if let Some(proxy) = maybe_proxy {
        // Adding the route back again should succeed.
        let _proxy = add_route_and_assert_added(proxy).await;

        fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
            routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
        })
        .await
        .expect("should succeed");

        println!("routes after re-adding = {routes:?}");
    }
}

fn specified_properties(
    metric: Option<fnet_routes::SpecifiedMetric>,
) -> Option<fnet_routes::SpecifiedRouteProperties> {
    Some(fnet_routes::SpecifiedRouteProperties { metric, ..Default::default() })
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Ok(());
    "accepts with no nexthop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    Some(fidl_ip_v4!("192.0.2.1")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Ok(());
    "accepts with valid nexthop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.1/24"),
    None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with set host bits"
)]
#[test_case(
    fnet::Ipv4AddressWithPrefix {
        addr: fidl_ip_v4!("192.0.2.0"),
        prefix_len: 33,
    },
    None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with invalid prefix length"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    Some(fidl_ip_v4!("255.255.255.255")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    ))) => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects broadcast next hop"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    Some(fidl_ip_v4!("0.0.0.0")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    ))) => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects next hop set to unspecified address"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    None,
    None
    => Err(fnet_routes_admin::RouteSetError::MissingRouteProperties);
    "rejects missing specified properties"
)]
#[test_case(
    fidl_ip_v4_with_prefix!("192.0.2.0/24"),
    None,
    specified_properties(None)
    => Err(fnet_routes_admin::RouteSetError::MissingMetric);
    "rejects missing metric"
)]
async fn validates_route_v4<N: Netstack>(
    name: &str,
    destination: fnet::Ipv4AddressWithPrefix,
    next_hop: Option<fnet::Ipv4Address>,
    specified_properties: Option<fnet_routes::SpecifiedRouteProperties>,
) -> Result<(), fnet_routes_admin::RouteSetError> {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _realm,
        network: _network,
        interface,
        route_table,
        global_route_table: _,
        state: _,
    } = TestSetup::<Ipv4>::new::<N>(&sandbox, name).await;
    let proxy = fnet_routes_ext::admin::new_route_set::<Ipv4>(&route_table).expect("new route set");
    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<Ipv4>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route = fnet_routes::RouteV4 {
        destination,
        action: fnet_routes::RouteActionV4::Forward(fnet_routes::RouteTargetV4 {
            outbound_interface: interface.id(),
            next_hop: next_hop.map(Box::new),
        }),
        properties: fnet_routes::RoutePropertiesV4 { specified_properties, ..Default::default() },
    };
    let add_result =
        fnet_routes_ext::admin::add_route::<Ipv4>(&proxy, &route).await.expect("no FIDL error");

    let remove_result =
        fnet_routes_ext::admin::remove_route::<Ipv4>(&proxy, &route).await.expect("no FIDL error");

    assert_eq!(add_result, remove_result);
    add_result.map(|_: bool| ())
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Ok(());
    "accepts with no nexthop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("2001:DB8::1")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Ok(());
    "accepts with valid nexthop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::1/64"), None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with set host bits"
)]
#[test_case(
    fnet::Ipv6AddressWithPrefix {
        addr: fidl_ip_v6!("2001:DB8::"),
        prefix_len: 129,
    }, None,
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidDestinationSubnet);
    "rejects destination subnet with invalid prefix length"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("ff0e:0:0:0:0:DB8::1")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects multicast next hop"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"), Some(fidl_ip_v6!("::")),
    specified_properties(Some(fnet_routes::SpecifiedMetric::InheritedFromInterface(
        fnet_routes::Empty,
    )))
    => Err(fnet_routes_admin::RouteSetError::InvalidNextHop);
    "rejects next hop set to unspecified address"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"),
    None,
    None
    => Err(fnet_routes_admin::RouteSetError::MissingRouteProperties);
    "rejects missing specified properties"
)]
#[test_case(
    fidl_ip_v6_with_prefix!("2001:DB8::/64"),
    None,
    specified_properties(None)
    => Err(fnet_routes_admin::RouteSetError::MissingMetric);
    "rejects missing metric"
)]
async fn validates_route_v6<N: Netstack>(
    name: &str,
    destination: fnet::Ipv6AddressWithPrefix,
    next_hop: Option<fnet::Ipv6Address>,
    specified_properties: Option<fnet_routes::SpecifiedRouteProperties>,
) -> Result<(), fnet_routes_admin::RouteSetError> {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _realm,
        network: _network,
        interface,
        route_table,
        global_route_table: _,
        state: _,
    } = TestSetup::<Ipv6>::new::<N>(&sandbox, name).await;
    let proxy = fnet_routes_ext::admin::new_route_set::<Ipv6>(&route_table).expect("new route set");
    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<Ipv6>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route = fnet_routes::RouteV6 {
        destination,
        action: fnet_routes::RouteActionV6::Forward(fnet_routes::RouteTargetV6 {
            outbound_interface: interface.id(),
            next_hop: next_hop.map(Box::new),
        }),
        properties: fnet_routes::RoutePropertiesV6 { specified_properties, ..Default::default() },
    };
    let add_result =
        fnet_routes_ext::admin::add_route::<Ipv6>(&proxy, &route).await.expect("no FIDL error");
    let remove_result =
        fnet_routes_ext::admin::remove_route::<Ipv6>(&proxy, &route).await.expect("no FIDL error");
    assert_eq!(add_result, remove_result);
    add_result.map(|_: bool| ())
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(SystemRouteProtocol::NetRootRoutes; "fuchsia.net.root/Routes")]
#[test_case(SystemRouteProtocol::NetStack; "fuchsia.net.stack/Stack")]
async fn add_route_twice_with_same_set<I: FidlRouteAdminIpExt + FidlRouteIpExt, N: Netstack>(
    name: &str,
    system_route_protocol: SystemRouteProtocol,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _realm,
        network: _network,
        interface,
        route_table,
        global_route_table,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let proxy = match system_route_protocol {
        SystemRouteProtocol::NetRootRoutes => {
            fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
                .expect("new global route set")
        }
        SystemRouteProtocol::NetStack => {
            fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set")
        }
    };

    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route_to_add = test_route(&interface, METRIC_TRACKS_INTERFACE);

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
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn add_route_with_multiple_route_sets<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _realm,
        network: _network,
        interface,
        route_table,
        global_route_table: _,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let get_route_set =
        || fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set");
    let proxy_a = get_route_set();
    let proxy_b = get_route_set();

    async fn authenticate<I: FidlRouteAdminIpExt + FidlRouteIpExt>(
        proxy: &<I::RouteSetMarker as ProtocolMarker>::Proxy,
        grant: &fidl_fuchsia_net_interfaces_admin::GrantForInterfaceAuthorization,
    ) {
        let proof = fnet_interfaces_ext::admin::proof_from_grant(grant);
        fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
            .await
            .expect("no FIDL error")
            .expect("authentication should succeed");
    }

    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    authenticate::<I>(&proxy_a, &grant).await;
    authenticate::<I>(&proxy_b, &grant).await;

    let route_to_add = test_route(&interface, METRIC_TRACKS_INTERFACE);

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
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(SystemRouteProtocol::NetRootRoutes; "fuchsia.net.root/Routes")]
#[test_case(SystemRouteProtocol::NetStack; "fuchsia.net.stack/Stack")]
async fn add_remove_system_route<I: FidlRouteAdminIpExt + FidlRouteIpExt, N: Netstack>(
    name: &str,
    system_route_protocol: SystemRouteProtocol,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _network, interface, route_table, global_route_table, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let route_set =
        fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set");
    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&route_set, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route_to_add = test_route::<I>(&interface, METRIC_TRACKS_INTERFACE);

    // Add a "system route".
    match system_route_protocol {
        SystemRouteProtocol::NetRootRoutes => {
            let proxy = fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
                .expect("new global route set");

            let grant = interface.get_authorization().await.expect("getting grant should succeed");
            let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
            fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
                .await
                .expect("no FIDL error")
                .expect("authentication should succeed");

            assert!(fnet_routes_ext::admin::add_route::<I>(
                &proxy,
                &route_to_add.try_into().expect("convert to FIDL")
            )
            .await
            .expect("no FIDL error")
            .expect("add route"));
        }
        SystemRouteProtocol::NetStack => {
            let fuchsia_net_stack = realm
                .connect_to_protocol::<fnet_stack::StackMarker>()
                .expect("connect to fuchsia.net.stack.Stack");
            fuchsia_net_stack
                .add_forwarding_entry(&route_to_add.try_into().expect("convert to ForwardingEntry"))
                .await
                .expect("should not have FIDL error")
                .expect("should succeed");
        }
    }

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
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(SystemRouteProtocol::NetRootRoutes; "fuchsia.net.root/Routes")]
#[test_case(SystemRouteProtocol::NetStack; "fuchsia.net.stack/Stack")]
async fn system_removes_route_from_route_set<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    system_route_protocol: SystemRouteProtocol,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _network, interface, route_table, global_route_table, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let route_set =
        fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set");
    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&route_set, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route_to_add = test_route::<I>(&interface, METRIC_TRACKS_INTERFACE);

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
    match system_route_protocol {
        SystemRouteProtocol::NetRootRoutes => {
            let proxy = fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
                .expect("new global route set");

            let grant = interface.get_authorization().await.expect("getting grant should succeed");
            let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
            fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
                .await
                .expect("no FIDL error")
                .expect("authentication should succeed");

            assert!(fnet_routes_ext::admin::remove_route::<I>(
                &proxy,
                &route_to_add.try_into().expect("convert to FIDL")
            )
            .await
            .expect("no FIDL error")
            .expect("add route"));
        }
        SystemRouteProtocol::NetStack => {
            let fuchsia_net_stack = realm
                .connect_to_protocol::<fnet_stack::StackMarker>()
                .expect("connect to fuchsia.net.stack.Stack");
            fuchsia_net_stack
                .del_forwarding_entry(&route_to_add.try_into().expect("convert to ForwardingEntry"))
                .await
                .expect("should not have FIDL error")
                .expect("should succeed");
        }
    }

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

// TODO(https://fxbug.dev/42081105): Remove all uses of {Add,Del}ForwardingEntry
// from this file.
#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(SystemRouteProtocol::NetRootRoutes; "fuchsia.net.root/Routes")]
#[test_case(SystemRouteProtocol::NetStack; "fuchsia.net.stack/Stack")]
async fn root_route_apis_can_remove_loopback_route<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    system_route_protocol: SystemRouteProtocol,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm,
        network: _network,
        interface: _interface,
        route_table: _,
        global_route_table,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

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
             table_id: _,
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
    match system_route_protocol {
        SystemRouteProtocol::NetRootRoutes => {
            let proxy = fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
                .expect("new global route set");

            let iface_id = if let RouteAction::Forward(target) = loopback_route.route.action {
                target.outbound_interface
            } else {
                panic!("could not determine interface for route");
            };

            let root_interfaces = realm
                .connect_to_protocol::<fidl_fuchsia_net_root::InterfacesMarker>()
                .expect("connect to protocol");
            let (interface_control, interface_control_server_end) =
                fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                    .expect("create proxy");
            let () = root_interfaces
                .get_admin(iface_id, interface_control_server_end)
                .expect("create root interfaces connection");

            let grant = interface_control
                .get_authorization_for_interface()
                .await
                .expect("getting grant should succeed");
            let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
            fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
                .await
                .expect("no FIDL error")
                .expect("authentication should succeed");

            assert!(fnet_routes_ext::admin::remove_route::<I>(
                &proxy,
                &loopback_route.route.try_into().expect("convert to FIDL")
            )
            .await
            .expect("should not have FIDL error")
            .expect("should succeed"));
        }
        SystemRouteProtocol::NetStack => {
            let fuchsia_net_stack = realm
                .connect_to_protocol::<fnet_stack::StackMarker>()
                .expect("connect to fuchsia.net.stack.Stack");
            fuchsia_net_stack
                .del_forwarding_entry(
                    &loopback_route.route.try_into().expect("convert to ForwardingEntry"),
                )
                .await
                .expect("should not have FIDL error")
                .expect("should succeed");
        }
    }

    // Loopback route should disappear.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        !routes.iter().any(is_loopback_route)
    })
    .await
    .expect("should succeed");
}

#[derive(Debug)]
enum DefaultRouteRemovalCase {
    DropRouteSet,
    ExplicitRemove,
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(DefaultRouteRemovalCase::DropRouteSet; "drop route set")]
#[test_case(DefaultRouteRemovalCase::ExplicitRemove; "explicit remove")]
async fn removing_one_default_route_does_not_flip_presence<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    removal_case: DefaultRouteRemovalCase,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm,
        network: _network,
        interface,
        route_table,
        global_route_table: _,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let route_set_1 =
        fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set");
    let route_set_2 =
        fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set");
    let authenticate = |proxy| async {
        let grant = interface.get_authorization().await.expect("getting grant should succeed");
        let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
        fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
            .await
            .expect("no FIDL error")
            .expect("authentication should succeed");
        proxy
    };

    let route_set_1 = authenticate(route_set_1).await;
    let route_set_2 = authenticate(route_set_2).await;

    let events = realm.get_interface_event_stream().expect("get interface event stream").fuse();
    let mut events = pin!(events);

    let default_route = |metric| {
        let destination =
            Subnet::new(I::UNSPECIFIED_ADDRESS, 0).expect("unspecified subnet should be valid");
        fnet_routes_ext::Route {
            destination,
            action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget::<I> {
                outbound_interface: interface.id(),
                next_hop: None,
            }),
            properties: fnet_routes_ext::RouteProperties {
                specified_properties: fnet_routes_ext::SpecifiedRouteProperties { metric },
            },
        }
    };
    let default_route_1 =
        default_route(fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty));
    let default_route_2 = default_route(fnet_routes::SpecifiedMetric::ExplicitMetric(123));

    // Add a default route.
    assert!(fnet_routes_ext::admin::add_route::<I>(
        &route_set_1,
        &default_route_1.clone().try_into().expect("convert to FIDL"),
    )
    .await
    .expect("should not get add route FIDL error")
    .expect("should not get add route error"));

    let mut interface_state = fnet_interfaces_ext::InterfaceState::Unknown(interface.id());

    fnet_interfaces_ext::wait_interface_with_id(
        &mut events,
        &mut interface_state,
        |fnet_interfaces_ext::PropertiesAndState {
             state: (),
             properties:
                 fnet_interfaces_ext::Properties {
                     has_default_ipv4_route, has_default_ipv6_route, ..
                 },
         }| {
            (match I::VERSION {
                net_types::ip::IpVersion::V4 => has_default_ipv4_route,
                net_types::ip::IpVersion::V6 => has_default_ipv6_route,
            })
            .then_some(())
        },
    )
    .await
    .expect("should have default route");

    // Add a default route with a different metric (so they don't get de-duped).
    assert!(fnet_routes_ext::admin::add_route::<I>(
        &route_set_2,
        &default_route_2.clone().try_into().expect("convert to FIDL"),
    )
    .await
    .expect("should not get add route FIDL error")
    .expect("should not get add route error"));

    let mut routes_state = HashSet::new();

    // Both routes should be present.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes_state, |routes| {
        routes.iter().any(|route| &route.route == &default_route_1)
            && routes.iter().any(|route| &route.route == &default_route_2)
    })
    .await
    .expect("should succeed");

    // Remove a default route.
    let opt_route_set_1 = match removal_case {
        DefaultRouteRemovalCase::DropRouteSet => {
            drop(route_set_1);
            None
        }
        DefaultRouteRemovalCase::ExplicitRemove => {
            assert!(fnet_routes_ext::admin::remove_route::<I>(
                &route_set_1,
                &default_route_1.clone().try_into().expect("convert to FIDL"),
            )
            .await
            .expect("should not get remove route FIDL error")
            .expect("should not get remove route error"));
            Some(route_set_1)
        }
    };

    // `default_route_1` should disappear.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes_state, |routes| {
        !routes.iter().any(|route| &route.route == &default_route_1)
    })
    .await
    .expect("should succeed");

    // Interface should still report having a default route.
    fnet_interfaces_ext::wait_interface_with_id(
        &mut events,
        &mut interface_state,
        |fnet_interfaces_ext::PropertiesAndState {
             state: (),
             properties:
                 fnet_interfaces_ext::Properties {
                     has_default_ipv4_route, has_default_ipv6_route, ..
                 },
         }| {
            (match I::VERSION {
                net_types::ip::IpVersion::V4 => !has_default_ipv4_route,
                net_types::ip::IpVersion::V6 => !has_default_ipv6_route,
            })
            .then_some(())
        },
    )
    .map(|_| panic!("has_default_ipvX_route should still be true"))
    .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || ())
    .await;

    // Ensure route sets are kept alive all the way through (unless they were
    // explicitly dropped above).
    drop((opt_route_set_1, route_set_2));
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn dropping_global_route_set_does_not_remove_routes<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup {
        realm: _,
        network: _network,
        interface,
        route_table: _,
        global_route_table,
        state,
    } = TestSetup::<I>::new::<N>(&sandbox, name).await;

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    let route_to_add = test_route::<I>(&interface, METRIC_TRACKS_INTERFACE);

    // Create a new global route set and add a new route with it.
    let proxy = fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
        .expect("new global route set");
    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

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

    // Drop the route set.  Since this is a global route set, the route created
    // by this route set should not be removed.  This is unlike a user route
    // set.
    drop(proxy);

    assert_matches!(
        fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
            routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
        })
        .map(Some)
        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None)
        .await,
        None
    );
}

enum InvalidProofKind {
    /// The invalid token is one generated by the client.
    ClientGenerated,
    /// The invalid token is one generated by the netstack, but is for a
    /// different interface.
    WrongInterface,
    /// The interface ID doesn't correspond to an extant interface.
    BadInterface,
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(
    InvalidProofKind::ClientGenerated,
    RouteSet::Global;
    "client-generated token global routeset"
)]
#[test_case(
    InvalidProofKind::ClientGenerated,
    RouteSet::User;
    "client-generated token user routeset"
)]
#[test_case(
    InvalidProofKind::WrongInterface,
    RouteSet::Global;
    "wrong interface token global routeset"
)]
#[test_case(
    InvalidProofKind::WrongInterface,
    RouteSet::User;
    "wrong interface token user routeset"
)]
#[test_case(
    InvalidProofKind::BadInterface,
    RouteSet::Global;
    "bad interface token global routeset"
)]
#[test_case(
    InvalidProofKind::BadInterface,
    RouteSet::User;
    "bad interface token user routeset"
)]
async fn interface_authorization_fails_with_invalid_token<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    invalid_proof_kind: InvalidProofKind,
    route_set_type: RouteSet,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _, interface, route_table, global_route_table, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let device = sandbox.create_endpoint(name).await.expect("create endpoint");
    let second_interface = realm
        .install_endpoint(device, netemul::InterfaceConfig::default())
        .await
        .expect("install interface");

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let proxy = match route_set_type {
        RouteSet::Global => fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
            .expect("new global route set"),
        RouteSet::User => {
            fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set")
        }
    };

    let proof = match invalid_proof_kind {
        InvalidProofKind::ClientGenerated => fnet_interfaces_admin::ProofOfInterfaceAuthorization {
            token: zx::Event::create(),
            interface_id: interface.id(),
        },
        InvalidProofKind::WrongInterface => {
            let grant =
                second_interface.get_authorization().await.expect("getting grant should succeed");

            // Note that the token comes from a different interface than the ID.
            fnet_interfaces_admin::ProofOfInterfaceAuthorization {
                token: grant.token,
                interface_id: interface.id(),
            }
        }
        InvalidProofKind::BadInterface => {
            let grant = interface.get_authorization().await.expect("getting grant should succeed");
            fnet_interfaces_admin::ProofOfInterfaceAuthorization {
                token: grant.token,
                interface_id: interface.id() + 1000,
            }
        }
    };

    assert_matches!(
        fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
            .await
            .expect("no FIDL error"),
        Err(fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication)
    );

    // Try adding a route just to be sure that the netstack didn't somehow
    // authorize the interface.
    let route_to_add = test_route(&interface, METRIC_TRACKS_INTERFACE);

    assert_matches!(
        fnet_routes_ext::admin::add_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error"),
        Err(fnet_routes_admin::RouteSetError::Unauthenticated)
    );

    let routes = fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
        .await
        .expect("collect routes should succeed");
    assert!(!routes.iter().any(|installed_route| &installed_route.route == &route_to_add));
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(RouteSet::User; "user routeset")]
#[test_case(RouteSet::Global; "global routeset")]
async fn authorizing_for_one_interface_out_of_two<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    route_set_type: RouteSet,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm, network: _, interface, route_table, global_route_table, state: _ } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;

    let device = sandbox.create_endpoint(name).await.expect("create endpoint");
    let second_interface = realm
        .install_endpoint(device, netemul::InterfaceConfig::default())
        .await
        .expect("install interface");

    let proxy = match route_set_type {
        RouteSet::Global => fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
            .expect("new global route set"),
        RouteSet::User => {
            fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set")
        }
    };

    let grant = second_interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    // Adding a route for the primary interface should fail, since this channel
    // was only authenticated for the second interface.
    let route_to_add = test_route(&interface, METRIC_TRACKS_INTERFACE);
    assert_matches!(
        fnet_routes_ext::admin::add_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error"),
        Err(fnet_routes_admin::RouteSetError::Unauthenticated)
    );
}

// interface_authorization_fails_with_invalid_token ensures that unauthenticated
// connections can't add routes.
#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(RouteSet::User)]
#[test_case(RouteSet::Global)]
async fn unauthenticated_connections_cannot_remove_routes<
    I: FidlRouteAdminIpExt + FidlRouteIpExt,
    N: Netstack,
>(
    name: &str,
    route_set_type: RouteSet,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let TestSetup { realm: _, network: _, interface, route_table, global_route_table, state } =
        TestSetup::<I>::new::<N>(&sandbox, name).await;
    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let route_to_add = test_route(&interface, METRIC_TRACKS_INTERFACE);
    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    {
        let proxy = fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
            .expect("new global route set");

        let grant = interface
            .control()
            .get_authorization_for_interface()
            .await
            .expect("getting grant should succeed");
        let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
        fnet_routes_ext::admin::authenticate_for_interface::<I>(&proxy, proof)
            .await
            .expect("no FIDL error")
            .expect("authentication should succeed");

        // Add a route to ensure authentication took effect.
        assert!(fnet_routes_ext::admin::add_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error")
        .expect("add route"));

        // Ensure the route was added
        fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
            routes.iter().any(|installed_route| &installed_route.route == &route_to_add)
        })
        .await
        .expect("should succeed");
    }

    let proxy = match route_set_type {
        RouteSet::Global => fnet_routes_ext::admin::new_global_route_set::<I>(&global_route_table)
            .expect("new global route set"),
        RouteSet::User => {
            fnet_routes_ext::admin::new_route_set::<I>(&route_table).expect("new route set")
        }
    };

    assert_matches!(
        fnet_routes_ext::admin::remove_route::<I>(
            &proxy,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error"),
        Err(fnet_routes_admin::RouteSetError::Unauthenticated)
    );

    // The route should not have been removed.
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
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn main_table_remove<I: FidlRouteAdminIpExt + FidlRouteIpExt, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let route_table = realm
        .connect_to_protocol::<I::RouteTableMarker>()
        .expect("connect to routes-admin RouteTable");
    assert_eq!(
        fnet_routes_ext::admin::remove_route_table::<I>(&route_table)
            .await
            .expect("fidl should succeed"),
        Err(fnet_routes_admin::BaseRouteTableRemoveError::InvalidOpOnMainTable),
    );
}

#[netstack_test]
#[variant(N, Netstack)]
async fn unique_main_table_id<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let main_table_v4 = realm
        .connect_to_protocol::<fnet_routes_admin::RouteTableV4Marker>()
        .expect("connect to routes-admin RouteTable");
    let main_table_v6 = realm
        .connect_to_protocol::<fnet_routes_admin::RouteTableV6Marker>()
        .expect("connect to routes-admin RouteTable");
    let v4_id = fnet_routes_ext::admin::get_table_id::<Ipv4>(&main_table_v4)
        .await
        .expect("fidl should succeed");
    let v6_id = fnet_routes_ext::admin::get_table_id::<Ipv6>(&main_table_v6)
        .await
        .expect("fidl should succeed");
    assert_ne!(v4_id, v6_id);
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn main_table_authorization<I: FidlRouteAdminIpExt + FidlRouteIpExt, N: Netstack>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm::<N, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let route_table = realm
        .connect_to_protocol::<I::RouteTableMarker>()
        .expect("connect to routes-admin RouteTable");
    let table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&route_table).await.expect("fidl should succeed");
    let fnet_routes_admin::GrantForRouteTableAuthorization {
        table_id: authorized_table_id,
        token: _,
    } = fnet_routes_ext::admin::get_authorization_for_route_table::<I>(&route_table)
        .await
        .expect("fidl should succeed");
    assert_eq!(table_id.get(), authorized_table_id);
}

// Netstack2 does not support fuchsia.net.routes.admin.RouteTableProviderV{4, 6}, so it closes the
// channel as soon as a request comes in.
#[netstack_test]
#[variant(I, Ip)]
async fn route_table_provider_netstack2_closes_channel<I: FidlRouteAdminIpExt + FidlRouteIpExt>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm::<Netstack2, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let route_table_provider = realm
        .connect_to_protocol::<I::RouteTableProviderMarker>()
        .expect("connect to route table provider");
    let _table = fnet_routes_ext::admin::new_route_table::<I>(&route_table_provider, None)
        .expect("create new route table");

    let signals =
        route_table_provider.on_closed().await.expect("should await closure successfully");
    assert!(signals.contains(zx::Signals::CHANNEL_PEER_CLOSED));
}

#[netstack_test]
#[variant(I, Ip)]
async fn add_route_table<I: FidlRouteAdminIpExt + FidlRouteIpExt>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support multiple route tables in netstack2.
    let realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let main_route_table =
        realm.connect_to_protocol::<I::RouteTableMarker>().expect("connect to main route table");
    let route_table_provider = realm
        .connect_to_protocol::<I::RouteTableProviderMarker>()
        .expect("connect to main route table");
    let user_route_table =
        fnet_routes_ext::admin::new_route_table::<I>(&route_table_provider, None)
            .expect("create new user table");
    let main_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&main_route_table).await.expect("get table id");
    let user_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&user_route_table).await.expect("get table id");
    assert_ne!(main_table_id, user_table_id);
}

fn route_set_err_stream<I: FidlRouteAdminIpExt>(
    route_set: <I::RouteSetMarker as ProtocolMarker>::Proxy,
) -> futures::stream::BoxStream<'static, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct In<I: FidlRouteAdminIpExt>(<I::RouteSetMarker as ProtocolMarker>::Proxy);

    #[allow(unreachable_patterns)] // TODO(https://fxbug.dev/360335974)
    let IpInvariant(err_stream) = net_types::map_ip_twice!(I, In(route_set), |In(route_set)| {
        IpInvariant(
            route_set
                .take_event_stream()
                .map(|result| match result {
                    Err(err) => err,
                    Ok(event) => match event {},
                })
                .boxed(),
        )
    });
    err_stream
}

#[netstack_test]
#[variant(I, Ip)]
#[test_matrix(
    [true, false],
    [true, false]
)]
async fn route_set_closed_when_table_removed<I: FidlRouteAdminIpExt + FidlRouteIpExt>(
    name: &str,
    explicit_remove: bool,
    detach: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support multiple route tables in netstack2.
    let TestSetup {
        realm,
        network: _network,
        interface,
        route_table: _,
        global_route_table: _,
        state,
    } = TestSetup::<I>::new::<Netstack3>(&sandbox, name).await;
    let route_table_provider = realm
        .connect_to_protocol::<I::RouteTableProviderMarker>()
        .expect("connect to main route table");
    let user_route_table =
        fnet_routes_ext::admin::new_route_table::<I>(&route_table_provider, None)
            .expect("create new user table");
    let user_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&user_route_table).await.expect("get table id");
    let user_route_set = fnet_routes_ext::admin::new_route_set::<I>(&user_route_table)
        .expect("failed to create new route set");

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");
    assert_eq!(routes.iter().find(|r| r.table_id == user_table_id), None);

    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&user_route_set, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");
    let route_to_add =
        test_route::<I>(&interface, fnet_routes::SpecifiedMetric::ExplicitMetric(10));

    assert!(fnet_routes_ext::admin::add_route::<I>(
        &user_route_set,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("add route"));

    fnet_routes_ext::wait_for_routes(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| {
            installed_route.matches_route_and_table_id(&route_to_add, user_table_id)
        })
    })
    .await
    .expect("should succeed");

    if detach {
        fnet_routes_ext::admin::detach_route_table::<I>(&user_route_table)
            .await
            .expect("fidl should succeed");
    }

    if explicit_remove {
        fnet_routes_ext::admin::remove_route_table::<I>(&user_route_table)
            .await
            .expect("fidl should succeed")
            .expect("removal should succeed");
        let _: zx::Signals = user_route_table.on_closed().await.expect("observing channel close");
    } else {
        std::mem::drop(user_route_table);
    }

    if detach && !explicit_remove {
        // If detached, the route table still exists if not explicitly removed.
        // We can still remove the route we installed.
        assert!(fnet_routes_ext::admin::remove_route::<I>(
            &user_route_set,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error")
        .expect("remove route"));
    } else {
        // If not detached, or the table is explicitly removed, the route set
        // should be closed.
        let mut err_stream = route_set_err_stream::<I>(user_route_set);
        assert_matches!(
            err_stream.next().await,
            Some(fidl::Error::ClientChannelClosed {
                status: zx::Status::UNAVAILABLE,
                protocol_name: _,
            })
        );
        assert_matches!(err_stream.next().await, None);

        // The route should also be removed by now.
        fnet_routes_ext::wait_for_routes(&mut routes_stream, &mut routes, |routes| {
            routes.iter().all(|installed_route| installed_route.table_id != user_table_id)
        })
        .await
        .expect("should succeed");
    }
}

#[netstack_test]
#[variant(I, Ip)]
async fn add_route_in_user_table<I: FidlRouteAdminIpExt + FidlRouteIpExt>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support multiple route tables in netstack2.
    let TestSetup {
        realm,
        network: _network,
        interface,
        route_table: _,
        global_route_table: _,
        state,
    } = TestSetup::<I>::new::<Netstack3>(&sandbox, name).await;
    let route_table_provider = realm
        .connect_to_protocol::<I::RouteTableProviderMarker>()
        .expect("connect to main route table");
    let user_route_table =
        fnet_routes_ext::admin::new_route_table::<I>(&route_table_provider, None)
            .expect("create new user table");
    let user_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&user_route_table).await.expect("get table id");
    let user_route_set = fnet_routes_ext::admin::new_route_set::<I>(&user_route_table)
        .expect("failed to create a new user route set");

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    assert_eq!(routes.iter().find(|r| r.table_id == user_table_id), None);

    let grant = interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<I>(&user_route_set, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    let route_to_add =
        test_route::<I>(&interface, fnet_routes::SpecifiedMetric::ExplicitMetric(10));

    assert!(fnet_routes_ext::admin::add_route::<I>(
        &user_route_set,
        &route_to_add.try_into().expect("convert to FIDL")
    )
    .await
    .expect("no FIDL error")
    .expect("add route"));

    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| {
            installed_route.matches_route_and_table_id(&route_to_add, user_table_id)
        })
    })
    .await
    .expect("should succeed");
}

#[netstack_test]
#[variant(I, Ip)]
async fn interface_removal_remove_routes_in_all_tables<I: FidlRouteAdminIpExt + FidlRouteIpExt>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support multiple route tables in netstack2.
    let TestSetup {
        realm,
        network: _network,
        interface,
        route_table,
        global_route_table: _,
        state,
    } = TestSetup::<I>::new::<Netstack3>(&sandbox, name).await;
    let route_table_provider = realm
        .connect_to_protocol::<I::RouteTableProviderMarker>()
        .expect("connect to main route table");
    let user_route_table =
        fnet_routes_ext::admin::new_route_table::<I>(&route_table_provider, None)
            .expect("create new user table");
    let user_route_set = fnet_routes_ext::admin::new_route_set::<I>(&user_route_table)
        .expect("failed to create a new user route set");
    let main_route_set = fnet_routes_ext::admin::new_route_set::<I>(&route_table)
        .expect("failed to create a new main route set");
    let main_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&route_table).await.expect("get table id");
    let user_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&user_route_table).await.expect("get table id");

    let route_to_add =
        test_route::<I>(&interface, fnet_routes::SpecifiedMetric::ExplicitMetric(10));

    for route_set in [&main_route_set, &user_route_set] {
        let grant = interface.get_authorization().await.expect("getting grant should succeed");
        let proof = fnet_interfaces_ext::admin::proof_from_grant(&grant);
        fnet_routes_ext::admin::authenticate_for_interface::<I>(&route_set, proof)
            .await
            .expect("no FIDL error")
            .expect("authentication should succeed");

        assert!(fnet_routes_ext::admin::add_route::<I>(
            &route_set,
            &route_to_add.try_into().expect("convert to FIDL")
        )
        .await
        .expect("no FIDL error")
        .expect("add route"));
    }

    let routes_stream =
        fnet_routes_ext::event_stream_from_state::<I>(&state).expect("should succeed");
    let mut routes_stream = pin!(routes_stream);

    let mut routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(&mut routes_stream)
            .await
            .expect("collect routes should succeed");

    assert_eq!(
        routes
            .iter()
            .filter_map(|installed_route| (&installed_route.route == &route_to_add)
                .then_some(installed_route.table_id))
            .collect::<HashSet<_>>(),
        HashSet::from([main_table_id, user_table_id])
    );

    drop(interface);

    // The route should be removed across all tables.
    fnet_routes_ext::wait_for_routes::<I, _, _>(&mut routes_stream, &mut routes, |routes| {
        routes.iter().all(|installed_route| {
            !installed_route.matches_route_and_table_id(&route_to_add, user_table_id)
                && !installed_route.matches_route_and_table_id(&route_to_add, main_table_id)
        })
    })
    .await
    .expect("should succeed");
}
