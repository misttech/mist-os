// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use assert_matches::assert_matches;
use fuchsia_async::{DurationExt as _, Task};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{FutureExt as _, SinkExt as _, StreamExt as _};
use log::info;
use net_declare::fidl_ip_v6;
use netcfg::NetworkTokenExt;
use netstack_testing_common::constants::ipv6 as ipv6_consts;
use netstack_testing_common::ndp::send_ra_with_router_lifetime;
use netstack_testing_common::realms::{Manager, ManagerConfig, Netstack, NetstackExt};
use netstack_testing_common::{wait_for_component_stopped, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT};
use netstack_testing_macros::netstack_test;
use packet_formats::icmp::ndp::options::{NdpOptionBuilder, RecursiveDnsServer};
use policy_testing_common::{with_netcfg_owned_device, NetcfgOwnedDeviceArgs};
use pretty_assertions::assert_eq;
use std::collections::HashSet;
use std::pin::pin;
use std::sync::Arc;
use {fidl_fuchsia_net as fnet, fidl_fuchsia_net_policy_properties as fnp_properties};

trait TakeNetwork {
    fn take_network(self) -> Option<fnp_properties::NetworkToken>;
}

impl TakeNetwork for fnp_properties::NetworksWatchDefaultResponse {
    fn take_network(self) -> Option<fidl_fuchsia_net_policy_properties::NetworkToken> {
        match self {
            fnp_properties::NetworksWatchDefaultResponse::Network(network_token) => {
                Some(network_token)
            }
            fnp_properties::NetworksWatchDefaultResponse::NoDefaultNetwork(_) => None,
            _ => None,
        }
    }
}

fn network_update(
    network_id: u64,
    marks: fnet::Marks,
) -> fnp_properties::DefaultNetworkUpdateRequest {
    fnp_properties::DefaultNetworkUpdateRequest {
        interface_id: Some(network_id),
        socket_marks: Some(marks),
        ..Default::default()
    }
}

fn marks(mark_1: Option<u32>, mark_2: Option<u32>) -> fnet::Marks {
    fnet::Marks { mark_1, mark_2, ..Default::default() }
}

fn expect_sequence(
    actual: &[Option<Vec<fnp_properties::PropertyUpdate>>],
    expected: &[Option<fnp_properties::PropertyUpdate>],
) {
    let mut actual = actual.iter().peekable();
    let expected = expected.iter();

    for expect in expected {
        let next = actual.next();
        match next {
            None => panic!("Missing property. Next expected property is: {expect:?}"),
            Some(value) => match (value, expect) {
                (Some(v), Some(e)) => {
                    if !v.contains(e) {
                        panic!("Found out of sequence entry (expected: {e:?}, found: {v:?}");
                    }
                }
                (None, None) => {}
                _ => panic!("Found out of sequence entry (expected: {expect:?}, found: {value:?}"),
            },
        }

        loop {
            let peek = actual.peek();
            match peek {
                None => break,
                Some(value) => match (value, expect) {
                    (Some(v), Some(e)) => {
                        if !v.contains(e) {
                            break;
                        }
                    }
                    (None, None) => {}
                    _ => break,
                },
            }
            let _ = actual.next();
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(M, Manager)]
async fn test_track_socket_marks<N: Netstack, M: Manager>(name: &str) {
    use fnp_properties::PropertyUpdate;

    let _if_name = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Empty,
        NetcfgOwnedDeviceArgs {
            use_out_of_stack_dhcp_client: N::USE_OUT_OF_STACK_DHCP_CLIENT,
            ..Default::default()
        },
        |_if_id, _network, _interface_state, realm, _sandbox| {
            async move {
                let (mut tx, mut rx) = mpsc::channel::<()>(1);
                let (mut shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

                let last_updates = Arc::new(Mutex::new(Vec::new()));
                Task::spawn({
                    let networks = realm
                        .connect_to_protocol::<fnp_properties::NetworksMarker>()
                        .expect("couldn't connect to fuchsia.net.policy.properties/Networks");
                    let last_updates = last_updates.clone();
                    async move {
                        let mut network = networks
                            .watch_default()
                            .await
                            .expect("failed to fetch default network")
                            .take_network()
                            .expect("the first return from watch default should never fail");
                        let watch_default = |networks: &fnp_properties::NetworksProxy| {
                            networks.watch_default().fuse()
                        };
                        let watch_update =
                            |networks: &fnp_properties::NetworksProxy,
                             network: &fnp_properties::NetworkToken| {
                                networks
                                    .watch_properties(
                                        fnp_properties::NetworksWatchPropertiesRequest {
                                            network: Some(
                                                network.duplicate().expect("couldn't duplicate"),
                                            ),
                                            properties: Some(vec![
                                                fnp_properties::Property::SocketMarks,
                                                fnp_properties::Property::DnsConfiguration,
                                            ]),
                                            ..Default::default()
                                        },
                                    )
                                    .fuse()
                            };
                        let mut next_network = watch_default(&networks);
                        let mut watch_for_updates = watch_update(&networks, &network);
                        let mut last_marks = marks(None, None);
                        loop {
                            futures::select! {
                                new_network = next_network => {
                                    match new_network
                                            .expect("failed to fetch default network")
                                            .take_network() {
                                        Some(net) => network = net,
                                        None => {
                                            info!("Default network was lost");
                                            {
                                                let mut updates = last_updates.lock().await;
                                                if updates.last() != Some(&None) {
                                                    updates.push(None);
                                                    tx.send(()).await.expect("Can't send update");
                                                }
                                            }
                                        }
                                    }
                                    next_network = watch_default(&networks);
                                }
                                property_update = watch_for_updates => {
                                    match property_update {
                                        Ok(Ok(update)) => {
                                            for part in &update.clone() {
                                                if let PropertyUpdate::SocketMarks(marks) = part {
                                                    if *marks != last_marks {
                                                        last_marks = marks.clone();
                                                        last_updates
                                                            .lock()
                                                            .await
                                                            .push(Some(update.clone()));
                                                        tx
                                                            .send(())
                                                            .await
                                                            .expect("Can't send update");
                                                    }
                                                }
                                            }
                                        }
                                        Ok(Err(fnp_properties::WatchError::DefaultNetworkLost)) => {
                                            {
                                                let mut updates = last_updates.lock().await;
                                                if updates.last() != Some(&None) {
                                                    updates.push(None);
                                                    tx.send(()).await.expect("Can't send update");
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                    watch_for_updates = watch_update(&networks, &network);
                                }
                                _ = shutdown_rx.next() => {
                                    return;
                                }
                            }
                        }
                    }
                })
                .detach();

                let default_network = realm
                    .connect_to_protocol::<fnp_properties::DefaultNetworkMarker>()
                    .expect("couldn't connect to fuchsia.net.policy.properties/DefaultNetwork");

                default_network
                    .update(&network_update(1, marks(Some(1), None)))
                    .await
                    .expect("fidl error")
                    .map_err(|e| anyhow::anyhow!("err {e:?}"))
                    .expect("protocol error");

                // Verify that a second call to update will fail.
                #[derive(Debug, thiserror::Error)]
                enum UpdateErrType {
                    #[error("Error while connecting {0}")]
                    Connect(#[from] anyhow::Error),
                    #[error("Error while calling update {0}")]
                    UpdateError(#[from] fidl::Error),
                    #[error("Protocol error: {0:?}")]
                    ProtocolError(fnp_properties::UpdateDefaultNetworkError),
                }
                let do_update_again =
                    async |realm: &netemul::TestRealm<'_>| -> Result<(), UpdateErrType> {
                        let default_network2 =
                            realm.connect_to_protocol::<fnp_properties::DefaultNetworkMarker>()?;
                        default_network2
                            .update(&network_update(2, marks(Some(2), None)))
                            .await?
                            .map_err(UpdateErrType::ProtocolError)?;

                        Ok(())
                    };
                assert_matches!(
                    do_update_again(&realm).await,
                    Err(UpdateErrType::UpdateError(fidl::Error::ClientChannelClosed {
                        status: zx::Status::CONNECTION_ABORTED,
                        protocol_name: _,
                        epitaph: _,
                    }))
                );

                let _ = rx.next().await;

                default_network
                    .update(&network_update(1, marks(Some(2), Some(10))))
                    .await
                    .expect("fidl error")
                    .map_err(|e| anyhow::anyhow!("err {e:?}"))
                    .expect("protocol error");
                let _ = rx.next().await;

                default_network
                    .update(&network_update(1, marks(Some(4), None)))
                    .await
                    .expect("fidl error")
                    .map_err(|e| anyhow::anyhow!("err {e:?}"))
                    .expect("protocol error");
                let _ = rx.next().await;

                default_network
                    .update(&Default::default())
                    .await
                    .expect("fidl error")
                    .map_err(|e| anyhow::anyhow!("err {e:?}"))
                    .expect("protocol error");
                let _ = rx.next().await;

                default_network
                    .update(&network_update(1, marks(Some(8), None)))
                    .await
                    .expect("fidl error")
                    .map_err(|e| anyhow::anyhow!("err {e:?}"))
                    .expect("protocol error");
                let _ = rx.next().await;

                let updates = last_updates.lock().await.clone();
                expect_sequence(
                    &updates,
                    &vec![
                        Some(PropertyUpdate::SocketMarks(marks(Some(1), None))),
                        Some(PropertyUpdate::SocketMarks(marks(Some(2), Some(10)))),
                        Some(PropertyUpdate::SocketMarks(marks(Some(4), None))),
                        // None update represents the empty default_network.update call
                        None,
                        Some(PropertyUpdate::SocketMarks(marks(Some(8), None))),
                    ],
                );

                shutdown_tx.send(()).await.expect("couldn't trigger clean shutdown");
            }
            .boxed_local()
        },
    )
    .await;
}

trait PropertyUpdateExt {
    fn dns_configuration(&self) -> Option<&fnp_properties::DnsConfiguration>;
}

impl PropertyUpdateExt for fnp_properties::PropertyUpdate {
    fn dns_configuration(&self) -> Option<&fidl_fuchsia_net_policy_properties::DnsConfiguration> {
        match self {
            fidl_fuchsia_net_policy_properties::PropertyUpdate::DnsConfiguration(
                dns_configuration,
            ) => Some(dns_configuration),
            fidl_fuchsia_net_policy_properties::PropertyUpdate::SocketMarks(_) | _ => None,
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(M, Manager)]
async fn test_track_dns_changes<N: Netstack, M: Manager>(name: &str) -> Result<(), anyhow::Error> {
    const NDP_DNS_SERVER1: fnet::Ipv6Address = fidl_ip_v6!("20a::1234:5678");
    const NDP_DNS_SERVER2: fnet::Ipv6Address = fidl_ip_v6!("20a::2345:6789");
    const NDP_DNS_SERVER3: fnet::Ipv6Address = fidl_ip_v6!("20a::3456:7890");

    const DEFAULT_DNS_PORT: u16 = 53;

    let _if_name = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Empty,
        NetcfgOwnedDeviceArgs {
            use_out_of_stack_dhcp_client: N::USE_OUT_OF_STACK_DHCP_CLIENT,
            ..Default::default()
        },
        |_if_id, network, _interface_state, realm, _sandbox| {
            async move {
                let fake_ep =
                    network.create_fake_endpoint().expect("failed to create fake endpoint");

                async fn update_dns(
                    fake_ep: &netemul::TestFakeEndpoint<'_>,
                    addresses: &[fnet::Ipv6Address],
                ) {
                    let addresses =
                        addresses.into_iter().map(|addr| addr.addr.into()).collect::<Vec<_>>();
                    let rdnss = RecursiveDnsServer::new(9999, &addresses);
                    let options = [NdpOptionBuilder::RecursiveDnsServer(rdnss)];
                    send_ra_with_router_lifetime(
                        fake_ep,
                        0,
                        &options,
                        ipv6_consts::LINK_LOCAL_ADDR,
                    )
                    .await
                    .expect("failed to send router advertisment");
                }

                let wait_for_netmgr = wait_for_component_stopped(
                    &realm,
                    M::MANAGEMENT_AGENT.get_component_name(),
                    None,
                )
                .fuse();
                let mut wait_for_netmgr = pin!(wait_for_netmgr);
                let default_network = realm
                    .connect_to_protocol_from_child::<fnp_properties::DefaultNetworkMarker>(
                        M::MANAGEMENT_AGENT.get_component_name(),
                    )
                    .expect("failed to connect to DefaultNetwork");
                default_network
                    .update(&network_update(2, Default::default()))
                    .await
                    .expect("fidl error")
                    .expect("protocol error");
                let networks = realm
                    .connect_to_protocol_from_child::<fnp_properties::NetworksMarker>(
                        M::MANAGEMENT_AGENT.get_component_name(),
                    )
                    .expect("failed to connect to Networks");
                let network = networks
                    .watch_default()
                    .await
                    .expect("failed to fetch default network")
                    .take_network()
                    .expect("the first return from watch default should never fail");
                let watch_update =
                    |networks: &fnp_properties::NetworksProxy,
                     network: &fnp_properties::NetworkToken| {
                        networks
                            .watch_properties(fnp_properties::NetworksWatchPropertiesRequest {
                                network: Some(network.duplicate().expect("couldn't duplicate")),
                                properties: Some(vec![fnp_properties::Property::DnsConfiguration]),
                                ..Default::default()
                            })
                            .fuse()
                    };
                let mut watch = watch_update(&networks, &network);
                let mut dns_sequence = std::collections::VecDeque::from([
                    vec![NDP_DNS_SERVER1],
                    vec![NDP_DNS_SERVER1, NDP_DNS_SERVER2],
                    vec![NDP_DNS_SERVER1, NDP_DNS_SERVER2, NDP_DNS_SERVER3],
                ]);
                let mut seen_updates = Vec::new();
                'main: loop {
                    let () = futures::select! {
                        update = watch => {
                            watch = watch_update(&networks, &network);
                            let update = update.expect("fidl error").expect("protocol error");
                            let server_count = update[0]
                                .dns_configuration()
                                .unwrap()
                                .servers
                                .as_ref()
                                .map(|s|s.len())
                                .unwrap_or(0);
                            seen_updates.push(update);
                            if let Some(list) = dns_sequence.pop_front() {
                                // Each update is 1 more server than the
                                // last. Wait until we see the previous
                                // update.
                                if list.len() - 1 == server_count {
                                    update_dns(&fake_ep, &list).await;
                                } else {
                                    dns_sequence.push_front(list);
                                }
                            }
                            // The final update should have 3 DNS servers.
                            if server_count >= 3 {
                                break 'main;
                            }
                        },
                        () = fuchsia_async::Timer::new(
                            ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now()
                        ).fuse() => {
                            panic!("timed out waiting for DNS server list");
                        },
                        stopped_event = wait_for_netmgr => {
                            panic!("the network manager: {stopped_event:?}");
                        }
                    };
                }

                let dns_servers = seen_updates
                    .into_iter()
                    .map(|upd| {
                        upd[0]
                            .dns_configuration()
                            .unwrap()
                            .servers
                            .as_ref()
                            .unwrap()
                            .into_iter()
                            .map(|server| server.address)
                            .collect::<HashSet<_>>()
                    })
                    .collect::<Vec<_>>();

                // We expect there to be 4 total DNS server updates:
                assert_eq!(dns_servers.len(), 4);

                // 1st update: Initial empty list.
                assert_eq!(dns_servers[0], HashSet::new());

                // 2nd update: Just NDP_DNS_SERVER1.
                assert_eq!(
                    dns_servers[1],
                    vec![Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                        address: NDP_DNS_SERVER1,
                        port: DEFAULT_DNS_PORT,
                        zone_index: 0
                    })),]
                    .into_iter()
                    .collect::<HashSet<_>>()
                );

                // 3rd update: Just NDP_DNS_SERVER1 and NDP_DNS_SERVER2.
                assert_eq!(
                    dns_servers[2],
                    vec![
                        Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                            address: NDP_DNS_SERVER1,
                            port: DEFAULT_DNS_PORT,
                            zone_index: 0
                        })),
                        Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                            address: NDP_DNS_SERVER2,
                            port: DEFAULT_DNS_PORT,
                            zone_index: 0
                        })),
                    ]
                    .into_iter()
                    .collect::<HashSet<_>>()
                );

                // 4th update: Just NDP_DNS_SERVER1, NDP_DNS_SERVER2, and NDP_DNS_SERVER3.
                assert_eq!(
                    dns_servers[3],
                    vec![
                        Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                            address: NDP_DNS_SERVER1,
                            port: DEFAULT_DNS_PORT,
                            zone_index: 0
                        })),
                        Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                            address: NDP_DNS_SERVER2,
                            port: DEFAULT_DNS_PORT,
                            zone_index: 0
                        })),
                        Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
                            address: NDP_DNS_SERVER3,
                            port: DEFAULT_DNS_PORT,
                            zone_index: 0
                        })),
                    ]
                    .into_iter()
                    .collect::<HashSet<_>>()
                );
            }
            .boxed_local()
        },
    )
    .await;

    Ok(())
}
