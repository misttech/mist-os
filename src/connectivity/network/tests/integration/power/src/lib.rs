// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

//! Netstack power framework integration tests.

use std::pin::pin;

use assert_matches::assert_matches;
use cm_rust::NativeIntoFidl as _;
use fidl::endpoints::{DiscoverableProtocolMarker as _, ServiceMarker as _};
use fidl::AsHandleRef;
use fuchsia_async::TimeoutExt as _;
use futures::stream::FusedStream;
use futures::{Stream, StreamExt as _};
use net_declare::{fidl_subnet, std_socket_addr_v6};
use netstack_testing_common::realms::{KnownServiceProvider, NetstackVersion};
use netstack_testing_common::ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT;
use netstack_testing_macros::netstack_test;
use packet::ParsablePacket as _;
use packet_formats::ip::{IpPacket, IpProto, Ipv6Proto};
use packet_formats::ipv6::Ipv6Packet;
use packet_formats::udp::{UdpPacket, UdpParseArgs};
use test_case::test_case;
use {
    fidl_fuchsia_hardware_network as fhardware_network, fidl_fuchsia_hardware_suspend as fhsuspend,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as finterfaces_ext, fidl_fuchsia_net_tun as fnet_tun,
    fidl_fuchsia_netemul as fnetemul, fidl_fuchsia_posix_socket as fposix_socket,
    fidl_fuchsia_power_broker as fpower_broker, fidl_fuchsia_power_system as fpower_system,
    fidl_test_sagcontrol as fsagcontrol, fidl_test_suspendcontrol as ftest_suspendcontrol,
    fuchsia_async as fasync,
};

// TODO(https://fxbug.dev/372010366): Revisit this test as we consider better integrating
// fake-suspend with fake SAG.
async fn set_up_default_suspender(device: &ftest_suspendcontrol::DeviceProxy) {
    device
        .set_suspend_states(&ftest_suspendcontrol::DeviceSetSuspendStatesRequest {
            suspend_states: Some(vec![fhsuspend::SuspendState {
                resume_latency: Some(0),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await
        .expect("fake-suspend set_suspend_states")
        .expect("fake-suspend set_suspend_states")
}

async fn create_power_realm<'a>(
    sandbox: &'a netemul::TestSandbox,
    name: &'a str,
    netstack_suspend_enabled: bool,
) -> netemul::TestRealm<'a> {
    const SUSPENDER_URL: &str = "#meta/fake-suspend.cm";
    const SUSPENDER_NAME: &str = "fake-suspend";

    const SAG_URL: &str = "#meta/fake-system-activity-governor.cm";
    const SAG_NAME: &str = "system-activity-governor";

    const PB_URL: &str = "#meta/power-broker.cm";
    const PB_NAME: &str = "power-broker";

    const CONFIG_USE_SUSPENDER_URL: &str = "config-use-suspender#meta/config-use-suspender.cm";
    const CONFIG_USE_SUSPENDER_NAME: &str = "config-use-suspender";
    const CONFIG_USE_SUSPENDER_CONFIG: &str = "fuchsia.power.UseSuspender";

    const CONFIG_NO_SUSPENDING_TOKEN_CONFIG: &str = "fuchsia.power.WaitForSuspendingToken";

    fn suspender_dep() -> fnetemul::Capability {
        fnetemul::Capability::ChildDep(fnetemul::ChildDep {
            name: Some(SUSPENDER_NAME.to_string()),
            capability: Some(fnetemul::ExposedCapability::Service(
                fhsuspend::SuspendServiceMarker::SERVICE_NAME.to_string(),
            )),
            ..Default::default()
        })
    }

    fn power_broker_dep() -> fnetemul::Capability {
        fnetemul::Capability::ChildDep(fnetemul::ChildDep {
            name: Some(PB_NAME.to_string()),
            capability: Some(fnetemul::ExposedCapability::Protocol(
                fpower_broker::TopologyMarker::PROTOCOL_NAME.to_string(),
            )),
            ..Default::default()
        })
    }

    let mut netstack_def: fnetemul::ChildDef =
        KnownServiceProvider::Netstack(NetstackVersion::Netstack3).into();
    // Add the dependencies on SAG and PB on top of what netemul is aware of.
    let fnetemul::ChildUses::Capabilities(netstack_uses) =
        netstack_def.uses.get_or_insert_with(|| fnetemul::ChildUses::Capabilities(vec![]));
    netstack_uses.push(power_broker_dep());
    netstack_uses.push(fnetemul::Capability::ChildDep(fnetemul::ChildDep {
        name: Some(SAG_NAME.to_string()),
        capability: Some(fnetemul::ExposedCapability::Protocol(
            fpower_system::ActivityGovernorMarker::PROTOCOL_NAME.to_string(),
        )),
        ..Default::default()
    }));
    netstack_uses.push(fnetemul::Capability::ChildDep(fnetemul::ChildDep {
        name: Some(SAG_NAME.to_string()),
        capability: Some(fnetemul::ExposedCapability::Protocol(
            fpower_system::BootControlMarker::PROTOCOL_NAME.to_string(),
        )),
        ..Default::default()
    }));
    netstack_def.config_values.get_or_insert_with(|| Default::default()).push(
        fnetemul::ChildConfigValue {
            key: "suspend_enabled".to_string(),
            value: cm_rust::ConfigValue::from(netstack_suspend_enabled).native_into_fidl(),
        },
    );

    let sag_def = fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(SAG_URL.to_string())),
        name: Some(SAG_NAME.to_string()),
        uses: Some(fnetemul::ChildUses::Capabilities(vec![
            fnetemul::Capability::LogSink(fnetemul::Empty {}),
            power_broker_dep(),
            suspender_dep(),
            fnetemul::Capability::ChildDep(fnetemul::ChildDep {
                name: Some(CONFIG_USE_SUSPENDER_NAME.to_string()),
                capability: Some(fnetemul::ExposedCapability::Configuration(
                    CONFIG_USE_SUSPENDER_CONFIG.to_string(),
                )),
                ..Default::default()
            }),
            fnetemul::Capability::ChildDep(fnetemul::ChildDep {
                name: None,
                capability: Some(fnetemul::ExposedCapability::Configuration(
                    CONFIG_NO_SUSPENDING_TOKEN_CONFIG.to_string(),
                )),
                ..Default::default()
            }),
        ])),
        exposes: Some(vec![
            fpower_system::ActivityGovernorMarker::PROTOCOL_NAME.to_string(),
            fpower_system::BootControlMarker::PROTOCOL_NAME.to_string(),
            fsagcontrol::StateMarker::PROTOCOL_NAME.to_string(),
        ]),
        ..Default::default()
    };

    let suspender_def = fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(SUSPENDER_URL.to_string())),
        name: Some(SUSPENDER_NAME.to_string()),
        uses: Some(fnetemul::ChildUses::Capabilities(vec![fnetemul::Capability::LogSink(
            fnetemul::Empty {},
        )])),
        exposes: Some(vec![ftest_suspendcontrol::DeviceMarker::PROTOCOL_NAME.to_string()]),
        ..Default::default()
    };

    let pb_def = fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(PB_URL.to_string())),
        name: Some(PB_NAME.to_string()),
        uses: Some(fnetemul::ChildUses::Capabilities(vec![fnetemul::Capability::LogSink(
            fnetemul::Empty {},
        )])),
        ..Default::default()
    };

    let sag_config_suspender_def = fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(CONFIG_USE_SUSPENDER_URL.to_string())),
        name: Some(CONFIG_USE_SUSPENDER_NAME.to_string()),
        ..Default::default()
    };

    let realm = sandbox
        .create_realm(
            name,
            [netstack_def, sag_def, suspender_def, pb_def, sag_config_suspender_def],
        )
        .expect("failed to create realm");

    // Start SAG and put it in a good state for all tests before we go starting
    // netstack.
    let sagctl =
        realm.connect_to_protocol::<fsagcontrol::StateMarker>().expect("connect to SAG ctl");
    let mut sag_state = pin!(execution_state_level_stream(&sagctl));
    sagctl
        .set(&fsagcontrol::SystemActivityGovernorState {
            application_activity_level: Some(fpower_system::ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .expect("SAG set")
        .expect("SAG set");
    assert_eq!(sag_state.next().await, Some(fpower_system::ExecutionStateLevel::Active));

    // Kick SAG out of boot mode.
    let boot_control = realm
        .connect_to_protocol::<fpower_system::BootControlMarker>()
        .expect("Couldn't connect to SAG");
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete");

    realm
}

fn extract_udp_frame_in_ipv6_packet(ipv6_frame: &[u8]) -> Option<&[u8]> {
    let mut buffer = ipv6_frame;
    let ipv6 = Ipv6Packet::parse(&mut buffer, ()).expect("failed to parse IPv6");
    if ipv6.proto() != Ipv6Proto::Proto(IpProto::Udp) {
        return None;
    }
    let udp = UdpPacket::parse(&mut buffer, UdpParseArgs::new(ipv6.src_ip(), ipv6.dst_ip()))
        .expect("failed to parse UDP");
    Some(udp.into_body())
}

fn execution_state_level_stream(
    sagctl: &fsagcontrol::StateProxy,
) -> impl Stream<Item = fpower_system::ExecutionStateLevel> + FusedStream + '_ {
    futures::stream::unfold(None, move |prev| async move {
        loop {
            let execution_level = sagctl
                .watch()
                .await
                .expect("sagctl watch")
                .execution_state_level
                .expect("missing execution state level");
            if prev.as_ref().map(|l| l != &execution_level).unwrap_or(true) {
                break Some((execution_level, Some(execution_level)));
            }
        }
    })
    .fuse()
}

#[netstack_test]
#[test_case(true; "suspend enabled")]
#[test_case(false; "suspend disabled")]
async fn tx_suspension(name: &str, netstack_suspend_enabled: bool) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = create_power_realm(&sandbox, name, netstack_suspend_enabled).await;

    let suspend_device = realm
        .connect_to_protocol::<ftest_suspendcontrol::DeviceMarker>()
        .expect("Couldn't connect to suspend device");
    set_up_default_suspender(&suspend_device).await;

    let sagctl =
        realm.connect_to_protocol::<fsagcontrol::StateMarker>().expect("connect to SAG ctl");
    let mut sag_state = pin!(execution_state_level_stream(&sagctl));
    assert_eq!(sag_state.next().await, Some(fpower_system::ExecutionStateLevel::Active));

    let (tun_device, device) =
        netstack_testing_common::devices::create_tun_device_with(fnet_tun::DeviceConfig {
            blocking: Some(true),
            ..Default::default()
        });
    let (tun_port, port) = netstack_testing_common::devices::create_ip_tun_port(
        &tun_device,
        netstack_testing_common::devices::TUN_DEFAULT_PORT_ID,
    )
    .await;
    tun_port.set_online(true).await.expect("set online");

    let mut udp_frame_stream = pin!(futures::stream::unfold((), |()| async {
        loop {
            let fnet_tun::Frame { data, frame_type, .. } =
                tun_device.read_frame().await.expect("FIDL error").expect("read frame error");
            let data = data.unwrap();
            let frame_type = frame_type.unwrap();
            if frame_type != fhardware_network::FrameType::Ipv6 {
                continue;
            }
            if let Some(udp_frame) = extract_udp_frame_in_ipv6_packet(&data) {
                break Some((udp_frame.to_vec(), ()));
            }
        }
    })
    .fuse());

    let device_control = netstack_testing_common::devices::install_device(&realm, device);
    let interface_control = finterfaces_ext::admin::Control::new(
        netstack_testing_common::devices::add_pure_ip_interface(&port, &device_control, name).await,
    );
    let if_id = interface_control.get_id().await.expect("get id");
    assert_matches!(interface_control.enable().await, Ok(Ok(true)));

    let src = fidl_subnet!("fe80::1/64");
    let mut dst = std_socket_addr_v6!("[fe80::2]:1010");
    dst.set_scope_id(if_id.try_into().unwrap());

    // Add a local IP address so we can send some traffic.
    let _asp = netstack_testing_common::interfaces::add_address_wait_assigned(
        &interface_control,
        src,
        Default::default(),
    )
    .await
    .expect("add addr");
    let sock = realm
        .datagram_socket(fposix_socket::Domain::Ipv6, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("create socket");

    let payload = &[1, 2, 3][..];
    assert_eq!(sock.send_to(payload, &dst.into()).expect("sendto"), payload.len());
    // Wait for the frame to show up in tun.
    assert_eq!(udp_frame_stream.next().await.unwrap(), payload);

    // Send another frame and make sure it's ready to be read by tun.
    let payload = &[4, 5, 6][..];
    assert_eq!(sock.send_to(payload, &dst.into()).expect("sendto"), payload.len());
    let tun_signals = tun_device.get_signals().await.expect("get tun signals");
    let _: zx::Signals = fasync::OnSignals::new(
        &tun_signals,
        zx::Signals::from_bits(fnet_tun::Signals::READABLE.bits()).unwrap(),
    )
    .await
    .expect("waiting readable");

    // Allow the system to go through suspension now.
    sagctl
        .set(&fsagcontrol::SystemActivityGovernorState {
            execution_state_level: Some(fpower_system::ExecutionStateLevel::Inactive),
            application_activity_level: Some(fpower_system::ApplicationActivityLevel::Inactive),
            ..Default::default()
        })
        .await
        .expect("SAG set")
        .expect("SAG set");

    if netstack_suspend_enabled {
        // When suspension is enabled, netstack is holding the system up.

        // TODO(https://fxbug.dev/367774549): We could do better than a timeout
        // here, we're just trying to guarantee that netstack is holding the system
        // from suspension. Ideally we'd observe the required and current power
        // levels of the tx power element instead.
        assert_eq!(
            sag_state
                .next()
                .on_timeout(
                    fasync::MonotonicInstant::after(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT),
                    || None
                )
                .await,
            None
        );
    } else {
        // When netstack suspension is disabled the system goes immediately to
        // inactive.
        assert_eq!(sag_state.next().await, Some(fpower_system::ExecutionStateLevel::Inactive));
    }

    // Now just drain the tun interface until we see the system going inactive
    // (in the suspend enabled case).
    assert_eq!(udp_frame_stream.next().await.unwrap(), payload);

    if netstack_suspend_enabled {
        // Keep polling UDP state stream until we observe inactive in case there
        // were other netstack-originated frames here, but there should be no UDP
        // traffic.
        let system_state = futures::select! {
            s = sag_state.next() => s,
            v = udp_frame_stream.next() => panic!("unexpected extra UDP frame {v:?}"),
        };
        assert_eq!(system_state, Some(fpower_system::ExecutionStateLevel::Inactive));
        assert_eq!(suspend_device.await_suspend().await.unwrap().unwrap().state_index, Some(0));
    }

    // Send more data over the socket.
    let payload = &[7, 8, 9][..];
    assert_eq!(sock.send_to(payload, &dst.into()).expect("sendto"), payload.len());

    if netstack_suspend_enabled {
        // While the system is inactive we should not observe netstack
        // attempting to send anything over the device.
        assert_eq!(
            udp_frame_stream
                .next()
                .on_timeout(
                    fasync::MonotonicInstant::after(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT),
                    || None
                )
                .await,
            None,
        );

        // But it does come out when the system wakes back up from suspension.
        suspend_device
            .resume(&ftest_suspendcontrol::DeviceResumeRequest::Result(
                ftest_suspendcontrol::SuspendResult { ..Default::default() },
            ))
            .await
            .expect("fake-suspend resume")
            .expect("fake-suspend resume");
        sagctl
            .set(&fsagcontrol::SystemActivityGovernorState {
                execution_state_level: Some(fpower_system::ExecutionStateLevel::Active),
                application_activity_level: Some(fpower_system::ApplicationActivityLevel::Active),
                ..Default::default()
            })
            .await
            .expect("SAG set")
            .expect("SAG set");

        assert_eq!(udp_frame_stream.next().await.unwrap(), payload);
    } else {
        // In the no suspension case, the payload should be available even if
        // SAG is still in the inactive state, because netstack is not observing
        // suspension.
        assert_eq!(udp_frame_stream.next().await.unwrap(), payload);
    }
}

#[netstack_test]
#[test_case(true; "suspend enabled")]
#[test_case(false; "suspend disabled")]
async fn rx_lease_drops(name: &str, netstack_suspend_enabled: bool) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = create_power_realm(&sandbox, name, netstack_suspend_enabled).await;
    let (tun_device, device) =
        netstack_testing_common::devices::create_tun_device_with(fnet_tun::DeviceConfig {
            blocking: Some(true),
            ..Default::default()
        });
    let (tun_port, port) = netstack_testing_common::devices::create_ip_tun_port(
        &tun_device,
        netstack_testing_common::devices::TUN_DEFAULT_PORT_ID,
    )
    .await;
    tun_port.set_online(true).await.expect("set online");
    let device_control = netstack_testing_common::devices::install_device(&realm, device);
    let interface_control = finterfaces_ext::admin::Control::new(
        netstack_testing_common::devices::add_pure_ip_interface(&port, &device_control, name).await,
    );
    let if_id = interface_control.get_id().await.expect("get id");
    assert_matches!(interface_control.enable().await, Ok(Ok(true)));

    let interfaces_state =
        realm.connect_to_protocol::<fnet_interfaces::StateMarker>().expect("connect to protocol");
    netstack_testing_common::interfaces::wait_for_online(&interfaces_state, if_id, true)
        .await
        .expect("wait online");

    for packet_number in 1..=4 {
        let (lease, send_lease) = zx::Channel::create();
        let frame = fnet_tun::Frame {
            frame_type: Some(fhardware_network::FrameType::Ipv4),
            // NB: We don't need to send a proper packet, we just need it to
            // make it to netstack.
            data: Some(vec![0x01, 0x02, 0x03, 0x04]),
            port: Some(netstack_testing_common::devices::TUN_DEFAULT_PORT_ID),
            ..Default::default()
        };
        let delegated_lease = fhardware_network::DelegatedRxLease {
            handle: Some(fhardware_network::DelegatedRxLeaseHandle::Channel(send_lease)),
            hold_until_frame: Some(packet_number),
            ..Default::default()
        };

        // Make the test more interesting by changing the order of things
        // reaching tun.
        if packet_number % 2 == 0 {
            tun_device.delegate_rx_lease(delegated_lease).expect("delegate lease");
            tun_device.write_frame(&frame).await.expect("write frame").expect("write frame error");
        } else {
            tun_device.write_frame(&frame).await.expect("write frame").expect("write frame error");
            tun_device.delegate_rx_lease(delegated_lease).expect("delegate lease");
        };
        // Lease should always be dropped because netdevice drops leases even
        // when netstack is not subscribed to it.
        assert_eq!(
            lease
                .wait_handle(zx::Signals::CHANNEL_PEER_CLOSED, zx::MonotonicInstant::INFINITE)
                .expect("wait closed"),
            zx::Signals::CHANNEL_PEER_CLOSED
        );

        // Check that netstack was the one dropping the lease via inspect. It's
        // sad to depend on the inspect interface here, but we're guaranteeing
        // this is not racy because we're waiting for the lease to be closed
        // above before checking.
        let expect_inspect_value = if netstack_suspend_enabled { packet_number } else { 0 };
        let property = netstack_testing_common::get_inspect_property(
            &realm,
            "netstack",
            "root/Counters/Bindings/Power:DroppedRxLeases",
            netstack_testing_common::constants::inspect::DEFAULT_INSPECT_TREE_NAME,
        )
        .await
        .expect("getting inspect property");
        assert_eq!(property.uint(), Some(expect_inspect_value));
    }
}
