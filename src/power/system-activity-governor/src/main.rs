// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod system_activity_governor;

use crate::system_activity_governor::SystemActivityGovernor;
use anyhow::Error;
use fidl_fuchsia_power_broker as fbroker;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::health::Reporter;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    tracing::info!("started");

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());
    fuchsia_inspect::component::health().set_starting_up();

    // Set up the SystemActivityGovernor.
    let sag = SystemActivityGovernor::new(
        &connect_to_protocol::<fbroker::TopologyMarker>()?,
        inspector.root().clone_weak(),
    )
    .await?;

    fuchsia_inspect::component::health().set_ok();

    // This future should never complete.
    let result = sag.run().await;
    tracing::error!(?result, "Unexpected exit");
    fuchsia_inspect::component::health().set_unhealthy(&format!("Unexpected exit: {:?}", result));
    result
}
