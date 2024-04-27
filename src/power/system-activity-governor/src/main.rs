// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod system_activity_governor;

use crate::system_activity_governor::SystemActivityGovernor;
use anyhow::Result;
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_power_broker as fbroker;
use fuchsia_async::{DurationExt, TimeoutExt};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::health::Reporter;
use fuchsia_zircon::Duration;
use futures::prelude::*;

const SUSPEND_DEV_PATH: &'static str = "/dev/class/suspend";
const SUSPEND_DEVICE_TIMEOUT: Duration = Duration::from_seconds(5);

async fn connect_to_suspender(dir_path: &str) -> Result<fhsuspend::SuspenderProxy> {
    let dir = fuchsia_fs::directory::open_in_namespace(dir_path, fuchsia_fs::OpenFlags::empty())?;
    let mut stream = device_watcher::watch_for_files(&dir).await?;

    let filename = match stream
        .try_next()
        .on_timeout(SUSPEND_DEVICE_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("Timeout waiting for suspend device in {dir_path}"))
        })
        .await
    {
        Ok(Some(filename)) => filename,
        e => return Err(anyhow::anyhow!("Failed to find suspend device: {e:?}")),
    };

    let filename_str = filename.to_str().ok_or(anyhow::anyhow!("to_str for filename failed"))?;
    tracing::info!(?filename_str, "Opening suspend device");

    let (device, server_end) = fidl::endpoints::create_proxy::<fhsuspend::SuspenderMarker>()?;
    dir.open(
        fio::OpenFlags::NOT_DIRECTORY,
        fio::ModeType::empty(),
        &filename_str,
        server_end.into_channel().into(),
    )?;

    return Ok(device);
}

#[fuchsia::main]
async fn main() -> Result<()> {
    tracing::info!("started");

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());
    fuchsia_inspect::component::health().set_starting_up();

    // Set up the SystemActivityGovernor.
    let suspender = match connect_to_suspender(SUSPEND_DEV_PATH).await {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(
                "Unable to connect to suspender prototocol at {SUSPEND_DEV_PATH}: {e:?}"
            );
            None
        }
    };
    let sag = SystemActivityGovernor::new(
        &connect_to_protocol::<fbroker::TopologyMarker>()?,
        inspector.root().clone_weak(),
        suspender,
    )
    .await?;

    fuchsia_inspect::component::health().set_ok();

    // This future should never complete.
    let result = sag.run().await;
    tracing::error!(?result, "Unexpected exit");
    fuchsia_inspect::component::health().set_unhealthy(&format!("Unexpected exit: {:?}", result));
    result
}
