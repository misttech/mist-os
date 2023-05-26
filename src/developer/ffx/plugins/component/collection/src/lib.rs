// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use component_debug::cli::{collection_list_cmd, collection_show_cmd};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_collection_args::{CollectionCommand, ShowArgs, SubCommandEnum};
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct CollectionTool {
    #[command]
    cmd: CollectionCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(CollectionTool);

#[async_trait(?Send)]
impl FfxMain for CollectionTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        match self.cmd.subcommand {
            SubCommandEnum::List(_) => collection_list_cmd(realm_query, writer).await,
            SubCommandEnum::Show(ShowArgs { query }) => {
                collection_show_cmd(query, realm_query, writer).await
            }
        }
        .map_err(|e| FfxError::Error(e, 1))?;

        Ok(())
    }
}
