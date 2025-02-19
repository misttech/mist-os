// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::cli::graph_cmd;
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_graph_args::ComponentGraphCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use target_holders::RemoteControlProxyHolder;

#[derive(FfxTool)]
pub struct GraphTool {
    #[command]
    cmd: ComponentGraphCommand,
    rcs: RemoteControlProxyHolder,
}

fho::embedded_plugin!(GraphTool);

#[async_trait(?Send)]
impl FfxMain for GraphTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        graph_cmd(self.cmd.filter, self.cmd.orientation, realm_query, writer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
