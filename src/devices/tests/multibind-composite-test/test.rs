// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    fidl_fuchsia_driver_test as fdt, fuchsia_async as fasync,
    fuchsia_component_test::{RealmBuilder, RealmInstance},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
};

async fn start_driver_test_realm(use_dfv2: bool) -> Result<RealmInstance> {
    let builder = RealmBuilder::new().await.context("Failed to create realm builder")?;
    builder.driver_test_realm_setup().await.context("Failed to setup driver test realm")?;
    let instance = builder.build().await.context("Failed to build realm instance")?;

    let args = fdt::RealmArgs {
        root_driver: Some("fuchsia-boot:///#meta/test-parent-sys.cm".to_string()),
        use_driver_framework_v2: Some(use_dfv2),
        ..Default::default()
    };

    instance.driver_test_realm_start(args).await.context("Failed to start driver test realm")?;

    Ok(instance)
}

// Tests that the legacy and spec composites are successfully assembled, bound, and
// added to the topology in DFv1.
#[fasync::run_singlethreaded(test)]
async fn test_composites_v1() -> Result<()> {
    let instance = start_driver_test_realm(false).await?;
    let dev = instance.driver_test_realm_connect_to_dev()?;

    device_watcher::recursive_wait(&dev, "sys/test/root").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_a").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_b").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_c").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_d").await?;

    device_watcher::recursive_wait(&dev, "sys/test/node_a/legacy_composite_1/composite").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_b/legacy_composite_2/composite").await?;

    device_watcher::recursive_wait(&dev, "sys/test/node_a/spec_1/composite").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_d/spec_2/composite").await?;

    Ok(())
}

// Tests that the legacy and spec composites are successfully assembled, bound, and
// added to the topology in DFv2.
// TODO(fxb/122531): Re-enable after fixing multibind in DFv2.
#[ignore]
#[fasync::run_singlethreaded(test)]
async fn test_composites_v2() -> Result<()> {
    let instance = start_driver_test_realm(true).await?;
    let dev = instance.driver_test_realm_connect_to_dev()?;

    device_watcher::recursive_wait(&dev, "sys/test/root").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_a").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_b").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_c").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_d").await?;

    device_watcher::recursive_wait(&dev, "sys/test/node_a/legacy_composite_1/composite").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_b/legacy_composite_2/composite").await?;

    device_watcher::recursive_wait(&dev, "sys/test/node_a/spec_1/composite").await?;
    device_watcher::recursive_wait(&dev, "sys/test/node_d/spec_2/composite").await?;

    Ok(())
}
