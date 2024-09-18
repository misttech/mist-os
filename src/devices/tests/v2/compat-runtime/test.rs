// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fuchsia_component_test::{RealmBuilder, Ref};
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use {
    fidl_fuchsia_compat_runtime_test as ft, fidl_fuchsia_driver_test as fdt,
    fuchsia_async as fasync,
};

#[fasync::run_singlethreaded(test)]
async fn test_compat_runtime() -> Result<()> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;

    let offer = fuchsia_component_test::Capability::protocol::<ft::WaiterMarker>().into();
    let dtr_offers = vec![offer];

    builder.driver_test_realm_add_dtr_offers(&dtr_offers, Ref::parent()).await?;
    let instance = builder.build().await?;

    // Start the DriverTestRealm.
    let args = fdt::RealmArgs {
        root_driver: Some("#meta/root.cm".to_string()),
        dtr_offers: Some(dtr_offers),
        ..Default::default()
    };
    instance.driver_test_realm_start(args).await?;

    // Connect to our driver.
    let dev = instance.driver_test_realm_connect_to_dev()?;
    let driver = device_watcher::recursive_wait_and_open::<ft::LeafMarker>(&dev, "v1/leaf").await?;
    let response = driver.get_string().await.unwrap();
    assert_eq!(response, "hello world!");
    Ok(())
}
