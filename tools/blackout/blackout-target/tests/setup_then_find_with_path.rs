// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    blackout_target::{find_partition, set_up_partition},
    fuchsia_async as fasync,
    ramdevice_client::RamdiskClient,
};

#[fasync::run_singlethreaded(test)]
async fn setup_then_find_with_path() {
    let ramdisk = RamdiskClient::create(8192, 128).await.unwrap();
    let setup_controller = set_up_partition("test-label", ramdisk.as_dir(), false)
        .await
        .expect("failed to set up device");
    let setup_topo_path = setup_controller
        .get_topological_path()
        .await
        .expect("transport error on get_topological_path")
        .expect("failed to get topological path");

    let find_controller =
        find_partition("test-label", ramdisk.as_dir()).await.expect("failed to find device");
    let find_topo_path = find_controller
        .get_topological_path()
        .await
        .expect("transport error on get_topological_path")
        .expect("failed to get topological path");

    assert_eq!(setup_topo_path, find_topo_path);
}
