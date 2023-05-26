// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use component_debug::cli::{
    storage_copy_cmd, storage_delete_cmd, storage_list_cmd, storage_make_directory_cmd,
};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_storage_args::{StorageCommand, SubCommandEnum};
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;

#[derive(FfxTool)]
pub struct StorageTool {
    #[command]
    cmd: StorageCommand,
    rcs: RemoteControlProxy,
}

fho::embedded_plugin!(StorageTool);

#[async_trait(?Send)]
impl FfxMain for StorageTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        match self.cmd.subcommand {
            SubCommandEnum::Copy(copy_args) => {
                storage_copy_cmd(
                    self.cmd.provider,
                    self.cmd.capability,
                    copy_args.source_path,
                    copy_args.destination_path,
                    realm_query,
                )
                .await
            }
            SubCommandEnum::Delete(delete_args) => {
                storage_delete_cmd(
                    self.cmd.provider,
                    self.cmd.capability,
                    delete_args.path,
                    realm_query,
                )
                .await
            }
            SubCommandEnum::List(list_args) => {
                storage_list_cmd(
                    self.cmd.provider,
                    self.cmd.capability,
                    list_args.path,
                    realm_query,
                    writer,
                )
                .await
            }
            SubCommandEnum::MakeDirectory(make_dir_args) => {
                storage_make_directory_cmd(
                    self.cmd.provider,
                    self.cmd.capability,
                    make_dir_args.path,
                    realm_query,
                )
                .await
            }
        }
        .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
