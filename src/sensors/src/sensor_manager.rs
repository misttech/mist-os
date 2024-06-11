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
    pub fn new(driver_proxy: playback_fidl::DriverProxy) -> Self {
        let sensors = HashMap::new();

        Self { sensors, driver_proxy }
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
                // These two values get ignored by playback
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

    async fn connect_playback() -> playback_fidl::PlaybackProxy {
        fuchsia_component::client::connect_to_protocol::<playback_fidl::PlaybackMarker>().unwrap()
    }

    async fn setup_playback(
        playback_proxy: &playback_fidl::PlaybackProxy,
        source_config: playback_fidl::PlaybackSourceConfig,
    ) {
        let _ = playback_proxy.configure_playback(&source_config).await;
    }

    async fn setup_manager() -> ManagerProxy {
        let (mut sender, receiver) = mpsc::unbounded::<ManagerControlHandle>();

        let driver_proxy =
            fuchsia_component::client::connect_to_protocol::<playback_fidl::DriverMarker>()
                .unwrap();
        let driver_event_stream = driver_proxy.take_event_stream();

        let mut manager = SensorManager::new(driver_proxy);
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

    async fn setup() -> (ManagerProxy, playback_fidl::PlaybackProxy) {
        let playback_proxy = connect_playback().await;
        setup_playback(&playback_proxy, get_playback_config()).await;

        let manager_proxy = setup_manager().await;
        // When the SensorManager starts, it gets the initial list of sensors before handling any
        // requests. These tests do not call SensorManager::run, so the sensor list can be
        // populated by calling get_sensors_list.
        let _ = manager_proxy.get_sensors_list().await;

        (manager_proxy, playback_proxy)
    }

    #[fuchsia::test]
    async fn test_get_sensors_list() {
        // Playback is misconfigured.
        let mut fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: None,
            sensor_events: None,
            ..Default::default()
        };
        let playback_proxy = connect_playback().await;
        setup_playback(
            &playback_proxy,
            playback_fidl::PlaybackSourceConfig::FixedValuesConfig(fixed_values_config),
        )
        .await;

        let proxy = setup_manager().await;

        let fidl_sensors = proxy.get_sensors_list().await.unwrap();
        assert!(fidl_sensors.is_empty());

        // Playback is configured with empty values.
        fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
            sensor_list: Some(Vec::new()),
            sensor_events: Some(Vec::new()),
            ..Default::default()
        };
        setup_playback(
            &playback_proxy,
            playback_fidl::PlaybackSourceConfig::FixedValuesConfig(fixed_values_config),
        )
        .await;
        let fidl_sensors = proxy.get_sensors_list().await.unwrap();
        assert!(fidl_sensors.is_empty());

        // Playback is configured with the default sensor.
        setup_playback(&playback_proxy, get_playback_config()).await;
        let fidl_sensors = proxy.get_sensors_list().await.unwrap();
        assert!(fidl_sensors.contains(&get_test_sensor()));
    }

    #[fuchsia::test]
    async fn test_activate_sensor() {
        let (proxy, _playback_proxy) = setup().await;
        let id = get_test_sensor().sensor_id.expect("sensor_id");

        assert!(proxy.activate(id).await.unwrap().is_ok());

        // Activate an already activated sensor
        assert!(proxy.activate(id).await.unwrap().is_ok());

        assert_eq!(proxy.activate(-1).await.unwrap(), Err(ActivateSensorError::InvalidSensorId));
    }

    #[fuchsia::test]
    async fn test_deactivate_sensor() {
        let (proxy, _playback_proxy) = setup().await;
        let _ = proxy.get_sensors_list().await;
        let id = get_test_sensor().sensor_id.expect("sensor_id");

        assert!(proxy.deactivate(id).await.unwrap().is_ok());

        // Deactivate an already deactivated sensor
        assert!(proxy.deactivate(id).await.unwrap().is_ok());

        assert_eq!(
            proxy.deactivate(-1).await.unwrap(),
            Err(DeactivateSensorError::InvalidSensorId)
        );
    }

    #[fuchsia::test]
    async fn test_configure_sensor_rates() {
        let (proxy, _playback_proxy) = setup().await;
        let _ = proxy.get_sensors_list().await;
        let id = get_test_sensor().sensor_id.expect("sensor_id");

        let mut config = SensorRateConfig {
            sampling_period_ns: Some(1),
            max_reporting_latency_ns: Some(1),
            ..Default::default()
        };

        assert!(proxy.configure_sensor_rates(id, &config.clone()).await.unwrap().is_ok());

        let mut res = proxy.configure_sensor_rates(-1, &config.clone()).await.unwrap();
        assert_eq!(res, Err(ConfigureSensorRateError::InvalidSensorId));

        config.max_reporting_latency_ns = None;
        config.sampling_period_ns = None;
        res = proxy.configure_sensor_rates(id, &config.clone()).await.unwrap();
        assert_eq!(res, Err(ConfigureSensorRateError::InvalidConfig));
    }

    #[fuchsia::test]
    async fn test_sensor_event_stream() {
        let (proxy, _playback_proxy) = setup().await;
        let id = get_test_sensor().sensor_id.unwrap();
        let _ = proxy.activate(id).await;

        let config = SensorRateConfig {
            sampling_period_ns: Some(0),
            max_reporting_latency_ns: Some(0),
            ..Default::default()
        };

        assert!(proxy.configure_sensor_rates(id, &config.clone()).await.unwrap().is_ok());

        let mut event_stream = proxy.take_event_stream();
        let mut events: Vec<SensorEvent> = Vec::new();
        for _i in 1..4 {
            let mut event: SensorEvent =
                event_stream.next().await.unwrap().unwrap().into_on_sensor_event().unwrap();
            // The test cannot know these values ahead of time, so it can zero them so that it can
            // match the rest of the event.
            event.timestamp = 0;
            event.sequence_number = 0;

            events.push(event);
        }

        assert_eq!(events.len(), 3);

        let test_events = get_test_events();
        for event in events {
            assert!(test_events.contains(&event));
        }
    }
}
