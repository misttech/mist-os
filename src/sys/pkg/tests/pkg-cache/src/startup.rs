// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {crate::TestEnv, mock_reboot::RebootReason};

#[fuchsia::test]
async fn reboots_on_startup_failure() {
    let env = TestEnv::builder()
        .blobfs_and_system_image_hash(
            blobfs_ramdisk::BlobfsRamdisk::builder().impl_from_env().start().await.unwrap(),
            Some([0u8; 32].into()),
        )
        .build()
        .await;

    let _ = env.proxies.package_cache.sync().await;

    assert_eq!(env.take_reboot_reasons(), vec![RebootReason::CriticalComponentFailure]);
}

#[fuchsia::test]
async fn does_not_reboot_on_startup_success() {
    let env = TestEnv::builder().build().await;

    let _ = env.proxies.package_cache.sync().await;

    assert_eq!(env.take_reboot_reasons(), vec![]);
}
