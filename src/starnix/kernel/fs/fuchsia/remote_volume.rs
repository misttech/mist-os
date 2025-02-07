// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_fshost::{StarnixVolumeProviderMarker, StarnixVolumeProviderSynchronousProxy};
use fidl_fuchsia_fxfs::CryptMarker;
use fidl_fuchsia_io as fio;
use starnix_core::fs::fuchsia::{RemoteFs, RemoteNode};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    derive_wrapping_key, CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FsNode, FsNodeHandle, FsStr,
};
use starnix_logging::log_error;
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio, statfs};
use std::sync::Arc;
use syncio::{zxio_node_attr_has_t, zxio_node_attributes_t, Zxio};

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
    // TODO(https://fxbug.dev/392164422): Switch to using the metadata encryption key provided by
    // Android in the /metadata partition.
    let raw_key: Vec<u8> = b"fake software key".to_vec();
    let (wrapping_key_id, wrapping_key_bytes) = derive_wrapping_key(raw_key.as_slice());

    let (exposed_dir_client_end, exposed_dir_server) =
        fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();

    let crypt = kernel.crypt_service.clone();
    crypt.add_wrapping_key(wrapping_key_id, wrapping_key_bytes.to_vec(), 0)?;
    crypt.set_metadata_key(wrapping_key_id)?;

    kernel.kthreads.spawn_executor(|_, _| async move {
        if let Err(e) = crypt.handle_connection(crypt_proxy.into_stream()).await {
            log_error!("Error while handling a Crypt request {e}");
        }
    });

    volume_provider
        .mount(crypt_client_end, exposed_dir_server, zx::MonotonicInstant::INFINITE)
        .map_err(|_| errno!(ENOENT))?
        .map_err(|_| errno!(EIO))?;

    let root = syncio::directory_open_directory_async(
        &exposed_dir_client_end.into_sync_proxy(),
        "root",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .map_err(|e| errno!(EIO, format!("Failed to open root: {e}")))?;

    let rights = fio::PERM_READABLE | fio::PERM_WRITABLE;
    let root = syncio::directory_open_directory_async(
        &root,
        std::str::from_utf8(&options.source)
            .map_err(|_| errno!(EINVAL, "source path is not utf8"))?,
        rights,
    )
    .map_err(|e| errno!(EIO, format!("Failed to open root: {e}")))?;

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
