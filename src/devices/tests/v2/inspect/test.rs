// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error, Result};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, DiagnosticsHierarchy, Inspect};
use fuchsia_component_test::RealmBuilder;
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use {fidl_fuchsia_driver_test as fdt, fuchsia_async as fasync};

async fn get_inspect_hierarchy(moniker: String) -> Result<DiagnosticsHierarchy, Error> {
    ArchiveReader::new()
        .add_selector(format!("{}:[name=root-driver]root", moniker))
        .snapshot::<Inspect>()
        .await?
        .into_iter()
        .next()
        .and_then(|result| result.payload)
        .ok_or(format_err!("expected one inspect hierarchy"))
}

#[fasync::run_singlethreaded(test)]
async fn test_driver_inspect() -> Result<()> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    // Build the Realm.
    let instance = builder.build().await?;
    // Start DriverTestRealm
    let args = fdt::RealmArgs {
        root_driver: Some("fuchsia-boot:///dtr#meta/test-parent-sys.cm".to_string()),
        ..Default::default()
    };
    instance.driver_test_realm_start(args).await?;

    // Connect to our driver.
    let dev = instance.driver_test_realm_connect_to_dev()?;
    let driver = device_watcher::recursive_wait_and_open::<
        fidl_fuchsia_inspect_test::HandshakeMarker,
    >(&dev, "sys/test/root-driver")
    .await?;

    let moniker = format!(
        "realm_builder\\:{}/driver_test_realm/realm_builder\\:0/boot-drivers\\:dev.sys.test",
        instance.root.child_name()
    );
    // Check the inspect metrics.
    let mut hierarchy = get_inspect_hierarchy(moniker.clone()).await?;
    assert_data_tree!(hierarchy, root: contains {
        connection_info: contains {
            request_count: 0u64,
        }
    });

    // Do the request and check the inspect metrics again.
    driver.r#do().await.unwrap();

    hierarchy = get_inspect_hierarchy(moniker).await?;
    assert_data_tree!(hierarchy, root: contains {
        connection_info: contains {
            request_count: 1u64,
        }
    });

    Ok(())
}
