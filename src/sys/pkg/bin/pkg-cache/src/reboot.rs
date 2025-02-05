// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context};
use fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, RebootOptions, RebootReason2};
use fuchsia_component::client::connect_to_protocol;
use log::error;

/// Reboots the system, logging errors instead of failing.
pub(super) async fn reboot() {
    if let Err(e) = async move {
        let proxy = connect_to_protocol::<AdminMarker>()
            .context("connect to fuchsia.hardware.power.statecontrol.Admin")?;

        proxy
            // FIXME(b/298716497): Replace with a unique reboot reason
            .perform_reboot(&RebootOptions {
                reasons: Some(vec![RebootReason2::CriticalComponentFailure]),
                ..Default::default()
            })
            .await
            .context("while performing reboot call")?
            .map_err(zx::Status::from_raw)
            .context("reboot responded with")
    }
    .await
    {
        error!("error initiating reboot: {:#}", anyhow!(e));
    }
}
