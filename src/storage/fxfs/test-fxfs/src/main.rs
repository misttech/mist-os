// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl::endpoints::{create_proxy, Proxy, ServerEnd};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fidl_fuchsia_io as fio;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use fxfs::filesystem::FxFilesystem;
use fxfs::object_store::volume::root_volume;
use fxfs::object_store::Directory;
use fxfs_platform::directory::FxDirectory;
use fxfs_platform::fuchsia::RemoteCrypt;
use fxfs_platform::volume::{FxVolume, RootDir};
use std::sync::{Arc, Weak};
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;
use vfs::path::Path;

const BLOCK_SIZE: u32 = 4096; // 8KiB
pub const DATA_KEY: [u8; 32] = [
    0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0x10, 0x11,
    0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
pub const METADATA_KEY: [u8; 32] = [
    0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1, 0xf0,
    0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4, 0xe3, 0xe2, 0xe1, 0xe0,
];

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    // Android's bionic unit tests will fail with a smaller disk.
    // TODO(https://fxbug.dev/378744012): Make the size of FakeDevice configurable.
    let device = DeviceHolder::new(FakeDevice::new(393216, BLOCK_SIZE));
    let filesystem = FxFilesystem::new_empty(device).await.unwrap();

    let crypt_management = connect_to_protocol::<CryptManagementMarker>()?;
    let wrapping_key_id_0 = [0; 16];
    let mut wrapping_key_id_1 = [0; 16];
    wrapping_key_id_1[0] = 1;
    crypt_management
        .add_wrapping_key(&wrapping_key_id_0, &DATA_KEY)
        .await
        .expect("FIDL transport error")
        .expect("failed to add data wrapping key");
    crypt_management
        .add_wrapping_key(&wrapping_key_id_1, &METADATA_KEY)
        .await
        .expect("FIDL transport error")
        .expect("failed to add metadata wrapping key");
    crypt_management
        .set_active_key(KeyPurpose::Data, &wrapping_key_id_0)
        .await
        .expect("FIDL transport error")
        .expect("failed to set active data key");
    crypt_management
        .set_active_key(KeyPurpose::Metadata, &wrapping_key_id_1)
        .await
        .expect("FIDL transport error")
        .expect("failed to set active metadata key");

    let crypt_proxy =
        connect_to_protocol::<CryptMarker>().expect("failed to connect to the Crypt protocol");
    let crypt = Arc::new(RemoteCrypt::new(
        crypt_proxy
            .into_channel()
            .expect("failed to convert CryptProxy into a channel")
            .into_zx_channel()
            .into(),
    ));

    let root_volume = root_volume(filesystem.clone()).await.context("root_volume failed")?;
    let store =
        root_volume.new_volume("vol", Some(crypt.clone())).await.context("new_volume failed")?;
    let volume = Arc::new(FxVolume::new(Weak::new(), store.clone(), store.store_object_id())?);
    let root_object_id = volume.store().root_directory_object_id();
    let root_dir = Arc::new(FxDirectory::from(Directory::open(&volume, root_object_id).await?));

    let (root, server_end) = create_proxy::<fio::DirectoryMarker>().expect("create_proxy failed");

    root_dir.as_directory().open(
        volume.scope().clone(),
        fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        Path::dot(),
        ServerEnd::new(server_end.into_channel()),
    );

    let mut fs = ServiceFs::new();
    fs.add_remote("data", root);
    fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;
    Ok(())
}
