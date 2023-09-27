// Copyright 2023 The Fuchsia Authors.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::Error;
use sensors_lib::sensor_manager::SensorManager;

#[fuchsia::main(logging_tags = [ "sensors" ])]
async fn main() -> Result<(), Error> {
    tracing::info!("Sensors Server Started");
    let mut sensor_manager = SensorManager::new();

    // This should run forever.
    let result = sensor_manager.run().await;
    tracing::error!("Unexpected exit with result: {:?}", result);
    result
}
