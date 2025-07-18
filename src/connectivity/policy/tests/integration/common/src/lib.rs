// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::pin::pin;

use futures::future::{FutureExt as _, LocalBoxFuture};
use netemul::{TestEndpoint, TestNetwork, TestRealm};
use netstack_testing_common::realms::{
    KnownServiceProvider, Manager, ManagerConfig, Netstack, TestSandboxExt,
};
use netstack_testing_common::{
    interfaces, wait_for_component_stopped, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use {
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_netemul_network as fnetemul_network,
};

#[derive(Default)]
pub struct NetcfgOwnedDeviceArgs {
    // Whether to use the out of stack DHCP client.
    pub use_out_of_stack_dhcp_client: bool,
    // Whether to include the socketproxy protocols in netcfg.
    pub use_socket_proxy: bool,
    // Additional service providers to include in the realm.
    pub extra_known_service_providers: Vec<KnownServiceProvider>,
}

/// Initialize a realm with a device that is owned by netcfg.
/// The device is discovered through devfs and installed into
/// the Netstack via netcfg. `after_interface_up` is called
/// once the interface has been discovered via the Netstack
/// interfaces watcher.
pub async fn with_netcfg_owned_device<
    M: Manager,
    N: Netstack,
    F: for<'a> FnOnce(
        u64,
        &'a netemul::TestNetwork<'a>,
        &'a fnet_interfaces::StateProxy,
        &'a netemul::TestRealm<'a>,
        &'a netemul::TestSandbox,
    ) -> LocalBoxFuture<'a, ()>,
>(
    name: &str,
    manager_config: ManagerConfig,
    additional_args: NetcfgOwnedDeviceArgs,
    after_interface_up: F,
) -> String {
    let NetcfgOwnedDeviceArgs {
        use_out_of_stack_dhcp_client,
        use_socket_proxy,
        extra_known_service_providers,
    } = additional_args;
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            [
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client,
                    use_socket_proxy,
                    config: manager_config,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ]
            .into_iter()
            .chain(extra_known_service_providers)
            // If the client requested an out of stack DHCP client or to use
            // the socket proxy, add them to the list of service providers.
            .chain(
                use_out_of_stack_dhcp_client
                    .then_some(KnownServiceProvider::DhcpClient)
                    .into_iter(),
            )
            .chain(use_socket_proxy.then_some(KnownServiceProvider::SocketProxy).into_iter()),
        )
        .expect("create netstack realm");

    // Add a device to the realm.
    let network = sandbox.create_network(name).await.expect("create network");
    let _endpoint = add_device_to_devfs::<M>(&network, &realm, name.to_string(), None).await;

    // Make sure the Netstack got the new device added.
    let (interface_state, if_id, if_name) = verify_interface_added::<M>(&realm).await;

    after_interface_up(if_id, &network, &interface_state, &realm, &sandbox).await;

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    realm.shutdown().await.expect("failed to shutdown realm");

    if_name
}

/// Add a device into devfs to be fully managed by netcfg. Netcfg will discover
/// and install the device into the Netstack. We cannot assume that netcfg has
/// observed and installed the device as a result of this function, that
/// condition must be observed via the interfaces watcher.
///
/// Returns the new endpoint that was added to the realm.
pub async fn add_device_to_devfs<'a, M: Manager>(
    network: &'a TestNetwork<'a>,
    realm: &'a TestRealm<'a>,
    name: String,
    endpoint_config: Option<fnetemul_network::EndpointConfig>,
) -> TestEndpoint<'a> {
    let endpoint = match endpoint_config {
        Some(config) => network.create_endpoint_with(name, config).await,
        None => network.create_endpoint(name).await,
    }
    .expect("create endpoint");
    endpoint.set_link_up(true).await.expect("set link up");
    let endpoint_mount_path = netemul::devfs_device_path("ep");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    realm.add_virtual_device(&endpoint, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device {}: {:?}", endpoint_mount_path.display(), e)
    });
    endpoint
}

/// Observe a new non-loopback interface via Netstack's interface watcher.
///
/// Returns the interface state proxy and the interface's id and name.
pub async fn verify_interface_added<'a, M: Manager>(
    realm: &'a TestRealm<'a>,
) -> (fnet_interfaces::StateProxy, u64, String) {
    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None).fuse();
    let mut wait_for_netmgr = pin!(wait_for_netmgr);
    let (if_id, if_name): (u64, String) = interfaces::wait_for_non_loopback_interface_up(
        &interface_state,
        &mut wait_for_netmgr,
        None,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("wait for non loopback interface");
    (interface_state, if_id, if_name.clone())
}
