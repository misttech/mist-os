// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_neighbor as fnet_neighbor;
use fidl_fuchsia_net_reachability as fnet_reachability;
use fidl_fuchsia_net_stack as fnet_stack;
use fidl_fuchsia_testing as ftesting;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;

use assert_matches::assert_matches;
use either::Either;
use fidl::endpoints::Proxy;
use futures::{
    stream::FusedStream, Future, FutureExt as _, Stream, StreamExt as _, TryFutureExt as _,
};
use net_declare::{fidl_subnet, net_ip_v4, net_ip_v6, net_mac};
use netstack_testing_common::{
    constants::{ipv4 as ipv4_consts, ipv6 as ipv6_consts},
    get_inspect_data, interfaces,
    realms::{constants, KnownServiceProvider, Netstack2, TestSandboxExt as _},
    wait_for_component_stopped,
};
use netstack_testing_macros::netstack_test;
use packet::{Buf, InnerPacketBuilder as _, Serializer as _};
use packet_formats::{
    ethernet::{EtherType, EthernetFrameBuilder},
    icmp::{IcmpEchoRequest, IcmpPacketBuilder, IcmpUnusedCode, MessageBody as _},
    ip::{Ipv4Proto, Ipv6Proto},
    ipv4::Ipv4PacketBuilder,
    ipv6::Ipv6PacketBuilder,
    testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
};
use reachability_core::{State, FIDL_TIMEOUT_ID};
use std::{cell::RefCell, collections::HashMap};
use test_case::test_case;
use tracing::info;

const INTERNET_V4: net_types::ip::Ipv4Addr = net_ip_v4!("8.8.8.8");
const INTERNET_V6: net_types::ip::Ipv6Addr = net_ip_v6!("2001:4860:4860::8888");

const PREFIX_V4: u8 = 24;
const PREFIX_V6: u8 = 64;

const LOWER_METRIC: u32 = 0;
const HIGHER_METRIC: u32 = 100;

#[derive(Debug, Clone)]
struct InterfaceConfig {
    name_suffix: &'static str,
    gateway_v4: net_types::ip::Ipv4Addr,
    addr_v4: net_types::ip::Ipv4Addr,
    gateway_v6: net_types::ip::Ipv6Addr,
    addr_v6: net_types::ip::Ipv6Addr,
    gateway_mac: net_types::ethernet::Mac,
    metric: u32,
}

impl InterfaceConfig {
    const fn new_primary(metric: u32) -> Self {
        Self {
            name_suffix: "primary",
            gateway_v4: net_ip_v4!("192.168.0.1"),
            addr_v4: net_ip_v4!("192.168.0.2"),
            gateway_v6: net_ip_v6!("fe80::1"),
            addr_v6: net_ip_v6!("fe80::2"),
            gateway_mac: net_mac!("02:00:00:00:00:01"),
            metric,
        }
    }

    const fn new_secondary(metric: u32) -> Self {
        Self {
            name_suffix: "secondary",
            gateway_v4: net_ip_v4!("192.168.1.1"),
            addr_v4: net_ip_v4!("192.168.1.2"),
            gateway_v6: net_ip_v6!("fe81::1"),
            addr_v6: net_ip_v6!("fe81::2"),
            gateway_mac: net_mac!("03:00:00:00:00:01"),
            metric,
        }
    }
}

/// Try to parse `frame` as an ICMP or ICMPv6 Echo Request message, and if successful returns the
/// Echo Reply message that the netstack would expect as a reply.
fn reply_if_echo_request(
    frame: Vec<u8>,
    want: State,
    gateway_v4: &net_types::ip::Ipv4Addr,
    gateway_v6: &net_types::ip::Ipv6Addr,
) -> Option<Buf<Vec<u8>>> {
    let mut icmp_body = Vec::new();
    let r = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
        net_types::ip::Ipv4,
        _,
        IcmpEchoRequest,
        _,
    >(&frame, |p| {
        icmp_body.extend(p.body().bytes());
    });
    match r {
        Ok((src_mac, dst_mac, src_ip, dst_ip, _ttl, message, _code)) => {
            return match want {
                State::Gateway => dst_ip == *gateway_v4,
                State::Internet => dst_ip == *gateway_v4 || dst_ip == INTERNET_V4,
                _ => false,
            }
            .then(|| {
                icmp_body
                    .into_serializer()
                    .encapsulate(IcmpPacketBuilder::<net_types::ip::Ipv4, &[u8], _>::new(
                        dst_ip,
                        src_ip,
                        IcmpUnusedCode,
                        message.reply(),
                    ))
                    .encapsulate(Ipv4PacketBuilder::new(
                        dst_ip,
                        src_ip,
                        ipv4_consts::DEFAULT_TTL,
                        Ipv4Proto::Icmp,
                    ))
                    .encapsulate(EthernetFrameBuilder::new(dst_mac, src_mac, EtherType::Ipv4))
                    .serialize_vec_outer()
                    .expect("failed to serialize ICMPv4 packet")
                    .unwrap_b()
            });
        }
        Err(packet_formats::error::IpParseError::Parse {
            error: packet_formats::error::ParseError::NotExpected,
        }) => {}
        Err(e) => {
            panic!("parse packet as ICMPv4 error: {}", e);
        }
    }

    let mut icmp_body = Vec::new();
    let r = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
        net_types::ip::Ipv6,
        _,
        IcmpEchoRequest,
        _,
    >(&frame, |p| {
        icmp_body.extend(p.body().bytes());
    });
    match r {
        Ok((src_mac, dst_mac, src_ip, dst_ip, _ttl, message, _code)) => {
            return match want {
                State::Gateway => dst_ip == *gateway_v6,
                State::Internet => dst_ip == *gateway_v6 || dst_ip == INTERNET_V6,
                _ => false,
            }
            .then(|| {
                icmp_body
                    .into_serializer()
                    .encapsulate(IcmpPacketBuilder::<net_types::ip::Ipv6, &[u8], _>::new(
                        dst_ip,
                        src_ip,
                        IcmpUnusedCode,
                        message.reply(),
                    ))
                    .encapsulate(Ipv6PacketBuilder::new(
                        dst_ip,
                        src_ip,
                        ipv6_consts::DEFAULT_HOP_LIMIT,
                        Ipv6Proto::Icmpv6,
                    ))
                    .encapsulate(EthernetFrameBuilder::new(dst_mac, src_mac, EtherType::Ipv6))
                    .serialize_vec_outer()
                    .expect("failed to serialize ICMPv6 packet")
                    .unwrap_b()
            })
        }
        Err(packet_formats::error::IpParseError::Parse {
            error: packet_formats::error::ParseError::NotExpected,
        }) => {}
        Err(e) => {
            panic!("parse packet as ICMPv6 error: {}", e);
        }
    }
    None
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
struct IpStates {
    ipv4: State,
    ipv6: State,
}

impl IpStates {
    fn new(state: State) -> Self {
        Self { ipv4: state, ipv6: state }
    }
}

/// Extract the most recent reachability states for IPv4 and IPv6 from the inspect data.
fn extract_reachability_states(
    data: &diagnostics_hierarchy::DiagnosticsHierarchy,
) -> (HashMap<u64, IpStates>, IpStates) {
    let get_per_ip_state = |states: &diagnostics_hierarchy::DiagnosticsHierarchy| {
        let (_, state): (i64, _) = states.children.iter().fold(
            (-1, State::None),
            |(latest_seqnum, latest_state), state| {
                let seqnum = state
                    .name
                    .parse::<i64>()
                    .expect("failed to parse reachability state sequence number as integer");
                // NB: Since node creation is not atomic, it's possible to read a node with a
                // sequence number but no state value, so must ensure that state is `Some` before
                // using it as the latest state.
                match state.properties.iter().find_map(|p| {
                    if p.key() == "state" {
                        p.string().map(|s| {
                            s.parse::<State>().unwrap_or_else(|e| {
                                panic!("failed to parse reachability state: {}: {:?}", s, e)
                            })
                        })
                    } else {
                        None
                    }
                }) {
                    Some(state) if seqnum > latest_seqnum => (seqnum, state),
                    Some(_) | None => (latest_seqnum, latest_state),
                }
            },
        );
        state
    };
    let get_state = |data: &diagnostics_hierarchy::DiagnosticsHierarchy| {
        data.children.iter().fold(IpStates::default(), |IpStates { ipv4, ipv6 }, info| {
            if info.name == "IPv4" {
                IpStates { ipv4: get_per_ip_state(info), ipv6 }
            } else if info.name == "IPv6" {
                IpStates { ipv4, ipv6: get_per_ip_state(info) }
            } else {
                IpStates { ipv4, ipv6 }
            }
        })
    };
    data.children.iter().fold(
        (HashMap::new(), IpStates::default()),
        |(mut interfaces, mut system), data| {
            if data.name == "system" {
                system = get_state(data);
            } else {
                match data.name.parse::<u64>() {
                    Ok(id) => match interfaces.insert(id, get_state(data)) {
                        Some(state) => {
                            panic!(
                                "duplicate interface found in inspect data; id: {} state: {:?}",
                                id, state
                            );
                        }
                        None => {}
                    },
                    Err(_) => {}
                }
            }
            (interfaces, system)
        },
    )
}

struct NetemulInterface<'a> {
    _network: netemul::TestNetwork<'a>,
    interface: netemul::TestInterface<'a>,
    fake_ep: netemul::TestFakeEndpoint<'a>,
    is_sending_replies: RefCell<bool>,
}

impl<'a> NetemulInterface<'a> {
    fn id(&self) -> u64 {
        self.interface.id()
    }
}

async fn configure_interface<'a>(
    iface: &'a netemul::TestInterface<'a>,
    controller: &fnet_neighbor::ControllerProxy,
    stack: &fnet_stack::StackProxy,
    InterfaceConfig {
        name_suffix: _,
        gateway_v4,
        addr_v4,
        gateway_v6,
        addr_v6,
        gateway_mac,
        metric,
    }: InterfaceConfig,
) {
    // Add addresses.
    futures::stream::iter([
        fnet::Subnet {
            addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: addr_v4.ipv4_bytes() }),
            prefix_len: PREFIX_V4,
        },
        fnet::Subnet {
            addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: addr_v6.ipv6_bytes() }),
            prefix_len: PREFIX_V6,
        },
    ])
    .for_each_concurrent(None, |subnet| async move {
        let address_state_provider = interfaces::add_subnet_address_and_route_wait_assigned(
            iface,
            subnet,
            fnet_interfaces_admin::AddressParameters::EMPTY,
        )
        .await
        .expect("add subnet address and route");
        let () = address_state_provider.detach().expect("address detach");
    })
    .await;

    let device_id = iface.id();

    // Add neighbor table entries for the gateway addresses.
    let mut gateway_v4 = fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: gateway_v4.ipv4_bytes() });
    let mut gateway_v6 = fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: gateway_v6.ipv6_bytes() });
    let mut gateway_mac = fnet::MacAddress { octets: gateway_mac.bytes() };
    let () = controller
        .add_entry(device_id, &mut gateway_v6, &mut gateway_mac)
        .await
        .expect("neighbor add_entry FIDL error")
        .map_err(zx::Status::from_raw)
        .expect("add IPv6 gateway neighbor table entry failed");
    let () = controller
        .add_entry(device_id, &mut gateway_v4, &mut gateway_mac)
        .await
        .expect("neighbor add_entry FIDL error")
        .map_err(zx::Status::from_raw)
        .expect("add IPv4 gateway neighbor table entry failed");

    // Add default routes.
    let () = stack
        .add_forwarding_entry(&mut fnet_stack::ForwardingEntry {
            subnet: fidl_subnet!("0.0.0.0/0"),
            device_id,
            next_hop: Some(Box::new(gateway_v4)),
            metric,
        })
        .await
        .expect("add IPv4 default route FIDL error")
        .expect("add IPv4 default route error");
    let () = stack
        .add_forwarding_entry(&mut fnet_stack::ForwardingEntry {
            subnet: fidl_subnet!("::/0"),
            device_id,
            next_hop: Some(Box::new(gateway_v6)),
            metric,
        })
        .await
        .expect("add IPv6 default route FIDL error")
        .expect("add IPv6 default route error");
}

fn handle_frame_stream<'a>(
    fake_ep: &'a netemul::TestFakeEndpoint<'_>,
    want: State,
    gateway_v4: &'a net_types::ip::Ipv4Addr,
    gateway_v6: &'a net_types::ip::Ipv6Addr,
    is_sending_replies: &'a RefCell<bool>,
) -> impl Stream<Item = ()> + 'a {
    fake_ep.frame_stream().map(|r| r.expect("fake endpoint frame stream error")).filter_map(
        move |(frame, dropped)| {
            assert_eq!(dropped, 0);
            async move {
                let reply = if *is_sending_replies.borrow() {
                    match reply_if_echo_request(frame, want, &gateway_v4, &gateway_v6) {
                        Some(reply) => reply,
                        None => return None,
                    }
                } else {
                    return None;
                };
                fake_ep
                    .write(reply.as_ref())
                    .await
                    .map(Some)
                    .expect("failed to write echo reply to fake endpoint")
            }
        },
    )
}

struct ReachabilityEnv<'a> {
    sandbox: &'a netemul::TestSandbox,
    realm: netemul::TestRealm<'a>,
    controller: fnet_neighbor::ControllerProxy,
    stack: fnet_stack::StackProxy,
    fake_clock: ftesting::FakeClockControlProxy,
}

fn setup_reachability_env<'a>(
    name: &'a str,
    sandbox: &'a netemul::TestSandbox,
) -> ReachabilityEnv<'a> {
    let realm = sandbox
        .create_netstack_realm_with::<Netstack2, _, _>(
            name,
            &[
                KnownServiceProvider::Reachability,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::DnsResolver,
            ],
        )
        .expect("failed to create realm");
    let controller = realm
        .connect_to_protocol::<fnet_neighbor::ControllerMarker>()
        .expect("failed to connect to Controller");
    let stack =
        realm.connect_to_protocol::<fnet_stack::StackMarker>().expect("failed to connect to Stack");
    let fake_clock = realm
        .connect_to_protocol::<ftesting::FakeClockControlMarker>()
        .expect("failed to connect to FakeClockControl");

    ReachabilityEnv { sandbox, realm, controller, stack, fake_clock }
}

async fn create_netemul_interfaces<'a>(
    name: &'a str,
    sandbox: &'a netemul::TestSandbox,
    env: &'a ReachabilityEnv<'_>,
    configs: &[(InterfaceConfig, State)],
) -> Vec<NetemulInterface<'a>> {
    futures::stream::iter(configs.iter())
        .then(|(config, _): &(InterfaceConfig, State)| {
            let config = config.clone();
            async {
                let name = format!("{}_{}", name, config.name_suffix);
                let network =
                    sandbox.create_network(name.clone()).await.expect("failed to create network");
                let fake_ep =
                    network.create_fake_endpoint().expect("failed to create fake endpoint");
                let interface = env
                    .realm
                    .join_network(&network, name)
                    .await
                    .expect("failed to join network with netdevice endpoint");
                let is_sending_replies = RefCell::new(true);

                configure_interface(&interface, &env.controller, &env.stack, config).await;
                NetemulInterface { _network: network, interface, fake_ep, is_sending_replies }
            }
        })
        .collect::<_>()
        .await
}

fn create_echo_reply_streams<'a>(
    interfaces: &'a Vec<NetemulInterface<'_>>,
    configs: &'a [(InterfaceConfig, State)],
) -> impl FusedStream<Item = ()> + 'a {
    // TODO(fxbug.dev/119965): Combine configs with the NetemulInterface struct
    interfaces
        .iter()
        .zip(configs.iter())
        .map(
            |(
                NetemulInterface { _network, interface: _, fake_ep, is_sending_replies },
                (config, want_state),
            )| {
                Box::pin(handle_frame_stream(
                    &fake_ep,
                    *want_state,
                    &config.gateway_v4,
                    &config.gateway_v6,
                    &is_sending_replies,
                ))
            },
        )
        .collect::<futures::stream::SelectAll<_>>()
        .fuse()
}

async fn initialize_fake_clock_with_deadline<T>(fake_clock: &T) -> zx::EventPair
where
    T: ftesting::FakeClockControlProxyInterface,
{
    // Set up the fake clock and start the timer.
    fake_clock.pause().await.expect("failed to pause the clock");
    let (deadline_set_event, deadline_set_server) = zx::EventPair::create();

    fake_clock
        .add_stop_point(
            &mut FIDL_TIMEOUT_ID.into(),
            ftesting::DeadlineEventType::Set,
            deadline_set_server,
        )
        .await
        .expect("add_stop_point failed")
        .expect("add_stop_point returned error");

    fake_clock
        .resume_with_increments(
            zx::Duration::from_millis(1).into_nanos(),
            &mut ftesting::Increment::Determined(zx::Duration::from_millis(1).into_nanos()),
        )
        .await
        .expect("resume with increment failed")
        .expect("resume with increment returned error");

    deadline_set_event
}

fn accelerate_time_until_timeout<'a, T>(
    fake_clock: &'a T,
    deadline_set_event: &'a zx::EventPair,
) -> impl Future + 'a
where
    T: ftesting::FakeClockControlProxyInterface + 'a,
{
    async move {
        // Wait for the timer to be set.
        let _ = fasync::OnSignals::new(deadline_set_event, zx::Signals::EVENTPAIR_SIGNALED)
            .await
            .expect("waiting for timer set failed");

        let (deadline_expire_event, deadline_expire_server) = zx::EventPair::create();

        fake_clock
            .add_stop_point(
                &mut FIDL_TIMEOUT_ID.into(),
                ftesting::DeadlineEventType::Expired,
                deadline_expire_server,
            )
            .await
            .expect("add_stop_point failed")
            .expect("add_stop_point returned error");

        // Fast increment speed after timer is set.
        fake_clock
            .resume_with_increments(
                zx::Duration::from_millis(1).into_nanos(),
                &mut ftesting::Increment::Determined(zx::Duration::from_seconds(10).into_nanos()),
            )
            .await
            .expect("resume with increment failed")
            .expect("resume with increment returned error");
        info!("time accelerated until timeout reached");

        // When the deadline expires, return speed to normal.
        let _ = fasync::OnSignals::new(&deadline_expire_event, zx::Signals::EVENTPAIR_SIGNALED)
            .await
            .expect("waiting for timer expired failed");

        fake_clock
            .resume_with_increments(
                zx::Duration::from_millis(1).into_nanos(),
                &mut ftesting::Increment::Determined(zx::Duration::from_millis(1).into_nanos()),
            )
            .await
            .expect("resume with increments failed")
            .expect("resme with increment returned error");
        info!("time slowed after timeout reached");
    }
}

#[netstack_test]
#[test_case(
    "gateway",
    &[(InterfaceConfig::new_primary(LOWER_METRIC), State::Gateway)];
    "gateway")]
#[test_case(
    "internet",
    &[(InterfaceConfig::new_primary(LOWER_METRIC), State::Internet)];
    "internet")]
#[test_case(
    "gateway_gateway",
    &[
        (InterfaceConfig::new_primary(LOWER_METRIC), State::Gateway),
        (InterfaceConfig::new_secondary(HIGHER_METRIC), State::Gateway),
    ];
    "gateway_gateway")]
#[test_case(
    "gateway_internet",
    &[
        (InterfaceConfig::new_primary(LOWER_METRIC), State::Gateway),
        (InterfaceConfig::new_secondary(HIGHER_METRIC), State::Internet),
    ];
    "gateway_internet")]
// This test case guards against the regression where if ICMP echo requests are routed according
// to the default route rather than explicitly through each interface, packets destined for the
// internet intended to be sent out the secondary interface will actually be sent out the primary
// interface due to the lower metric, resulting in the reachability state of the secondary
// interface to be Internet rather than Gateway, causing the test to fail.
#[test_case(
    "internet_gateway",
    &[
        (InterfaceConfig::new_primary(LOWER_METRIC), State::Internet),
        (InterfaceConfig::new_secondary(HIGHER_METRIC), State::Gateway),
    ];
    "internet_gateway")]
#[test_case(
    "internet_internet",
    &[
        (InterfaceConfig::new_primary(LOWER_METRIC), State::Internet),
        (InterfaceConfig::new_secondary(HIGHER_METRIC), State::Internet),
    ];
    "internet_internet")]
async fn test_state(name: &str, sub_test_name: &str, configs: &[(InterfaceConfig, State)]) {
    let name = format!("{}_{}", name, sub_test_name);
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let env = setup_reachability_env(&name, &sandbox);
    let deadline_set_event = initialize_fake_clock_with_deadline(&env.fake_clock).await;

    let interfaces = create_netemul_interfaces(&name, &env.sandbox, &env, &configs).await;
    let want_interfaces = interfaces
        .iter()
        .zip(configs.iter())
        .map(|(interface, (_, want_state)): (&NetemulInterface<'_>, &(InterfaceConfig, State))| {
            (interface.id(), IpStates::new(*want_state))
        })
        .collect::<HashMap<_, _>>();

    let mut echo_reply_streams = create_echo_reply_streams(&interfaces, &configs);

    // TODO(https://fxbug.dev/65585): Get reachability monitor's reachability state over FIDL rather
    // than the inspect data. Watching for updates to inspect data is currently not supported, so
    // poll instead.
    const INSPECT_COMPONENT: &str = "reachability";
    const INSPECT_TREE_SELECTOR: &str = "root";
    let realm_ref = &env.realm;
    let inspect_data_stream = futures::stream::unfold(None, |duration| async move {
        let () = match duration {
            None => {}
            Some(duration) => fasync::Timer::new(duration).await,
        };
        let duration = std::time::Duration::from_millis(100);
        let yielded = loop {
            match get_inspect_data(realm_ref, INSPECT_COMPONENT, INSPECT_TREE_SELECTOR, "")
                .map_ok(|data| extract_reachability_states(&data))
                .await
            {
                Ok(yielded) => break yielded,
                // Archivist can sometimes return transient `InconsistentSnapshot` errors. Our
                // wrapper function discards type information so we can't match on the specific
                // error.
                Err(err @ anyhow::Error { .. }) => println!("inspect data stream error: {:?}", err),
            }
            let () = fasync::Timer::new(duration).await;
        };
        Some((yielded, Some(duration)))
    })
    .fuse();
    futures::pin_mut!(inspect_data_stream);

    // Ensure that at least one echo request has been replied to before polling the inspect data
    // stream to guarantee that reachability monitor has initialized its inspect data tree.
    let () = echo_reply_streams.next().await.expect("echo reply stream ended unexpectedly");

    let IpStates { ipv4: want_system_ipv4, ipv6: want_system_ipv6 } =
        want_interfaces.values().fold(
            IpStates::new(State::None),
            |IpStates { ipv4: best_ipv4, ipv6: best_ipv6 }, &IpStates { ipv4, ipv6 }| IpStates {
                ipv4: if ipv4 > best_ipv4 { ipv4 } else { best_ipv4 },
                ipv6: if ipv6 > best_ipv6 { ipv6 } else { best_ipv6 },
            },
        );

    let reachability_monitor_wait_fut =
        wait_for_component_stopped(&env.realm, constants::reachability::COMPONENT_NAME, None)
            .fuse();
    futures::pin_mut!(reachability_monitor_wait_fut);

    let time_control_fut =
        accelerate_time_until_timeout(&env.fake_clock, &deadline_set_event).fuse();
    futures::pin_mut!(time_control_fut);

    loop {
        futures::select! {
            _ = time_control_fut => (),
            o = echo_reply_streams.next() => {
                let () = o.expect("interface echo reply stream ended unexpectedly");
            }
            r = inspect_data_stream.next() => {
                let (got_interfaces, IpStates { ipv4: got_system_ipv4, ipv6: got_system_ipv6 }) = r
                    .expect("inspect data stream ended unexpectedly");
                if got_system_ipv4 > want_system_ipv4 {
                    panic!(
                        "system IPv4 state exceeded; got: {:?}, want: {:?}",
                        got_system_ipv4, want_system_ipv4,
                    )
                }
                if got_system_ipv6 > want_system_ipv6 {
                    panic!(
                        "system IPv6 state exceeded; got: {:?}, want: {:?}",
                        got_system_ipv6, want_system_ipv6,
                    )
                }
                let equal_count = got_interfaces
                    .iter()
                    .filter_map(|(id, got)| {
                        let IpStates { ipv4: want_ipv4, ipv6: want_ipv6 } =
                            want_interfaces.get(&id).unwrap_or_else(|| {
                                panic!(
                                    "unknown interface {} with state {:?} found in inspect data",
                                    id, got,
                                )
                            });
                        let IpStates { ipv4: got_ipv4, ipv6: got_ipv6 } = got;
                        if got_ipv4 > want_ipv4 {
                            panic!(
                                "interface {} IPv4 state exceeded; got: {:?}, want: {:?}",
                                id, got_ipv4, want_ipv4,
                            )
                        }
                        if got_ipv6 > want_ipv6 {
                            panic!(
                                "interface {} IPv6 state exceeded; got: {:?}, want: {:?}",
                                id, got_ipv6, want_ipv6,
                            )
                        }
                        (got_ipv4 == want_ipv4 && got_ipv6 == want_ipv6).then(|| ())
                    })
                    .count();
                if got_system_ipv4 == want_system_ipv4
                    && got_system_ipv6 == want_system_ipv6
                    && equal_count == interfaces.len()
                {
                    break;
                }
            }
            event = reachability_monitor_wait_fut => {
                panic!("reachability monitor terminated unexpectedly with event: {:?}", event);
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum InterfacesWithInternetInitiallyTrue {
    All,
    AllThenZero,
    AtLeastOne,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum InterfacesWithInternetInitiallyFalse {
    ZeroThenAll,
    ZeroThenOne,
    Zero,
}

#[netstack_test]
#[test_case(&Either::Left(InterfacesWithInternetInitiallyTrue::All))]
#[test_case(&Either::Left(InterfacesWithInternetInitiallyTrue::AllThenZero))]
#[test_case(&Either::Left(InterfacesWithInternetInitiallyTrue::AtLeastOne))]
#[test_case(&Either::Right(InterfacesWithInternetInitiallyFalse::ZeroThenAll))]
#[test_case(&Either::Right(InterfacesWithInternetInitiallyFalse::ZeroThenOne))]
#[test_case(&Either::Right(InterfacesWithInternetInitiallyFalse::Zero))]
async fn test_internet_available(
    name: &str,
    interfaces_with_internet: &Either<
        InterfacesWithInternetInitiallyTrue,
        InterfacesWithInternetInitiallyFalse,
    >,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let env = setup_reachability_env(name, &sandbox);

    let deadline_set_event = initialize_fake_clock_with_deadline(&env.fake_clock).await;
    let monitor = env
        .realm
        .connect_to_protocol::<fnet_reachability::MonitorMarker>()
        .expect("failed to connect to fuchsia.net.reachability.Monitor");

    let configs = vec![
        (InterfaceConfig::new_primary(LOWER_METRIC), State::Internet),
        (InterfaceConfig::new_secondary(HIGHER_METRIC), State::Internet),
    ];
    let interfaces = create_netemul_interfaces(name, &env.sandbox, &env, &configs).await;

    let mut echo_reply_streams = create_echo_reply_streams(&interfaces, &configs);
    let () = echo_reply_streams.next().await.expect("echo reply stream ended unexpectedly");

    let reachability_monitor_wait_fut =
        wait_for_component_stopped(&env.realm, constants::reachability::COMPONENT_NAME, None)
            .fuse();
    futures::pin_mut!(reachability_monitor_wait_fut);

    // Creates a stream from Watch so it can be called again when the response has been received.
    let monitor_ref = &monitor;
    let monitor_watch_stream =
        futures::stream::unfold((), |_| async move { Some((monitor_ref.watch().await, ())) })
            .fuse();
    futures::pin_mut!(monitor_watch_stream);

    let time_control_fut =
        accelerate_time_until_timeout(&env.fake_clock, &deadline_set_event).fuse();
    futures::pin_mut!(time_control_fut);

    let expected_final_state = match interfaces_with_internet {
        Either::Left(interface_case) => {
            // The first call to the Monitor should return the Some(false) default response.
            let snapshot: fnet_reachability::Snapshot = monitor
                .watch()
                .await
                .expect("failed to fetch snapshot fuchsia.net.reachability.Monitor");
            assert_eq!(snapshot.internet_available, Some(false));

            match interface_case {
                InterfacesWithInternetInitiallyTrue::All => true,
                InterfacesWithInternetInitiallyTrue::AllThenZero => false,
                InterfacesWithInternetInitiallyTrue::AtLeastOne => {
                    // Disable replies for all but one interface.
                    *interfaces[0].is_sending_replies.borrow_mut() = false;
                    true
                }
            }
        }
        Either::Right(interface_case) => {
            // Disable replies for all interfaces.
            *interfaces[0].is_sending_replies.borrow_mut() = false;
            *interfaces[1].is_sending_replies.borrow_mut() = false;

            match interface_case {
                InterfacesWithInternetInitiallyFalse::ZeroThenAll => true,
                InterfacesWithInternetInitiallyFalse::ZeroThenOne => true,
                InterfacesWithInternetInitiallyFalse::Zero => false,
            }
        }
    };

    loop {
        futures::select! {
            _ = time_control_fut => (),
            o = echo_reply_streams.next() => {
                let () = o.expect("interface echo reply stream ended unexpectedly");
            }
            r = monitor_watch_stream.next() => {
                match r {
                   Some(Ok(x)) => {
                       let internet_available = x.internet_available.unwrap();

                       match interfaces_with_internet {
                            Either::Left(interface_case) => {
                                if internet_available {
                                    match interface_case {
                                        InterfacesWithInternetInitiallyTrue::AllThenZero => {
                                            // Disable replies for all interfaces.
                                            *interfaces[0].is_sending_replies.borrow_mut() = false;
                                            *interfaces[1].is_sending_replies.borrow_mut() = false;
                                            continue;
                                        },
                                        _ => (),
                                    }
                                }
                            },
                            Either::Right(interface_case) => {
                                if !internet_available {
                                    match interface_case {
                                        InterfacesWithInternetInitiallyFalse::ZeroThenAll => {
                                            // Enable replies for all interfaces.
                                            *interfaces[0].is_sending_replies.borrow_mut() = true;
                                            *interfaces[1].is_sending_replies.borrow_mut() = true;
                                            continue;
                                        },
                                        InterfacesWithInternetInitiallyFalse::ZeroThenOne => {
                                            // Enable replies for one interface.
                                            *interfaces[0].is_sending_replies.borrow_mut() = true;
                                            continue;
                                        },
                                        InterfacesWithInternetInitiallyFalse::Zero => (),
                                    }
                                }
                            },
                       };
                       assert_eq!(internet_available, expected_final_state);
                       break;
                   },
                   Some(Err(e)) => panic!("failed to fetch updated snapshot {}", e),
                   None => unreachable!("monitor_watch_stream unexpectedly ended"),
                }
            }
            event = reachability_monitor_wait_fut => {
                panic!("reachability monitor terminated unexpectedly with event: {:?}", event);
            }
        }
    }
}

#[netstack_test]
async fn test_hanging_get_multiple_clients(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<Netstack2, _, _>(
            name,
            &[
                KnownServiceProvider::Reachability,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::DnsResolver,
            ],
        )
        .expect("failed to create realm");

    let monitor_client1 = realm
        .connect_to_protocol::<fnet_reachability::MonitorMarker>()
        .expect("client 1: failed to connect to fuchsia.net.reachability.Monitor");

    let monitor_client2 = realm
        .connect_to_protocol::<fnet_reachability::MonitorMarker>()
        .expect("client 2: failed to connect to fuchsia.net.reachability.Monitor");

    assert_eq!(
        monitor_client1
            .watch()
            .await
            .expect("client 1: failed to fetch updated snapshot")
            .internet_available,
        Some(false)
    );
    assert_eq!(
        monitor_client2
            .watch()
            .await
            .expect("client 2: failed to fetch updated snapshot")
            .internet_available,
        Some(false)
    );
}

#[netstack_test]
async fn test_cannot_call_set_options_after_watch(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<Netstack2, _, _>(
            name,
            &[
                KnownServiceProvider::Reachability,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::DnsResolver,
            ],
        )
        .expect("failed to create realm");

    let monitor = realm
        .connect_to_protocol::<fnet_reachability::MonitorMarker>()
        .expect("failed to connect to fuchsia.net.reachability.Monitor");

    assert_matches!(
        monitor.watch().await.expect("failed to fetch updated snapshot").internet_available,
        Some(_)
    );
    assert_matches!(monitor.set_options(fnet_reachability::MonitorOptions::EMPTY), Ok(()));

    // The request stream should be closed if the client calls SetOptions after calling Watch
    // previously.
    assert_matches!(monitor.on_closed().await, Ok(_));
}

#[netstack_test]
async fn test_cannot_call_set_options_twice(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<Netstack2, _, _>(
            name,
            &[
                KnownServiceProvider::Reachability,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::DnsResolver,
            ],
        )
        .expect("failed to create realm");

    let monitor = realm
        .connect_to_protocol::<fnet_reachability::MonitorMarker>()
        .expect("failed to connect to fuchsia.net.reachability.Monitor");

    let () = monitor
        .set_options(fnet_reachability::MonitorOptions::EMPTY)
        .expect("failed to set reachability API options");
    let () = monitor
        .set_options(fnet_reachability::MonitorOptions::EMPTY)
        .expect("failed to set reachability API options");

    // The request stream should be closed if the client calls SetOptions after calling it once
    // already.
    assert_matches!(monitor.on_closed().await, Ok(_));
}
