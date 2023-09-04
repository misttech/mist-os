// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test launching filesystems as static child components in a custom environment.

use {
    fidl_fuchsia_fxfs::MountOptions,
    fidl_fuchsia_io as fio,
    fs_management::{filesystem::Filesystem, Blobfs, ComponentType, Fxfs, Minfs},
    ramdevice_client::RamdiskClient,
};

#[fuchsia::test]
async fn blobfs_static_child() {
    let mut ramdisk = RamdiskClient::create(1024, 1 << 16).await.unwrap();

    let config = Blobfs { component_type: ComponentType::StaticChild, ..Default::default() };
    let controller = ramdisk.take_controller().unwrap();
    let mut blobfs = Filesystem::new(controller, config);

    blobfs.format().await.unwrap();
    blobfs.fsck().await.unwrap();
    let fs = blobfs.serve().await.unwrap();
    fs.shutdown().await.unwrap();

    ramdisk.destroy().await.unwrap();
}

#[fuchsia::test]
async fn minfs_static_child() {
    let mut ramdisk = RamdiskClient::create(1024, 1 << 16).await.unwrap();

    let config = Minfs { component_type: ComponentType::StaticChild, ..Default::default() };
    let controller = ramdisk.take_controller().unwrap();
    let mut minfs = Filesystem::new(controller, config);

    minfs.format().await.unwrap();
    minfs.fsck().await.unwrap();
    let fs = minfs.serve().await.unwrap();
    fs.shutdown().await.unwrap();

    ramdisk.destroy().await.unwrap();
}

#[fuchsia::test]
async fn fxfs_static_child() {
    let mut ramdisk = RamdiskClient::create(1024, 1 << 16).await.unwrap();

    let config = Fxfs { component_type: ComponentType::StaticChild, ..Default::default() };
    let controller = ramdisk.take_controller().unwrap();
    let mut fxfs = Filesystem::new(controller, config);

    fxfs.format().await.unwrap();
    fxfs.fsck().await.unwrap();
    let mut fs = fxfs.serve_multi_volume().await.unwrap();

    let volume =
        fs.create_volume("test", MountOptions { crypt: None, as_blob: false }).await.unwrap();
    let _: Box<fio::FilesystemInfo> = volume.query().await.unwrap();

    fs.shutdown().await.unwrap();

    ramdisk.destroy().await.unwrap();
}
