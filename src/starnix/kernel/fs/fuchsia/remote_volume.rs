// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_fshost::{StarnixVolumeProviderMarker, StarnixVolumeProviderSynchronousProxy};
use fidl_fuchsia_fxfs::{CryptMarker, KeyPurpose};
use starnix_core::fs::fuchsia::{RemoteFs, RemoteNode};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    derive_wrapping_key, CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FsNode, FsNodeHandle, FsStr,
};
use starnix_logging::{log_error, log_info};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio, statfs};
use std::sync::Arc;
use syncio::{zxio_node_attr_has_t, zxio_node_attributes_t, Zxio};
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

const CRYPT_THREAD_ROLE: &str = "fuchsia.starnix.remotevol.crypt";
// `KEY_FILE_PATH` determines where the volume-wide keys for the Starnix volume will live in the
// container's data storage capability. The `KEY_FILE_SIZE` is the size of the key file, which
// contains the metadata key in the first half of the file and the data key in the second half.
const KEY_FILE_SIZE: usize = 64;
const KEY_FILE_PATH: &str = "key_file";

pub struct RemoteVolume {
    remotefs: RemoteFs,
    volume_provider: StarnixVolumeProviderSynchronousProxy,
}

impl RemoteVolume {
    pub fn remotefs(&self) -> &RemoteFs {
        &self.remotefs
    }
}

impl FileSystemOps for RemoteVolume {
    fn statfs(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        fs: &FileSystem,
        current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        self.remotefs.statfs(locked, fs, current_task)
    }

    fn name(&self) -> &'static FsStr {
        self.remotefs.name()
    }

    fn generate_node_ids(&self) -> bool {
        self.remotefs.generate_node_ids()
    }

    fn rename(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        fs: &FileSystem,
        current_task: &CurrentTask,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        renamed: &FsNodeHandle,
        replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        self.remotefs.rename(
            locked,
            fs,
            current_task,
            old_parent,
            old_name,
            new_parent,
            new_name,
            renamed,
            replaced,
        )
    }

    fn unmount(&self) {
        match self.volume_provider.unmount(zx::MonotonicInstant::INFINITE) {
            Ok(Ok(_)) => (),
            Ok(Err(e)) => log_error!(e:%; "StarnixVolumeProvider.Unmount failed"),
            Err(e) => log_error!(e:%; "StarnixVolumeProvider.Unmount failed at FIDL layer"),
        }
    }
}

fn get_or_create_volume_keys(
    data: &fio::DirectorySynchronousProxy,
    key_path: &str,
) -> Result<(Vec<u8>, Vec<u8>, bool), Errno> {
    match syncio::directory_read_file(data, key_path, zx::MonotonicInstant::INFINITE) {
        Ok(key) => {
            let mut metadata_key = key;
            let data_key = metadata_key.split_off(KEY_FILE_SIZE / 2);
            Ok((metadata_key, data_key, false))
        }
        Err(_) => {
            log_info!("No key file exists. Creating one.");
            let mut raw_key = vec![0u8; KEY_FILE_SIZE];
            zx::cprng_draw(&mut raw_key);
            let tmp_file = syncio::directory_create_tmp_file(
                data,
                fio::PERM_READABLE,
                zx::MonotonicInstant::INFINITE,
            )
            .map_err(|e| {
                let err = from_status_like_fdio!(e);
                log_error!("Failed to create tmp file with error: {:?}", err);
                err
            })?;
            tmp_file
                .write(&raw_key, zx::MonotonicInstant::INFINITE)
                .map_err(|e| {
                    log_error!("FIDL transport error on File.Write {:?}", e);
                    errno!(ENOENT)
                })?
                .map_err(|e| {
                    let err = from_status_like_fdio!(zx::Status::from_raw(e));
                    log_error!("File.Write failed with {:?}", err);
                    err
                })?;
            tmp_file
                .sync(zx::MonotonicInstant::INFINITE)
                .map_err(|e| {
                    log_error!("FIDL transport error on File.Sync {:?}", e);
                    errno!(ENOENT)
                })?
                .map_err(|e| {
                    let err = from_status_like_fdio!(zx::Status::from_raw(e));
                    log_error!("File.Sync failed with {:?}", err);
                    err
                })?;
            let (status, token) = data.get_token(zx::MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("transport error on get_token for the data directory, error: {:?}", e);
                errno!(ENOENT)
            })?;
            zx::Status::ok(status).map_err(|e| {
                let err = from_status_like_fdio!(e);
                log_error!("Failed to get_token for the data directory, error: {:?}", err);
                err
            })?;

            tmp_file
                .link_into(
                    zx::Event::from(token.ok_or_else(|| errno!(ENOENT))?),
                    key_path,
                    zx::MonotonicInstant::INFINITE,
                )
                .map_err(|e| {
                    log_error!("FIDL transport error on File.LinkInto {:?}", e);
                    errno!(EIO)
                })?
                .map_err(|e| {
                    let err = from_status_like_fdio!(zx::Status::from_raw(e));
                    log_error!("File.LinkInto failed with {:?}", err);
                    err
                })?;
            let data_key = raw_key.split_off(KEY_FILE_SIZE / 2);
            Ok((raw_key, data_key, true))
        }
    }
}

pub fn new_remote_vol(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let kernel = current_task.kernel();
    let volume_provider = current_task
        .kernel()
        .connect_to_protocol_at_container_svc::<StarnixVolumeProviderMarker>()
        .map_err(|_| errno!(ENOENT))?
        .into_sync_proxy();

    let (crypt_client_end, crypt_proxy) = fidl::endpoints::create_endpoints::<CryptMarker>();

    let data = match kernel.container_namespace.get_namespace_channel("/data") {
        Ok(channel) => fio::DirectorySynchronousProxy::new(channel),
        Err(err) => {
            log_error!("Unable to find a channel for /data. Received error: {}", err);
            return Err(errno!(ENOENT));
        }
    };

    let (metadata_encryption_key, data_encryption_key, created_key_file) =
        get_or_create_volume_keys(&data, KEY_FILE_PATH)?;

    let (metadata_wrapping_key_id, metadata_wrapping_key_bytes) =
        derive_wrapping_key(&metadata_encryption_key);

    let (data_wrapping_key_id, data_wrapping_key_bytes) = derive_wrapping_key(&data_encryption_key);

    let (exposed_dir_client_end, exposed_dir_server) =
        fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();

    let crypt = kernel.crypt_service.clone();
    crypt.add_wrapping_key(metadata_wrapping_key_id, metadata_wrapping_key_bytes.to_vec(), 0)?;
    crypt.add_wrapping_key(data_wrapping_key_id, data_wrapping_key_bytes.to_vec(), 0)?;

    crypt.set_active_key(metadata_wrapping_key_id, KeyPurpose::Metadata)?;
    crypt.set_active_key(data_wrapping_key_id, KeyPurpose::Data)?;

    kernel.kthreads.spawner().spawn_with_role(CRYPT_THREAD_ROLE, |_, _| {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            if let Err(e) = crypt.handle_connection(crypt_proxy.into_stream()).await {
                log_error!("Error while handling a Crypt request {e}");
            }
        });
    });

    if created_key_file {
        volume_provider
            .create(crypt_client_end, exposed_dir_server, zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("FIDL transport error on StarnixVolumeProvider.Mount {:?}", e);
                errno!(ENOENT)
            })?
            .map_err(|e| {
                let err = from_status_like_fdio!(zx::Status::from_raw(e));
                log_error!("StarnixVolumeProvider.Mount failed with {:?}", err);
                err
            })?;
    } else {
        volume_provider
            .mount(crypt_client_end, exposed_dir_server, zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("FIDL transport error on StarnixVolumeProvider.Mount {:?}", e);
                errno!(ENOENT)
            })?
            .map_err(|e| {
                let err = from_status_like_fdio!(zx::Status::from_raw(e));
                log_error!("StarnixVolumeProvider.Mount failed with {:?}", err);
                err
            })?;
    }

    let root = syncio::directory_open_directory_async(
        &exposed_dir_client_end.into_sync_proxy(),
        "root",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .map_err(|e| errno!(EIO, format!("Failed to open root: {e}")))?;

    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;

    let (client_end, server_end) = zx::Channel::create();
    let remotefs = RemoteFs::new(root.into_channel(), server_end)?;
    let mut attrs = zxio_node_attributes_t {
        has: zxio_node_attr_has_t { id: true, ..Default::default() },
        ..Default::default()
    };
    let (remote_node, node_id) =
        match Zxio::create_with_on_representation(client_end.into(), Some(&mut attrs)) {
            Err(status) => return Err(from_status_like_fdio!(status)),
            Ok(zxio) => (RemoteNode::new(Arc::new(zxio), rights), attrs.id),
        };

    let use_remote_ids = remotefs.use_remote_ids();
    let remotevol = RemoteVolume { remotefs, volume_provider };
    let fs =
        FileSystem::new(&kernel, CacheMode::Cached(CacheConfig::default()), remotevol, options)?;
    let mut root_node = FsNode::new_root(remote_node);
    if use_remote_ids {
        root_node.node_id = node_id;
    }
    fs.set_root_node(root_node);
    Ok(fs)
}
