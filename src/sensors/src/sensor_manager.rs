// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{Context as _, Error};
use fidl::endpoints::{ControlHandle, RequestStream};
use fidl_fuchsia_hardware_sensors as playback_fidl;
use fidl_fuchsia_sensors::*;
use fidl_fuchsia_sensors_types::*;
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::stream::FusedStream;
use futures::{select, SinkExt};
use futures_util::{StreamExt, TryStreamExt};
use itertools::Itertools;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SensorManager {
    sensors: HashMap<i32, SensorInfo>,
    driver_proxy: playback_fidl::DriverProxy,
    playback_proxy: playback_fidl::PlaybackProxy,
    playback_configured: bool,
}

enum IncomingRequest {
    SensorManager(ManagerRequestStream),
}

async fn handle_sensors_request(
    request: ManagerRequest,
    manager: &mut SensorManager,
) -> anyhow::Result<()> {
    match request {
        ManagerRequest::GetSensorsList { responder } => {
            if !manager.playback_configured {
                tracing::warn!("Received request for sensor list, but playback has not been configured. Sending empty list");
                let _ = responder.send(Vec::<SensorInfo>::new().as_slice());
                return Ok(());
            }

            if let Ok(sensors) = manager.driver_proxy.get_sensors_list().await {
                manager.sensors = HashMap::new();
                for sensor in sensors {
                    if let Some(id) = sensor.sensor_id {
                        tracing::debug!("Sensor id being added: {:#?}", sensor.sensor_id);
                        manager.sensors.insert(id, sensor);
                    } else {
                        tracing::error!("Sensor obtained from driver did not have an id. Sensor will not be added: {:#?}", sensor);
                    }
                }
                let fidl_sensors =
                    manager.sensors.values().map(|sensor| sensor.clone()).collect::<Vec<_>>();
                let _ = responder.send(fidl_sensors.as_slice());
            } else {
                tracing::warn!("Failed to get sensor list from driver. Sending empty list");
                let _ = responder.send(Vec::<SensorInfo>::new().as_slice());
            }
        }
        ManagerRequest::ConfigureSensorRates { id, sensor_rate_config, responder } => {
            if !manager.playback_configured {
                tracing::warn!(
                    "Received request to configure a sensor before playback was configured"
                );
                let _ = responder.send(Err(ConfigureSensorRateError::DriverUnavailable));
                return Ok(());
            }

            if !manager.sensors.keys().contains(&id) {
                tracing::warn!(
                    "Received ConfigureSensorRates request for unknown sensor id: {}",
                    id
                );
                let _ = responder.send(Err(ConfigureSensorRateError::InvalidSensorId));
            } else {
                match manager.driver_proxy.configure_sensor_rate(id, &sensor_rate_config).await {
                    Ok(Ok(())) => {
                        let _ = responder.send(Ok(()));
                    }
                    Ok(Err(playback_fidl::ConfigureSensorRateError::InvalidSensorId)) => {
                        tracing::warn!(
                            "Received ConfigureSensorRates request for unknown sensor id: {}",
                            id
                        );
                        let _ = responder.send(Err(ConfigureSensorRateError::InvalidSensorId));
                    }
                    Ok(Err(playback_fidl::ConfigureSensorRateError::InvalidConfig)) => {
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
            }
        }
        ManagerRequest::Activate { id, responder } => {
            if !manager.playback_configured {
                tracing::warn!("Received request to activate sensor with id: {} before playback was configured", id);
                let _ = responder.send(Err(ActivateSensorError::DriverUnavailable));
                return Ok(());
            }

            if !manager.sensors.keys().contains(&id) {
                tracing::warn!("Received request to activate unknown sensor id: {}", id);
                let _ = responder.send(Err(ActivateSensorError::InvalidSensorId));
            } else {
                let res = manager.driver_proxy.activate_sensor(id).await;
                if let Err(e) = res {
                    tracing::warn!("Error while activating sensor: {:#?}", e);
                    let _ = responder.send(Err(ActivateSensorError::DriverUnavailable));
                } else {
                    let _ = responder.send(Ok(()));
                }
            }
        }
        ManagerRequest::Deactivate { id, responder } => {
            if !manager.playback_configured {
                tracing::warn!("Received request to deactivate sensor with id: {} before playback was configured", id);
                let _ = responder.send(Err(DeactivateSensorError::DriverUnavailable));
                return Ok(());
            }

            if !manager.sensors.keys().contains(&id) {
                tracing::warn!("Received request to deactivate unknown sensor id: {}", id);
                let _ = responder.send(Err(DeactivateSensorError::InvalidSensorId));
            } else {
                let res = manager.driver_proxy.deactivate_sensor(id).await;
                if let Err(e) = res {
                    tracing::warn!("Error while deactivating sensor: {:#?}", e);
                    let _ = responder.send(Err(DeactivateSensorError::DriverUnavailable));
                } else {
                    let _ = responder.send(Ok(()));
                }
            }
        }
        ManagerRequest::ConfigurePlayback { source_config, responder } => {
            let res = manager.playback_proxy.configure_playback(&source_config).await;
            let response: Result<(), ConfigurePlaybackError>;

            match res {
                Ok(Ok(())) => {
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

            manager.playback_configured = response.is_ok();
            let _ = responder.send(response);
        }
        ManagerRequest::_UnknownMethod { ordinal, .. } => {
            tracing::warn!("ManagerRequest::_UnknownMethod with ordinal {}", ordinal);
        }
    }
    Ok(())
}

async fn sensor_event_sender(
    mut receiver: mpsc::UnboundedReceiver<ManagerControlHandle>,
    mut event_stream: playback_fidl::DriverEventStream,
) {
    let mut clients: HashMap<u8, ManagerControlHandle> = HashMap::new();
    let mut client_id: u8 = 0;

    loop {
        if event_stream.is_terminated() {
            tracing::error!("Driver event stream was terminated.");
            break;
        }
        select! {
            sensor_event = event_stream.next() => {
                match sensor_event {
                    Some(Ok(playback_fidl::DriverEvent::OnSensorEvent { event })) => {
                        for (id, client) in clients.clone().into_iter() {
                            if !client.is_closed() {
                                if let Err(e) = client.send_on_sensor_event(&event) {
                                    tracing::warn!("Failed to send sensor event: {:#?}", e);
                                }
                            } else {
                                tracing::error!("Client was PEER_CLOSED! Removing from clients list");
                                clients.remove(&id);
                            }
                        }
                    }
                    Some(Ok(playback_fidl::DriverEvent::_UnknownEvent { ordinal, .. })) => {
                        tracing::warn!(
                            "SensorManager received an UnknownEvent with ordinal: {:#?}",
                            ordinal
                        );
                    }
                    Some(Err(e)) => {
                        tracing::error!("Received an error from sensor driver: {:#?}", e);
                        break;
                    }
                    None => {
                        tracing::error!("Got None from driver");
                        break;
                    }
                }
            },
            new_control_handle = receiver.next() => {
                if let Some(control_handle) = new_control_handle {
                    clients.insert(client_id, control_handle);
                    client_id += 1;
                }
            },
        }
    }
}

async fn handle_sensor_manager_request_stream(
    mut stream: ManagerRequestStream,
    manager: &mut SensorManager,
) -> Result<(), Error> {
    while let Some(request) =
        stream.try_next().await.context("Error handling SensorManager events")?
    {
        handle_sensors_request(request, manager).await.expect("Error handling sensor request");
    }
    Ok(())
}

impl SensorManager {
    pub fn new(
        driver_proxy: playback_fidl::DriverProxy,
        playback_proxy: playback_fidl::PlaybackProxy,
    ) -> Self {
        let sensors = HashMap::new();

        Self { sensors, driver_proxy, playback_proxy, playback_configured: false }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        // Get the initial list of sensors so that the manager doesn't need to ask the drivers
        // on every request.
        if let Ok(sensors) = self.driver_proxy.get_sensors_list().await {
            for sensor in sensors {
                if let Some(id) = sensor.sensor_id {
                    tracing::debug!("Sensor id being added: {:#?}", sensor.sensor_id);
                    self.sensors.insert(id, sensor);
                } else {
                    tracing::error!("Sensor obtained from driver did not have an id. Sensor will not be added: {:#?}", sensor);
                }
            }
        }

        let (sender, receiver) = mpsc::unbounded::<ManagerControlHandle>();
        let event_stream = self.driver_proxy.take_event_stream();
        fuchsia_async::Task::spawn(async move {
            sensor_event_sender(receiver, event_stream).await;
        })
        .detach();

        let mut fs = ServiceFs::new_local();
        fs.dir("svc").add_fidl_service(IncomingRequest::SensorManager);
        fs.take_and_serve_directory_handle()?;
        fs.for_each_concurrent(None, move |request: IncomingRequest| {
            let mut manager = self.clone();
            let mut handle_sender = sender.clone();
            async move {
                match request {
                    IncomingRequest::SensorManager(stream) => {
                        // When there is a new client, add the handle to the list of clients that
                        // are receiving sensor events.
                        if let Err(e) = handle_sender.send(stream.control_handle().clone()).await {
                            tracing::warn!("Failed to send id to sensor_event_sender: {:#?}", e);
                        }
                        handle_sensor_manager_request_stream(stream, &mut manager)
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

fn from_driver_playback_error(
    val: playback_fidl::ConfigurePlaybackError,
) -> ConfigurePlaybackError {
    match val {
        playback_fidl::ConfigurePlaybackError::InvalidConfigType => {
            ConfigurePlaybackError::InvalidConfigType
        }
        playback_fidl::ConfigurePlaybackError::ConfigMissingFields => {
            ConfigurePlaybackError::ConfigMissingFields
        }
        playback_fidl::ConfigurePlaybackError::DuplicateSensorInfo => {
            ConfigurePlaybackError::DuplicateSensorInfo
        }
        playback_fidl::ConfigurePlaybackError::NoEventsForSensor => {
            ConfigurePlaybackError::NoEventsForSensor
        }
        playback_fidl::ConfigurePlaybackError::EventFromUnknownSensor => {
            ConfigurePlaybackError::EventFromUnknownSensor
        }
        playback_fidl::ConfigurePlaybackError::EventSensorTypeMismatch => {
            ConfigurePlaybackError::EventSensorTypeMismatch
        }
        playback_fidl::ConfigurePlaybackError::EventPayloadTypeMismatch => {
            ConfigurePlaybackError::EventPayloadTypeMismatch
        }
        playback_fidl::ConfigurePlaybackError::FileOpenFailed => {
            ConfigurePlaybackError::FileOpenFailed
        }
        playback_fidl::ConfigurePlaybackError::FileParseError => {
            ConfigurePlaybackError::FileParseError
        }
        playback_fidl::ConfigurePlaybackError::__SourceBreaking { unknown_ordinal } => {
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

    fn get_test_sensor() -> SensorInfo {
        SensorInfo {
            sensor_id: Some(1),
            name: Some(String::from("HEART_RATE")),
            vendor: Some(String::from("Fuchsia")),
            version: Some(1),
            sensor_type: Some(SensorType::HeartRate),
            wake_up: Some(SensorWakeUpType::NonWakeUp),
            reporting_mode: Some(SensorReportingMode::OnChange),
            ..Default::default()
        }
    }

    fn get_test_events() -> Vec<SensorEvent> {
        let mut events: Vec<SensorEvent> = Vec::new();
        for i in 1..4 {
            let event = SensorEvent {
                sensor_id: get_test_sensor().sensor_id.unwrap(),
                sensor_type: SensorType::HeartRate,
                payload: EventPayload::Float(i as f32),
                // These two values get ignored by playback.
                sequence_number: 0,
                timestamp: 0,
            };
            events.push(event);
        }

        events
    }

    fn get_playback_config() -> playback_fidl::PlaybackSourceConfig {
        let test_sensor = get_test_sensor();
        let events = get_test_events();

        let fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: Some(vec![test_sensor]),
            sensor_events: Some(events),
            ..Default::default()
        };

        playback_fidl::PlaybackSourceConfig::FixedValuesConfig(fixed_values_config)
    }

    async fn setup_manager() -> ManagerProxy {
        let (mut sender, receiver) = mpsc::unbounded::<ManagerControlHandle>();

        let playback_proxy =
            fuchsia_component::client::connect_to_protocol::<playback_fidl::PlaybackMarker>()
                .unwrap();

        let driver_proxy =
            fuchsia_component::client::connect_to_protocol::<playback_fidl::DriverMarker>()
                .unwrap();
        let driver_event_stream = driver_proxy.take_event_stream();

        let mut manager = SensorManager::new(driver_proxy, playback_proxy);
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<ManagerMarker>().unwrap();

        fuchsia_async::Task::spawn(async move {
            sensor_event_sender(receiver, driver_event_stream).await;
        })
        .detach();
        let _ = sender.send(stream.control_handle()).await;

        fuchsia_async::Task::spawn(async move {
            handle_sensor_manager_request_stream(stream, &mut manager)
                .await
                .expect("Failed to process request stream");
        })
        .detach();

        proxy
    }

    async fn setup() -> ManagerProxy {
        let manager_proxy = setup_manager().await;
        let _ = manager_proxy.configure_playback(&get_playback_config()).await;

        // When the SensorManager starts, it gets the initial list of sensors before handling any
        // requests. These tests do not call SensorManager::run, so the sensor list can be
        // populated by calling get_sensors_list.
        let _ = manager_proxy.get_sensors_list().await;

        manager_proxy
    }

    async fn clear_playback_config(proxy: &ManagerProxy) {
        let fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: None,
            sensor_events: None,
            ..Default::default()
        };

        let _ = proxy
            .configure_playback(&playback_fidl::PlaybackSourceConfig::FixedValuesConfig(
                fixed_values_config,
            ))
            .await;
    }

    #[fuchsia::test]
    async fn test_configure_playback() {
        // Creates an invalid playback_proxy so that ConfigurePlayback gets PEER_CLOSED when trying
        // to make a request.
        let (playback_proxy, _) =
            fidl::endpoints::create_proxy::<playback_fidl::PlaybackMarker>().unwrap();

        let driver_proxy =
            fuchsia_component::client::connect_to_protocol::<playback_fidl::DriverMarker>()
                .unwrap();

        let mut manager = SensorManager::new(driver_proxy, playback_proxy);
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<ManagerMarker>().unwrap();
        fuchsia_async::Task::spawn(async move {
            handle_sensor_manager_request_stream(stream, &mut manager)
                .await
                .expect("Failed to process request stream");
        })
        .detach();

        let res = proxy.configure_playback(&get_playback_config()).await;
        assert_eq!(
            res.unwrap(),
            Err(fidl_fuchsia_sensors::ConfigurePlaybackError::PlaybackUnavailable)
        );

        // Set up a new manager with a valid playback connection.
        let proxy = setup().await;
        clear_playback_config(&proxy).await;

        let mut fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: Some(vec![get_test_sensor()]),
            sensor_events: Some(get_test_events()),
            ..Default::default()
        };
        let res = proxy
            .configure_playback(&playback_fidl::PlaybackSourceConfig::FixedValuesConfig(
                fixed_values_config,
            ))
            .await;
        assert!(res.is_ok());

        fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: None,
            sensor_events: None,
            ..Default::default()
        };

        let res = proxy
            .configure_playback(&playback_fidl::PlaybackSourceConfig::FixedValuesConfig(
                fixed_values_config,
            ))
            .await;

        assert_eq!(res.unwrap(), Err(ConfigurePlaybackError::ConfigMissingFields));
    }
}
