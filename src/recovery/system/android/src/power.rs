// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_hardware_power_statecontrol as fpower_statecontrol;
use fuchsia_component::client::connect_to_protocol;

pub async fn reboot() -> Result<(), Error> {
    let proxy = connect_to_protocol::<fpower_statecontrol::AdminMarker>()?;
    proxy
        .perform_reboot(&fpower_statecontrol::RebootOptions {
            reasons: Some(vec![fpower_statecontrol::RebootReason2::UserRequest]),
            ..Default::default()
        })
        .await?
        .map_err(zx::Status::from_raw)?;
    Ok(())
}

pub async fn reboot_to_bootloader() -> Result<(), Error> {
    let proxy = connect_to_protocol::<fpower_statecontrol::AdminMarker>()?;
    proxy.reboot_to_bootloader().await?.map_err(zx::Status::from_raw)?;
    Ok(())
}

pub async fn power_off() -> Result<(), Error> {
    let proxy = connect_to_protocol::<fpower_statecontrol::AdminMarker>()?;
    proxy.poweroff().await?.map_err(zx::Status::from_raw)?;
    Ok(())
}
