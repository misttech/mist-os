// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::create_endpoints;
use fidl_fuchsia_sensors::*;
use fidl_fuchsia_sensors_types::*;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at};
use futures_util::StreamExt;
use realm_client::{extend_namespace, InstalledNamespace};
use {fidl_fuchsia_hardware_sensors as playback_fidl, fidl_fuchsia_sensors_realm as sensors_realm};

async fn setup_realm() -> anyhow::Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<sensors_realm::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();

    realm_factory.create_realm(dict_server).await?.map_err(realm_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;

    Ok(ns)
}

fn get_heart_rate_sensor() -> SensorInfo {
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

fn get_heart_rate_events() -> Vec<SensorEvent> {
    let mut events: Vec<SensorEvent> = Vec::new();
    for i in 1..4 {
        let event = SensorEvent {
            sensor_id: get_heart_rate_sensor().sensor_id.unwrap(),
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
    let test_sensor = get_heart_rate_sensor();
    let events = get_heart_rate_events();

    let fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
        sensor_list: Some(vec![test_sensor]),
        sensor_events: Some(events),
        ..Default::default()
    };

    playback_fidl::PlaybackSourceConfig::FixedValuesConfig(fixed_values_config)
}

async fn setup() -> anyhow::Result<(InstalledNamespace, ManagerProxy), anyhow::Error> {
    let realm = setup_realm().await?;
    let manager_proxy = connect_to_protocol_at::<ManagerMarker>(&realm)?;

    let _ = manager_proxy.configure_playback(&get_playback_config()).await?;
    // Populate the internal sensors list.
    let _ = manager_proxy.get_sensors_list().await;

    Ok((realm, manager_proxy))
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
async fn test_get_sensors_list() -> anyhow::Result<()> {
    let (_realm, proxy) = setup().await?;

    let fidl_sensors = proxy.get_sensors_list().await.unwrap();
    assert!(fidl_sensors.contains(&get_heart_rate_sensor()));

    clear_playback_config(&proxy).await;

    let fidl_sensors = proxy.get_sensors_list().await.unwrap();
    assert!(fidl_sensors.is_empty());

    Ok(())
}

#[fuchsia::test]
async fn test_activate_sensor() -> anyhow::Result<()> {
    let (_realm, proxy) = setup().await?;
    let id = get_heart_rate_sensor().sensor_id.expect("sensor_id");

    assert!(proxy.activate(id).await.unwrap().is_ok());

    // Activate an already activated sensor
    assert!(proxy.activate(id).await.unwrap().is_ok());

    assert_eq!(proxy.activate(-1).await.unwrap(), Err(ActivateSensorError::InvalidSensorId));

    clear_playback_config(&proxy).await;
    assert_eq!(proxy.activate(id).await.unwrap(), Err(ActivateSensorError::DriverUnavailable));
    Ok(())
}

#[fuchsia::test]
async fn test_deactivate_sensor() -> anyhow::Result<()> {
    let (_realm, proxy) = setup().await?;
    let id = get_heart_rate_sensor().sensor_id.unwrap();

    assert!(proxy.deactivate(id).await?.is_ok());

    // Deactivate an already deactivated sensor.
    assert!(proxy.deactivate(id).await?.is_ok());

    // Deactivate an invalid sensor id.
    assert_eq!(proxy.deactivate(-1).await?, Err(DeactivateSensorError::InvalidSensorId));

    Ok(())
}

#[fuchsia::test]
async fn test_configure_sensor_rates() -> anyhow::Result<()> {
    let (_realm, proxy) = setup().await?;
    let id = get_heart_rate_sensor().sensor_id.unwrap();

    let mut config = SensorRateConfig {
        sampling_period_ns: Some(1),
        max_reporting_latency_ns: Some(1),
        ..Default::default()
    };

    assert!(proxy.activate(id).await?.is_ok());
    assert!(proxy.configure_sensor_rates(id, &config.clone()).await?.is_ok());

    assert_eq!(
        proxy.configure_sensor_rates(-1, &config.clone()).await?,
        Err(ConfigureSensorRateError::InvalidSensorId)
    );

    config.max_reporting_latency_ns = None;
    config.sampling_period_ns = None;
    assert_eq!(
        proxy.configure_sensor_rates(id, &config.clone()).await?,
        Err(ConfigureSensorRateError::InvalidConfig)
    );

    Ok(())
}

#[fuchsia::test]
async fn test_sensor_event_stream() -> anyhow::Result<()> {
    let (_realm, proxy) = setup().await?;
    let id = get_heart_rate_sensor().sensor_id.unwrap();

    let config = SensorRateConfig {
        sampling_period_ns: Some(0),
        max_reporting_latency_ns: Some(0),
        ..Default::default()
    };

    assert!(proxy.activate(id).await?.is_ok());
    assert!(proxy.configure_sensor_rates(id, &config.clone()).await?.is_ok());

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

    let test_events = get_heart_rate_events();
    for event in events {
        assert!(test_events.contains(&event));
    }

    Ok(())
}

#[fuchsia::test]
async fn test_two_clients() -> anyhow::Result<()> {
    let realm = setup_realm().await?;
    let manager1 = connect_to_protocol_at::<ManagerMarker>(&realm)?;
    let manager2 = connect_to_protocol_at::<ManagerMarker>(&realm)?;

    let _ = manager1.configure_playback(&get_playback_config()).await?;
    // Populate the internal sensors list.
    let _ = manager1.get_sensors_list().await;

    let fixed_values_config = playback_fidl::FixedValuesPlaybackConfig {
        sensor_list: Some(vec![get_heart_rate_sensor()]),
        sensor_events: Some(get_heart_rate_events()),
        ..Default::default()
    };

    let _ = manager1
        .configure_playback(&playback_fidl::PlaybackSourceConfig::FixedValuesConfig(
            fixed_values_config,
        ))
        .await?;

    let id = get_heart_rate_sensor().sensor_id.unwrap();
    assert!(manager1.activate(id).await?.is_ok());

    let mut stream1 = manager1.take_event_stream();
    let mut stream2 = manager2.take_event_stream();

    let mut events1: Vec<SensorEvent> = Vec::new();
    let mut events2: Vec<SensorEvent> = Vec::new();

    for _i in 1..4 {
        let mut event: SensorEvent = stream1.next().await.unwrap()?.into_on_sensor_event().unwrap();
        // The test cannot know these values ahead of time, so it can zero them so that it can
        // match the rest of the event.
        event.timestamp = 0;
        event.sequence_number = 0;

        events1.push(event);
    }

    for _i in 1..4 {
        let mut event: SensorEvent = stream2.next().await.unwrap()?.into_on_sensor_event().unwrap();
        // The test cannot know these values ahead of time, so it can zero them so that it can
        // match the rest of the event.
        event.timestamp = 0;
        event.sequence_number = 0;

        events2.push(event);
    }

    let test_events = get_heart_rate_events();
    for event in &events1 {
        assert!(test_events.contains(&event));
    }

    assert_eq!(events1, events2);

    Ok(())
}
