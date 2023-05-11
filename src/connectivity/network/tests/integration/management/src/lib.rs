// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

pub mod virtualization;

use std::{collections::HashMap, num::NonZeroU16};

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_dhcpv6 as fnet_dhcpv6;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use fuchsia_zircon as zx;

use anyhow::Context as _;
use fidl::endpoints::Proxy as _;
use futures::{
    future::{FutureExt as _, LocalBoxFuture, TryFutureExt as _},
    stream::{self, StreamExt as _, TryStreamExt as _},
};
use net_declare::{fidl_ip_v4, net_ip_v6, net_subnet_v6};
use net_types::{ethernet::Mac, ip as net_types_ip};
use netstack_testing_common::{
    interfaces,
    realms::{
        KnownServiceProvider, Manager, ManagerConfig, Netstack, TestRealmExt as _, TestSandboxExt,
    },
    try_all, try_any, wait_for_component_stopped, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::{EmptyBuf, InnerPacketBuilder as _, ParsablePacket as _, Serializer as _};
use packet_formats::{
    ethernet::{
        EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck,
        ETHERNET_MIN_BODY_LEN_NO_TAG,
    },
    ip::{IpProto, Ipv6Proto},
    ipv6::Ipv6PacketBuilder,
    testutil::parse_ip_packet,
    udp::{UdpPacket, UdpPacketBuilder, UdpParseArgs},
};
use packet_formats_dhcp::v6 as dhcpv6;
use test_case::test_case;

async fn with_netcfg_owned_device<
    M: Manager,
    N: Netstack,
    F: for<'a> FnOnce(
        u64,
        &'a netemul::TestNetwork<'a>,
        &'a fnet_interfaces::StateProxy,
        &'a netemul::TestRealm<'a>,
    ) -> LocalBoxFuture<'a, ()>,
>(
    name: &str,
    manager_config: ManagerConfig,
    with_dhcpv6_client: bool,
    after_interface_up: F,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            [
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: manager_config,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ]
            .iter()
            .chain(with_dhcpv6_client.then_some(&KnownServiceProvider::Dhcpv6Client)),
        )
        .expect("create netstack realm");

    // Add a device to the realm.
    let network = sandbox.create_network(name).await.expect("create network");
    let endpoint = network.create_endpoint(name).await.expect("create endpoint");
    let () = endpoint.set_link_up(true).await.expect("set link up");
    let endpoint_mount_path = netemul::devfs_device_path("ep");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    let () = realm.add_virtual_device(&endpoint, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device {}: {:?}", endpoint_mount_path.display(), e)
    });

    // Make sure the Netstack got the new device added.
    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None).fuse();
    futures::pin_mut!(wait_for_netmgr);
    let (if_id, _if_name): (u64, String) = interfaces::wait_for_non_loopback_interface_up(
        &interface_state,
        &mut wait_for_netmgr,
        None,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("wait for non loopback interface");

    let () = after_interface_up(if_id, &network, &interface_state, &realm).await;

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    realm.shutdown().await.expect("failed to shutdown realm");
}

/// Test that NetCfg discovers a newly added device and it adds the device
/// to the Netstack.
#[netstack_test]
async fn test_oir<M: Manager, N: Netstack>(name: &str) {
    with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Empty,
        false, /* with_dhcpv6_client */
        |_if_id: u64,
         _: &netemul::TestNetwork<'_>,
         _: &fnet_interfaces::StateProxy,
         _: &netemul::TestRealm<'_>| async {}.boxed_local(),
    )
    .await
}

/// Tests that stable interface name conflicts are handled gracefully.
#[netstack_test]
async fn test_oir_interface_name_conflict<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Empty,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");

    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None);

    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let interfaces_stream =
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interface_state)
            .expect("get interface event stream")
            .map(|r| r.expect("watcher error"))
            .filter_map(|event| {
                futures::future::ready(match event {
                    fidl_fuchsia_net_interfaces::Event::Added(
                        fidl_fuchsia_net_interfaces::Properties { id, name, .. },
                    )
                    | fidl_fuchsia_net_interfaces::Event::Existing(
                        fidl_fuchsia_net_interfaces::Properties { id, name, .. },
                    ) => Some((
                        id.expect("missing interface ID"),
                        name.expect("missing interface name"),
                    )),
                    fidl_fuchsia_net_interfaces::Event::Removed(id) => {
                        let _: u64 = id;
                        None
                    }
                    fidl_fuchsia_net_interfaces::Event::Idle(
                        fidl_fuchsia_net_interfaces::Empty {},
                    )
                    | fidl_fuchsia_net_interfaces::Event::Changed(
                        fidl_fuchsia_net_interfaces::Properties { .. },
                    ) => None,
                })
            });
    let interfaces_stream = futures::stream::select(
        interfaces_stream,
        futures::stream::once(wait_for_netmgr.map(|r| panic!("network manager exited {:?}", r))),
    )
    .fuse();
    futures::pin_mut!(interfaces_stream);
    // Observe the initially existing loopback interface.
    let _: (u64, String) = interfaces_stream.select_next_some().await;

    // Add a device to the realm and wait for it to be added to the netstack.
    //
    // Non PCI and USB devices get their interface names from their MAC addresses.
    // Using the same MAC address for different devices will result in the same
    // interface name.
    let mac = || Some(fnet::MacAddress { octets: [2, 3, 4, 5, 6, 7] });
    let ethx7 = sandbox
        .create_endpoint_with("ep1", netemul::new_endpoint_config(1500, mac()))
        .await
        .expect("create ethx7");
    let endpoint_mount_path = netemul::devfs_device_path("ep1");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    let () = realm.add_virtual_device(&ethx7, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device1 {}: {:?}", endpoint_mount_path.display(), e)
    });

    let (id_ethx7, name_ethx7) = interfaces_stream.select_next_some().await;
    assert_eq!(
        &name_ethx7, "ethx7",
        "first interface should use a stable name based on its MAC address"
    );

    // Create an interface that the network manager does not know about that will cause a
    // name conflict with the first temporary name.
    let name = "etht0";
    let etht0 = sandbox.create_endpoint(name).await.expect("create eth0");
    let etht0 = etht0
        .into_interface_in_realm_with_name(
            &realm,
            netemul::InterfaceConfig { name: Some(name.into()), ..Default::default() },
        )
        .await
        .expect("install interface");
    let netstack_id_etht0 = etht0.id();

    let (id_etht0, name_etht0) = interfaces_stream.select_next_some().await;
    assert_eq!(id_etht0, u64::from(netstack_id_etht0));
    assert_eq!(&name_etht0, "etht0");

    // Add another device from the network manager with the same MAC address and wait for it
    // to be added to the netstack. Its first two attempts at adding a name should conflict
    // with the above two devices.
    let etht1 = sandbox
        .create_endpoint_with("ep2", netemul::new_endpoint_config(1500, mac()))
        .await
        .expect("create etht1");
    let endpoint_mount_path = netemul::devfs_device_path("ep2");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    let () = realm.add_virtual_device(&etht1, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device2 {}: {:?}", endpoint_mount_path.display(), e)
    });
    let (id_etht1, name_etht1) = interfaces_stream.select_next_some().await;
    assert_ne!(id_ethx7, id_etht1, "interface IDs should be different");
    assert_ne!(id_etht0, id_etht1, "interface IDs should be different");
    assert_eq!(
        &name_etht1, "etht1",
        "second interface from network manager should use a temporary name"
    );

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    let () = realm.shutdown().await.expect("failed to shutdown realm");
}

/// Make sure the DHCP server is configured to start serving requests when NetCfg discovers
/// a WLAN AP interface and stops serving when the interface is removed.
///
/// Also make sure that a new WLAN AP interface may be added after a previous interface has been
/// removed from the netstack.
#[netstack_test]
async fn test_wlan_ap_dhcp_server<M: Manager, N: Netstack>(name: &str) {
    // Use a large timeout to check for resolution.
    //
    // These values effectively result in a large timeout of 60s which should avoid
    // flakes. This test was run locally 100 times without flakes.
    /// Duration to sleep between polls.
    const POLL_WAIT: zx::Duration = zx::Duration::from_seconds(1);
    /// Maximum number of times we'll poll the DHCP server to check its parameters.
    const RETRY_COUNT: u64 = 120;

    /// Check if the DHCP server is started.
    async fn check_dhcp_status(dhcp_server: &fnet_dhcp::Server_Proxy, started: bool) {
        for _ in 0..RETRY_COUNT {
            let () = fuchsia_async::Timer::new(POLL_WAIT.after_now()).await;

            if started == dhcp_server.is_serving().await.expect("query server status request") {
                return;
            }
        }

        panic!("timed out checking DHCP server status");
    }

    /// Make sure the DHCP server is configured to start serving requests when NetCfg discovers
    /// a WLAN AP interface and stops serving when the interface is removed.
    ///
    /// When `wlan_ap_dhcp_server_inner` returns successfully, the interface that it creates will
    /// have been removed.
    async fn wlan_ap_dhcp_server_inner<'a>(
        sandbox: &'a netemul::TestSandbox,
        realm: &netemul::TestRealm<'a>,
        offset: u8,
    ) {
        // These constants are all hard coded in NetCfg for the WLAN AP interface and
        // the DHCP server.
        const DHCP_LEASE_TIME: u32 = 24 * 60 * 60; // 1 day in seconds.
        const NETWORK_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.248");
        const NETWORK_PREFIX_LEN: u8 = 29;
        const INTERFACE_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.249");
        const DHCP_POOL_START_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.250");
        const DHCP_POOL_END_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.254");
        const NETWORK_ADDR_SUBNET: net_types_ip::Subnet<net_types_ip::Ipv4Addr> = unsafe {
            net_types_ip::Subnet::new_unchecked(
                net_types_ip::Ipv4Addr::new(NETWORK_ADDR.addr),
                NETWORK_PREFIX_LEN,
            )
        };

        // Add a device to the realm that looks like a WLAN AP from the
        // perspective of NetCfg. The topological path for the interface must
        // include "wlanif-ap" as that is how NetCfg identifies a WLAN AP
        // interface.
        let network = sandbox
            .create_network(format!("dhcp-server-{}", offset))
            .await
            .expect("create network");
        let wlan_ap = network
            .create_endpoint(format!("wlanif-ap-dhcp-server-{}", offset))
            .await
            .expect("create wlan ap");
        let path = netemul::devfs_device_path(&format!("dhcp-server-ep-{}", offset));
        let () = realm
            .add_virtual_device(&wlan_ap, path.as_path())
            .await
            .unwrap_or_else(|e| panic!("add WLAN AP virtual device {}: {:?}", path.display(), e));
        let () = wlan_ap.set_link_up(true).await.expect("set wlan ap link up");

        // Make sure the WLAN AP interface is added to the Netstack and is brought up with
        // the right IP address.
        let interface_state = realm
            .connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("connect to fuchsia.net.interfaces/State service");
        let event_stream =
            fidl_fuchsia_net_interfaces_ext::event_stream_from_state(&interface_state)
                .expect("get interface event stream");
        futures::pin_mut!(event_stream);
        let mut if_map = HashMap::<u64, _>::new();
        let (wlan_ap_id, wlan_ap_name) = fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| {
                if_map.iter().find_map(
                    |(
                        id,
                        fidl_fuchsia_net_interfaces_ext::Properties {
                            name, online, addresses, ..
                        },
                    )| {
                        (*online
                            && addresses.iter().any(
                                |&fidl_fuchsia_net_interfaces_ext::Address {
                                     addr: fnet::Subnet { addr, prefix_len: _ },
                                     valid_until: _,
                                 }| {
                                    addr == INTERFACE_ADDR.into_ext()
                                },
                            ))
                        .then(|| (*id, name.clone()))
                    },
                )
            },
        )
        .map_err(anyhow::Error::from)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("timed out"))
        })
        .await
        .expect("failed to wait for presence of a WLAN AP interface");

        // Check the DHCP server's configured parameters.
        let dhcp_server = realm
            .connect_to_protocol::<fnet_dhcp::Server_Marker>()
            .expect("connect to DHCP server service");
        let checks = [
            (
                fnet_dhcp::ParameterName::IpAddrs,
                fnet_dhcp::Parameter::IpAddrs(vec![INTERFACE_ADDR]),
            ),
            (
                fnet_dhcp::ParameterName::LeaseLength,
                fnet_dhcp::Parameter::Lease(fnet_dhcp::LeaseLength {
                    default: Some(DHCP_LEASE_TIME),
                    max: Some(DHCP_LEASE_TIME),
                    ..Default::default()
                }),
            ),
            (
                fnet_dhcp::ParameterName::BoundDeviceNames,
                fnet_dhcp::Parameter::BoundDeviceNames(vec![wlan_ap_name]),
            ),
            (
                fnet_dhcp::ParameterName::AddressPool,
                fnet_dhcp::Parameter::AddressPool(fnet_dhcp::AddressPool {
                    prefix_length: Some(NETWORK_PREFIX_LEN),
                    range_start: Some(DHCP_POOL_START_ADDR),
                    range_stop: Some(DHCP_POOL_END_ADDR),
                    ..Default::default()
                }),
            ),
        ];

        let dhcp_server_ref = &dhcp_server;
        let checks_ref = &checks;
        if !try_any(stream::iter(0..RETRY_COUNT).then(|i| async move {
            let () = fuchsia_async::Timer::new(POLL_WAIT.after_now()).await;
            try_all(stream::iter(checks_ref.iter()).then(|(param_name, param_value)| async move {
                Ok(dhcp_server_ref
                    .get_parameter(*param_name)
                    .await
                    .unwrap_or_else(|e| panic!("get {:?} parameter request: {:?}", param_name, e))
                    .unwrap_or_else(|e| {
                        panic!(
                            "error getting {:?} parameter: {}",
                            param_name,
                            zx::Status::from_raw(e)
                        )
                    })
                    == *param_value)
            }))
            .await
            .with_context(|| format!("{}-th iteration checking DHCP parameters", i))
        }))
        .await
        .expect("checking DHCP parameters")
        {
            // Too many retries.
            panic!("timed out waiting for DHCP server configurations");
        }

        // The DHCP server should be started.
        let () = check_dhcp_status(&dhcp_server, true).await;

        // Add a host endpoint to the network. It should be configured by the DHCP server.
        let host = network
            .create_endpoint(format!("host-dhcp-client-{}", offset))
            .await
            .expect("create host");
        let path = netemul::devfs_device_path(&format!("dhcp-client-ep-{}", offset));
        let () = realm
            .add_virtual_device(&host, path.as_path())
            .await
            .unwrap_or_else(|e| panic!("add host virtual device {}: {:?}", path.display(), e));
        let () = host.set_link_up(true).await.expect("set host link up");
        let host_id = fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| {
                if_map.iter().find_map(
                    |(
                        id,
                        fidl_fuchsia_net_interfaces_ext::Properties { online, addresses, .. },
                    )| {
                        (*id != wlan_ap_id
                            && *online
                            && addresses.iter().any(
                                |&fidl_fuchsia_net_interfaces_ext::Address {
                                     addr: fnet::Subnet { addr, prefix_len: _ },
                                     valid_until: _,
                                 }| match addr {
                                    fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr }) => {
                                        NETWORK_ADDR_SUBNET
                                            .contains(&net_types_ip::Ipv4Addr::new(addr))
                                    }
                                    fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: _ }) => false,
                                },
                            ))
                        .then_some(*id)
                    },
                )
            },
        )
        .map_err(anyhow::Error::from)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("timed out"))
        })
        .await
        .expect("wait for host interface to be configured");

        // Take the interface down, the DHCP server should be stopped.
        let () = wlan_ap.set_link_up(false).await.expect("set wlan ap link down");
        let () = check_dhcp_status(&dhcp_server, false).await;

        // Bring the interface back up, the DHCP server should be started.
        let () = wlan_ap.set_link_up(true).await.expect("set wlan ap link up");
        let () = check_dhcp_status(&dhcp_server, true).await;
        // Remove the interface, the DHCP server should be stopped.
        drop(wlan_ap);
        let () = check_dhcp_status(&dhcp_server, false).await;

        // Remove the host endpoint from the network and wait for the interface
        // to be removed from the netstack.
        //
        // This is necessary to ensure this function can be called multiple
        // times and observe DHCP address acquisition on a new interface each
        // time.
        drop(host);
        let () = fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| (!if_map.contains_key(&host_id)).then_some(()),
        )
        .await
        .expect("wait for host interface to be removed");
    }

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: true,
                    config: ManagerConfig::Empty,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::DhcpServer { persistent: false },
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("create netstack realm");
    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None).fuse();
    futures::pin_mut!(wait_for_netmgr);

    // Add a WLAN AP, make sure the DHCP server gets configured and starts or
    // stops when the interface is added and brought up or brought down/removed.
    // A loop is used to emulate interface flaps.
    for i in 0..=1 {
        let test_fut = wlan_ap_dhcp_server_inner(&sandbox, &realm, i).fuse();
        futures::pin_mut!(test_fut);
        let () = futures::select! {
            () = test_fut => {},
            stopped_event = wait_for_netmgr => {
                panic!(
                    "NetCfg unexpectedly exited with exit status = {:?}",
                    stopped_event
                );
            }
        };
    }
}

/// Tests that netcfg observes component stop events and exits cleanly.
#[netstack_test]
async fn observes_stop_events<M: Manager, N: Netstack>(name: &str) {
    use component_events::events::{self};

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Empty,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");
    let mut event_stream = events::EventStream::open_at_path("/events/started_stopped")
        .await
        .expect("subscribe to events");

    let event_matcher = netstack_testing_common::get_child_component_event_matcher(
        &realm,
        M::MANAGEMENT_AGENT.get_component_name(),
    )
    .await
    .expect("get child moniker");

    // Wait for netcfg to start.
    let events::StartedPayload {} = event_matcher
        .clone()
        .wait::<events::Started>(&mut event_stream)
        .await
        .expect("got started event")
        .result()
        .expect("error event on started");

    let () = realm.shutdown().await.expect("shutdown");

    let event =
        event_matcher.wait::<events::Stopped>(&mut event_stream).await.expect("got stopped event");
    // NB: event::result below borrows from event, it needs to be in a different
    // statement.
    let events::StoppedPayload { status } = event.result().expect("error event on stopped");
    assert_matches::assert_matches!(status, events::ExitStatus::Clean);
}

/// Test that NetCfg enables forwarding on interfaces when the device class is configured to have
/// that enabled.
#[netstack_test]
async fn test_forwarding<M: Manager, N: Netstack>(name: &str) {
    with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Forwarding,
        false, /* with_dhcpv6_client */
        |if_id, _: &netemul::TestNetwork<'_>, _: &fnet_interfaces::StateProxy, realm| {
            async move {
                let control = realm
                    .interface_control(if_id)
                    .expect("connect to fuchsia.net.interfaces.admin/Control for new interface");

                // The configuration installs forwarding on v4 on Virtual interfaces
                // and v6 on Ethernet. We should only observe the configuration to be
                // installed on v4 because the device installed by this test doesn't
                // match the Ethernet device class.
                assert_eq!(
                    control.get_configuration().await.expect("get_configuration FIDL error"),
                    Ok(fnet_interfaces_admin::Configuration {
                        ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                            forwarding: Some(true),
                            multicast_forwarding: Some(true),
                            igmp: Some(fnet_interfaces_admin::IgmpConfiguration {
                                version: Some(fnet_interfaces_admin::IgmpVersion::V3),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                            forwarding: Some(false),
                            multicast_forwarding: Some(false),
                            mld: Some(fnet_interfaces_admin::MldConfiguration {
                                version: Some(fnet_interfaces_admin::MldVersion::V2),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                );
            }
            .boxed_local()
        },
    )
    .await
}

#[netstack_test]
async fn test_prefix_provider_not_supported<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Empty,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    // Attempt to Acquire a prefix when DHCPv6 is not supported (DHCPv6 client
    // is not made available to netcfg).
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider
        .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
        .expect("acquire prefix");
    assert_eq!(
        prefix_control
            .take_event_stream()
            .map_ok(fnet_dhcpv6::PrefixControlEvent::into_on_exit)
            .try_collect::<Vec<_>>()
            .await
            .expect("collect event stream")[..],
        [Some(fnet_dhcpv6::PrefixControlExitReason::NotSupported)],
    );
}

// TODO(https://fxbug.dev/114132): Remove this test when multiple clients
// requesting prefixes is supported.
#[netstack_test]
async fn test_prefix_provider_already_acquiring<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Dhcpv6,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    {
        // Acquire a prefix.
        let (_prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
        prefix_provider
            .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
            .expect("acquire prefix");

        // Calling acquire_prefix a second time results in ALREADY_ACQUIRING.
        {
            let (prefix_control, server_end) =
                fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                    .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
            prefix_provider
                .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
                .expect("acquire prefix");
            let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
                .take_event_stream()
                .try_next()
                .await
                .expect("next PrefixControl event")
                .expect("PrefixControl event stream ended");
            assert_eq!(reason, fnet_dhcpv6::PrefixControlExitReason::AlreadyAcquiring);
        }

        // The PrefixControl channel is dropped here.
    }

    // Retry acquire_prefix in a loop (server may take some time to notice PrefixControl
    // closure) and expect that it succeeds eventually.
    loop {
        let (prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
        prefix_provider
            .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
            .expect("acquire prefix");
        match prefix_control
            .take_event_stream()
            .next()
            .map(Some)
            .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None)
            .await
        {
            None => {
                assert!(!prefix_control.is_closed());
                break;
            }
            Some(item) => {
                assert_matches::assert_matches!(
                    item,
                    Some(Ok(fnet_dhcpv6::PrefixControlEvent::OnExit {
                        reason: fnet_dhcpv6::PrefixControlExitReason::AlreadyAcquiring,
                    }))
                );
            }
        }
    }
}

#[netstack_test]
#[test_case(
    fnet_dhcpv6::AcquirePrefixConfig {
        interface_id: Some(42),
        ..Default::default()
    },
    fnet_dhcpv6::PrefixControlExitReason::InvalidInterface;
    "interface not found"
)]
#[test_case(
    fnet_dhcpv6::AcquirePrefixConfig {
        preferred_prefix_len: Some(129),
        ..Default::default()
    },
    fnet_dhcpv6::PrefixControlExitReason::InvalidPrefixLength;
    "invalid prefix length"
)]
async fn test_prefix_provider_config_error<M: Manager, N: Netstack>(
    name: &str,
    config: fnet_dhcpv6::AcquirePrefixConfig,
    want_reason: fnet_dhcpv6::PrefixControlExitReason,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Dhcpv6,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider.acquire_prefix(&config, server_end).expect("acquire prefix");
    let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
        .take_event_stream()
        .try_next()
        .await
        .expect("next PrefixControl event")
        .expect("PrefixControl event stream ended");
    assert_eq!(reason, want_reason);
}

#[netstack_test]
async fn test_prefix_provider_double_watch<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    config: ManagerConfig::Dhcpv6,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    // Acquire a prefix.
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider
        .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
        .expect("acquire prefix");

    let (res1, res2) =
        futures::future::join(prefix_control.watch_prefix(), prefix_control.watch_prefix()).await;
    for res in [res1, res2] {
        assert_matches::assert_matches!(res, Err(fidl::Error::ClientChannelClosed { status, .. }) => {
            assert_eq!(status, zx::Status::PEER_CLOSED);
        });
    }
    let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
        .take_event_stream()
        .try_next()
        .await
        .expect("next PrefixControl event")
        .expect("PrefixControl event stream ended");
    assert_eq!(reason, fnet_dhcpv6::PrefixControlExitReason::DoubleWatch);

    // TODO(https://fxbug.dev/74241): Cannot expected `is_closed` to return true
    // even though PEER_CLOSED has already been observed on the channel.
    assert_eq!(prefix_control.on_closed().await, Ok(zx::Signals::CHANNEL_PEER_CLOSED));
    assert!(prefix_control.is_closed());
}

#[netstack_test]
async fn test_prefix_provider_full_integration<M: Manager, N: Netstack>(name: &str) {
    const SERVER_ADDR: net_types_ip::Ipv6Addr = net_ip_v6!("fe80::5122");
    const SERVER_ID: [u8; 3] = [2, 5, 1];
    const PREFIX: net_types_ip::Subnet<net_types_ip::Ipv6Addr> = net_subnet_v6!("a::/64");
    const RENEWED_PREFIX: net_types_ip::Subnet<net_types_ip::Ipv6Addr> = net_subnet_v6!("b::/64");
    const DHCPV6_CLIENT_PORT: NonZeroU16 = const_unwrap::const_unwrap_option(NonZeroU16::new(546));
    const DHCPV6_SERVER_PORT: NonZeroU16 = const_unwrap::const_unwrap_option(NonZeroU16::new(547));
    const INFINITE_TIME_VALUE: u32 = u32::MAX;
    const ONE_SECOND_TIME_VALUE: u32 = 1;
    // The DHCPv6 Client always sends IAs with the first IAID starting at 0.
    const EXPECTED_IAID: dhcpv6::IAID = dhcpv6::IAID::new(0);

    struct Dhcpv6ClientMessage {
        tx_id: [u8; 3],
        client_id: Vec<u8>,
    }

    async fn send_dhcpv6_message(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        client_addr: net_types_ip::Ipv6Addr,
        prefix: Option<net_types_ip::Subnet<net_types_ip::Ipv6Addr>>,
        invalidated_prefix: Option<net_types_ip::Subnet<net_types_ip::Ipv6Addr>>,
        tx_id: [u8; 3],
        msg_type: dhcpv6::MessageType,
        client_id: Vec<u8>,
    ) {
        let iaprefix_options = prefix
            .into_iter()
            .map(|prefix| {
                dhcpv6::DhcpOption::IaPrefix(dhcpv6::IaPrefixSerializer::new(
                    INFINITE_TIME_VALUE,
                    INFINITE_TIME_VALUE,
                    prefix,
                    &[],
                ))
            })
            .chain(invalidated_prefix.into_iter().map(|prefix| {
                dhcpv6::DhcpOption::IaPrefix(dhcpv6::IaPrefixSerializer::new(0, 0, prefix, &[]))
            }))
            .collect::<Vec<_>>();

        let options = [
            dhcpv6::DhcpOption::ServerId(&SERVER_ID),
            dhcpv6::DhcpOption::ClientId(&client_id),
            dhcpv6::DhcpOption::IaPd(dhcpv6::IaPdSerializer::new(
                EXPECTED_IAID,
                ONE_SECOND_TIME_VALUE,
                INFINITE_TIME_VALUE,
                iaprefix_options.as_ref(),
            )),
        ]
        .into_iter()
        // If this is an Advertise message, include a preference option with
        // the maximum preference value so that clients stop server discovery
        // and use this server immediately.
        .chain(
            (msg_type == dhcpv6::MessageType::Advertise)
                .then_some(dhcpv6::DhcpOption::Preference(u8::MAX)),
        )
        .collect::<Vec<_>>();

        let buf: packet::Either<EmptyBuf, _> =
            dhcpv6::MessageBuilder::new(msg_type, tx_id, &options)
                .into_serializer()
                .encapsulate(UdpPacketBuilder::new(
                    SERVER_ADDR,
                    client_addr,
                    Some(DHCPV6_SERVER_PORT),
                    DHCPV6_CLIENT_PORT,
                ))
                .encapsulate(Ipv6PacketBuilder::new(
                    SERVER_ADDR,
                    client_addr,
                    64, /* ttl */
                    Ipv6Proto::Proto(IpProto::Udp),
                ))
                .encapsulate(EthernetFrameBuilder::new(
                    Mac::BROADCAST,
                    Mac::BROADCAST,
                    EtherType::Ipv6,
                    ETHERNET_MIN_BODY_LEN_NO_TAG,
                ))
                .serialize_vec_outer()
                .expect("error serializing dhcpv6 packet");

        let () =
            fake_ep.write(buf.unwrap_b().as_ref()).await.expect("error sending dhcpv6 message");
    }

    async fn wait_for_message(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        expected_src_ip: net_types_ip::Ipv6Addr,
        want_msg_type: dhcpv6::MessageType,
    ) -> Dhcpv6ClientMessage {
        let stream = fake_ep
            .frame_stream()
            .map(|r| r.expect("error getting OnData event"))
            .filter_map(|(data, dropped)| {
                async move {
                    assert_eq!(dropped, 0);
                    let mut data = &data[..];

                    let eth = EthernetFrame::parse(&mut data, EthernetFrameLengthCheck::NoCheck)
                        .expect("error parsing ethernet frame");

                    if eth.ethertype() != Some(EtherType::Ipv6) {
                        // Ignore non-IPv6 packets.
                        return None;
                    }

                    let (mut payload, src_ip, dst_ip, proto, _ttl) =
                        parse_ip_packet::<net_types_ip::Ipv6>(&data)
                            .expect("error parsing IPv6 packet");
                    if src_ip != expected_src_ip {
                        return None;
                    }

                    if proto != Ipv6Proto::Proto(IpProto::Udp) {
                        // Ignore non-UDP packets.
                        return None;
                    }

                    let udp = UdpPacket::parse(&mut payload, UdpParseArgs::new(src_ip, dst_ip))
                        .expect("error parsing ICMPv6 packet");
                    if udp.src_port() != Some(DHCPV6_CLIENT_PORT)
                        || udp.dst_port() != DHCPV6_SERVER_PORT
                    {
                        // Ignore packets with non-DHCPv6 ports.
                        return None;
                    }

                    let dhcpv6 = dhcpv6::Message::parse(&mut payload, ())
                        .expect("error parsing DHCPv6 message");

                    if dhcpv6.msg_type() != want_msg_type {
                        return None;
                    }

                    let mut client_id = None;
                    let mut saw_ia_pd = false;
                    for opt in dhcpv6.options() {
                        match opt {
                            dhcpv6::ParsedDhcpOption::ClientId(id) => {
                                assert_eq!(
                                    core::mem::replace(&mut client_id, Some(id.to_vec())),
                                    None
                                )
                            }
                            dhcpv6::ParsedDhcpOption::IaPd(iapd) => {
                                assert_eq!(iapd.iaid(), EXPECTED_IAID.get());
                                assert!(!saw_ia_pd);
                                saw_ia_pd = true;
                            }
                            _ => {}
                        }
                    }
                    assert!(saw_ia_pd);

                    Some(Dhcpv6ClientMessage {
                        tx_id: *dhcpv6.transaction_id(),
                        client_id: client_id.unwrap(),
                    })
                }
            });

        futures::pin_mut!(stream);
        stream.next().await.expect("expected DHCPv6 message")
    }

    with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Dhcpv6,
        true, /* with_dhcpv6_client */
        |if_id, network, interface_state, realm| {
            async move {
                // Fake endpoint to inject server packets and intercept client packets.
                let fake_ep = network.create_fake_endpoint().expect("create fake endpoint");

                // Request Prefixes to be acquired.
                let prefix_provider = realm
                    .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
                    .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
                let (prefix_control, server_end) =
                    fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                        .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
                prefix_provider
                    .acquire_prefix(
                        &fnet_dhcpv6::AcquirePrefixConfig {
                            interface_id: Some(if_id),
                            ..Default::default()
                        },
                        server_end,
                    )
                    .expect("acquire prefix");

                let if_ll_addr = interfaces::wait_for_v6_ll(interface_state, if_id)
                    .await
                    .expect("error waiting for link-local address");
                let fake_ep = &fake_ep;

                // Perform the prefix negotiation.
                for (expected, send) in [
                    (dhcpv6::MessageType::Solicit, dhcpv6::MessageType::Advertise),
                    (dhcpv6::MessageType::Request, dhcpv6::MessageType::Reply),
                ] {
                    let Dhcpv6ClientMessage { tx_id, client_id } =
                        wait_for_message(&fake_ep, if_ll_addr, expected).await;
                    send_dhcpv6_message(
                        &fake_ep,
                        if_ll_addr,
                        Some(PREFIX),
                        None,
                        tx_id,
                        send,
                        client_id,
                    )
                    .await;
                }
                assert_eq!(
                    prefix_control.watch_prefix().await.expect("error watching prefix"),
                    fnet_dhcpv6::PrefixEvent::Assigned(fnet_dhcpv6::Prefix {
                        prefix: fnet::Ipv6AddressWithPrefix {
                            addr: fnet::Ipv6Address { addr: PREFIX.network().ipv6_bytes() },
                            prefix_len: PREFIX.prefix(),
                        },
                        lifetimes: fnet_dhcpv6::Lifetimes {
                            valid_until: zx::Time::INFINITE.into_nanos(),
                            preferred_until: zx::Time::INFINITE.into_nanos(),
                        },
                    }),
                );

                for (new_prefix, old_prefix, res) in [
                    // Renew the IA with a new prefix and invalidate the old prefix.
                    (
                        Some(RENEWED_PREFIX),
                        Some(PREFIX),
                        fnet_dhcpv6::PrefixEvent::Assigned(fnet_dhcpv6::Prefix {
                            prefix: fnet::Ipv6AddressWithPrefix {
                                addr: fnet::Ipv6Address {
                                    addr: RENEWED_PREFIX.network().ipv6_bytes(),
                                },
                                prefix_len: RENEWED_PREFIX.prefix(),
                            },
                            lifetimes: fnet_dhcpv6::Lifetimes {
                                valid_until: zx::Time::INFINITE.into_nanos(),
                                preferred_until: zx::Time::INFINITE.into_nanos(),
                            },
                        }),
                    ),
                    // Invalidate the prefix.
                    (
                        None,
                        Some(RENEWED_PREFIX),
                        fnet_dhcpv6::PrefixEvent::Unassigned(fnet_dhcpv6::Empty {}),
                    ),
                ] {
                    let Dhcpv6ClientMessage { tx_id, client_id } =
                        wait_for_message(&fake_ep, if_ll_addr, dhcpv6::MessageType::Renew).await;
                    send_dhcpv6_message(
                        &fake_ep,
                        if_ll_addr,
                        new_prefix,
                        old_prefix,
                        tx_id,
                        dhcpv6::MessageType::Reply,
                        client_id,
                    )
                    .await;
                    assert_eq!(
                        prefix_control.watch_prefix().await.expect("error watching prefix"),
                        res,
                    );
                }
            }
            .boxed_local()
        },
    )
    .await;
}
