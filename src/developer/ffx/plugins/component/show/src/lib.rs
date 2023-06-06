// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::cli::{show::ShowCmdInstance, show_cmd_print, show_cmd_serialized};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_show_args::ComponentShowCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct ShowTool {
    #[command]
    cmd: ComponentShowCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(ShowTool);

#[async_trait(?Send)]
impl FfxMain for ShowTool {
    type Writer = MachineWriter<Vec<ShowCmdInstance>>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        if writer.is_machine() {
            let output = show_cmd_serialized(self.cmd.query, realm_query)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
            writer.machine(&output)?;
        } else {
            show_cmd_print(self.cmd.query, realm_query, writer)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
        }
        Ok(())
    }
}
