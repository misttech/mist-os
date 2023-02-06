// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::cli::{doctor_cmd_print, doctor_cmd_serialized},
    component_debug::doctor::RouteReport,
    errors::FfxError,
    ffx_component::rcs::{connect_to_realm_explorer, connect_to_route_validator},
    ffx_component_doctor_args::DoctorCommand,
    ffx_core::ffx_plugin,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol as rc,
};

#[ffx_plugin]
pub async fn cmd(
    rcs_proxy: rc::RemoteControlProxy,
    args: DoctorCommand,
    #[ffx(machine = RouteReport)] writer: Writer,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;
    let route_validator = connect_to_route_validator(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    if writer.is_machine() {
        let output = doctor_cmd_serialized(args.query, route_validator, realm_explorer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        writer.machine(&output)
    } else {
        doctor_cmd_print(args.query, route_validator, realm_explorer, writer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
