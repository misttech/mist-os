// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::audio::types::{AudioInfo, AudioSettingSource, AudioStream, AudioStreamType};
use crate::audio::{build_audio_default_settings, create_default_modified_counters};
use crate::base::SettingType;
use crate::config::base::AgentType;
use crate::config::default_settings::DefaultSetting;
use crate::ingress::fidl::Interface;
use crate::inspect::config_logger::InspectConfigLogger;
use crate::storage::testing::InMemoryStorageFactory;
use crate::tests::fakes::audio_core_service::{self, AudioCoreService};
use crate::tests::fakes::service_registry::ServiceRegistry;
use crate::tests::test_failure_utils::create_test_env_with_failures_and_config;
use crate::EnvironmentBuilder;
use assert_matches::assert_matches;
use fidl::Error::ClientChannelClosed;
use fidl_fuchsia_media::{AudioRenderUsage, AudioRenderUsage2};
use fidl_fuchsia_settings::*;
use fuchsia_component::server::ProtocolConnector;
use fuchsia_inspect::component;
use futures::lock::Mutex;
use settings_storage::device_storage::DeviceStorage;
use std::collections::HashMap;
use std::rc::Rc;
use zx::Status;

const ENV_NAME: &str = "settings_service_audio_test_environment";

const CHANGED_VOLUME_LEVEL: f32 = 0.7;
const CHANGED_VOLUME_MUTED: bool = true;

fn changed_media_stream_settings() -> AudioStreamSettings {
    AudioStreamSettings {
        stream: Some(AudioRenderUsage::Media),
        source: Some(AudioStreamSettingSource::User),
        user_volume: Some(Volume {
            level: Some(CHANGED_VOLUME_LEVEL),
            muted: Some(CHANGED_VOLUME_MUTED),
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn changed_media_stream_settings2() -> AudioStreamSettings2 {
    AudioStreamSettings2 {
        stream: Some(AudioRenderUsage2::Media),
        source: Some(AudioStreamSettingSource::User),
        user_volume: Some(Volume {
            level: Some(CHANGED_VOLUME_LEVEL),
            muted: Some(CHANGED_VOLUME_MUTED),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn default_audio_info() -> DefaultSetting<AudioInfo, &'static str> {
    let config_logger =
        Rc::new(std::sync::Mutex::new(InspectConfigLogger::new(component::inspector().root())));
    build_audio_default_settings(config_logger)
}

fn load_default_audio_info(
    default_settings: &mut DefaultSetting<AudioInfo, &'static str>,
) -> AudioInfo {
    default_settings.load_default_value().expect("config should exist and parse for test").unwrap()
}

/// Creates an environment that will fail on a get request.
async fn create_audio_test_env_with_failures(
    storage_factory: Rc<InMemoryStorageFactory>,
) -> AudioProxy {
    create_test_env_with_failures_and_config(
        storage_factory,
        ENV_NAME,
        Interface::Audio,
        SettingType::Audio,
        |builder| builder.audio_configuration(default_audio_info()),
    )
    .await
    .connect_to_protocol::<AudioMarker>()
    .unwrap()
}

// Used to store fake services for mocking dependencies and checking input/outputs.
// To add a new fake to these tests, add here, in create_services, and then use
// in your test.
struct FakeServices {
    audio_core: Rc<Mutex<AudioCoreService>>,
}

fn get_default_stream(stream_type: AudioStreamType, info: AudioInfo) -> AudioStream {
    info.streams.into_iter().find(|x| x.stream_type == stream_type).expect("contains stream")
}

// Verifies that a stream equal to |stream| is inside of |settings|.
fn verify_audio_stream(settings: &AudioSettings, stream: AudioStreamSettings) {
    let _ = settings
        .streams
        .as_ref()
        .expect("audio settings contain streams")
        .iter()
        .find(|x| **x == stream)
        .expect("contains stream");
}
fn verify_audio_stream2(settings: &AudioSettings2, stream: AudioStreamSettings2) {
    let _ = settings
        .streams
        .as_ref()
        .expect("audio settings contain streams")
        .iter()
        .find(|x| **x == stream)
        .expect("contains stream");
}

// Returns a registry and audio related services it is populated with
async fn create_services(
    default_settings: AudioInfo,
) -> (Rc<Mutex<ServiceRegistry>>, FakeServices) {
    let service_registry = ServiceRegistry::create();
    let audio_core_service_handle = audio_core_service::Builder::new(default_settings).build();
    service_registry.lock().await.register_service(audio_core_service_handle.clone());

    (service_registry, FakeServices { audio_core: audio_core_service_handle })
}

async fn create_environment(
    service_registry: Rc<Mutex<ServiceRegistry>>,
    mut default_settings: DefaultSetting<AudioInfo, &'static str>,
) -> (ProtocolConnector, Rc<DeviceStorage>) {
    let storage_factory = Rc::new(InMemoryStorageFactory::with_initial_data(
        &load_default_audio_info(&mut default_settings),
    ));

    let connector = EnvironmentBuilder::new(Rc::clone(&storage_factory))
        .service(ServiceRegistry::serve(service_registry))
        .fidl_interfaces(&[Interface::Audio])
        .audio_configuration(default_settings)
        .spawn_and_get_protocol_connector(ENV_NAME)
        .await
        .unwrap();
    let store = storage_factory.get_device_storage().await;
    (connector, store)
}

// Test that the audio settings are restored correctly.
#[fuchsia::test(allow_stalls = false)]
async fn test_volume_restore() {
    let mut default_settings = default_audio_info();
    let mut stored_info = load_default_audio_info(&mut default_settings);
    let (service_registry, fake_services) = create_services(stored_info.clone()).await;
    let expected_info = (0.9, false);
    for stream in stored_info.streams.iter_mut() {
        if stream.stream_type == AudioStreamType::Media {
            stream.user_volume_level = expected_info.0;
            stream.user_volume_muted = expected_info.1;
        }
    }

    let storage_factory = InMemoryStorageFactory::with_initial_data(&stored_info);
    assert!(EnvironmentBuilder::new(Rc::new(storage_factory))
        .service(Box::new(ServiceRegistry::serve(service_registry)))
        .agents(vec![AgentType::Restore.into()])
        .fidl_interfaces(&[Interface::Audio])
        .audio_configuration(default_settings)
        .spawn_nested(ENV_NAME)
        .await
        .is_ok());

    let stored_info =
        fake_services.audio_core.lock().await.get_level_and_mute(AudioRenderUsage2::Media).unwrap();
    assert_eq!(stored_info, expected_info);
}

#[fuchsia::test(allow_stalls = false)]
async fn test_persisted_values_applied_at_start() {
    let mut default_settings = default_audio_info();
    let (service_registry, fake_services) =
        create_services(load_default_audio_info(&mut default_settings)).await;

    let test_audio_info = AudioInfo {
        streams: [
            AudioStream {
                stream_type: AudioStreamType::Background,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Media,
                source: AudioSettingSource::User,
                user_volume_level: 0.6,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Interruption,
                source: AudioSettingSource::System,
                user_volume_level: 0.3,
                user_volume_muted: false,
            },
            AudioStream {
                stream_type: AudioStreamType::SystemAgent,
                source: AudioSettingSource::User,
                user_volume_level: 0.7,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Communication,
                source: AudioSettingSource::User,
                user_volume_level: 0.8,
                user_volume_muted: false,
            },
            AudioStream {
                stream_type: AudioStreamType::Accessibility,
                source: AudioSettingSource::User,
                user_volume_level: 0.9,
                user_volume_muted: false,
            },
        ],
        modified_counters: Some(create_default_modified_counters()),
    };

    let storage_factory = InMemoryStorageFactory::with_initial_data(&test_audio_info);

    let env = EnvironmentBuilder::new(Rc::new(storage_factory))
        .service(ServiceRegistry::serve(service_registry))
        .agents(vec![AgentType::Restore.into()])
        .fidl_interfaces(&[Interface::Audio])
        .audio_configuration(default_settings)
        .spawn_and_get_protocol_connector(ENV_NAME)
        .await
        .unwrap();

    let audio_proxy = env.connect_to_protocol::<AudioMarker>().unwrap();

    let settings = audio_proxy.watch2().await.expect("watch2 completed");

    // Check that stored values were returned by watch2() and applied to the audio core service.
    for stream in test_audio_info.streams.iter() {
        verify_audio_stream2(&settings, AudioStreamSettings2::from(*stream));
        assert_eq!(
            (stream.user_volume_level, stream.user_volume_muted),
            fake_services
                .audio_core
                .lock()
                .await
                .get_level_and_mute(AudioRenderUsage2::from(stream.stream_type))
                .unwrap()
        );
    }
}

//
// Each test listed below is performed both with Watch/Set methods, and with Watch2/Set2 methods.
//

// Ensure watch() won't crash if audio core fails.
#[fuchsia::test(allow_stalls = false)]
async fn test_watch_without_audio_core() {
    let mut default_settings = default_audio_info();
    let default_info = load_default_audio_info(&mut default_settings);
    let service_registry = ServiceRegistry::create();

    let (connector, _) = create_environment(service_registry, default_settings).await;

    // At this point we should not crash.
    let audio_proxy = connector.connect_to_protocol::<AudioMarker>().unwrap();

    let settings = audio_proxy.watch().await.expect("watch completed");
    verify_audio_stream(
        &settings,
        AudioStreamSettings::try_from(get_default_stream(AudioStreamType::Media, default_info))
            .unwrap(),
    );
}

// Ensure watch2() won't crash if audio core fails.
#[fuchsia::test(allow_stalls = false)]
async fn test_watch2_without_audio_core() {
    let mut default_settings = default_audio_info();
    let default_info = load_default_audio_info(&mut default_settings);
    let service_registry = ServiceRegistry::create();

    let (connector, _) = create_environment(service_registry, default_settings).await;

    // At this point we should not crash.
    let audio_proxy = connector.connect_to_protocol::<AudioMarker>().unwrap();

    let settings = audio_proxy.watch2().await.expect("watch2 completed");
    verify_audio_stream2(
        &settings,
        AudioStreamSettings2::from(get_default_stream(AudioStreamType::Media, default_info)),
    );
}

// Ensure that we can handle an AudioCore channel closure, triggered by a watch() call.
#[fuchsia::test(allow_stalls = false)]
async fn test_channel_failure_watch() {
    let audio_proxy =
        create_audio_test_env_with_failures(Rc::new(InMemoryStorageFactory::new())).await;
    let result = audio_proxy.watch().await;
    assert_matches!(result, Err(ClientChannelClosed { status: Status::UNAVAILABLE, .. }));
}

// Ensure that we can handle an AudioCore channel closure, triggered by a watch2() call.
#[fuchsia::test(allow_stalls = false)]
async fn test_channel_failure_watch2() {
    let audio_proxy =
        create_audio_test_env_with_failures(Rc::new(InMemoryStorageFactory::new())).await;
    let result = audio_proxy.watch2().await;
    assert_matches!(result, Err(ClientChannelClosed { status: Status::UNAVAILABLE, .. }));
}

// set() and set2() calls for stream types not in our settings should fail but remain connected.
#[fuchsia::test(allow_stalls = false)]
async fn test_invalid_stream_fails() {
    let mut default_settings = default_audio_info();
    // Create a service registry with a fake audio core service that suppresses client errors, since
    // the invalid set call will cause the connection to close.
    let service_registry = ServiceRegistry::create();
    let audio_core_service_handle =
        audio_core_service::Builder::new(load_default_audio_info(&mut default_settings))
            .set_suppress_client_errors(true)
            .build();
    service_registry.lock().await.register_service(audio_core_service_handle.clone());

    // The AudioInfo settings must have 6 streams, but include a duplicate of the Background stream
    // type so that we can perform a set call with a Media stream that isn't in the AudioInfo.
    let counters: HashMap<_, _> = [
        (AudioStreamType::Background, 0),
        (AudioStreamType::Interruption, 0),
        (AudioStreamType::SystemAgent, 0),
        (AudioStreamType::Communication, 0),
        (AudioStreamType::Accessibility, 0),
    ]
    .into();

    let test_audio_info = AudioInfo {
        streams: [
            AudioStream {
                stream_type: AudioStreamType::Background,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Background,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Interruption,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::SystemAgent,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Communication,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Accessibility,
                source: AudioSettingSource::User,
                user_volume_level: 0.5,
                user_volume_muted: true,
            },
        ],
        modified_counters: Some(counters),
    };

    // Start the environment with the hand-crafted data.
    let storage_factory = InMemoryStorageFactory::with_initial_data(&test_audio_info);
    let env = EnvironmentBuilder::new(Rc::new(storage_factory))
        .service(ServiceRegistry::serve(service_registry))
        .agents(vec![AgentType::Restore.into()])
        .fidl_interfaces(&[Interface::Audio])
        .audio_configuration(default_settings)
        .spawn_and_get_protocol_connector(ENV_NAME)
        .await
        .unwrap();

    // Connect to the service and make a watch call that should succeed.
    let audio_proxy = env.connect_to_protocol::<AudioMarker>().unwrap();
    let _ = audio_proxy.watch().await.expect("watch completed");

    // Call set() to change the volume of the media stream, which isn't present and should fail.
    let audio_settings = AudioSettings {
        streams: Some(vec![changed_media_stream_settings()]),
        ..Default::default()
    };
    let _ = audio_proxy
        .set(&audio_settings)
        .await
        .expect("set completed")
        .expect_err("set should fail");

    // Call set2() to change the volume of the media stream, which isn't present and should fail.
    let audio_settings = AudioSettings2 {
        streams: Some(vec![changed_media_stream_settings2()]),
        ..Default::default()
    };
    let _ = audio_proxy
        .set2(&audio_settings)
        .await
        .expect("set2 completed")
        .expect_err("set2 should fail");
}
