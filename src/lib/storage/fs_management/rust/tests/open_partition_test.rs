// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fs_management::partition::{find_partition, PartitionMatcher},
    fuchsia_zircon::Duration,
    ramdevice_client::RamdiskClient,
};

#[fuchsia::test]
async fn find_partition_test() {
    let _ramdisk = RamdiskClient::create(1024, 1 << 16).await.unwrap();
    let matcher = PartitionMatcher {
        parent_device: Some(String::from("/dev/sys/platform")),
        ..Default::default()
    };

    let controller = find_partition(matcher, Duration::from_seconds(10)).await.unwrap();
    assert_eq!(
        &controller.get_topological_path().await.unwrap().unwrap(),
        "/dev/sys/platform/00:00:2d/ramctl/ramdisk-0/block",
    );
}
