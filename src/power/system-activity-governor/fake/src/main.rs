// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod system_activity_governor_control;

use crate::system_activity_governor_control::SystemActivityGovernorControl;
use anyhow::{Context, Result};
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_test_suspendcontrol as tsc;
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;

async fn connect_to_suspend_ctrl_and_setup_suspend_device() -> Result<tsc::DeviceProxy> {
    let suspend_device =
        fuchsia_component::client::connect_to_protocol::<tsc::DeviceMarker>().unwrap();

    // Set up default state.
    suspend_device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(vec![fhsuspend::SuspendState {
                resume_latency: Some(0),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    Ok(suspend_device)
}

#[fuchsia::main]
async fn main() -> Result<()> {
    tracing::info!("started");

    let suspend_ctrl = connect_to_suspend_ctrl_and_setup_suspend_device().await?;
    let sagctrl = SystemActivityGovernorControl::new(suspend_ctrl).await;

    let mut service_fs = ServiceFs::new_local();

    sagctrl.run(&mut service_fs).await;

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    // This future should never complete.
    service_fs.collect::<()>().await;

    tracing::error!("Unexpected exit");
    Ok(())
}
