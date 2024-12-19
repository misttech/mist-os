// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::client::*;
use crate::sensor_update_sender::SensorUpdateSender;
use crate::service_watcher::*;
use crate::utils::*;
use anyhow::{Context as _, Error};
use fidl::endpoints::{Proxy, RequestStream};
use fidl::AsHandleRef;
use fidl_fuchsia_hardware_sensors::{self as driver_fidl, PlaybackSourceConfig};
use fidl_fuchsia_sensors::*;
use fidl_fuchsia_sensors_types::*;
use fuchsia_component::client as fclient;
use fuchsia_component::server::ServiceFs;
use futures::lock::Mutex;
use futures_util::{StreamExt, TryStreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub type SensorId = i32;

#[derive(Debug, Clone)]
pub struct SensorManager {
    pub(crate) sensors: HashMap<SensorId, Sensor>,
    driver_proxies: Vec<driver_fidl::DriverProxy>,
    playback: Option<Playback>,
    clients: HashSet<Client>,
    pub(crate) update_sender: SensorUpdateSender,
}

#[derive(Debug, Clone)]
pub struct Sensor {
    driver: driver_fidl::DriverProxy,
    info: SensorInfo,
    // A subset of SensorManager::clients.
    pub(crate) clients: HashSet<Client>,
}

#[derive(Debug, Clone)]
pub struct Playback {
    driver_proxy: driver_fidl::DriverProxy,
    playback_proxy: driver_fidl::PlaybackProxy,
    playback_sensor_ids: Vec<SensorId>,
    configured: bool,
}

enum IncomingRequest {
    SensorManager(ManagerRequestStream),
}

async fn handle_sensors_request(
    request: ManagerRequest,
    manager: &Arc<Mutex<SensorManager>>,
    client: &Client,
) -> anyhow::Result<()> {
    let mut manager = manager.lock().await;
    match request {
        ManagerRequest::GetSensorsList { responder } => {
            manager.populate_sensors().await;

            if manager.sensors.len() > 0 {
                let mut fidl_sensors = Vec::<SensorInfo>::new();
                for sensor in manager.sensors.values().map(|sensor| sensor.info.clone()) {
                    fidl_sensors.push(sensor);
                }
                let _ = responder.send(&fidl_sensors);
            } else {
                tracing::warn!("Failed to get any sensors from driver. Sending empty list");
                let _ = responder.send(Vec::<SensorInfo>::new().as_slice());
            }
        }
        ManagerRequest::ConfigureSensorRates { id, sensor_rate_config, responder } => {
            if let Some(sensor) = manager.sensors.get(&id) {
                match sensor.driver.configure_sensor_rate(id, &sensor_rate_config).await {
                    Ok(Ok(())) => {
                        let _ = responder.send(Ok(()));
                    }
                    Ok(Err(driver_fidl::ConfigureSensorRateError::InvalidSensorId)) => {
                        tracing::warn!(
                            "Received ConfigureSensorRates request for unknown sensor id: {}",
                            id
                        );
                        let _ = responder.send(Err(ConfigureSensorRateError::InvalidSensorId));
                    }
                    Ok(Err(driver_fidl::ConfigureSensorRateError::InvalidConfig)) => {
                        tracing::warn!(
                            "Received ConfigureSensorRates request for invalid config: {:#?}",
                            sensor_rate_config
                        );
                        let _ = responder.send(Err(ConfigureSensorRateError::InvalidConfig));
                    }
                    Err(e) => {
                        tracing::warn!("Error while configuring sensor rates: {:#?}", e);
                        let _ = responder.send(Err(ConfigureSensorRateError::DriverUnavailable));
                    }
                    Ok(Err(_)) => unreachable!(),
                }
            } else {
                tracing::warn!(
                    "Received ConfigureSensorRates request for unknown sensor id: {}",
                    id
                );
                let _ = responder.send(Err(ConfigureSensorRateError::InvalidSensorId));
            }
        }
        ManagerRequest::Activate { id, responder } => {
            if let Some(sensor) = manager.sensors.get_mut(&id) {
                // Activating an already active sensor is a valid operation, so the manager does
                // not need to check if this is the first time the sensor is activated.
                let res = sensor.driver.activate_sensor(id).await;
                if let Err(e) = res {
                    tracing::warn!("Error while activating sensor: {:#?}", e);
                    let _ = responder.send(Err(ActivateSensorError::DriverUnavailable));
                } else {
                    sensor.clients.insert(client.clone());
                    let _ = responder.send(Ok(()));
                }
            } else {
                tracing::warn!("Received request to activate unknown sensor id: {}", id);
                let _ = responder.send(Err(ActivateSensorError::InvalidSensorId));
            }
        }
        ManagerRequest::Deactivate { id, responder } => {
            let mut response: Result<(), DeactivateSensorError> = Ok(());
            if let Some(sensor) = manager.sensors.get_mut(&id) {
                // If this is the last subscriber for this sensor, deactivate it.
                if sensor.clients.len() == 1 {
                    if let Err(e) = sensor.driver.deactivate_sensor(id).await {
                        tracing::warn!("Error while deactivating sensor: {:#?}", e);
                        response = Err(DeactivateSensorError::DriverUnavailable);
                    }
                } else {
                    tracing::info!(
                        "Unsubscribing client from sensor {:#?}, but there are other subscribers.",
                        id,
                    );
                }
                sensor.clients.remove(client);
            } else {
                tracing::warn!("Received request to deactivate unknown sensor id: {}", id);
                response = Err(DeactivateSensorError::InvalidSensorId);
            }
            let _ = responder.send(response);
        }
        ManagerRequest::ConfigurePlayback { source_config, responder } => {
            let mut response: Result<(), ConfigurePlaybackError> = Ok(());

            if let Some(mut playback) = manager.playback.clone() {
                let res = playback.playback_proxy.configure_playback(&source_config).await;

                match res {
                    Ok(Ok(())) => {
                        // In a FixedValuesConfig, the list of sensors is known, so they can be
                        // added directly to the map of sensors.
                        //
                        // In a FilePlaybackConfig, the playback_controller needs to read the list
                        // of sensors from a file first, so the list needs to come from the proxy.
                        if let PlaybackSourceConfig::FixedValuesConfig(val) = source_config {
                            if let Some(sensor_list) = val.sensor_list {
                                for sensor in sensor_list {
                                    if is_sensor_valid(&sensor) {
                                        let id = sensor.sensor_id.expect("sensor_id");
                                        manager.sensors.insert(
                                            id,
                                            Sensor {
                                                driver: playback.driver_proxy.clone(),
                                                info: sensor,
                                                clients: HashSet::new(),
                                            },
                                        );
                                        playback.playback_sensor_ids.push(id);
                                    }
                                }
                            }
                        } else {
                            if let Ok(sensors) = playback.driver_proxy.get_sensors_list().await {
                                for sensor in sensors {
                                    if is_sensor_valid(&sensor) {
                                        let id = sensor.sensor_id.expect("sensor_id");
                                        manager.sensors.insert(
                                            id,
                                            Sensor {
                                                driver: playback.driver_proxy.clone(),
                                                info: sensor,
                                                clients: HashSet::new(),
                                            },
                                        );

                                        playback.playback_sensor_ids.push(id);
                                    }
                                }
                            }
                        }

                        manager.driver_proxies.push(playback.driver_proxy.clone());
                        response = Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("Error while configuring sensor playback: {:#?}", e);
                        response = Err(ConfigurePlaybackError::PlaybackUnavailable);
                    }
                    Ok(Err(e)) => {
                        response = Err(from_driver_playback_error(e));
                    }
                }

                if !response.is_ok() {
                    // Remove the playback driver proxy and its sensors.
                    manager.driver_proxies.retain(|x| {
                        x.as_channel().raw_handle()
                            != playback.driver_proxy.as_channel().raw_handle()
                    });
                    for id in &playback.playback_sensor_ids {
                        manager.sensors.remove(id);
                    }
                    playback.playback_sensor_ids.clear();
                } else {
                    playback.configured = response.is_ok();
                    manager.playback = Some(playback);
                }
            }

            let _ = responder.send(response);
        }
        ManagerRequest::_UnknownMethod { ordinal, .. } => {
            tracing::warn!("ManagerRequest::_UnknownMethod with ordinal {}", ordinal);
        }
    }

    manager.update_sender.update_sensor_map(manager.sensors.clone()).await;

    Ok(())
}

async fn handle_sensor_manager_request_stream(
    mut stream: ManagerRequestStream,
    manager: Arc<Mutex<SensorManager>>,
    client: Client,
) -> Result<(), Error> {
    while let Some(request) =
        stream.try_next().await.context("Error handling SensorManager events")?
    {
        handle_sensors_request(request, &manager, &client)
            .await
            .expect("Error handling sensor request");
    }
    Ok(())
}

impl Playback {
    pub fn new(
        driver_proxy: driver_fidl::DriverProxy,
        playback_proxy: driver_fidl::PlaybackProxy,
    ) -> Self {
        Self { driver_proxy, playback_proxy, playback_sensor_ids: Vec::new(), configured: false }
    }
}

impl SensorManager {
    pub fn new(update_sender: SensorUpdateSender, playback: Option<Playback>) -> Self {
        let sensors = HashMap::new();
        let clients = HashSet::new();
        let driver_proxies = Vec::new();

        Self { sensors, driver_proxies, playback, clients, update_sender }
    }

    pub(crate) async fn add_instance(&mut self, driver_proxy: driver_fidl::DriverProxy) {
        let event_stream = driver_proxy.take_event_stream();
        self.driver_proxies.push(driver_proxy);
        self.populate_sensors().await;
        self.update_sender.update_sensor_map(self.sensors.clone()).await;
        self.update_sender.add_event_stream(event_stream).await;
    }

    async fn populate_sensors(&mut self) {
        let mut sensors = HashMap::new();
        for proxy in self.driver_proxies.clone() {
            if let Ok(driver_sensors) = proxy.get_sensors_list().await {
                for sensor in driver_sensors {
                    if is_sensor_valid(&sensor) {
                        let id = sensor.sensor_id.expect("sensor_id");
                        let mut clients: HashSet<Client> = HashSet::new();
                        if let Some(sensor) = self.sensors.get(&id) {
                            clients = sensor.clients.clone();
                        }

                        sensors.insert(id, Sensor { driver: proxy.clone(), info: sensor, clients });
                    }
                }
            }
        }

        self.sensors = sensors;
    }

    async fn start_service_watcher(&self, manager: Arc<Mutex<SensorManager>>) -> Result<(), Error> {
        let svc = fclient::Service::open(driver_fidl::ServiceMarker)?;
        // Attempt to watch the service directory. If this fails, the manager will check if
        // playback is configured and exit early if it is not.
        svc.watch().await?;

        fuchsia_async::Task::spawn(async move {
            if let Err(e) = watch_service_directory(svc, manager).await {
                tracing::error!("Failed to open sensor service! Error: {:#?}", e);
            }
        })
        .detach();

        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        // Collect all the driver event streams into a set of futures that will be polled when the
        // futures contain a sensor event.
        if let Some(playback) = &self.playback {
            self.add_instance(playback.driver_proxy.clone()).await;
        }

        let manager: Arc<Mutex<SensorManager>> = Arc::new(Mutex::new(self.clone()));

        if let Err(_) = self.start_service_watcher(manager.clone()).await {
            if self.playback.is_some() {
                tracing::warn!("Failed to open sensor driver service directory. Starting with playback sensors only.");
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to open sensors service and sensor playback is not enabled on the system"
                ));
            }
        }

        let mut fs = ServiceFs::new_local();
        fs.dir("svc").add_fidl_service(IncomingRequest::SensorManager);
        fs.take_and_serve_directory_handle()?;
        fs.for_each_concurrent(None, move |request: IncomingRequest| {
            let manager = manager.clone();
            async move {
                match request {
                    IncomingRequest::SensorManager(stream) => {
                        let client = Client::new(stream.control_handle());
                        manager.lock().await.clients.insert(client.clone());
                        handle_sensor_manager_request_stream(stream, manager, client)
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

fn from_driver_playback_error(val: driver_fidl::ConfigurePlaybackError) -> ConfigurePlaybackError {
    match val {
        driver_fidl::ConfigurePlaybackError::InvalidConfigType => {
            ConfigurePlaybackError::InvalidConfigType
        }
        driver_fidl::ConfigurePlaybackError::ConfigMissingFields => {
            ConfigurePlaybackError::ConfigMissingFields
        }
        driver_fidl::ConfigurePlaybackError::DuplicateSensorInfo => {
            ConfigurePlaybackError::DuplicateSensorInfo
        }
        driver_fidl::ConfigurePlaybackError::NoEventsForSensor => {
            ConfigurePlaybackError::NoEventsForSensor
        }
        driver_fidl::ConfigurePlaybackError::EventFromUnknownSensor => {
            ConfigurePlaybackError::EventFromUnknownSensor
        }
        driver_fidl::ConfigurePlaybackError::EventSensorTypeMismatch => {
            ConfigurePlaybackError::EventSensorTypeMismatch
        }
        driver_fidl::ConfigurePlaybackError::EventPayloadTypeMismatch => {
            ConfigurePlaybackError::EventPayloadTypeMismatch
        }
        driver_fidl::ConfigurePlaybackError::FileOpenFailed => {
            ConfigurePlaybackError::FileOpenFailed
        }
        driver_fidl::ConfigurePlaybackError::FileParseError => {
            ConfigurePlaybackError::FileParseError
        }
        driver_fidl::ConfigurePlaybackError::__SourceBreaking { unknown_ordinal } => {
            // This should be unreachable because playback is subpackaged with the sensor manager.
            tracing::error!(
                "Received unknown error from Sensor Playback with ordinal: {:#?}",
                unknown_ordinal
            );
            ConfigurePlaybackError::PlaybackUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::*;
    use fidl_fuchsia_hardware_sensors::*;
    use futures::channel::mpsc;

    #[fuchsia::test]
    async fn test_invalid_configure_playback() {
        // Creates an invalid playback_proxy so that ConfigurePlayback gets PEER_CLOSED when trying
        // to make a request.
        let (playback_proxy, _) = create_proxy::<PlaybackMarker>();

        let (driver_proxy, _) = create_proxy::<DriverMarker>();
        let (sender, _receiver) = mpsc::channel(100);
        let sender = SensorUpdateSender::new(Arc::new(Mutex::new(sender)));
        let sm = SensorManager::new(sender, Some(Playback::new(driver_proxy, playback_proxy)));

        let manager = Arc::new(Mutex::new(sm));
        let (proxy, stream) = create_proxy_and_stream::<ManagerMarker>();
        let client = Client::new(stream.control_handle().clone());
        fuchsia_async::Task::spawn(async move {
            manager.lock().await.clients.insert(client.clone());
            handle_sensor_manager_request_stream(stream, manager, client)
                .await
                .expect("Failed to process request stream");
        })
        .detach();

        let res = proxy
            .configure_playback(&PlaybackSourceConfig::FixedValuesConfig(
                FixedValuesPlaybackConfig {
                    sensor_list: None,
                    sensor_events: None,
                    ..Default::default()
                },
            ))
            .await;
        assert_eq!(
            res.unwrap(),
            Err(fidl_fuchsia_sensors::ConfigurePlaybackError::PlaybackUnavailable)
        );
    }
}
