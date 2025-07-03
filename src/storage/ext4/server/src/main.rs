// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ext4_readonly reads and exposes read-only ext4 file systems to clients.

use anyhow::{format_err, Context as _, Error};
use ext4_parser::{construct_fs, FsSourceType};
use fidl::endpoints::{DiscoverableProtocolMarker as _, ServerEnd};
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fidl_fuchsia_io as fio;
use fuchsia_runtime::{take_startup_handle, HandleType};
use log::info;
use vfs::execution_scope::ExecutionScope;

#[fuchsia::main(threads = 10)]
async fn main() -> Result<(), Error> {
    info!("Starting ext4_readonly");

    let (block_device, server) = fidl::endpoints::create_endpoints();
    fuchsia_component::client::connect_channel_to_protocol_at_path(
        server.into_channel(),
        &format!("/block/{}", VolumeMarker::PROTOCOL_NAME),
    )
    .context("Failed to connect to Volume")?;

    let tree = match construct_fs(FsSourceType::BlockDevice(block_device)) {
        Ok(tree) => tree,
        Err(err) => return Err(format_err!("Failed to construct file system: {:?}", err)),
    };

    let directory_handle = take_startup_handle(HandleType::DirectoryRequest.into()).unwrap();
    let scope = ExecutionScope::new();
    vfs::directory::serve_on(
        vfs::pseudo_directory! {
            "root" => tree,
        },
        fio::PERM_READABLE,
        scope.clone(),
        ServerEnd::new(directory_handle.into()),
    );

    // Wait until the directory connection is closed by the client before exiting.
    scope.wait().await;
    info!("ext4 directory connection dropped, exiting");
    Ok(())
}
