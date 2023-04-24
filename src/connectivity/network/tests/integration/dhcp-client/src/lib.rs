// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use netstack_testing_common::realms::{Netstack, TestSandboxExt as _};

use assert_matches::assert_matches;
use fidl::endpoints;
use fidl_fuchsia_net_dhcp::{
    ClientEvent, ClientExitReason, ClientMarker, ClientProviderMarker, NewClientParams,
};
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_netemul as fnetemul;
use fidl_fuchsia_netemul_network as fnetemul_network;
use futures::{join, FutureExt, TryStreamExt};
use netstack_testing_macros::netstack_test;

const MAC: net_types::ethernet::Mac = net_declare::net_mac!("00:00:00:00:00:01");

struct SingleInterfaceTestRealm<'a> {
    realm: netemul::TestRealm<'a>,
    iface: netemul::TestInterface<'a>,
}

async fn create_single_interface_test_realm<'a, N: Netstack>(
    sandbox: &'a netemul::TestSandbox,
    name: &'a str,
) -> SingleInterfaceTestRealm<'a> {
    let network =
        sandbox.create_network("dhcp-test-network").await.expect("create network should succeed");
    let realm: netemul::TestRealm<'_> = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("client-realm-{name}"),
            std::iter::once(fnetemul::ChildDef::from(
                &netstack_testing_common::realms::KnownServiceProvider::DhcpClient,
            )),
        )
        .expect("create realm should succeed");

    let iface = realm
        .join_network_with(
            &network,
            "iface",
            fnetemul_network::EndpointConfig {
                mtu: netemul::DEFAULT_MTU,
                mac: Some(Box::new(fnet_ext::MacAddress { octets: MAC.bytes() }.into())),
            },
            netemul::InterfaceConfig { name: Some("iface".into()), metric: None },
        )
        .await
        .expect("join network with realm should succeed");

    SingleInterfaceTestRealm { realm, iface }
}

#[netstack_test]
async fn client_provider_two_overlapping_clients_on_same_interface<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let SingleInterfaceTestRealm { realm, iface } =
        create_single_interface_test_realm::<N>(&sandbox, name).await;

    let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let iface = &iface;

    let (client_a, server_end_a) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    let (client_b, server_end_b) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

    proxy
        .new_client(
            iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end_a,
        )
        .expect("creating new client should succeed");

    proxy
        .new_client(
            iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end_b,
        )
        .expect("creating new client should succeed");

    let watch_fut_a = client_a.watch_configuration();
    let watch_fut_b = client_b.watch_configuration();
    let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

    assert_matches!(result_a, Err(fidl::Error::ClientChannelClosed { .. }));
    assert_matches!(result_b, Err(fidl::Error::ClientChannelClosed { .. }));

    let on_exit = client_b
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(
        on_exit,
        ClientEvent::OnExit { reason: ClientExitReason::ClientAlreadyExistsOnInterface }
    )
}

#[netstack_test]
async fn client_provider_two_non_overlapping_clients_on_same_interface<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let SingleInterfaceTestRealm { realm, iface } =
        create_single_interface_test_realm::<N>(&sandbox, name).await;

    let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let iface = &iface;

    let proxy = &proxy;
    // Executing the following block twice demonstrates that we can run
    // and shutdown DHCP clients on the same interface without running
    // afoul of the multiple-clients-on-same-interface restriction.
    for () in [(), ()] {
        let (client, server_end) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

        proxy
            .new_client(
                iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end,
            )
            .expect("creating new client should succeed");

        client.shutdown().expect("shutdown call should not have FIDL error");
        let watch_result = client.watch_configuration().await;

        assert_matches!(watch_result, Err(fidl::Error::ClientChannelClosed { .. }));

        let on_exit = client
            .take_event_stream()
            .try_next()
            .now_or_never()
            .expect("event should be already available")
            .expect("event stream should not have ended before yielding exit reason")
            .expect("event stream should not have FIDL error");
        assert_matches!(
            on_exit,
            ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown }
        );
    }
}

#[netstack_test]
async fn client_provider_double_watch<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let SingleInterfaceTestRealm { realm, iface } =
        create_single_interface_test_realm::<N>(&sandbox, name).await;

    let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let iface = &iface;

    let (client, server_end) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    proxy
        .new_client(
            iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("new client");

    let watch_fut_a = client.watch_configuration();
    let watch_fut_b = client.watch_configuration();
    let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

    assert_matches!(result_a, Err(_));
    assert_matches!(result_b, Err(_));

    let on_exit = client
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(
        on_exit,
        ClientEvent::OnExit { reason: ClientExitReason::WatchConfigurationAlreadyPending }
    )
}

#[netstack_test]
async fn client_provider_shutdown<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let SingleInterfaceTestRealm { realm, iface } =
        create_single_interface_test_realm::<N>(&sandbox, name).await;

    let proxy = realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let iface = &iface;

    let (client, server_end) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    proxy
        .new_client(
            iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("creating new client should succeed");

    let watch_fut = client.watch_configuration();
    let shutdown_fut = async {
        futures_lite::future::yield_now().await;
        client.shutdown().expect("shutdown should not have FIDL error");
    };
    let (watch_result, ()) = join!(watch_fut, shutdown_fut);

    assert_matches!(watch_result, Err(_));

    let on_exit = client
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(on_exit, ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown })
}
