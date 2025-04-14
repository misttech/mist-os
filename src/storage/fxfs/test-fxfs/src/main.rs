// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use fidl::endpoints::{create_proxy, ClientEnd, DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl_fuchsia_fshost::{
    StarnixVolumeProviderMarker, StarnixVolumeProviderRequest, StarnixVolumeProviderRequestStream,
};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fidl_fuchsia_io::{self as fio, DirectoryMarker};
use fidl_fuchsia_test_fxfs::{
    StarnixVolumeAdminMarker, StarnixVolumeAdminRequest, StarnixVolumeAdminRequestStream,
};
use fuchsia_component_client::connect_to_protocol;
use fuchsia_runtime::HandleType;
use fuchsia_sync::Mutex;
use futures::StreamExt;
use fxfs::errors::FxfsError;
use fxfs::filesystem::FxFilesystem;
use fxfs::object_store::volume::root_volume;
use fxfs_crypto::Crypt;
use fxfs_platform::fuchsia::RemoteCrypt;
use fxfs_platform::volumes_directory::VolumesDirectory;
use std::sync::{Arc, Weak};
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::ToObjectRequest;

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

struct MountedVolume {
    store_id: u64,
    root_dir: fio::DirectoryProxy,
}

async fn mount_user_volume(
    crypt: ClientEnd<CryptMarker>,
    starnix_exposed_dir: ServerEnd<DirectoryMarker>,
    volumes_directory: &Arc<VolumesDirectory>,
    mounted_volume: &Mutex<Option<MountedVolume>>,
    inspect_node: &Mutex<fuchsia_inspect::Node>,
) -> Result<(), Error> {
    let remote_crypt = Arc::new(RemoteCrypt::new(crypt));
    let vol = match volumes_directory
        .mount_volume(USER_VOLUME_NAME, Some(remote_crypt.clone() as Arc<dyn Crypt>), false)
        .await
    {
        Ok(vol) => vol,
        Err(e) if FxfsError::NotFound.matches(&e) => {
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

    let (exposed_dir, server_end) = create_proxy::<fio::DirectoryMarker>();
    volumes_directory.serve_volume(&vol, server_end, false).context("failed to serve volume")?;
    exposed_dir.clone(starnix_exposed_dir.into_channel().into())?;
    update_mounted_volume(mounted_volume, exposed_dir, vol.volume().store().store_object_id())
        .await?;

    inspect_node.lock().record_bool("mounted", true);
    Ok(())
}

async fn update_mounted_volume(
    mounted_volume: &Mutex<Option<MountedVolume>>,
    exposed_dir: fio::DirectoryProxy,
    store_id: u64,
) -> Result<(), Error> {
    let mut guard = mounted_volume.lock();
    let (root_dir_client_end, server_end) = zx::Channel::create();
    exposed_dir.open(
        "root",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
        &Default::default(),
        server_end,
    )?;
    *guard = Some(MountedVolume {
        store_id: store_id,
        root_dir: ClientEnd::<fio::DirectoryMarker>::new(root_dir_client_end).into_proxy(),
    });

    Ok(())
}

async fn delete_user_volume(volumes_directory: &Arc<VolumesDirectory>) -> Result<(), Error> {
    volumes_directory.remove_volume(USER_VOLUME_NAME).await?;
    Ok(())
}

async fn get_user_volume_root(
    mounted_volume: &Mutex<Option<MountedVolume>>,
) -> Result<fio::DirectoryProxy, Error> {
    let guard = mounted_volume.lock();
    let (root, server_end) = create_proxy::<fio::DirectoryMarker>();
    if let Some(vol) = &*guard {
        vol.root_dir.clone(server_end.into_channel().into())?;
        Ok(root)
    } else {
        Err(anyhow!("tried to get the root of an unmounted volume"))
    }
}

async fn create_user_volume(
    crypt: ClientEnd<CryptMarker>,
    starnix_exposed_dir: ServerEnd<DirectoryMarker>,
    volumes_directory: &Arc<VolumesDirectory>,
    mounted_volume: &Mutex<Option<MountedVolume>>,
    inspect_node: &Mutex<fuchsia_inspect::Node>,
) -> Result<(), Error> {
    let remote_crypt = Arc::new(RemoteCrypt::new(crypt));
    if let Some(vol) = mounted_volume.lock().take() {
        volumes_directory.lock().await.unmount(vol.store_id).await.context("unmount failed")?;
        inspect_node.lock().record_bool("mounted", false);
    }
    let vol = match volumes_directory
        .create_and_mount_volume(
            USER_VOLUME_NAME,
            Some(remote_crypt.clone() as Arc<dyn Crypt>),
            false,
        )
        .await
    {
        Ok(vol) => vol,
        Err(e) if FxfsError::AlreadyExists.matches(&e) => {
            volumes_directory.remove_volume(USER_VOLUME_NAME).await?;
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

    let (exposed_dir, server_end) = create_proxy::<fio::DirectoryMarker>();
    volumes_directory.serve_volume(&vol, server_end, false).context("failed to serve volume")?;
    exposed_dir.clone(starnix_exposed_dir.into_channel().into())?;
    update_mounted_volume(mounted_volume, exposed_dir, vol.volume().store().store_object_id())
        .await?;

    inspect_node.lock().record_bool("mounted", true);
    Ok(())
}

async fn unmount_user_volume(
    volumes_directory: &Arc<VolumesDirectory>,
    mounted_volume: &Mutex<Option<MountedVolume>>,
    inspect_node: &Mutex<fuchsia_inspect::Node>,
) -> Result<(), Error> {
    if let Some(vol) = mounted_volume.lock().take() {
        volumes_directory.lock().await.unmount(vol.store_id).await.context("unmount failed")?;
        inspect_node.lock().record_bool("mounted", false);
        Ok(())
    } else {
        Err(anyhow!("tried to unmount a volume that was never mounted"))
    }
}

async fn handle_starnix_volume_admin_requests(
    mut stream: StarnixVolumeAdminRequestStream,
    volumes_directory: Arc<VolumesDirectory>,
    mounted_volume: Arc<Mutex<Option<MountedVolume>>>,
) {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            StarnixVolumeAdminRequest::Delete { responder } => {
                log::info!("volume admin delete called");
                let res = match delete_user_volume(&volumes_directory).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        log::error!("volume admin service: delete failed: {:?}", e);
                        Err(zx::Status::INTERNAL.into_raw())
                    }
                };
                responder.send(res).unwrap_or_else(|e| {
                    log::error!("failed to send Delete response. error: {:?}", e);
                });
            }
            StarnixVolumeAdminRequest::GetRoot { responder } => {
                log::info!("volume admin get_root called");
                let res: Result<ClientEnd<fio::DirectoryMarker>, i32> =
                    match get_user_volume_root(&mounted_volume).await {
                        Ok(root_dir) => Ok(root_dir.into_client_end().unwrap()),
                        Err(e) => {
                            log::error!("volume admin service: get_root failed: {:?}", e);
                            Err(zx::Status::INTERNAL.into_raw())
                        }
                    };
                responder.send(res).unwrap_or_else(|e| {
                    log::error!("failed to send GetRoot response. error: {:?}", e);
                });
            }
        }
    }
}

async fn handle_starnix_volume_provider_requests(
    mut stream: StarnixVolumeProviderRequestStream,
    volumes_directory: Arc<VolumesDirectory>,
    inspect_node: Arc<Mutex<fuchsia_inspect::Node>>,
    mounted_volume: Arc<Mutex<Option<MountedVolume>>>,
) {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            StarnixVolumeProviderRequest::Mount { crypt, exposed_dir, responder } => {
                log::info!("volume provider mount called");
                let res = match mount_user_volume(
                    crypt,
                    exposed_dir,
                    &volumes_directory,
                    &mounted_volume,
                    &inspect_node,
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
            StarnixVolumeProviderRequest::Create { crypt, exposed_dir, responder } => {
                log::info!("volume provider create called");
                let res = match create_user_volume(
                    crypt,
                    exposed_dir,
                    &volumes_directory,
                    &mounted_volume,
                    &inspect_node,
                )
                .await
                {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        log::error!("volume provider service: create failed: {:?}", e);
                        Err(zx::Status::INTERNAL.into_raw())
                    }
                };
                responder.send(res).unwrap_or_else(|e| {
                    log::error!("failed to send Create response. error: {:?}", e);
                });
            }
            StarnixVolumeProviderRequest::Unmount { responder } => {
                log::info!("volume provider unmount called");
                let res =
                    match unmount_user_volume(&volumes_directory, &mounted_volume, &inspect_node)
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
    let inspect_node = Arc::new(Mutex::new(inspector.root().create_child("starnix_volume")));

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

    let mounted_volume = Arc::new(Mutex::new(None));
    let svc_dir = vfs::directory::immutable::Simple::new();
    svc_dir
        .add_entry(
            StarnixVolumeProviderMarker::PROTOCOL_NAME,
            vfs::service::host({
                let volumes_directory = volumes_directory.clone();
                let inspect_node = inspect_node.clone();
                let mounted_volume = mounted_volume.clone();
                move |stream| {
                    handle_starnix_volume_provider_requests(
                        stream,
                        volumes_directory.clone(),
                        inspect_node.clone(),
                        mounted_volume.clone(),
                    )
                }
            }),
        )
        .unwrap();
    svc_dir
        .add_entry(
            StarnixVolumeAdminMarker::PROTOCOL_NAME,
            vfs::service::host(move |stream| {
                handle_starnix_volume_admin_requests(
                    stream,
                    volumes_directory.clone(),
                    mounted_volume.clone(),
                )
            }),
        )
        .unwrap();

    let out_dir = vfs::directory::immutable::Simple::new();
    out_dir.add_entry("svc", svc_dir).unwrap();
    out_dir.add_entry("data", vol.root_dir()).unwrap();

    let export_handle = fuchsia_runtime::take_startup_handle(HandleType::DirectoryRequest.into())
        .context("Missing startup handle")?;
    let scope = ExecutionScope::new();
    let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE | fio::PERM_WRITABLE;
    flags
        .to_object_request(export_handle)
        .handle(|object_request| out_dir.open(scope.clone(), Path::dot(), flags, object_request));
    scope.wait().await;

    Ok(())
}
