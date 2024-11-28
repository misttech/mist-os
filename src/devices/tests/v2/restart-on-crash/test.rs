// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fidl::endpoints::ServiceMarker;
use fuchsia_async::{self as fasync, DurationExt, Timer};
use fuchsia_component::client;
use fuchsia_component_test::RealmBuilder;
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use {
    fidl_fuchsia_component_test as ftest, fidl_fuchsia_crashdriver_test as fcdt,
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_test as fdt,
};

fn send_get_device_info_request(
    service: &fdd::ManagerProxy,
    device_filter: &[String],
    exact_match: bool,
) -> Result<fdd::NodeInfoIteratorProxy> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::NodeInfoIteratorMarker>();

    service
        .get_node_info(device_filter, iterator_server, exact_match)
        .context("FIDL call to get device info failed")?;

    Ok(iterator)
}

async fn get_device_info(
    service: &fdd::ManagerProxy,
    device_filter: &[String],
    exact_match: bool,
) -> Result<Vec<fdd::NodeInfo>> {
    let iterator = send_get_device_info_request(service, device_filter, exact_match)?;

    let mut device_infos = Vec::new();
    loop {
        let mut device_info =
            iterator.get_next().await.context("FIDL call to get device info failed")?;
        if device_info.len() == 0 {
            break;
        }
        device_infos.append(&mut device_info);
    }
    Ok(device_infos)
}

async fn wait_for_instance(realm: &fuchsia_component_test::RealmInstance) -> Result<String> {
    let instance;
    let service = client::open_service_at_dir::<fcdt::DeviceMarker>(realm.root.get_exposed_dir())
        .context("Failed to open service")?;
    loop {
        // TODO(https://fxbug.dev/42124541): Once component manager supports watching for
        // service instances, this loop shousld be replaced by a watcher.
        let entries = fuchsia_fs::directory::readdir(&service)
            .await
            .context("Failed to read service instances")?;
        if let Some(entry) = entries.iter().next() {
            instance = entry.name.clone();
            break;
        }
        Timer::new(zx::MonotonicDuration::from_millis(100).after_now()).await;
    }

    return Ok(instance);
}

#[fasync::run_singlethreaded(test)]
async fn test_restart_on_crash() -> Result<()> {
    let exposes = vec![ftest::Capability::Service(ftest::Service {
        name: Some(fcdt::DeviceMarker::SERVICE_NAME.to_string()),
        ..Default::default()
    })];

    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    builder.driver_test_realm_add_dtr_exposes(&exposes).await?;
    // Build the Realm.
    let realm = builder.build().await?;

    // Start the DriverTestRealm.
    let args = fdt::RealmArgs {
        root_driver: Some("#meta/crasher.cm".to_string()),
        dtr_exposes: Some(exposes),
        ..Default::default()
    };
    realm.driver_test_realm_start(args).await?;

    // Find an instance of the `Device` service.
    let instance = wait_for_instance(&realm).await?;

    let driver_dev = realm.root.connect_to_protocol_at_exposed_dir::<fdd::ManagerMarker>()?;
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
    assert_eq!(1, device_infos.len());
    let driver_host_koid_1 = device_infos[0].driver_host_koid;

    // Connect to the `Device` service.
    let device = client::connect_to_service_instance_at_dir::<fcdt::DeviceMarker>(
        realm.root.get_exposed_dir(),
        &instance,
    )
    .context("Failed to open service")?;
    let crasher = device.connect_to_crasher()?;
    let pong_1 = crasher.ping().await?;

    // CRASH
    crasher.crash()?;

    // Wait until the node comes back with a new host.
    let mut driver_host_koid_2: Option<u64>;
    loop {
        let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
        assert_eq!(1, device_infos.len());
        driver_host_koid_2 = device_infos[0].driver_host_koid;
        if driver_host_koid_2.is_some() && driver_host_koid_2 != driver_host_koid_1 {
            break;
        }
        Timer::new(zx::MonotonicDuration::from_millis(100).after_now()).await;
    }

    assert_ne!(driver_host_koid_1, driver_host_koid_2);

    // Connect to the new one.
    let instance = wait_for_instance(&realm).await?;
    let device = client::connect_to_service_instance_at_dir::<fcdt::DeviceMarker>(
        realm.root.get_exposed_dir(),
        &instance,
    )
    .context("Failed to open service")?;
    let crasher = device.connect_to_crasher()?;

    // Check that it is able to communicate with the new one.
    let pong_2 = crasher.ping().await?;
    assert_ne!(pong_1, pong_2);
    Ok(())
}
