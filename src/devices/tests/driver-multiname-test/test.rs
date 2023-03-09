// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_driver_test as fdt, fuchsia_async as fasync,
    fuchsia_component_test::new::RealmBuilder,
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon as zx,
};

#[fasync::run_singlethreaded(test)]
async fn test_sample_driver() {
    // Start the driver test realm.
    let builder = RealmBuilder::new().await.unwrap();
    let _: &RealmBuilder = builder.driver_test_realm_setup().await.unwrap();
    let instance = builder.build().await.unwrap();
    let () = instance.driver_test_realm_start(fdt::RealmArgs::EMPTY).await.unwrap();

    // Connect to our driver.
    let dev = instance.driver_test_realm_connect_to_dev().unwrap();
    let parent = device_watcher::recursive_wait_and_open::<
        fidl_driver_multiname_test::TestAddDeviceMarker,
    >(&dev, "sys/test/parent_device")
    .await
    .unwrap();

    // Call a FIDL method to add a device. This should succeed.
    let () = parent.add_device().await.unwrap().unwrap();

    // Make sure the child device exists.
    let child =
        device_watcher::recursive_wait_and_open_directory(&dev, "sys/test/parent_device/duplicate")
            .await
            .unwrap();
    let () = child.close().await.unwrap().unwrap();

    // Call it again to add a second device with the same name, which should fail.
    let response = parent.add_device().await.unwrap();
    assert_eq!(response.map_err(zx::Status::from_raw), Err(zx::Status::BAD_STATE));
}
