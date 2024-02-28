// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Context as _, Error},
    fidl_fuchsia_sensors as sensors_fidl, fidl_fuchsia_sensors_types as sensors_types_fidl,
    fuchsia_component::server::ServiceFs,
    futures_util::{StreamExt, TryStreamExt},
    sensors_fidl::ManagerRequest,
    std::collections::HashMap,
};

#[derive(Clone, Debug)]
pub struct SensorInfo {
    id: i32,
    name: String,
}

impl SensorInfo {
    pub fn to_fidl(&mut self) -> sensors_types_fidl::SensorInfo {
        sensors_types_fidl::SensorInfo {
            sensor_id: Some(self.id.clone()),
            name: Some(self.name.clone()),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
pub struct SensorManager {
    sensors: HashMap<i32, SensorInfo>,
}

enum IncomingRequest {
    SensorManager(sensors_fidl::ManagerRequestStream),
}

async fn handle_sensors_request(
    request: ManagerRequest,
    sensors: HashMap<i32, SensorInfo>,
) -> anyhow::Result<()> {
    match request {
        ManagerRequest::GetSensorsList { responder } => {
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
    sensors: HashMap<i32, SensorInfo>,
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
    async fn test_handle_get_sensors_list() {
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

        let fidl_sensors = proxy.get_sensors_list().await.unwrap();
        assert!(fidl_sensors.is_empty());
    }
}
