// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::cli::resolve_cmd;
use errors::FfxError;
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_resolve_args::ComponentResolveCommand;
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool};
use target_holders::RemoteControlProxyHolder;

#[derive(FfxTool)]
pub struct ResolveTool {
    #[command]
    cmd: ComponentResolveCommand,
    rcs: RemoteControlProxyHolder,
}

fho::embedded_plugin!(ResolveTool);

#[async_trait(?Send)]
impl FfxMain for ResolveTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let lifecycle_controller = connect_to_lifecycle_controller(&self.rcs).await?;
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        resolve_cmd(self.cmd.query, lifecycle_controller, realm_query, writer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
