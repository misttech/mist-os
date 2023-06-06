// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::cli::destroy_cmd;
use errors::FfxError;
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_destroy_args::DestroyComponentCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct DestroyTool {
    #[command]
    cmd: DestroyComponentCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(DestroyTool);

#[async_trait(?Send)]
impl FfxMain for DestroyTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let lifecycle_controller = connect_to_lifecycle_controller(&self.rcs).await?;
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        destroy_cmd(self.cmd.query, lifecycle_controller, realm_query, writer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
