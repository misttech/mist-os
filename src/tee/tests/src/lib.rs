// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(unused_imports)]
use anyhow::{Context, Error};
use fidl_fuchsia_tee::ApplicationMarker;
use fuchsia_component::client::connect_to_protocol_at_path;

#[fuchsia::test]
async fn noop_ta_lifecycle() -> Result<(), Error> {
    // Connect to noop TA at /svc/fuchsia.tee.Application.<noop UUID>
    let app =
        connect_to_protocol_at_path::<ApplicationMarker>("/svc/fuchsia.tee.Application.NOOP-UUID")
            .context("Failed to connect to application instance")?;
    // Close the application connection.
    std::mem::drop(app);
    Ok(())
}
