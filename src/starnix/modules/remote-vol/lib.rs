// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_fshost::StarnixVolumeProviderMarker;
use fidl_fuchsia_fxfs::CryptMarker;
use starnix_core::fs::fuchsia::create_remotefs_filesystem;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{derive_wrapping_key, FileSystemHandle, FileSystemOptions};
use starnix_logging::log_error;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

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

    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            if let Err(e) = crypt.handle_connection(crypt_proxy.into_stream()).await {
                log_error!("Error while handling a Crypt request {e}");
            }
        });
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
    return create_remotefs_filesystem(kernel, &root, options, rights);
}
