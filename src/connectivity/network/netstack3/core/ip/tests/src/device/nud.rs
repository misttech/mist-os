// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::collections::{HashMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use core::num::{NonZeroU16, NonZeroUsize};

use assert_matches::assert_matches;
use ip_test_macro::ip_test;
use net_declare::net_ip_v6;
use net_types::ip::{AddrSubnet, Ip, IpAddress as _, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet};
use net_types::{SpecifiedAddr, UnicastAddr, Witness as _};
use packet::{Buf, InnerPacketBuilder as _, Serializer as _};
use packet_formats::ethernet::{EtherType, EthernetFrameLengthCheck};
use packet_formats::icmp::ndp::options::NdpOptionBuilder;
use packet_formats::icmp::ndp::{
    NeighborAdvertisement, NeighborSolicitation, OptionSequenceBuilder, RouterAdvertisement,
};
use packet_formats::icmp::{IcmpDestUnreachable, IcmpPacketBuilder, IcmpZeroCode};
use packet_formats::ip::{FragmentOffset, IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::ipv4::Ipv4PacketBuilder;
use packet_formats::ipv6::Ipv6PacketBuilder;
use packet_formats::testutil::{
    parse_ethernet_frame, parse_icmp_packet_in_ip_packet_in_ethernet_frame,
};
use packet_formats::udp::UdpPacketBuilder;
use test_case::test_case;

use netstack3_base::testutil::{
    FakeNetwork, FakeNetworkLinks, TestAddrs, TestIpExt, WithFakeFrameContext,
};
use netstack3_base::{DeviceIdContext, FrameDestination, InstantContext as _};
use netstack3_core::device::{
    EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice, WeakDeviceId,
};
use netstack3_core::testutil::{
    new_simple_fake_network, CtxPairExt, DispatchedFrame, FakeBindingsCtx, FakeCtx, FakeCtxBuilder,
    FakeCtxNetworkSpec, DEFAULT_INTERFACE_METRIC,
};
use netstack3_core::{IpExt, TxMetadata, UnlockedCoreCtx};
use netstack3_device::testutil::IPV6_MIN_IMPLIED_MAX_FRAME_SIZE;
use netstack3_ip::device::{Ipv6DeviceConfigurationUpdate, SlaacConfigurationUpdate};
use netstack3_ip::icmp::{self, REQUIRED_NDP_IP_PACKET_HOP_LIMIT};
use netstack3_ip::nud::{
    self, Delay, DynamicNeighborState, DynamicNeighborUpdateSource, Incomplete, NeighborState,
    NudConfigContext, NudContext, NudHandler, Reachable, Stale,
};
use netstack3_ip::{self as ip, AddableEntry, AddableMetric};
use netstack3_tcp::{self as tcp, TcpSocketId};

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
fn assert_neighbors<I: IpExt>(
    ctx: &mut FakeCtx,
    device_id: &EthernetDeviceId<FakeBindingsCtx>,
    expected: HashMap<SpecifiedAddr<I::Addr>, NeighborState<EthernetLinkDevice, FakeBindingsCtx>>,
) {
    NudContext::<I, EthernetLinkDevice, _>::with_nud_state_mut(
        &mut ctx.core_ctx(),
        device_id,
        |state, _config| assert_eq!(state.neighbors(), &expected),
    )
}

#[test]
fn router_advertisement_with_source_link_layer_option_should_add_neighbor() {
    let TestAddrs { local_mac, remote_mac, local_ip: _, remote_ip: _, subnet: _ } =
        Ipv6::TEST_ADDRS;

    let mut ctx = FakeCtx::default();
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, true), false);

    let remote_mac_bytes = remote_mac.bytes();
    let options = vec![NdpOptionBuilder::SourceLinkLayerAddress(&remote_mac_bytes[..])];

    let src_ip = remote_mac.to_ipv6_link_local().addr();
    let dst_ip = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get();
    let ra_packet_buf = |options: &[NdpOptionBuilder<'_>]| {
        OptionSequenceBuilder::new(options.iter())
            .into_serializer()
            .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                src_ip,
                dst_ip,
                IcmpZeroCode,
                RouterAdvertisement::new(0, false, false, 0, 0, 0),
            ))
            .encapsulate(Ipv6PacketBuilder::new(
                src_ip,
                dst_ip,
                REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap()
            .unwrap_b()
    };

    // First receive a Router Advertisement without the source link layer
    // and make sure no new neighbor gets added.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        ra_packet_buf(&[][..]),
    );
    let link_device_id = device_id.clone().try_into().unwrap();
    assert_neighbors::<Ipv6>(&mut ctx, &link_device_id, Default::default());

    // RA with a source link layer option should create a new entry.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        ra_packet_buf(&options[..]),
    );
    assert_neighbors::<Ipv6>(
        &mut ctx,
        &link_device_id,
        HashMap::from([(
            {
                let src_ip: UnicastAddr<_> = src_ip.into_addr();
                src_ip.into_specified()
            },
            NeighborState::Dynamic(DynamicNeighborState::Stale(Stale {
                link_address: remote_mac.get(),
            })),
        )]),
    );
}

const LOCAL_IP: Ipv6Addr = net_ip_v6!("fe80::1");
const OTHER_IP: Ipv6Addr = net_ip_v6!("fe80::2");
const MULTICAST_IP: Ipv6Addr = net_ip_v6!("ff02::1234");

#[test_case(LOCAL_IP, None, true; "targeting assigned address")]
#[test_case(LOCAL_IP, NonZeroU16::new(1), false; "targeting tentative address")]
#[test_case(OTHER_IP, None, false; "targeting other host")]
#[test_case(MULTICAST_IP, None, false; "targeting multicast address")]
fn ns_response(target_addr: Ipv6Addr, dad_transmits: Option<NonZeroU16>, expect_handle: bool) {
    let TestAddrs { local_mac, remote_mac, local_ip: _, remote_ip: _, subnet: _ } =
        Ipv6::TEST_ADDRS;

    let mut ctx = FakeCtx::default();
    let link_device_id =
        ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        );
    let device_id = link_device_id.clone().into();
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, true), false);

    // Set DAD config after enabling the device so that the default address
    // does not perform DAD.
    let _: Ipv6DeviceConfigurationUpdate = ctx
        .core_api()
        .device_ip::<Ipv6>()
        .update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(dad_transmits),
                ..Default::default()
            },
        )
        .unwrap();
    ctx.core_api()
        .device_ip::<Ipv6>()
        .add_ip_addr_subnet(&device_id, AddrSubnet::new(LOCAL_IP, Ipv6Addr::BYTES * 8).unwrap())
        .unwrap();
    if let Some(NonZeroU16 { .. }) = dad_transmits {
        // Take DAD message.
        assert_matches!(
            &ctx.bindings_ctx.take_ethernet_frames()[..],
            [(got_device_id, got_frame)] => {
                assert_eq!(got_device_id, &link_device_id);

                let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) =
                    parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        Ipv6,
                        _,
                        NeighborSolicitation,
                        _,
                    >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                        .unwrap();
                let dst_ip = LOCAL_IP.to_solicited_node_address();
                assert_eq!(src_mac, local_mac.get());
                assert_eq!(dst_mac, dst_ip.into());
                assert_eq!(got_src_ip, Ipv6::UNSPECIFIED_ADDRESS);
                assert_eq!(got_dst_ip, dst_ip.get());
                assert_eq!(ttl, REQUIRED_NDP_IP_PACKET_HOP_LIMIT);
                assert_eq!(message.target_address(), &LOCAL_IP);
                assert_eq!(code, IcmpZeroCode);
            }
        );
    }

    // Send a neighbor solicitation with the test target address to the
    // host.
    let src_ip = remote_mac.to_ipv6_link_local().addr();
    let snmc = target_addr.to_solicited_node_address();
    let dst_ip = snmc.get();
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        icmp::testutil::neighbor_solicitation_ip_packet(**src_ip, dst_ip, target_addr, *remote_mac),
    );

    // Check if a neighbor advertisement was sent as a response and the
    // new state of the neighbor table.
    let expected_neighbors = if expect_handle {
        assert_matches!(
            &ctx.bindings_ctx.take_ethernet_frames()[..],
            [(got_device_id, got_frame)] => {
                assert_eq!(got_device_id, &link_device_id);

                let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) =
                    parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        Ipv6,
                        _,
                        NeighborAdvertisement,
                        _,
                    >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                        .unwrap();
                assert_eq!(src_mac, local_mac.get());
                assert_eq!(dst_mac, remote_mac.get());
                assert_eq!(got_src_ip, target_addr);
                assert_eq!(got_dst_ip, src_ip.into_addr());
                assert_eq!(ttl, REQUIRED_NDP_IP_PACKET_HOP_LIMIT);
                assert_eq!(message.target_address(), &target_addr);
                assert_eq!(code, IcmpZeroCode);
            }
        );

        HashMap::from([(
            {
                let src_ip: UnicastAddr<_> = src_ip.into_addr();
                src_ip.into_specified()
            },
            // TODO(https://fxbug.dev/42081683): expect STALE instead once we correctly do not
            // go through NUD to send NDP packets.
            NeighborState::Dynamic(DynamicNeighborState::Delay(Delay {
                link_address: remote_mac.get(),
            })),
        )])
    } else {
        assert_matches!(&ctx.bindings_ctx.take_ethernet_frames()[..], []);
        HashMap::default()
    };

    assert_neighbors::<Ipv6>(&mut ctx, &link_device_id, expected_neighbors);
    // Remove device to clear all dangling references.
    core::mem::drop(device_id);
    ctx.core_api().device().remove_device(link_device_id).into_removed();
}

#[test]
fn ipv6_integration() {
    let TestAddrs { local_mac, remote_mac, local_ip: _, remote_ip: _, subnet: _ } =
        Ipv6::TEST_ADDRS;

    let mut ctx = FakeCtx::default();
    let eth_device_id =
        ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        );
    let device_id = eth_device_id.clone().into();
    // Configure the device to generate a link-local address.
    let _: Ipv6DeviceConfigurationUpdate = ctx
        .core_api()
        .device_ip::<Ipv6>()
        .update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, true), false);

    let neighbor_ip = remote_mac.to_ipv6_link_local().addr();
    let neighbor_ip: UnicastAddr<_> = neighbor_ip.into_addr();
    let dst_ip = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get();
    let na_packet_buf = |solicited_flag, override_flag| {
        icmp::testutil::neighbor_advertisement_ip_packet(
            *neighbor_ip,
            dst_ip,
            false, /* router_flag */
            solicited_flag,
            override_flag,
            *remote_mac,
        )
    };

    // NeighborAdvertisements should not create a new entry even if
    // the advertisement has both the solicited and override flag set.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        na_packet_buf(false, false),
    );
    let link_device_id = device_id.clone().try_into().unwrap();
    assert_neighbors::<Ipv6>(&mut ctx, &link_device_id, Default::default());
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        na_packet_buf(true, true),
    );
    assert_neighbors::<Ipv6>(&mut ctx, &link_device_id, Default::default());

    let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
    assert_eq!(bindings_ctx.take_ethernet_frames(), []);

    // Trigger a neighbor solicitation to be sent.
    let body = [u8::MAX];
    let pending_frames = VecDeque::from([(Buf::new(body.to_vec(), ..), TxMetadata::default())]);
    assert_matches!(
        NudHandler::<Ipv6, EthernetLinkDevice, _>::send_ip_packet_to_neighbor(
            &mut core_ctx.context(),
            bindings_ctx,
            &eth_device_id,
            neighbor_ip.into_specified(),
            Buf::new(body, ..),
            TxMetadata::default(),
        ),
        Ok(())
    );
    assert_matches!(
        &bindings_ctx.take_ethernet_frames()[..],
        [(got_device_id, got_frame)] => {
            assert_eq!(got_device_id, &eth_device_id);

            let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                Ipv6,
                _,
                NeighborSolicitation,
                _,
            >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                .unwrap();
            let target = neighbor_ip;
            let snmc = target.to_solicited_node_address();
            assert_eq!(src_mac, local_mac.get());
            assert_eq!(dst_mac, snmc.into());
            assert_eq!(got_src_ip, local_mac.to_ipv6_link_local().addr().into());
            assert_eq!(got_dst_ip, snmc.get());
            assert_eq!(ttl, 255);
            assert_eq!(message.target_address(), &target.get());
            assert_eq!(code, IcmpZeroCode);
        }
    );

    let max_multicast_solicit = NudContext::<Ipv6, EthernetLinkDevice, _>::with_nud_state_mut(
        &mut core_ctx.context(),
        &link_device_id,
        |_, nud_config| {
            // NB: Because we're using the real core context here and it
            // implements NudConfigContext for both Ipv4 and Ipv6 we need to
            // nudge the compiler to the IPv6 implementation.
            NudConfigContext::<Ipv6>::max_multicast_solicit(nud_config).get()
        },
    );

    assert_neighbors::<Ipv6>(
        &mut ctx,
        &link_device_id,
        HashMap::from([(
            neighbor_ip.into_specified(),
            NeighborState::Dynamic(DynamicNeighborState::Incomplete(
                Incomplete::new_with_pending_frames_and_transmit_counter(
                    pending_frames,
                    NonZeroU16::new(max_multicast_solicit - 1),
                ),
            )),
        )]),
    );

    // A Neighbor advertisement should now update the entry.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        na_packet_buf(true, true),
    );
    let now = ctx.bindings_ctx.now();
    assert_neighbors::<Ipv6>(
        &mut ctx,
        &link_device_id,
        HashMap::from([(
            neighbor_ip.into_specified(),
            NeighborState::Dynamic(DynamicNeighborState::Reachable(Reachable {
                link_address: remote_mac.get(),
                last_confirmed_at: now,
            })),
        )]),
    );
    let frames = ctx.bindings_ctx.take_ethernet_frames();
    let (got_device_id, got_frame) = assert_matches!(&frames[..], [x] => x);
    assert_eq!(got_device_id, &eth_device_id);

    let (payload, src_mac, dst_mac, ether_type) =
        parse_ethernet_frame(got_frame, EthernetFrameLengthCheck::NoCheck).unwrap();
    assert_eq!(src_mac, local_mac.get());
    assert_eq!(dst_mac, remote_mac.get());
    assert_eq!(ether_type, Some(EtherType::Ipv6));
    assert_eq!(payload, body);

    // Disabling the device should clear the neighbor table.
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, false), true);
    assert_neighbors::<Ipv6>(&mut ctx, &link_device_id, HashMap::new());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
}

type FakeNudNetwork<L> = FakeNetwork<FakeCtxNetworkSpec, &'static str, L>;

fn new_test_net<I: TestIpExt>() -> (
    FakeNudNetwork<
        impl FakeNetworkLinks<DispatchedFrame, EthernetDeviceId<FakeBindingsCtx>, &'static str>,
    >,
    EthernetDeviceId<FakeBindingsCtx>,
    EthernetDeviceId<FakeBindingsCtx>,
) {
    let build_ctx = |config: TestAddrs<I::Addr>| {
        let mut builder = FakeCtxBuilder::default();
        let device =
            builder.add_device_with_ip(config.local_mac, config.local_ip.get(), config.subnet);
        let (ctx, device_ids) = builder.build();
        (ctx, device_ids[device].clone())
    };

    let (local, local_device) = build_ctx(I::TEST_ADDRS);
    let (remote, remote_device) = build_ctx(I::TEST_ADDRS.swap());
    let net = new_simple_fake_network(
        "local",
        local,
        local_device.downgrade(),
        "remote",
        remote,
        remote_device.downgrade(),
    );
    (net, local_device, remote_device)
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
fn bind_and_connect_sockets<
    I: TestIpExt + IpExt,
    L: FakeNetworkLinks<DispatchedFrame, EthernetDeviceId<FakeBindingsCtx>, &'static str>,
>(
    net: &mut FakeNudNetwork<L>,
    local_buffers: tcp::testutil::ProvidedBuffers,
) -> TcpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx> {
    const REMOTE_PORT: NonZeroU16 = NonZeroU16::new(33333).unwrap();

    net.with_context("remote", |ctx| {
        let mut tcp_api = ctx.core_api().tcp::<I>();
        let socket = tcp_api.create(tcp::testutil::ProvidedBuffers::default());
        tcp_api
            .bind(
                &socket,
                Some(net_types::ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)),
                Some(REMOTE_PORT),
            )
            .unwrap();
        tcp_api.listen(&socket, NonZeroUsize::new(1).unwrap()).unwrap();
    });

    net.with_context("local", |ctx| {
        let mut tcp_api = ctx.core_api().tcp::<I>();
        let socket = tcp_api.create(local_buffers);
        tcp_api
            .connect(
                &socket,
                Some(net_types::ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)),
                REMOTE_PORT,
            )
            .unwrap();
        socket
    })
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn upper_layer_confirmation_tcp_handshake<I: TestIpExt + IpExt>()
where
    for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>: DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<FakeBindingsCtx>>
        + NudContext<I, EthernetLinkDevice, FakeBindingsCtx>,
{
    let (mut net, local_device, remote_device) = new_test_net::<I>();

    let TestAddrs { local_ip, local_mac, remote_mac, remote_ip, .. } = I::TEST_ADDRS;

    // Insert a STALE neighbor in each node's neighbor table so that they don't
    // initiate neighbor resolution before performing the TCP handshake.
    for (ctx, device, neighbor, link_addr) in [
        ("local", local_device.clone(), remote_ip, remote_mac),
        ("remote", remote_device.clone(), local_ip, local_mac),
    ] {
        net.with_context(ctx, |FakeCtx { core_ctx, bindings_ctx }| {
            NudHandler::handle_neighbor_update(
                &mut core_ctx.context(),
                bindings_ctx,
                &device,
                neighbor,
                link_addr.get(),
                DynamicNeighborUpdateSource::Probe,
            );
            nud::testutil::assert_dynamic_neighbor_state(
                &mut core_ctx.context(),
                device.clone(),
                neighbor,
                DynamicNeighborState::Stale(Stale { link_address: link_addr.get() }),
            );
        });
    }

    // Initiate a TCP connection and make sure the SYN and resulting SYN/ACK are
    // received by each context.
    let _: TcpSocketId<I, _, _> =
        bind_and_connect_sockets::<I, _>(&mut net, tcp::testutil::ProvidedBuffers::default());
    for _ in 0..2 {
        assert_eq!(net.step().frames_sent, 1);
    }

    // The three-way handshake should now be complete, and the neighbor should have
    // transitioned to REACHABLE.
    net.with_context("local", |FakeCtx { core_ctx, bindings_ctx }| {
        nud::testutil::assert_dynamic_neighbor_state(
            &mut core_ctx.context(),
            local_device.clone(),
            remote_ip,
            DynamicNeighborState::Reachable(Reachable {
                link_address: remote_mac.get(),
                last_confirmed_at: bindings_ctx.now(),
            }),
        );
    });

    // Remove the devices so that existing NUD timers get cleaned up;
    // otherwise, they would hold dangling references to the devices when
    // the `StackState`s are dropped at the end of the test.
    for (ctx, device) in [("local", local_device), ("remote", remote_device)] {
        net.with_context(ctx, |ctx| {
            ctx.test_api().clear_routes_and_remove_device(device);
        });
    }
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn upper_layer_confirmation_tcp_ack<I: TestIpExt + IpExt>()
where
    for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>: DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<FakeBindingsCtx>>
        + NudContext<I, EthernetLinkDevice, FakeBindingsCtx>,
{
    let (mut net, local_device, remote_device) = new_test_net::<I>();

    let TestAddrs { remote_mac, remote_ip, .. } = I::TEST_ADDRS;

    // Initiate a TCP connection, allow the handshake to complete, and wait until
    // the neighbor entry goes STALE due to lack of traffic on the connection.
    let client_ends = tcp::testutil::WriteBackClientBuffers::default();
    let local_socket = bind_and_connect_sockets::<I, _>(
        &mut net,
        tcp::testutil::ProvidedBuffers::Buffers(client_ends.clone()),
    );
    net.run_until_idle();
    net.with_context("local", |FakeCtx { core_ctx, bindings_ctx: _ }| {
        nud::testutil::assert_dynamic_neighbor_state(
            &mut core_ctx.context(),
            local_device.clone(),
            remote_ip,
            DynamicNeighborState::Stale(Stale { link_address: remote_mac.get() }),
        );
    });

    // Send some data on the local socket and wait for it to be ACKed by the peer.
    let tcp::testutil::ClientBuffers { send, receive: _ } =
        client_ends.0.as_ref().lock().take().unwrap();
    send.lock().extend_from_slice(b"hello");
    net.with_context("local", |ctx| {
        ctx.core_api().tcp().do_send(&local_socket);
    });
    for _ in 0..2 {
        assert_eq!(net.step().frames_sent, 1);
    }

    // The ACK should have been processed, and the neighbor should have transitioned
    // to REACHABLE.
    net.with_context("local", |FakeCtx { core_ctx, bindings_ctx }| {
        nud::testutil::assert_dynamic_neighbor_state(
            &mut core_ctx.context(),
            local_device.clone(),
            remote_ip,
            DynamicNeighborState::Reachable(Reachable {
                link_address: remote_mac.get(),
                last_confirmed_at: bindings_ctx.now(),
            }),
        );
    });

    // Remove the devices so that existing NUD timers get cleaned up;
    // otherwise, they would hold dangling references to the devices when
    // the `StackState`s are dropped at the end of the test.
    for (ctx, device) in [("local", local_device), ("remote", remote_device)] {
        net.with_context(ctx, |ctx| {
            ctx.test_api().clear_routes_and_remove_device(device);
        });
    }
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn icmp_error_on_address_resolution_failure_tcp_local<I: TestIpExt + IpExt>() {
    let mut builder = FakeCtxBuilder::default();
    let _device_id = builder.add_device_with_ip(
        I::TEST_ADDRS.local_mac,
        I::TEST_ADDRS.local_ip.get(),
        I::TEST_ADDRS.subnet,
    );
    let (mut ctx, _): (_, Vec<EthernetDeviceId<_>>) = builder.build();

    // Add a loopback interface because local delivery of the ICMP error
    // relies on loopback.
    let _loopback_id = ctx.test_api().add_loopback();

    let mut tcp_api = ctx.core_api().tcp::<I>();
    let socket = tcp_api.create(tcp::testutil::ProvidedBuffers::default());
    const REMOTE_PORT: NonZeroU16 = NonZeroU16::new(33333).unwrap();
    tcp_api
        .connect(&socket, Some(net_types::ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)), REMOTE_PORT)
        .unwrap();

    while ctx.test_api().handle_queued_rx_packets()
        || CtxPairExt::trigger_next_timer(&mut ctx).is_some()
    {}

    let mut tcp_api = ctx.core_api().tcp::<I>();
    assert_eq!(tcp_api.get_socket_error(&socket), Some(tcp::ConnectionError::HostUnreachable),);
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn icmp_error_on_address_resolution_failure_tcp_forwarding<I: TestIpExt + IpExt>() {
    let (mut net, local_device, remote_device) = new_test_net::<I>();

    let TestAddrs { remote_ip, .. } = I::TEST_ADDRS;

    // These default routes mean that later when local tries to connect to an
    // address not in the subnet on the network, it will send the SYN to remote,
    // and remote will attempt to forward to the address as a neighbor (and
    // link address resolution will fail here).
    for (ctx, device, gateway) in
        [("local", local_device, Some(remote_ip)), ("remote", remote_device.clone(), None)]
    {
        net.with_context(ctx, |ctx| {
            let mut core_ctx = ctx.core_ctx();
            ip::testutil::add_route::<I, _>(
                &mut core_ctx,
                AddableEntry {
                    subnet: Subnet::new(I::UNSPECIFIED_ADDRESS, 0).unwrap(),
                    device: device.into(),
                    gateway: gateway,
                    metric: AddableMetric::MetricTracksInterface,
                },
            )
            .expect("add default route");
        });
    }

    net.with_context("remote", |ctx| {
        ctx.test_api().set_unicast_forwarding_enabled::<I>(&remote_device.into(), true);
    });

    let socket = net.with_context("local", |ctx| {
        let mut tcp_api = ctx.core_api().tcp::<I>();
        let socket = tcp_api.create(tcp::testutil::ProvidedBuffers::default());
        const REMOTE_PORT: NonZeroU16 = NonZeroU16::new(33333).unwrap();
        tcp_api
            .connect(
                &socket,
                Some(net_types::ZonedAddr::Unzoned(I::get_other_remote_ip_address(1))),
                REMOTE_PORT,
            )
            .unwrap();
        socket
    });

    net.run_until_idle();
    net.with_context("local", |ctx| {
        let mut tcp_api = ctx.core_api().tcp::<I>();
        assert_eq!(tcp_api.get_socket_error(&socket), Some(tcp::ConnectionError::HostUnreachable),);
    });
}

#[test_case(1; "non_initial_fragment")]
#[test_case(0; "initial_fragment")]
fn icmp_error_fragment_offset(fragment_offset: u16) {
    let mut builder = FakeCtxBuilder::default();
    let _device_id = builder.add_device_with_ip(
        Ipv4::TEST_ADDRS.local_mac,
        Ipv4::TEST_ADDRS.local_ip.get(),
        Ipv4::TEST_ADDRS.subnet,
    );
    let (mut ctx, mut device_ids) = builder.build();
    let device_id = device_ids.pop().unwrap();

    // Add a static neighbor entry for `FROM_ADDR` so that NUD trivially
    // succeeds if an ICMP dest unreachable message destined for the address
    // is generated.
    const FROM_ADDR: SpecifiedAddr<Ipv4Addr> = Ipv4::TEST_ADDRS.remote_ip;
    ctx.core_api()
        .neighbor::<Ipv4, _>()
        .insert_static_entry(&device_id, FROM_ADDR.get(), Ipv4::TEST_ADDRS.remote_mac.get())
        .expect("add static NUD entry for FROM_ADDR");

    ctx.test_api().set_unicast_forwarding_enabled::<Ipv4>(&device_id.clone().into(), true);

    // Receive an IPv4 packet with the per test-case fragment offset value.
    let to = Ipv4::get_other_ip_address(254);
    let mut ipv4_packet_builder =
        Ipv4PacketBuilder::new(FROM_ADDR, to, 255 /* ttl */, Ipv4Proto::Proto(IpProto::Udp));
    ipv4_packet_builder.fragment_offset(FragmentOffset::new(fragment_offset).unwrap());
    let non_initial_fragment_packet_buf = packet::Buf::new(&mut [], ..)
        .encapsulate(UdpPacketBuilder::new(
            FROM_ADDR.get(),
            to.get(),
            None,
            NonZeroU16::new(12345).unwrap(),
        ))
        .encapsulate(ipv4_packet_builder)
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b();
    ctx.test_api().receive_ip_packet::<Ipv4, _>(
        &device_id.into(),
        Some(FrameDestination::Individual { local: false }),
        non_initial_fragment_packet_buf,
    );

    // Should only see ICMP dest unreachable for initial fragments, i.e.
    // fragment offset equal to 0.
    while ctx.test_api().handle_queued_rx_packets()
        || CtxPairExt::trigger_next_timer(&mut ctx).is_some()
    {}
    ctx.bindings_ctx.with_fake_frame_ctx_mut(|ctx| {
        let found = ctx.take_frames().drain(..).find_map(|(_meta, buf)| {
            packet_formats::testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                Ipv4,
                _,
                IcmpDestUnreachable,
                _,
            >(&buf, packet_formats::ethernet::EthernetFrameLengthCheck::NoCheck, |_| ())
            .ok()
        });
        if fragment_offset == 0 {
            assert!(found.is_some());
        } else {
            assert_eq!(found, None);
        }
    })
}
