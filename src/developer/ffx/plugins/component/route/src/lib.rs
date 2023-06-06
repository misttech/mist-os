// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::{cli, route::RouteReport};
use errors::FfxError;
use ffx_component::rcs;
use ffx_component_route_args::RouteCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_remotecontrol as rc;

#[derive(FfxTool)]
pub struct RouteTool {
    #[command]
    cmd: RouteCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(RouteTool);

#[async_trait(?Send)]
impl FfxMain for RouteTool {
    type Writer = MachineWriter<Vec<RouteReport>>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let realm_query = rcs::connect_to_realm_query(&self.rcs).await?;
        let route_validator = rcs::connect_to_route_validator(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        if writer.is_machine() {
            let output = cli::route_cmd_serialized(
                self.cmd.target,
                self.cmd.filter,
                route_validator,
                realm_query,
            )
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
            writer.machine(&output)?;
        } else {
            cli::route_cmd_print(
                self.cmd.target,
                self.cmd.filter,
                route_validator,
                realm_query,
                writer,
            )
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        }
        Ok(())
    }
}
