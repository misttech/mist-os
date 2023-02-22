// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::{
    storage_copy_cmd, storage_delete_cmd, storage_list_cmd, storage_make_directory_cmd,
};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_storage_args::{StorageCommand, SubCommandEnum};
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;

#[ffx_plugin()]
pub async fn storage(remote_proxy: RemoteControlProxy, args: StorageCommand) -> Result<()> {
    let realm_query = connect_to_realm_query(&remote_proxy).await?;

    // All errors from component_debug library are user-visible.
    match args.subcommand {
        SubCommandEnum::Copy(copy_args) => {
            storage_copy_cmd(
                args.provider,
                args.capability,
                copy_args.source_path,
                copy_args.destination_path,
                realm_query,
            )
            .await
        }
        SubCommandEnum::Delete(delete_args) => {
            storage_delete_cmd(args.provider, args.capability, delete_args.path, realm_query).await
        }
        SubCommandEnum::List(list_args) => {
            storage_list_cmd(
                args.provider,
                args.capability,
                list_args.path,
                realm_query,
                std::io::stdout(),
            )
            .await
        }
        SubCommandEnum::MakeDirectory(make_dir_args) => {
            storage_make_directory_cmd(
                args.provider,
                args.capability,
                make_dir_args.path,
                realm_query,
            )
            .await
        }
    }
    .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
