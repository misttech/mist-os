// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod system_activity_governor;

use crate::system_activity_governor::SystemActivityGovernor;
use anyhow::Error;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    tracing::info!("started");

    // Set up the SystemActivityGovernor.
    let sag = SystemActivityGovernor::new();

    // This future should never complete.
    let result = sag.run().await;
    tracing::error!("Unexpected exit with result: {result:?}");
    result
}
