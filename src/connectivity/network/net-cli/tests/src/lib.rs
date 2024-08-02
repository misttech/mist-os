// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration tests that exercise net-cli commands within a network-test-realm.
//! For simply integration testing net-cli, network-test-realm would be unnecessary -- netemul is
//! sufficient and running network-test-realm via netemul is redundant.
//! However, because there are out-of-tree end-to-end test usages of net-cli within
//! network-test-realm, it's helpful to exercise net-cli-within-network-test-realm in this way
//! in order to help validate that the correct capabilities are available in-tree.

#![cfg(test)]

use anyhow::Result;
use argh::FromArgs as _;
use net_declare::{fidl_ip_v6, fidl_mac};
use net_types::ip::IpVersion;
use netstack_testing_common::realms::KnownServiceProvider;
use test_case::test_case;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_net as fnet,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_test_realm as fntr,
};

struct NetworkTestRealmConnector<'a> {
    realm: &'a netemul::TestRealm<'a>,
}

#[async_trait::async_trait]
impl<'a, P: fidl::endpoints::DiscoverableProtocolMarker> net_cli::ServiceConnector<P>
    for NetworkTestRealmConnector<'a>
{
    async fn connect(
        &self,
    ) -> Result<<P as fidl::endpoints::ProtocolMarker>::Proxy, anyhow::Error> {
        Ok(connect_to_hermetic_network_realm_protocol::<P>(self.realm).await)
    }
}

/// Connects to a protocol within the hermetic network realm.
async fn connect_to_hermetic_network_realm_protocol<
    P: fidl::endpoints::DiscoverableProtocolMarker,
>(
    realm: &netemul::TestRealm<'_>,
) -> P::Proxy {
    let realm_proxy = realm
        .connect_to_protocol::<fcomponent::RealmMarker>()
        .expect("failed to connect to realm protocol");
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
        .expect("failed to create Directory proxy");
    let child_ref = network_test_realm::create_hermetic_network_realm_child_ref();
    realm_proxy
        .open_exposed_dir(&child_ref, server_end)
        .await
        .expect("open_exposed_dir failed")
        .expect("open_exposed_dir error");
    fuchsia_component::client::connect_to_protocol_at_dir_root::<P>(&directory_proxy)
        .unwrap_or_else(|e| {
            panic!(
                "failed to connect to hermetic network realm protocol {} with error: {:?}",
                P::PROTOCOL_NAME,
                e
            )
        })
}

/// Adds an interface to the hermetic Netstack with `interface_name` and
/// `mac_address`.
///
/// The added interface is assigned a static IP address based on `subnet`.
/// Additionally, the interface joins the provided `network`.
async fn join_network_with_hermetic_netstack<'a>(
    realm: &'a netemul::TestRealm<'a>,
    network: &'a netemul::TestNetwork<'a>,
    interface_name: &'a str,
    mac_address: fnet::MacAddress,
    subnet: fnet::Subnet,
) -> (
    netemul::TestEndpoint<'a>,
    u64,
    fnet_interfaces_ext::admin::Control,
    fnet_interfaces_admin::DeviceControlProxy,
) {
    let installer =
        connect_to_hermetic_network_realm_protocol::<fnet_interfaces_admin::InstallerMarker>(realm)
            .await;

    let interface_state =
        connect_to_hermetic_network_realm_protocol::<fnet_interfaces::StateMarker>(realm).await;

    let (endpoint, id, control, device_control) = realm
        .join_network_with_installer(
            &network,
            installer,
            interface_state,
            interface_name,
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(mac_address)),
            netemul::InterfaceConfig { name: Some(interface_name.into()), ..Default::default() },
        )
        .await
        .expect("join_network failed");

    let address_state_provider = netstack_testing_common::interfaces::add_address_wait_assigned(
        &control,
        subnet,
        fidl_fuchsia_net_interfaces_admin::AddressParameters {
            add_subnet_route: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("add_address_wait_assigned failed");

    // Allow the address to live beyond the `address_state_provider` handle.
    address_state_provider.detach().expect("detatch failed");

    (endpoint, id, control, device_control)
}

const INTERFACE1_NAME: &'static str = "iface1";
const INTERFACE1_MAC_ADDRESS: fnet::MacAddress = fidl_mac!("02:03:04:05:06:07");
const DEFAULT_IPV6_LINK_LOCAL_SOURCE_ADDR: fnet::Ipv6Address = fidl_ip_v6!("fe80::2");
const DEFAULT_IPV6_LINK_LOCAL_SOURCE_SUBNET: fnet::Subnet = fnet::Subnet {
    addr: fnet::IpAddress::Ipv6(DEFAULT_IPV6_LINK_LOCAL_SOURCE_ADDR),
    prefix_len: 64,
};

#[test_case(IpVersion::V4)]
#[test_case(IpVersion::V6)]
#[fuchsia_async::run_singlethreaded(test)]
async fn add_del_route(ip_version: IpVersion) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("network").await.expect("failed to create network");
    let realm = sandbox
        .create_realm(
            "net-cli-test-realm",
            &[KnownServiceProvider::NetworkTestRealm { require_outer_netstack: false }],
        )
        .expect("creating realm should succeed");

    let network_test_realm = realm
        .connect_to_protocol::<fntr::ControllerMarker>()
        .expect("failed to connect to network test realm controller");

    network_test_realm
        .start_hermetic_network_realm(fntr::Netstack::V3)
        .await
        .expect("start_hermetic_network_realm failed")
        .expect("start_hermetic_network_realm error");

    let (_endpoint, nicid, _control, _device_control) = join_network_with_hermetic_netstack(
        &realm,
        &network,
        INTERFACE1_NAME,
        INTERFACE1_MAC_ADDRESS,
        DEFAULT_IPV6_LINK_LOCAL_SOURCE_SUBNET,
    )
    .await;

    let connector = NetworkTestRealmConnector { realm: &realm };

    let list_routes = || async {
        let buffers = ffx_writer::TestBuffers::default();
        net_cli::do_root(
            ffx_writer::MachineWriter::new_test(Some(ffx_writer::Format::JsonPretty), &buffers),
            net_cli::Command::from_args(&["net"], &["route", "list"])
                .expect("should parse args successfully"),
            &connector,
        )
        .await
        .expect("should succeed");
        let list: Vec<serde_json::Value> =
            serde_json::from_slice(&buffers.into_stdout()[..]).expect("should be valid JSON array");
        list
    };

    let initial_routes = list_routes().await;

    let (destination, gateway, prefix_len) = match ip_version {
        IpVersion::V4 => ("1.2.3.4", "1.2.3.5", 32),
        IpVersion::V6 => ("::1:2:3:4:5", "::1:2:3:4:6", 128),
    };

    let metric = 111;

    let route_record = serde_json::json!({
        "destination": serde_json::json!({
            "addr": destination,
            "prefix_len": prefix_len,
        }),
        "gateway": gateway,
        "metric": metric,
        "nicid": nicid,
    });

    assert!(
        !initial_routes.contains(&route_record),
        "{initial_routes:?} should not contain {route_record:?}"
    );

    {
        let prefix_len = prefix_len.to_string();
        let nicid = nicid.to_string();
        let metric = metric.to_string();
        net_cli::do_root(
            ffx_writer::MachineWriter::new(None),
            net_cli::Command::from_args(
                &["net"],
                &[
                    "route",
                    "add",
                    "--destination",
                    destination,
                    "--netmask",
                    prefix_len.as_str(),
                    "--gateway",
                    gateway,
                    "--nicid",
                    nicid.as_str(),
                    "--metric",
                    metric.as_str(),
                ],
            )
            .expect("should parse successfully"),
            &connector,
        )
        .await
        .expect("should succeed");
    }

    let after_add_routes = list_routes().await;

    assert!(
        after_add_routes.contains(&route_record),
        "{after_add_routes:?} should contain {route_record:?}"
    );

    {
        let prefix_len = prefix_len.to_string();
        let nicid = nicid.to_string();
        let metric = metric.to_string();
        net_cli::do_root(
            ffx_writer::MachineWriter::new(None),
            net_cli::Command::from_args(
                &["net"],
                &[
                    "route",
                    "del",
                    "--destination",
                    destination,
                    "--netmask",
                    prefix_len.as_str(),
                    "--gateway",
                    gateway,
                    "--nicid",
                    nicid.as_str(),
                    "--metric",
                    metric.as_str(),
                ],
            )
            .expect("should parse successfully"),
            &connector,
        )
        .await
        .expect("should succeed");
    }

    let after_remove_routes = list_routes().await;

    assert!(
        !after_remove_routes.contains(&route_record),
        "{after_remove_routes:?} should not contain {route_record:?}"
    );
}
