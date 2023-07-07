// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {crate::TestEnv, fidl_fuchsia_io as fio, fuchsia_zircon::Status, futures::TryFutureExt};

struct BrokenBlobfs;

impl crate::Blobfs for BrokenBlobfs {
    fn root_proxy(&self) -> fio::DirectoryProxy {
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap().0
    }
    fn svc_dir(&self) -> fio::DirectoryProxy {
        panic!("BrokenBlobfs does not have a svc dir")
    }
}

#[fuchsia::test]
async fn sync_success() {
    let env = TestEnv::builder().build().await;

    let res = env.proxies.package_cache.sync().await;

    assert_eq!(res.unwrap(), Ok(()));
}

#[fuchsia::test]
async fn sync_returns_errs() {
    let env = TestEnv::builder()
        .blobfs_and_system_image_hash(BrokenBlobfs, None)
        .ignore_system_image()
        .build()
        .await;

    assert_eq!(
        env.proxies.package_cache.sync().map_ok(|res| res.map_err(Status::from_raw)).await.unwrap(),
        Err(Status::INTERNAL)
    );
}
