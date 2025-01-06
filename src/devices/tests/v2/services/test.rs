// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fuchsia_component::client;
use fuchsia_component_test::RealmBuilder;
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use {fidl_fuchsia_driver_test as fdt, fidl_fuchsia_services_test as ft, fuchsia_async as fasync};

#[fasync::run_singlethreaded(test)]
async fn test_services() -> Result<()> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;

    let expose = fuchsia_component_test::Capability::service::<ft::DeviceMarker>().into();
    let dtr_exposes = vec![expose];

    builder.driver_test_realm_add_dtr_exposes(&dtr_exposes).await?;
    // Build the Realm.
    let realm = builder.build().await?;

    // Start the DriverTestRealm.
    let args = fdt::RealmArgs {
        root_driver: Some("#meta/root.cm".to_string()),
        dtr_exposes: Some(dtr_exposes),
        ..Default::default()
    };
    realm.driver_test_realm_start(args).await?;

    // Connect to the `Device` service.
    let device = client::Service::open_from_dir(realm.root.get_exposed_dir(), ft::DeviceMarker)
        .context("Failed to open service")?
        .watch_for_any()
        .await
        .context("Failed to find instance")?;
    // Use the `ControlPlane` protocol from the `Device` service.
    let control = device.connect_to_control()?;
    control.control_do().await?;
    // Use the `DataPlane` protocol from the `Device` service.
    let data = device.connect_to_data()?;
    data.data_do().await?;

    Ok(())
}
