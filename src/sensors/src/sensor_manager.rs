// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Context as _, Error},
    fidl_fuchsia_sensors as sensors_fidl,
    fuchsia_component::server::ServiceFs,
    futures_util::{StreamExt, TryStreamExt},
    sensors_fidl::ManagerRequest,
    std::collections::HashMap,
    tracing,
};

#[derive(Clone, Debug)]
pub struct Sensor {
    id: i32,
    name: String,
}

impl Sensor {
    pub fn to_fidl(&mut self) -> sensors_fidl::Sensor {
        sensors_fidl::Sensor {
            id: Some(sensors_fidl::SensorId { id: self.id.clone() }),
            name: Some(self.name.clone()),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
pub struct SensorManager {
    sensors: HashMap<i32, Sensor>,
}

enum IncomingRequest {
    SensorManager(sensors_fidl::ManagerRequestStream),
}

async fn handle_sensors_request(
    request: ManagerRequest,
    sensors: HashMap<i32, Sensor>,
) -> anyhow::Result<()> {
    match request {
        ManagerRequest::GetSensorsInfo { responder } => {
            let fidl_sensors =
                sensors.values().map(|sensor| sensor.clone().to_fidl()).collect::<Vec<_>>();
            let _ = responder.send(fidl_sensors.as_slice());
        }
        ManagerRequest::_UnknownMethod { ordinal, .. } => {
            tracing::warn!("ManagerRequest::_UnknownMethod with ordinal {}", ordinal);
        }
    }
    Ok(())
}

async fn handle_sensor_manager_request_stream(
    mut stream: sensors_fidl::ManagerRequestStream,
    sensors: HashMap<i32, Sensor>,
) -> Result<(), Error> {
    while let Some(request) =
        stream.try_next().await.context("Error handling SensorManager events")?
    {
        handle_sensors_request(request, sensors.clone())
            .await
            .expect("Error handling sensor request");
    }
    Ok(())
}

impl SensorManager {
    pub fn new() -> Self {
        Self { sensors: HashMap::new() }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let mut fs = ServiceFs::new_local();
        fs.dir("svc").add_fidl_service(IncomingRequest::SensorManager);
        fs.take_and_serve_directory_handle()?;
        fs.for_each_concurrent(None, move |request: IncomingRequest| {
            let sensors = self.sensors.clone();
            async move {
                match request {
                    IncomingRequest::SensorManager(stream) => {
                        handle_sensor_manager_request_stream(stream, sensors)
                            .await
                            .expect("Failed to serve sensor requests");
                    }
                }
            }
        })
        .await;

        Err(anyhow::anyhow!("SensorManager completed unexpectedly."))
    }
}

#[cfg(test)]
mod tests {
    use {super::*, fidl_fuchsia_sensors as sensors_fidl};

    #[fuchsia::test]
    async fn test_handle_get_sensor_info() {
        let manager = SensorManager::new();
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<sensors_fidl::ManagerMarker>().unwrap();
        let sensors = manager.sensors.clone();
        fuchsia_async::Task::spawn(async move {
            handle_sensor_manager_request_stream(stream, sensors)
                .await
                .expect("Failed to process request stream");
        })
        .detach();

        let fidl_sensors = proxy.get_sensors_info().await.unwrap();
        assert!(fidl_sensors.is_empty());
    }
}
