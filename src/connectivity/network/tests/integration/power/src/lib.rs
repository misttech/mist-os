// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

//! Netstack power framework integration tests.

use std::pin::pin;

use assert_matches::assert_matches;
use cm_rust::NativeIntoFidl as _;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fuchsia_async::TimeoutExt as _;
use futures::StreamExt as _;
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
    fidl_fuchsia_hardware_network as fhardware_network,
    fidl_fuchsia_net_interfaces_ext as finterfaces_ext, fidl_fuchsia_net_tun as fnet_tun,
    fidl_fuchsia_netemul as fnetemul, fidl_fuchsia_posix_socket as fposix_socket,
    fidl_fuchsia_power_broker as fpower_broker, fidl_fuchsia_power_system as fpower_system,
    fidl_test_sagcontrol as fsagcontrol, fuchsia_async as fasync, fuchsia_zircon as zx,
};

async fn create_power_realm<'a>(
    sandbox: &'a netemul::TestSandbox,
    name: &'a str,
    netstack_suspend_enabled: bool,
) -> netemul::TestRealm<'a> {
    const SAG_URL: &str = "#meta/fake-system-activity-governor.cm";
    const SAG_NAME: &str = "system-activity-governor";

    const PB_URL: &str = "#meta/power-broker.cm";
    const PB_NAME: &str = "power-broker";

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
        ])),
        exposes: Some(vec![
            fpower_system::ActivityGovernorMarker::PROTOCOL_NAME.to_string(),
            fsagcontrol::StateMarker::PROTOCOL_NAME.to_string(),
        ]),
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

    sandbox.create_realm(name, [netstack_def, sag_def, pb_def]).expect("failed to create realm")
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

#[netstack_test]
#[test_case(true; "suspend enabled")]
#[test_case(false; "suspend disabled")]
async fn tx_suspension(name: &str, netstack_suspend_enabled: bool) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");

    let realm = create_power_realm(&sandbox, name, netstack_suspend_enabled).await;

    let sagctl =
        realm.connect_to_protocol::<fsagcontrol::StateMarker>().expect("connect to SAG ctl");
    let sagctl = &sagctl;
    let mut sag_state = pin!(futures::stream::unfold(None, |prev| async move {
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
    .fuse());

    // Kick SAG out of boot mode.
    sagctl
        .set(&fsagcontrol::SystemActivityGovernorState {
            application_activity_level: Some(fpower_system::ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .expect("SAG set")
        .expect("SAG set");

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
                .on_timeout(fasync::Time::after(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT), || None)
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
                .on_timeout(fasync::Time::after(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT), || None)
                .await,
            None,
        );

        // But it does come out when the system wakes back up from suspension.
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
