// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    fidl::endpoints::{create_endpoints, create_proxy},
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_wlan_device_service as fidl_device,
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_fuchsia_wlan_tap as fidl_wlantap,
    fidl_test_wlan_realm as ftest,
    fuchsia_component::client::connect_to_protocol,
    realm_proxy::client::RealmProxyClient,
    tracing::info,
};

async fn create_realm(options: ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();

    realm_factory.set_realm_options(options).await?.map_err(realm_proxy::Error::OperationError)?;
    realm_factory.create_realm(server).await?.map_err(realm_proxy::Error::OperationError)?;

    Ok(RealmProxyClient::from(client))
}

#[fuchsia::test]
async fn test_realm_is_setup() -> Result<()> {
    let (devfs_client, devfs_server) = create_proxy()?;
    let realm_options =
        ftest::RealmOptions { devfs_server_end: Some(devfs_server), ..Default::default() };
    let realm = create_realm(realm_options).await?;

    info!("connected to test realm!");

    // check that we can connect to the exposed protocols
    let _wlan_client = realm.connect_to_protocol::<fidl_policy::ClientProviderMarker>().await?;

    let _wlan_ap = realm.connect_to_protocol::<fidl_policy::AccessPointProviderMarker>().await?;

    let _driver_test_realm = realm.connect_to_protocol::<fdt::RealmMarker>().await?;

    let _device_service = realm.connect_to_protocol::<fidl_device::DeviceMonitorMarker>().await?;
    let driver_test_realm = realm.connect_to_protocol::<fdt::RealmMarker>().await?;

    info!("connected to protocols");

    let _ = driver_test_realm
        .start(fdt::RealmArgs { use_driver_framework_v2: Some(true), ..Default::default() })
        .await?;

    // Open wlantapctl
    let _wlantapctl = device_watcher::recursive_wait_and_open::<fidl_wlantap::WlantapCtlMarker>(
        &devfs_client,
        "sys/test/wlantapctl",
    )
    .await?;

    Ok(())
}
