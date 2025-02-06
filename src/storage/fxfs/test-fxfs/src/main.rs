// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use fidl::endpoints::{create_proxy, ClientEnd, Proxy, ServerEnd};
use fidl_fuchsia_fshost::{StarnixVolumeProviderRequest, StarnixVolumeProviderRequestStream};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fidl_fuchsia_io::{self as fio, DirectoryMarker};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use fxfs::errors::FxfsError;
use fxfs::filesystem::FxFilesystem;
use fxfs::object_store::volume::root_volume;
use fxfs_crypto::Crypt;
use fxfs_platform::fuchsia::RemoteCrypt;
use fxfs_platform::volume::RootDir;
use fxfs_platform::volumes_directory::VolumesDirectory;
use std::sync::{Arc, Weak};
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;
use vfs::path::Path;

const BLOCK_SIZE: u32 = 4096; // 8KiB
const USER_VOLUME_NAME: &str = "test_fxfs_user_volume";

pub const DATA_KEY: [u8; 32] = [
    0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0x10, 0x11,
    0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
pub const METADATA_KEY: [u8; 32] = [
    0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1, 0xf0,
    0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4, 0xe3, 0xe2, 0xe1, 0xe0,
];

struct StarnixVolumeProvider(StarnixVolumeProviderRequestStream);

async fn mount_user_volume(
    crypt: ClientEnd<CryptMarker>,
    exposed_dir: ServerEnd<DirectoryMarker>,
    volumes_directory: &Arc<VolumesDirectory>,
    store_id: &mut Option<u64>,
    inspect_node: &fuchsia_inspect::Node,
) -> Result<(), Error> {
    let remote_crypt = Arc::new(RemoteCrypt::new(crypt));
    let vol = match volumes_directory
        .mount_volume(USER_VOLUME_NAME, Some(remote_crypt.clone() as Arc<dyn Crypt>), false)
        .await
    {
        Ok(vol) => vol,
        Err(e) if *e.root_cause().downcast_ref::<FxfsError>().unwrap() == FxfsError::NotFound => {
            volumes_directory
                .create_and_mount_volume(
                    USER_VOLUME_NAME,
                    Some(remote_crypt as Arc<dyn Crypt>),
                    false,
                )
                .await?
        }
        Err(e) => return Err(e),
    };
    *store_id = Some(vol.volume().store().store_object_id());
    volumes_directory.serve_volume(&vol, exposed_dir, false).context("failed to serve volume")?;
    inspect_node.record_bool("mounted", true);
    Ok(())
}

async fn unmount_user_volume(
    volumes_directory: &Arc<VolumesDirectory>,
    store_id: &mut Option<u64>,
    inspect_node: &fuchsia_inspect::Node,
) -> Result<(), Error> {
    if let Some(store_id) = store_id.take() {
        volumes_directory.lock().await.unmount(store_id).await.context("unmount failed")?;
        inspect_node.record_bool("mounted", false);
        Ok(())
    } else {
        Err(anyhow!("tried to unmount a volume that was never mounted"))
    }
}

async fn run(
    mut stream: StarnixVolumeProviderRequestStream,
    volumes_directory: Arc<VolumesDirectory>,
    inspect_node: &fuchsia_inspect::Node,
) -> Result<(), Error> {
    let volumes_directory = volumes_directory.clone();
    let mut store_id = None;
    while let Some(request) = stream.try_next().await? {
        match request {
            StarnixVolumeProviderRequest::Mount { crypt, exposed_dir, responder } => {
                log::info!("volume provider mount called");
                let res = match mount_user_volume(
                    crypt,
                    exposed_dir,
                    &volumes_directory,
                    &mut store_id,
                    inspect_node,
                )
                .await
                {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        log::error!("volume provider service: mount failed: {:?}", e);
                        Err(zx::Status::INTERNAL.into_raw())
                    }
                };
                responder.send(res).unwrap_or_else(|e| {
                    log::error!("failed to send Mount response. error: {:?}", e);
                });
            }
            StarnixVolumeProviderRequest::Unmount { responder } => {
                log::info!("volume provider unmount called");
                let res = match unmount_user_volume(&volumes_directory, &mut store_id, inspect_node)
                    .await
                {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        log::error!("volume provider service: unmount failed: {:?}", e);
                        Err(zx::Status::INTERNAL.into_raw())
                    }
                };
                responder.send(res).unwrap_or_else(|e| {
                    log::error!("failed to send Unmount response. error: {:?}", e);
                });
            }
        }
    }

    Ok(())
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    // Android's bionic unit tests will fail with a smaller disk.
    // TODO(https://fxbug.dev/378744012): Make the size of FakeDevice configurable.
    let device = DeviceHolder::new(FakeDevice::new(393216, BLOCK_SIZE));
    let filesystem = FxFilesystem::new_empty(device).await.unwrap();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());
    let inspect_node = inspector.root().create_child("starnix_volume");

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

    let volumes_directory = VolumesDirectory::new(
        root_volume(filesystem.clone()).await.context("root_volume failed")?,
        Weak::new(),
        None,
    )
    .await
    .context("failed to create the VolumesDirectory")?;

    let vol = volumes_directory
        .create_and_mount_volume("vol", Some(crypt.clone()), false)
        .await
        .context("create and mount volume failed on vol")?;
    let (root, server_end) = create_proxy::<fio::DirectoryMarker>();

    vol.root_dir().as_directory().open(
        vol.volume().scope().clone(),
        fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        Path::dot(),
        ServerEnd::new(server_end.into_channel()),
    );

    let mut fs = ServiceFs::new();
    fs.add_remote("data", root);
    fs.dir("svc").add_fidl_service(StarnixVolumeProvider);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |StarnixVolumeProvider(stream)| {
        run(stream, volumes_directory.clone(), &inspect_node)
            .unwrap_or_else(|e| log::error!("{:?}", e))
    })
    .await;

    Ok(())
}
