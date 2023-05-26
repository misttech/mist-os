// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use component_debug::{
    cli::{list_cmd_print, list_cmd_serialized},
    realm::Instance,
};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_list_args::ComponentListCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct ListTool {
    #[command]
    cmd: ComponentListCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(ListTool);

#[async_trait(?Send)]
impl FfxMain for ListTool {
    type Writer = MachineWriter<Vec<Instance>>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        if writer.is_machine() {
            let output = list_cmd_serialized(self.cmd.filter, realm_query)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
            writer.machine(&output)?;
        } else {
            list_cmd_print(self.cmd.filter, self.cmd.verbose, realm_query, writer)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
        }
        Ok(())
    }
}
