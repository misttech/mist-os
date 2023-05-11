// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::{collections::HashSet, mem::size_of};

use assert_matches::assert_matches;
use fidl_fuchsia_net as net;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use fuchsia_zircon as zx;

use anyhow::{self, Context as _};
use futures::{
    future, Future, FutureExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _,
};
use net_declare::net_ip_v6;
use net_types::{
    ethernet::Mac,
    ip::{self as net_types_ip, Ip, Ipv6},
    LinkLocalAddress as _, MulticastAddr, MulticastAddress as _, SpecifiedAddress as _,
    Witness as _,
};
use netstack_testing_common::{
    constants::{eth as eth_consts, ipv6 as ipv6_consts},
    interfaces,
    ndp::{
        self, assert_dad_failed, assert_dad_success, expect_dad_neighbor_solicitation,
        fail_dad_with_na, fail_dad_with_ns, send_ra, send_ra_with_router_lifetime, DadState,
    },
    realms::{constants, KnownServiceProvider, Netstack, NetstackVersion, TestSandboxExt as _},
    setup_network, setup_network_with, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::ParsablePacket as _;
use packet_formats::{
    ethernet::{EtherType, EthernetFrame, EthernetFrameLengthCheck},
    icmp::{
        mld::{MldPacket, Mldv2MulticastRecordType},
        ndp::{
            options::{NdpOption, NdpOptionBuilder, PrefixInformation, RouteInformation},
            NeighborSolicitation, RoutePreference, RouterAdvertisement, RouterSolicitation,
        },
        IcmpParseArgs, Icmpv6Packet,
    },
    ip::Ipv6Proto,
    testutil::{parse_icmp_packet_in_ip_packet_in_ethernet_frame, parse_ip_packet},
};
use test_case::test_case;

/// The expected number of Router Solicitations sent by the netstack when an
/// interface is brought up as a host.
const EXPECTED_ROUTER_SOLICIATIONS: u8 = 3;

/// The expected interval between sending Router Solicitation messages when
/// soliciting IPv6 routers.
const EXPECTED_ROUTER_SOLICITATION_INTERVAL: zx::Duration = zx::Duration::from_seconds(4);

/// The expected number of Neighbor Solicitations sent by the netstack when
/// performing Duplicate Address Detection.
const EXPECTED_DUP_ADDR_DETECT_TRANSMITS: u8 = 1;

/// The expected interval between sending Neighbor Solicitation messages when
/// performing Duplicate Address Detection.
const EXPECTED_DAD_RETRANSMIT_TIMER: zx::Duration = zx::Duration::from_seconds(1);

/// As per [RFC 7217 section 6] Hosts SHOULD introduce a random delay between 0 and
/// `IDGEN_DELAY` before trying a new tentative address.
///
/// [RFC 7217]: https://tools.ietf.org/html/rfc7217#section-6
const DAD_IDGEN_DELAY: zx::Duration = zx::Duration::from_seconds(1);

async fn install_and_get_ipv6_addrs_for_endpoint<N: Netstack>(
    realm: &netemul::TestRealm<'_>,
    endpoint: &netemul::TestEndpoint<'_>,
    name: &str,
) -> Vec<net::Subnet> {
    let (id, control, _device_control) = endpoint
        .add_to_stack(
            realm,
            netemul::InterfaceConfig { name: Some(name.into()), ..Default::default() },
        )
        .await
        .expect("installing interface");
    let did_enable = control.enable().await.expect("calling enable").expect("enable failed");
    assert!(did_enable);

    let interface_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State service");
    let mut state = fidl_fuchsia_net_interfaces_ext::InterfaceState::Unknown(id.into());
    let ipv6_addresses = fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interface_state)
            .expect("creating interface event stream"),
        &mut state,
        |fidl_fuchsia_net_interfaces_ext::Properties {
             id: _,
             name: _,
             device_class: _,
             online: _,
             addresses,
             has_default_ipv4_route: _,
             has_default_ipv6_route: _,
         }| {
            let ipv6_addresses = addresses
                .iter()
                .filter_map(|fidl_fuchsia_net_interfaces_ext::Address { addr, valid_until: _ }| {
                    match addr.addr {
                        net::IpAddress::Ipv6(net::Ipv6Address { .. }) => Some(addr),
                        net::IpAddress::Ipv4(net::Ipv4Address { .. }) => None,
                    }
                })
                .copied()
                .collect::<Vec<_>>();
            if ipv6_addresses.is_empty() {
                None
            } else {
                Some(ipv6_addresses)
            }
        },
    )
    .await
    .expect("failed to observe interface addition");

    ipv6_addresses
}

/// Test that across netstack runs, a device will initially be assigned the same
/// IPv6 addresses.
#[netstack_test]
async fn consistent_initial_ipv6_addrs<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox
        .create_realm(
            name,
            &[
                // This test exercises stash persistence. Netstack-debug, which
                // is the default used by test helpers, does not use
                // persistence.
                KnownServiceProvider::Netstack(match N::VERSION {
                    NetstackVersion::Netstack2 => NetstackVersion::ProdNetstack2,
                    NetstackVersion::Netstack3 => NetstackVersion::Netstack3,
                    v @ (NetstackVersion::Netstack2WithFastUdp
                    | NetstackVersion::ProdNetstack2
                    | NetstackVersion::ProdNetstack3) => {
                        panic!("netstack_test should only ever be parameterized with Netstack2 or Netstack3: got {:?}", v)
                    }
                }),
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("failed to create realm");
    let endpoint = sandbox.create_endpoint(name).await.expect("failed to create endpoint");
    let () = endpoint.set_link_up(true).await.expect("failed to set link up");

    // Make sure netstack uses the same addresses across runs for a device.
    let first_run_addrs =
        install_and_get_ipv6_addrs_for_endpoint::<N>(&realm, &endpoint, name).await;

    // Stop the netstack.
    let () = realm
        .stop_child_component(constants::netstack::COMPONENT_NAME)
        .await
        .expect("failed to stop netstack");

    let second_run_addrs =
        install_and_get_ipv6_addrs_for_endpoint::<N>(&realm, &endpoint, name).await;
    assert_eq!(first_run_addrs, second_run_addrs);
}

/// Enables IPv6 forwarding configuration.
async fn enable_ipv6_forwarding(iface: &netemul::TestInterface<'_>) {
    let config_with_ipv6_forwarding_set = |forwarding| fnet_interfaces_admin::Configuration {
        ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
            forwarding: Some(forwarding),
            ..Default::default()
        }),
        ..Default::default()
    };

    let configuration = iface
        .control()
        .set_configuration(config_with_ipv6_forwarding_set(true))
        .await
        .expect("set_configuration FIDL error")
        .expect("error setting configuration");

    assert_eq!(configuration, config_with_ipv6_forwarding_set(false))
}

/// Tests that `EXPECTED_ROUTER_SOLICIATIONS` Router Solicitation messages are transmitted
/// when the interface is brought up.
#[netstack_test]
#[test_case("host", false ; "host")]
#[test_case("router", true ; "router")]
async fn sends_router_solicitations<N: Netstack>(
    test_name: &str,
    sub_test_name: &str,
    forwarding: bool,
) {
    let name = format!("{}_{}", test_name, sub_test_name);
    let name = name.as_str();

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, _realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up network");

    if forwarding {
        enable_ipv6_forwarding(&iface).await;
    }

    // Make sure exactly `EXPECTED_ROUTER_SOLICIATIONS` RS messages are transmitted
    // by the netstack.
    let mut observed_rs = 0;
    loop {
        // When we have already observed the expected number of RS messages, do a
        // negative check to make sure that we don't send anymore.
        let extra_timeout = if observed_rs == EXPECTED_ROUTER_SOLICIATIONS {
            ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT
        } else {
            ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
        };

        let ret = fake_ep
            .frame_stream()
            .try_filter_map(|(data, dropped)| {
                assert_eq!(dropped, 0);
                let mut observed_slls = Vec::new();
                future::ok(
                    parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        net_types_ip::Ipv6,
                        _,
                        RouterSolicitation,
                        _,
                    >(&data, EthernetFrameLengthCheck::Check, |p| {
                        for option in p.body().iter() {
                            if let NdpOption::SourceLinkLayerAddress(a) = option {
                                let mut mac_bytes = [0; 6];
                                mac_bytes.copy_from_slice(&a[..size_of::<Mac>()]);
                                observed_slls.push(Mac::new(mac_bytes));
                            } else {
                                // We should only ever have an NDP Source Link-Layer Address
                                // option in a RS.
                                panic!("unexpected option in RS = {:?}", option);
                            }
                        }
                    })
                    .map_or(
                        None,
                        |(_src_mac, dst_mac, src_ip, dst_ip, ttl, _message, _code)| {
                            Some((dst_mac, src_ip, dst_ip, ttl, observed_slls))
                        },
                    ),
                )
            })
            .try_next()
            .map(|r| r.context("error getting OnData event"))
            .on_timeout((EXPECTED_ROUTER_SOLICITATION_INTERVAL + extra_timeout).after_now(), || {
                // If we already observed `EXPECTED_ROUTER_SOLICIATIONS` RS, then we shouldn't
                // have gotten any more; the timeout is expected.
                if observed_rs == EXPECTED_ROUTER_SOLICIATIONS {
                    return Ok(None);
                }

                return Err(anyhow::anyhow!("timed out waiting for the {}-th RS", observed_rs));
            })
            .await
            .unwrap();

        let (dst_mac, src_ip, dst_ip, ttl, observed_slls) = match ret {
            Some((dst_mac, src_ip, dst_ip, ttl, observed_slls)) => {
                (dst_mac, src_ip, dst_ip, ttl, observed_slls)
            }
            None => break,
        };

        assert_eq!(
            dst_mac,
            Mac::from(&net_types_ip::Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS)
        );

        // DAD should have resolved for the link local IPv6 address that is assigned to
        // the interface when it is first brought up. When a link local address is
        // assigned to the interface, it should be used for transmitted RS messages.
        if observed_rs > 0 {
            assert!(src_ip.is_specified())
        }

        assert_eq!(dst_ip, net_types_ip::Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS.get());

        assert_eq!(ttl, ndp::MESSAGE_TTL);

        // The Router Solicitation should only ever have at max 1 source
        // link-layer option.
        assert!(observed_slls.len() <= 1);
        let observed_sll = observed_slls.into_iter().nth(0);
        if src_ip.is_specified() {
            if observed_sll.is_none() {
                panic!("expected source-link-layer address option if RS has a specified source IP address");
            }
        } else if observed_sll.is_some() {
            panic!("unexpected source-link-layer address option for RS with unspecified source IP address");
        }

        observed_rs += 1;
    }

    assert_eq!(observed_rs, EXPECTED_ROUTER_SOLICIATIONS);
}

/// Tests that both stable and temporary SLAAC addresses are generated for a SLAAC prefix.
#[netstack_test]
#[test_case("host", false ; "host")]
#[test_case("router", true ; "router")]
async fn slaac_with_privacy_extensions<N: Netstack>(
    test_name: &str,
    sub_test_name: &str,
    forwarding: bool,
) {
    let name = format!("{}_{}", test_name, sub_test_name);
    let name = name.as_str();
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up network");

    if forwarding {
        enable_ipv6_forwarding(&iface).await;
    }

    // Wait for a Router Solicitation.
    //
    // The first RS should be sent immediately.
    let () = fake_ep
        .frame_stream()
        .try_filter_map(|(data, dropped)| {
            assert_eq!(dropped, 0);
            future::ok(
                parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                    net_types_ip::Ipv6,
                    _,
                    RouterSolicitation,
                    _,
                >(&data, EthernetFrameLengthCheck::Check, |_| {})
                .map_or(None, |_| Some(())),
            )
        })
        .try_next()
        .map(|r| r.context("error getting OnData event"))
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("timed out waiting for RS packet"))
        })
        .await
        .unwrap()
        .expect("failed to get next OnData event");

    // Send a Router Advertisement with information for a SLAAC prefix.
    let options = [NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
        ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
        false,                                /* on_link_flag */
        true,                                 /* autonomous_address_configuration_flag */
        99999,                                /* valid_lifetime */
        99999,                                /* preferred_lifetime */
        ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
    ))];
    send_ra_with_router_lifetime(&fake_ep, 0, &options, ipv6_consts::LINK_LOCAL_ADDR)
        .await
        .expect("failed to send router advertisement");

    // Wait for the SLAAC addresses to be generated.
    //
    // We expect two addresses for the SLAAC prefixes to be assigned to the NIC as the
    // netstack should generate both a stable and temporary SLAAC address.
    let interface_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");
    let expected_addrs = 2;
    fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interface_state)
            .expect("error getting interface state event stream"),
        &mut fidl_fuchsia_net_interfaces_ext::InterfaceState::Unknown(iface.id()),
        |fidl_fuchsia_net_interfaces_ext::Properties { addresses, .. }| {
            if addresses
                .iter()
                .filter_map(
                    |&fidl_fuchsia_net_interfaces_ext::Address {
                         addr: fidl_fuchsia_net::Subnet { addr, prefix_len: _ },
                         valid_until: _,
                     }| {
                        match addr {
                            net::IpAddress::Ipv4(net::Ipv4Address { .. }) => None,
                            net::IpAddress::Ipv6(net::Ipv6Address { addr }) => {
                                ipv6_consts::GLOBAL_PREFIX
                                    .contains(&net_types_ip::Ipv6Addr::from_bytes(addr))
                                    .then_some(())
                            }
                        }
                    },
                )
                .count()
                == expected_addrs as usize
            {
                Some(())
            } else {
                None
            }
        },
    )
    .map_err(anyhow::Error::from)
    .on_timeout(
        (EXPECTED_DAD_RETRANSMIT_TIMER * EXPECTED_DUP_ADDR_DETECT_TRANSMITS * expected_addrs
            + ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT)
            .after_now(),
        || Err(anyhow::anyhow!("timed out")),
    )
    .await
    .expect("failed to wait for SLAAC addresses to be generated")
}

/// Tests that if the netstack attempts to assign an address to an interface, and a remote node
/// is already assigned the address or attempts to assign the address at the same time, DAD
/// fails on the local interface.
///
/// If no remote node has any interest in an address the netstack is attempting to assign to
/// an interface, DAD should succeed.
#[netstack_test]
async fn duplicate_address_detection<N: Netstack>(name: &str) {
    /// Adds `ipv6_consts::LINK_LOCAL_ADDR` to the interface and makes sure a Neighbor Solicitation
    /// message is transmitted by the netstack for DAD.
    ///
    /// Calls `fail_dad_fn` after the DAD message is observed so callers can simulate a remote
    /// node that has some interest in the same address.
    async fn add_address_for_dad<
        'a,
        'b: 'a,
        R: 'b + Future<Output = ()>,
        FN: FnOnce(&'b netemul::TestFakeEndpoint<'a>) -> R,
    >(
        iface: &'b netemul::TestInterface<'a>,
        fake_ep: &'b netemul::TestFakeEndpoint<'a>,
        control: &'b fidl_fuchsia_net_interfaces_admin::ControlProxy,
        interface_up: bool,
        dad_fn: FN,
    ) -> impl futures::stream::Stream<Item = DadState> {
        let (address_state_provider, server) = fidl::endpoints::create_proxy::<
            fidl_fuchsia_net_interfaces_admin::AddressStateProviderMarker,
        >()
        .expect("create AddressStateProvider proxy");
        // Create the state stream before adding the address to observe all events.
        let state_stream =
            fidl_fuchsia_net_interfaces_ext::admin::assignment_state_stream(address_state_provider);

        // Note that DAD completes successfully after 1 second has elapsed if it
        // has not received a response to it's neighbor solicitation. This
        // introduces some inherent flakiness, as certain aspects of this test
        // (limited to this scope) MUST execute within this one second window
        // for the test to pass.
        let (get_addrs_fut, get_addrs_poll) = {
            let () = control
                .add_address(
                    &mut net::Subnet {
                        addr: net::IpAddress::Ipv6(net::Ipv6Address {
                            addr: ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes(),
                        }),
                        prefix_len: ipv6_consts::LINK_LOCAL_SUBNET_PREFIX,
                    },
                    &fidl_fuchsia_net_interfaces_admin::AddressParameters::default(),
                    server,
                )
                .expect("Control.AddAddress FIDL error");
            // `Box::pin` rather than `pin_mut!` allows `get_addr_fut` to be
            // moved out of this scope.
            let mut get_addrs_fut = Box::pin(iface.get_addrs());
            let get_addrs_poll = futures::poll!(&mut get_addrs_fut);
            if interface_up {
                expect_dad_neighbor_solicitation(fake_ep).await;
            }
            dad_fn(fake_ep).await;
            (get_addrs_fut, get_addrs_poll)
        }; // This marks the end of the time-sensitive operations.

        let addrs = match get_addrs_poll {
            std::task::Poll::Ready(addrs) => addrs,
            std::task::Poll::Pending => get_addrs_fut.await,
        };

        // Ensure that fuchsia.net.interfaces/Watcher doesn't erroneously report
        // the address as added before DAD completes successfully or otherwise.
        assert_eq!(
            addrs.expect("failed to get addresses").into_iter().find(
                |fidl_fuchsia_net_interfaces_ext::Address {
                     addr: fidl_fuchsia_net::Subnet { addr, prefix_len },
                     valid_until: _,
                 }| {
                    *prefix_len == ipv6_consts::LINK_LOCAL_SUBNET_PREFIX
                        && match addr {
                            fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address {
                                ..
                            }) => false,
                            fidl_fuchsia_net::IpAddress::Ipv6(fidl_fuchsia_net::Ipv6Address {
                                addr,
                            }) => *addr == ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes(),
                        }
                }
            ),
            None,
            "added IPv6 LL address already present even though it is tentative"
        );

        state_stream
    }

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up network");

    let debug_control = realm
        .connect_to_protocol::<fidl_fuchsia_net_debug::InterfacesMarker>()
        .expect("failed to connect to fuchsia.net.debug/Interfaces");
    let (control, server) =
        fidl::endpoints::create_proxy::<fidl_fuchsia_net_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    let () = debug_control
        .get_admin(iface.id(), server)
        .expect("fuchsia.net.debug/Interfaces.GetAdmin failed");

    // Add an address and expect it to fail DAD because we simulate another node
    // performing DAD at the same time.
    {
        let state_stream =
            add_address_for_dad(&iface, &fake_ep, &control, true, fail_dad_with_ns).await;
        assert_dad_failed(state_stream).await;
    }
    // Add an address and expect it to fail DAD because we simulate another node
    // already owning the address.
    {
        let state_stream =
            add_address_for_dad(&iface, &fake_ep, &control, true, fail_dad_with_na).await;
        assert_dad_failed(state_stream).await;
    }

    {
        // Add the address, and make sure it gets assigned.
        let mut state_stream =
            add_address_for_dad(&iface, &fake_ep, &control, true, |_| futures::future::ready(()))
                .await;

        assert_dad_success(&mut state_stream).await;

        // Disable the interface, ensure that the address becomes unavailable.
        let did_disable = iface.control().disable().await.expect("send disable").expect("disable");
        assert!(did_disable);

        assert_matches::assert_matches!(
            state_stream.by_ref().next().await,
            Some(Ok(fidl_fuchsia_net_interfaces_admin::AddressAssignmentState::Unavailable))
        );

        // Re-enable the interface, expect DAD to repeat and have it succeed.
        assert!(iface.control().enable().await.expect("send enable").expect("enable"));
        expect_dad_neighbor_solicitation(&fake_ep).await;
        assert_dad_success(&mut state_stream).await;

        let removed = control
            .remove_address(&mut net::Subnet {
                addr: net::IpAddress::Ipv6(net::Ipv6Address {
                    addr: ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes(),
                }),
                prefix_len: ipv6_consts::LINK_LOCAL_SUBNET_PREFIX,
            })
            .await
            .expect("FIDL error removing address")
            .expect("failed to remove address");
        assert!(removed);
    }

    // Disable the interface, this time add the address while it's down.
    let did_disable = iface.control().disable().await.expect("send disable").expect("disable");
    assert!(did_disable);
    let mut state_stream =
        add_address_for_dad(&iface, &fake_ep, &control, false, |_| futures::future::ready(()))
            .await;

    assert_matches::assert_matches!(
        state_stream.by_ref().next().await,
        Some(Ok(fidl_fuchsia_net_interfaces_admin::AddressAssignmentState::Unavailable))
    );

    // Re-enable the interface, DAD should run.
    let did_enable = iface.control().enable().await.expect("send enable").expect("enable");
    assert!(did_enable);

    expect_dad_neighbor_solicitation(&fake_ep).await;

    assert_dad_success(&mut state_stream).await;

    let addresses = iface.get_addrs().await.expect("addrs");
    assert!(
        addresses.iter().any(
            |&fidl_fuchsia_net_interfaces_ext::Address {
                 addr: fidl_fuchsia_net::Subnet { addr, prefix_len: _ },
                 valid_until: _,
             }| {
                match addr {
                    net::IpAddress::Ipv4(net::Ipv4Address { .. }) => false,
                    net::IpAddress::Ipv6(net::Ipv6Address { addr }) => {
                        addr == ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes()
                    }
                }
            }
        ),
        "addresses: {:?}",
        addresses
    );
}

/// Tests to make sure default router discovery, prefix discovery and more-specific
/// route discovery works.
#[netstack_test]
#[test_case("host", false ; "host")]
#[test_case("router", true ; "router")]
async fn on_and_off_link_route_discovery<N: Netstack>(
    test_name: &str,
    sub_test_name: &str,
    forwarding: bool,
) {
    pub const SUBNET_WITH_MORE_SPECIFIC_ROUTE: net_types_ip::Subnet<net_types_ip::Ipv6Addr> = unsafe {
        net_types_ip::Subnet::new_unchecked(
            net_types_ip::Ipv6Addr::new([0xa001, 0xf1f0, 0x4060, 0x0001, 0, 0, 0, 0]),
            64,
        )
    };

    async fn check_route_table(
        realm: &netemul::TestRealm<'_>,
        want_routes: &[fnet_routes_ext::InstalledRoute<Ipv6>],
    ) {
        let ipv6_route_stream = {
            let state_v6 = realm
                .connect_to_protocol::<fnet_routes::StateV6Marker>()
                .expect("connect to protocol");
            fnet_routes_ext::event_stream_from_state::<Ipv6>(&state_v6)
                .expect("failed to connect to watcher")
        };
        futures::pin_mut!(ipv6_route_stream);
        let mut routes = HashSet::new();
        fnet_routes_ext::wait_for_routes(ipv6_route_stream, &mut routes, |accumulated_routes| {
            want_routes.iter().all(|route| accumulated_routes.contains(route))
        })
        .on_timeout(fuchsia_async::Time::after(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT), || {
            panic!(
                "timed out on waiting for a route table entry after {} seconds",
                ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.into_seconds()
            )
        })
        .await
        .expect("error while waiting for routes to satisfy predicate");
    }

    let name = format!("{}_{}", test_name, sub_test_name);
    let name = name.as_str();

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    const METRIC: u32 = 200;
    let (_network, realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, Some(METRIC)).await.expect("failed to setup network");

    if forwarding {
        enable_ipv6_forwarding(&iface).await;
    }

    let options = [
        NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
            ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
            true,                                 /* on_link_flag */
            false,                                /* autonomous_address_configuration_flag */
            6234,                                 /* valid_lifetime */
            0,                                    /* preferred_lifetime */
            ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
        )),
        NdpOptionBuilder::RouteInformation(RouteInformation::new(
            SUBNET_WITH_MORE_SPECIFIC_ROUTE,
            1337, /* route_lifetime_seconds */
            RoutePreference::default(),
        )),
    ];
    let () = send_ra_with_router_lifetime(&fake_ep, 1234, &options, ipv6_consts::LINK_LOCAL_ADDR)
        .await
        .expect("failed to send router advertisement");

    let nicid = iface.id();
    check_route_table(
        &realm,
        &[
            // Test that a default route through the router is installed.
            fnet_routes_ext::InstalledRoute {
                route: fnet_routes_ext::Route {
                    destination: net_types::ip::Subnet::new(
                        net_types::ip::Ipv6::UNSPECIFIED_ADDRESS,
                        0,
                    )
                    .unwrap(),
                    action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                        outbound_interface: nicid,
                        next_hop: Some(net_types::SpecifiedAddr::new(ipv6_consts::LINK_LOCAL_ADDR))
                            .unwrap(),
                    }),
                    properties: fnet_routes_ext::RouteProperties {
                        specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                            metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(
                                fnet_routes::Empty,
                            ),
                        },
                    },
                },
                effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: METRIC },
            },
            // Test that a route to `SUBNET_WITH_MORE_SPECIFIC_ROUTE` exists
            // through the router.
            fnet_routes_ext::InstalledRoute {
                route: fnet_routes_ext::Route {
                    destination: SUBNET_WITH_MORE_SPECIFIC_ROUTE,
                    action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                        outbound_interface: nicid,
                        next_hop: Some(net_types::SpecifiedAddr::new(ipv6_consts::LINK_LOCAL_ADDR))
                            .unwrap(),
                    }),
                    properties: fnet_routes_ext::RouteProperties {
                        specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                            metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(
                                fnet_routes::Empty,
                            ),
                        },
                    },
                },
                effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: METRIC },
            },
            // Test that the prefix should be discovered after it is advertised.
            fnet_routes_ext::InstalledRoute::<Ipv6> {
                route: fnet_routes_ext::Route {
                    destination: ipv6_consts::GLOBAL_PREFIX,
                    action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                        outbound_interface: nicid,
                        next_hop: None,
                    }),
                    properties: fnet_routes_ext::RouteProperties {
                        specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                            metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(
                                fnet_routes::Empty,
                            ),
                        },
                    },
                },
                effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric: METRIC },
            },
        ][..],
    )
    .await
}

#[netstack_test]
async fn slaac_regeneration_after_dad_failure<N: Netstack>(name: &str) {
    // Expects an NS message for DAD within timeout and returns the target address of the message.
    async fn expect_ns_message_in(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        timeout: zx::Duration,
    ) -> net_types_ip::Ipv6Addr {
        fake_ep
            .frame_stream()
            .try_filter_map(|(data, dropped)| {
                assert_eq!(dropped, 0);
                future::ok(
                    parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        net_types_ip::Ipv6,
                        _,
                        NeighborSolicitation,
                        _,
                    >(&data, EthernetFrameLengthCheck::Check, |p| assert_eq!(p.body().iter().count(), 0))
                        .map_or(None, |(_src_mac, _dst_mac, _src_ip, _dst_ip, _ttl, message, _code)| {
                            // If the NS target_address does not have the prefix we have advertised,
                            // this is for some other address. We ignore it as it is not relevant to
                            // our test.
                            if !ipv6_consts::GLOBAL_PREFIX.contains(message.target_address()) {
                                return None;
                            }

                            Some(*message.target_address())
                        }),
                )
            })
            .try_next()
            .map(|r| r.context("error getting OnData event"))
            .on_timeout(timeout.after_now(), || {
                Err(anyhow::anyhow!(
                    "timed out waiting for a neighbor solicitation targetting address of prefix: {}",
                    ipv6_consts::GLOBAL_PREFIX,
                ))
            })
            .await.unwrap().expect("failed to get next OnData event")
    }

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, realm, iface, fake_ep) =
        setup_network_with::<N, _>(&sandbox, name, None, &[KnownServiceProvider::SecureStash])
            .await
            .expect("error setting up network");

    // Send a Router Advertisement with information for a SLAAC prefix.
    let options = [NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
        ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
        false,                                /* on_link_flag */
        true,                                 /* autonomous_address_configuration_flag */
        99999,                                /* valid_lifetime */
        99999,                                /* preferred_lifetime */
        ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
    ))];
    send_ra_with_router_lifetime(&fake_ep, 0, &options, ipv6_consts::LINK_LOCAL_ADDR)
        .await
        .expect("failed to send router advertisement");

    let tried_address = expect_ns_message_in(&fake_ep, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT).await;

    // We pretend there is a duplicate address situation.
    let snmc = tried_address.to_solicited_node_address();
    let () = ndp::write_message::<&[u8], _>(
        eth_consts::MAC_ADDR,
        Mac::from(&snmc),
        net_types_ip::Ipv6::UNSPECIFIED_ADDRESS,
        snmc.get(),
        NeighborSolicitation::new(tried_address),
        &[],
        &fake_ep,
    )
    .await
    .expect("failed to write DAD message");

    let target_address =
        expect_ns_message_in(&fake_ep, DAD_IDGEN_DELAY + ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT).await;

    // We expect two addresses for the SLAAC prefixes to be assigned to the NIC as the
    // netstack should generate both a stable and temporary SLAAC address.
    let expected_addrs = 2;
    let interface_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");
    let () = fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interface_state)
            .expect("error getting interfaces state event stream"),
        &mut fidl_fuchsia_net_interfaces_ext::InterfaceState::Unknown(iface.id()),
        |fidl_fuchsia_net_interfaces_ext::Properties { addresses, .. }| {
            // We have to make sure 2 things:
            // 1. We have `expected_addrs` addrs which have the advertised prefix for the
            // interface.
            // 2. The last tried address should be among the addresses for the interface.
            let (slaac_addrs, has_target_addr) = addresses.iter().fold(
                (0, false),
                |(mut slaac_addrs, mut has_target_addr),
                 &fidl_fuchsia_net_interfaces_ext::Address {
                     addr: fidl_fuchsia_net::Subnet { addr, prefix_len: _ },
                     valid_until: _,
                 }| {
                    match addr {
                        net::IpAddress::Ipv4(net::Ipv4Address { .. }) => {}
                        net::IpAddress::Ipv6(net::Ipv6Address { addr }) => {
                            let configured_addr = net_types_ip::Ipv6Addr::from_bytes(addr);
                            assert_ne!(
                                configured_addr, tried_address,
                                "address which previously failed DAD was assigned"
                            );
                            if ipv6_consts::GLOBAL_PREFIX.contains(&configured_addr) {
                                slaac_addrs += 1;
                            }
                            if configured_addr == target_address {
                                has_target_addr = true;
                            }
                        }
                    }
                    (slaac_addrs, has_target_addr)
                },
            );

            assert!(
                slaac_addrs <= expected_addrs,
                "more addresses found than expected, found {}, expected {}",
                slaac_addrs,
                expected_addrs
            );
            if slaac_addrs == expected_addrs && has_target_addr {
                Some(())
            } else {
                None
            }
        },
    )
    .map_err(anyhow::Error::from)
    .on_timeout(
        (EXPECTED_DAD_RETRANSMIT_TIMER * EXPECTED_DUP_ADDR_DETECT_TRANSMITS * expected_addrs
            + ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT)
            .after_now(),
        || Err(anyhow::anyhow!("timed out")),
    )
    .await
    .expect("failed to wait for SLAAC addresses");
}

fn check_mldv2_report(
    dst_ip: net_types_ip::Ipv6Addr,
    mld: MldPacket<&[u8]>,
    group: MulticastAddr<net_types_ip::Ipv6Addr>,
) -> bool {
    // Ignore non-report messages.
    let MldPacket::MulticastListenerReportV2(report) = mld else { return false };

    let has_snmc_record = report.body().iter_multicast_records().any(|record| {
        assert_eq!(record.sources(), &[]);
        let hdr = record.header();
        *hdr.multicast_addr() == group.get()
            && hdr.record_type() == Ok(Mldv2MulticastRecordType::ChangeToExcludeMode)
    });

    assert_eq!(
        dst_ip,
        net_ip_v6!("ff02::16"),
        "MLDv2 reports should should be sent to the MLDv2 routers address"
    );

    has_snmc_record
}

fn check_mldv1_report(
    dst_ip: net_types_ip::Ipv6Addr,
    mld: MldPacket<&[u8]>,
    expected_group: MulticastAddr<net_types_ip::Ipv6Addr>,
) -> bool {
    // Ignore non-report messages.
    let MldPacket::MulticastListenerReport(report) = mld else { return false };

    let group_addr = report.body().group_addr;
    assert!(
        group_addr.is_multicast(),
        "MLD reports must only be sent for multicast addresses; group_addr = {}",
        group_addr
    );
    if group_addr != expected_group.get() {
        return false;
    }

    assert_eq!(
        dst_ip, group_addr,
        "the destination of an MLD report should be the multicast group the report is for",
    );

    true
}

fn check_mld_report(
    mld_version: Option<fnet_interfaces_admin::MldVersion>,
    netstack_version: NetstackVersion,
    dst_ip: net_types_ip::Ipv6Addr,
    mld: MldPacket<&[u8]>,
    expected_group: MulticastAddr<net_types_ip::Ipv6Addr>,
) -> bool {
    match mld_version {
        Some(version) => match version {
            fnet_interfaces_admin::MldVersion::V1 => {
                check_mldv1_report(dst_ip, mld, expected_group)
            }
            fnet_interfaces_admin::MldVersion::V2 => {
                check_mldv2_report(dst_ip, mld, expected_group)
            }
            _ => panic!("unknown MLD version {:?}", version),
        },
        None => match netstack_version {
            NetstackVersion::Netstack2 => check_mldv2_report(dst_ip, mld, expected_group),
            NetstackVersion::Netstack3 => check_mldv1_report(dst_ip, mld, expected_group),
            v @ (NetstackVersion::Netstack2WithFastUdp
            | NetstackVersion::ProdNetstack2
            | NetstackVersion::ProdNetstack3) => {
                panic!("netstack_test should only be parameterized with Netstack2 or Netstack3: got {:?}", v);
            }
        },
    }
}

#[netstack_test]
#[test_case(Some(fnet_interfaces_admin::MldVersion::V1); "mldv1")]
#[test_case(Some(fnet_interfaces_admin::MldVersion::V2); "mldv2")]
#[test_case(None; "default")]
async fn sends_mld_reports<N: Netstack>(
    name: &str,
    mld_version: Option<fnet_interfaces_admin::MldVersion>,
) {
    let sandbox = netemul::TestSandbox::new().expect("error creating sandbox");
    let (_network, _realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up networking");

    if let Some(mld_version) = mld_version {
        let gen_config = |mld_version| fnet_interfaces_admin::Configuration {
            ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                mld: Some(fnet_interfaces_admin::MldConfiguration {
                    version: Some(mld_version),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let control = iface.control();
        let new_config = gen_config(mld_version);
        let old_config = gen_config(fnet_interfaces_admin::MldVersion::V2);
        assert_eq!(
            control
                .set_configuration(new_config.clone())
                .await
                .expect("set_configuration fidl error")
                .expect("failed to set interface configuration"),
            old_config,
        );
        // Set configuration again to check the returned value is the new config
        // to show that nothing actually changed.
        assert_eq!(
            control
                .set_configuration(new_config.clone())
                .await
                .expect("set_configuration fidl error")
                .expect("failed to set interface configuration"),
            new_config,
        );
        assert_matches::assert_matches!(
            control
                .get_configuration()
                .await
                .expect("get_configuration fidl error")
                .expect("failed to get interface configuration"),
            fnet_interfaces_admin::Configuration {
                ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                    mld: Some(fnet_interfaces_admin::MldConfiguration {
                        version: Some(got),
                        ..
                    }),
                    ..
                }),
                ..
            } => assert_eq!(got, mld_version)
        );
    }
    let _address_state_provider = {
        let subnet = net::Subnet {
            addr: net::IpAddress::Ipv6(net::Ipv6Address {
                addr: ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes(),
            }),
            prefix_len: 64,
        };

        // While Netstack3 installs a link-local subnet route when an interface is
        // added, Netstack2 installs it only when an interface is enabled. Add the
        // forwarding entry for Netstack2 only to compensate for this behavioral difference.
        // TODO(https://fxbug.dev/123440): Unify behavior for adding a link-local
        // subnet route between NS2/NS3.
        if N::VERSION == NetstackVersion::Netstack3 {
            interfaces::add_address_wait_assigned(
                &iface.control(),
                subnet,
                fidl_fuchsia_net_interfaces_admin::AddressParameters::default(),
            )
            .await
            .expect("add_address_wait_assigned failed")
        } else {
            interfaces::add_subnet_address_and_route_wait_assigned(
                &iface,
                subnet,
                fidl_fuchsia_net_interfaces_admin::AddressParameters::default(),
            )
            .await
            .expect("add subnet address and route")
        }
    };
    let snmc = ipv6_consts::LINK_LOCAL_ADDR.to_solicited_node_address();

    let stream = fake_ep
        .frame_stream()
        .map(|r| r.context("error getting OnData event"))
        .try_filter_map(|(data, dropped)| {
            async move {
                assert_eq!(dropped, 0);
                let mut data = &data[..];

                let eth = EthernetFrame::parse(&mut data, EthernetFrameLengthCheck::NoCheck)
                    .expect("error parsing ethernet frame");

                if eth.ethertype() != Some(EtherType::Ipv6) {
                    // Ignore non-IPv6 packets.
                    return Ok(None);
                }

                let (mut payload, src_ip, dst_ip, proto, ttl) =
                    parse_ip_packet::<net_types_ip::Ipv6>(&data)
                        .expect("error parsing IPv6 packet");

                if proto != Ipv6Proto::Icmpv6 {
                    // Ignore non-ICMPv6 packets.
                    return Ok(None);
                }

                let icmp = Icmpv6Packet::parse(&mut payload, IcmpParseArgs::new(src_ip, dst_ip))
                    .expect("error parsing ICMPv6 packet");

                let mld = if let Icmpv6Packet::Mld(mld) = icmp {
                    mld
                } else {
                    // Ignore non-MLD packets.
                    return Ok(None);
                };

                // As per RFC 3590 section 4,
                //
                //   MLD Report and Done messages are sent with a link-local address as
                //   the IPv6 source address, if a valid address is available on the
                //   interface. If a valid link-local address is not available (e.g., one
                //   has not been configured), the message is sent with the unspecified
                //   address (::) as the IPv6 source address.
                assert!(!src_ip.is_specified() || src_ip.is_link_local(), "MLD messages must be sent from the unspecified or link local address; src_ip = {}", src_ip);


                // As per RFC 2710 section 3,
                //
                //   All MLD messages described in this document are sent with a
                //   link-local IPv6 Source Address, an IPv6 Hop Limit of 1, ...
                assert_eq!(ttl, 1, "MLD messages must have a hop limit of 1");

                Ok(check_mld_report(mld_version, N::VERSION, dst_ip, mld, snmc).then_some(()))
            }
        });
    futures::pin_mut!(stream);
    let () = stream
        .try_next()
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            return Err(anyhow::anyhow!("timed out waiting for the MLD report"));
        })
        .await
        .unwrap()
        .expect("error getting our expected MLD report");
}

#[netstack_test]
async fn sending_ra_with_autoconf_flag_triggers_slaac<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("error creating sandbox");
    let (network, realm, iface, _fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up networking");

    let interfaces_state = &realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to protocol");

    // Wait for the netstack to be up before sending the RA.
    let iface_ip: net_types_ip::Ipv6Addr =
        netstack_testing_common::interfaces::wait_for_v6_ll(&interfaces_state, iface.id())
            .await
            .expect("waiting for link local address");

    // Pick a source address that is not the same as the interface's address by
    // flipping the low bytes.
    let src_ip = net_types_ip::Ipv6Addr::from_bytes(
        iface_ip
            .ipv6_bytes()
            .into_iter()
            .enumerate()
            .map(|(i, b)| if i < 8 { b } else { !b })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
    );

    let fake_router = network.create_fake_endpoint().expect("endpoint created");

    let options = [NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
        ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
        true,                                 /* on_link_flag */
        true,                                 /* autonomous_address_configuration_flag */
        6234,                                 /* valid_lifetime */
        0,                                    /* preferred_lifetime */
        ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
    ))];
    let ra = RouterAdvertisement::new(
        0,     /* current_hop_limit */
        false, /* managed_flag */
        false, /* other_config_flag */
        1234,  /* router_lifetime */
        0,     /* reachable_time */
        0,     /* retransmit_timer */
    );
    send_ra(&fake_router, ra, &options, src_ip).await.expect("RA sent");

    fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interfaces_state)
            .expect("creating interface event stream"),
        &mut fidl_fuchsia_net_interfaces_ext::InterfaceState::Unknown(iface.id()),
        |fidl_fuchsia_net_interfaces_ext::Properties {
             id: _,
             name: _,
             device_class: _,
             online: _,
             addresses,
             has_default_ipv4_route: _,
             has_default_ipv6_route: _,
         }| {
            addresses.into_iter().find_map(
                |fidl_fuchsia_net_interfaces_ext::Address {
                     addr: fidl_fuchsia_net::Subnet { addr, prefix_len: _ },
                     valid_until: _,
                 }| {
                    let addr = match addr {
                        fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address {
                            ..
                        }) => {
                            return None;
                        }
                        fidl_fuchsia_net::IpAddress::Ipv6(fidl_fuchsia_net::Ipv6Address {
                            addr,
                        }) => net_types_ip::Ipv6Addr::from_bytes(*addr),
                    };
                    ipv6_consts::GLOBAL_PREFIX.contains(&addr).then(|| ())
                },
            )
        },
    )
    .await
    .expect("error waiting for address assignment");
}

#[netstack_test]
async fn add_device_adds_link_local_subnet_route<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let endpoint = sandbox.create_endpoint(name).await.expect("create endpoint");
    endpoint.set_link_up(true).await.expect("set link up");
    let iface = endpoint.into_interface_in_realm(&realm).await.expect("install interface");
    let did_enable = iface.control().enable().await.expect("calling enable").expect("enable");
    assert!(did_enable);

    let id = iface.id();

    // Connect to the routes watcher and filter out all routing events unrelated
    // to the link-local subnet route.
    let ipv6_route_stream = {
        let state_v6 =
            realm.connect_to_protocol::<fnet_routes::StateV6Marker>().expect("connect to protocol");
        fnet_routes_ext::event_stream_from_state::<Ipv6>(&state_v6)
            .expect("failed to connect to watcher")
    };

    let link_local_subnet_route_events = ipv6_route_stream.filter_map(|event| {
        let event = event.expect("error in stream");
        futures::future::ready(match &event {
            fnet_routes_ext::Event::Existing(route)
            | fnet_routes_ext::Event::Added(route)
            | fnet_routes_ext::Event::Removed(route) => {
                let fnet_routes_ext::InstalledRoute {
                    route: fnet_routes_ext::Route { destination, action, properties: _ },
                    effective_properties: _,
                } = route;
                let (outbound_interface, next_hop) = match action {
                    fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                        outbound_interface,
                        next_hop,
                    }) => (outbound_interface, next_hop),
                    fnet_routes_ext::RouteAction::Unknown => {
                        panic!("observed route with unknown action")
                    }
                };
                (*outbound_interface == id
                    && next_hop.is_none()
                    && *destination == net_declare::net_subnet_v6!("fe80::/64"))
                .then(|| event)
            }
            fnet_routes_ext::Event::Idle => None,
            fnet_routes_ext::Event::Unknown => {
                panic!("observed unknown event in stream");
            }
        })
    });
    futures::pin_mut!(link_local_subnet_route_events);

    // Verify the link local subnet route is added.
    assert_matches!(
        link_local_subnet_route_events.next().await.expect("stream unexpectedly ended"),
        fnet_routes_ext::Event::Existing(_) | fnet_routes_ext::Event::Added(_)
    );

    // Removing the device should also remove the subnet route.
    drop(iface);
    assert_matches!(
        link_local_subnet_route_events.next().await.expect("stream unexpectedly ended"),
        fnet_routes_ext::Event::Removed(_)
    );
}
