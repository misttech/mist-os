// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use component_debug::cli::{doctor_cmd_print, doctor_cmd_serialized};
use component_debug::doctor::RouteReport;
use errors::FfxError;
use ffx_component::rcs::{connect_to_realm_query, connect_to_route_validator};
use ffx_component_doctor_args::DoctorCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO};
use target_holders::RemoteControlProxyHolder;

#[derive(FfxTool)]
pub struct DoctorTool {
    #[command]
    cmd: DoctorCommand,
    rcs: RemoteControlProxyHolder,
}

fho::embedded_plugin!(DoctorTool);

#[async_trait(?Send)]
impl FfxMain for DoctorTool {
    type Writer = MachineWriter<Vec<RouteReport>>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;
        let route_validator = connect_to_route_validator(&self.rcs).await?;

        // All errors from component_debug library are user-visible.
        if writer.is_machine() {
            let output = doctor_cmd_serialized(self.cmd.query, route_validator, realm_query)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
            writer.machine(&output)?;
        } else {
            doctor_cmd_print(self.cmd.query, route_validator, realm_query, writer)
                .await
                .map_err(|e| FfxError::Error(e, 1))?;
        }
        Ok(())
    }
}
