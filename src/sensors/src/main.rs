// Copyright 2023 The Fuchsia Authors.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::Error, fidl_fuchsia_hardware_sensors as playback_fidl,
    sensors_lib::sensor_manager::SensorManager,
};

#[fuchsia::main(logging_tags = [ "sensors" ])]
async fn main() -> Result<(), Error> {
    tracing::info!("Sensors Server Started");

    let driver_proxy =
        fuchsia_component::client::connect_to_protocol::<playback_fidl::DriverMarker>().map_err(
            |e| {
                tracing::error!("Failed to connect to sensor driver protocol. {:#?}", e);
                return e;
            },
        )?;

    let mut sensor_manager = SensorManager::new(driver_proxy);

    // This should run forever.
    let result = sensor_manager.run().await;
    tracing::error!("Unexpected exit with result: {:?}", result);
    result
}
