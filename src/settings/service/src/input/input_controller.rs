// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base::{SettingInfo, SettingType};
use crate::config::default_settings::DefaultSetting;
use crate::handler::base::Request;
use crate::handler::setting_handler::persist::{controller as data_controller, ClientProxy};
use crate::handler::setting_handler::{
    controller, ControllerError, ControllerStateResult, IntoHandlerResult, SettingHandlerResult,
    State,
};
use crate::input::common::connect_to_camera;
use crate::input::input_device_configuration::InputConfiguration;
use crate::input::types::{
    DeviceState, DeviceStateSource, InputDevice, InputDeviceType, InputInfo, InputInfoSources,
    InputState, Microphone,
};
use crate::input::MediaButtons;
use settings_storage::device_storage::{DeviceStorage, DeviceStorageCompatible};
use settings_storage::storage_factory::{NoneT, StorageAccess};

use anyhow::Error;
use async_trait::async_trait;
use fuchsia_trace as ftrace;
use futures::lock::Mutex;
use serde::{Deserialize, Serialize};
use std::rc::Rc;

pub(crate) const DEFAULT_CAMERA_NAME: &str = "camera";
pub(crate) const DEFAULT_MIC_NAME: &str = "microphone";

impl DeviceStorageCompatible for InputInfoSources {
    type Loader = NoneT;
    const KEY: &'static str = "input_info";

    fn try_deserialize_from(value: &str) -> Result<Self, Error> {
        Self::extract(value).or_else(|e| {
            log::info!("Failed to deserialize InputInfoSources. Falling back to V2: {e:?}");
            InputInfoSourcesV2::try_deserialize_from(value).map(Self::from)
        })
    }
}

impl From<InputInfoSourcesV2> for InputInfoSources {
    fn from(v2: InputInfoSourcesV2) -> Self {
        let mut input_state = v2.input_device_state;

        // Convert the old states into an input device.
        input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::HARDWARE,
            if v2.hw_microphone.muted { DeviceState::MUTED } else { DeviceState::AVAILABLE },
        );
        input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            if v2.sw_microphone.muted { DeviceState::MUTED } else { DeviceState::AVAILABLE },
        );

        InputInfoSources { input_device_state: input_state }
    }
}

impl From<InputInfoSources> for SettingInfo {
    fn from(info: InputInfoSources) -> SettingInfo {
        SettingInfo::Input(info.into())
    }
}

impl From<InputInfoSources> for InputInfo {
    fn from(info: InputInfoSources) -> InputInfo {
        InputInfo { input_device_state: info.input_device_state }
    }
}

impl From<InputInfo> for SettingInfo {
    fn from(info: InputInfo) -> SettingInfo {
        SettingInfo::Input(info)
    }
}

#[derive(PartialEq, Default, Debug, Clone, Serialize, Deserialize)]
pub struct InputInfoSourcesV2 {
    hw_microphone: Microphone,
    sw_microphone: Microphone,
    input_device_state: InputState,
}

impl DeviceStorageCompatible for InputInfoSourcesV2 {
    type Loader = NoneT;
    const KEY: &'static str = "input_info_sources_v2";

    fn try_deserialize_from(value: &str) -> Result<Self, Error> {
        Self::extract(value).or_else(|e| {
            log::info!("Failed to deserialize InputInfoSourcesV2. Falling back to V1: {e:?}");
            InputInfoSourcesV1::try_deserialize_from(value).map(Self::from)
        })
    }
}

impl From<InputInfoSourcesV1> for InputInfoSourcesV2 {
    fn from(v1: InputInfoSourcesV1) -> Self {
        InputInfoSourcesV2 {
            hw_microphone: v1.hw_microphone,
            sw_microphone: v1.sw_microphone,
            input_device_state: InputState::new(),
        }
    }
}

#[derive(PartialEq, Default, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InputInfoSourcesV1 {
    pub hw_microphone: Microphone,
    pub sw_microphone: Microphone,
}

impl DeviceStorageCompatible for InputInfoSourcesV1 {
    type Loader = NoneT;
    const KEY: &'static str = "input_info_sources_v1";
}

type InputControllerInnerHandle = Rc<Mutex<InputControllerInner>>;

/// Inner struct for the InputController.
///
/// Allows the controller to use a lock on its contents.
struct InputControllerInner {
    /// Client to communicate with persistent store and notify on.
    client: ClientProxy,

    /// Local tracking of the input device states.
    input_device_state: InputState,

    /// Configuration for this device.
    input_device_config: InputConfiguration,
}

impl InputControllerInner {
    // Wrapper around client.read() that fills in the config
    // as the default value if the read value is empty. It may be empty
    // after a migration from a previous InputInfoSources version
    // or on pave.
    async fn get_stored_info(&self) -> InputInfo {
        let mut input_info = self.client.read_setting::<InputInfo>(ftrace::Id::new()).await;
        if input_info.input_device_state.is_empty() {
            input_info.input_device_state = self.input_device_config.clone().into();
        }
        input_info
    }

    /// Gets the input state.
    async fn get_info(&mut self) -> Result<InputInfo, ControllerError> {
        Ok(InputInfo { input_device_state: self.input_device_state.clone() })
    }

    /// Restores the input state.
    async fn restore(&mut self) -> ControllerStateResult {
        let input_info = self.get_stored_info().await;
        self.input_device_state = input_info.input_device_state;

        if self.input_device_config.devices.iter().any(|d| d.device_type == InputDeviceType::CAMERA)
        {
            match self.get_cam_sw_state() {
                Ok(state) => {
                    // Camera setup failure should not prevent start of service. This also allows
                    // clients to see that the camera may not be usable.
                    if let Err(e) = self.push_cam_sw_state(state).await {
                        log::error!("Unable to restore camera state: {e:?}");
                        self.set_cam_err_state(state);
                    }
                }
                Err(e) => {
                    log::error!("Unable to load cam sw state: {e:?}");
                    self.set_cam_err_state(DeviceState::ERROR);
                }
            }
        }
        Ok(())
    }

    async fn set_sw_camera_mute(&mut self, disabled: bool, name: String) -> SettingHandlerResult {
        let mut input_info = self.get_stored_info().await;
        input_info.input_device_state.set_source_state(
            InputDeviceType::CAMERA,
            name.clone(),
            DeviceStateSource::SOFTWARE,
            if disabled { DeviceState::MUTED } else { DeviceState::AVAILABLE },
        );

        self.input_device_state.set_source_state(
            InputDeviceType::CAMERA,
            name.clone(),
            DeviceStateSource::SOFTWARE,
            if disabled { DeviceState::MUTED } else { DeviceState::AVAILABLE },
        );
        let id = ftrace::Id::new();
        self.client.write_setting(input_info.into(), id).await.into_handler_result()
    }

    /// Sets the hardware mic/cam state from the muted states in `media_buttons`.
    async fn set_hw_media_buttons_state(
        &mut self,
        media_buttons: MediaButtons,
    ) -> SettingHandlerResult {
        let mut states_to_process = Vec::new();
        if let Some(mic_mute) = media_buttons.mic_mute {
            states_to_process.push((InputDeviceType::MICROPHONE, mic_mute));
        }
        if let Some(camera_disable) = media_buttons.camera_disable {
            states_to_process.push((InputDeviceType::CAMERA, camera_disable));
        }

        let mut input_info = self.get_stored_info().await;

        for (device_type, muted) in states_to_process.into_iter() {
            // Fetch current state.
            let hw_state_res = input_info.input_device_state.get_source_state(
                device_type,
                device_type.to_string(),
                DeviceStateSource::HARDWARE,
            );

            let mut hw_state = hw_state_res.map_err(|err| {
                ControllerError::UnexpectedError(
                    format!("Could not fetch current hw mute state: {err:?}").into(),
                )
            })?;

            if muted {
                // Unset available and set muted.
                hw_state &= !DeviceState::AVAILABLE;
                hw_state |= DeviceState::MUTED;
            } else {
                // Set available and unset muted.
                hw_state |= DeviceState::AVAILABLE;
                hw_state &= !DeviceState::MUTED;
            }

            // Set the updated state.
            input_info.input_device_state.set_source_state(
                device_type,
                device_type.to_string(),
                DeviceStateSource::HARDWARE,
                hw_state,
            );
            self.input_device_state.set_source_state(
                device_type,
                device_type.to_string(),
                DeviceStateSource::HARDWARE,
                hw_state,
            );
        }

        // Store the newly set value.
        let id = ftrace::Id::new();
        self.client.write_setting(input_info.into(), id).await.into_handler_result()
    }

    /// Sets state for the given input devices.
    async fn set_input_states(
        &mut self,
        input_devices: Vec<InputDevice>,
        source: DeviceStateSource,
    ) -> SettingHandlerResult {
        let mut input_info = self.get_stored_info().await;
        let device_types = input_info.input_device_state.device_types();

        let cam_state = self.get_cam_sw_state().ok();

        for input_device in input_devices.iter() {
            if !device_types.contains(&input_device.device_type) {
                return Err(ControllerError::UnsupportedError(SettingType::Input));
            }
            input_info.input_device_state.insert_device(input_device.clone(), source);
            self.input_device_state.insert_device(input_device.clone(), source);
        }

        // If the device has a camera, it should successfully get the sw state, and
        // push the state if it has changed. If the device does not have a camera,
        // it should be None both here and above, and thus not detect a change.
        let modified_cam_state = self.get_cam_sw_state().ok();
        if cam_state != modified_cam_state {
            if let Some(state) = modified_cam_state {
                self.push_cam_sw_state(state).await?;
            }
        }

        // Store the newly set value.
        let id = ftrace::Id::new();
        self.client.write_setting(input_info.into(), id).await.into_handler_result()
    }

    #[allow(clippy::result_large_err)] // TODO(https://fxbug.dev/42069089)
    /// Pulls the current software state of the camera from the device state.
    fn get_cam_sw_state(&self) -> Result<DeviceState, ControllerError> {
        self.input_device_state
            .get_source_state(
                InputDeviceType::CAMERA,
                DEFAULT_CAMERA_NAME.to_string(),
                DeviceStateSource::SOFTWARE,
            )
            .map_err(|_| {
                ControllerError::UnexpectedError("Could not find camera software state".into())
            })
    }

    /// Set the camera state into an error condition.
    fn set_cam_err_state(&mut self, mut state: DeviceState) {
        state.set(DeviceState::ERROR, true);
        self.input_device_state.set_source_state(
            InputDeviceType::CAMERA,
            DEFAULT_CAMERA_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            state,
        )
    }

    /// Forwards the given software state to the camera3 api. Will first establish
    /// a connection to the camera3.DeviceWatcher api. This function should only be called
    /// when there is a camera included in the config. The config is used to populate the
    /// stored input_info, so the input_info's input_device_state can be checked whether its
    /// device_types contains Camera prior to calling this function.
    async fn push_cam_sw_state(&mut self, cam_state: DeviceState) -> Result<(), ControllerError> {
        let is_muted = cam_state.has_state(DeviceState::MUTED);

        // Start up a connection to the camera device watcher and connect to the
        // camera proxy using the id that is returned. The connection will drop out
        // of scope after the mute state is sent.
        let camera_proxy =
            connect_to_camera(self.client.get_service_context()).await.map_err(|e| {
                ControllerError::UnexpectedError(
                    format!("Could not connect to camera device: {e:?}").into(),
                )
            })?;

        camera_proxy.set_software_mute_state(is_muted).await.map_err(|e| {
            ControllerError::ExternalFailure(
                SettingType::Input,
                "fuchsia.camera3.Device".into(),
                "SetSoftwareMuteState".into(),
                format!("{e:?}").into(),
            )
        })
    }
}

pub struct InputController {
    /// Handle so that a lock can be used in the Handle trait implementation.
    inner: InputControllerInnerHandle,
}

impl StorageAccess for InputController {
    type Storage = DeviceStorage;
    type Data = InputInfoSources;
    const STORAGE_KEY: &'static str = InputInfoSources::KEY;
}

impl InputController {
    /// Alternate constructor that allows specifying a configuration.
    pub(crate) fn create_with_config(
        client: ClientProxy,
        input_device_config: InputConfiguration,
    ) -> Result<Self, ControllerError> {
        Ok(Self {
            inner: Rc::new(Mutex::new(InputControllerInner {
                client,
                input_device_state: InputState::new(),
                input_device_config,
            })),
        })
    }

    // Whether the configuration for this device contains a specific |device_type|.
    async fn has_input_device(&self, device_type: InputDeviceType) -> bool {
        let input_device_config_state: InputState =
            self.inner.lock().await.input_device_config.clone().into();
        input_device_config_state.device_types().contains(&device_type)
    }
}

impl data_controller::CreateWith for InputController {
    type Data = Rc<std::sync::Mutex<DefaultSetting<InputConfiguration, &'static str>>>;
    fn create_with(client: ClientProxy, data: Self::Data) -> Result<Self, ControllerError> {
        if let Ok(Some(config)) = data.lock().unwrap().load_default_value() {
            InputController::create_with_config(client, config)
        } else {
            Err(ControllerError::InitFailure("Invalid default input device config".into()))
        }
    }
}

#[async_trait(?Send)]
impl controller::Handle for InputController {
    async fn handle(&self, request: Request) -> Option<SettingHandlerResult> {
        match request {
            Request::Restore => Some(self.inner.lock().await.restore().await.map(|_| None)),
            Request::Get => Some(
                self.inner.lock().await.get_info().await.map(|info| Some(SettingInfo::Input(info))),
            ),
            Request::OnCameraSWState(is_muted) => {
                let old_state = match self
                    .inner
                    .lock()
                    .await
                    .get_stored_info()
                    .await
                    .input_device_state
                    .get_source_state(
                        InputDeviceType::CAMERA,
                        DEFAULT_CAMERA_NAME.to_string(),
                        DeviceStateSource::SOFTWARE,
                    )
                    .map_err(|_| {
                        ControllerError::UnexpectedError(
                            "Could not find camera software state".into(),
                        )
                    }) {
                    Ok(state) => state,
                    Err(e) => return Some(Err(e)),
                };
                Some(if old_state.has_state(DeviceState::MUTED) != is_muted {
                    self.inner
                        .lock()
                        .await
                        .set_sw_camera_mute(is_muted, DEFAULT_CAMERA_NAME.to_string())
                        .await
                } else {
                    Ok(None)
                })
            }
            Request::OnButton(mut buttons) => {
                if buttons.mic_mute.is_some()
                    && !self.has_input_device(InputDeviceType::MICROPHONE).await
                {
                    buttons.set_mic_mute(None);
                }
                if buttons.camera_disable.is_some()
                    && !self.has_input_device(InputDeviceType::CAMERA).await
                {
                    buttons.set_camera_disable(None);
                }
                Some(self.inner.lock().await.set_hw_media_buttons_state(buttons).await)
            }
            Request::SetInputStates(input_states) => Some(
                self.inner
                    .lock()
                    .await
                    .set_input_states(input_states, DeviceStateSource::SOFTWARE)
                    .await,
            ),
            _ => None,
        }
    }

    async fn change_state(&mut self, state: State) -> Option<ControllerStateResult> {
        match state {
            State::Startup => Some(self.inner.lock().await.restore().await),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::handler::setting_handler::controller::Handle;
    use crate::handler::setting_handler::ClientImpl;
    use crate::input::input_device_configuration::{InputDeviceConfiguration, SourceState};
    use crate::inspect::config_logger::InspectConfigLogger;
    use crate::service_context::ServiceContext;
    use crate::storage::{Payload as StoragePayload, StorageRequest, StorageResponse};
    use crate::tests::fakes::service_registry::ServiceRegistry;
    use crate::{service, Address};

    use fuchsia_async as fasync;
    use fuchsia_inspect::component;
    use settings_storage::UpdateState;

    use super::*;

    #[fuchsia::test]
    fn test_input_migration_v1_to_current() {
        const MUTED_MIC: Microphone = Microphone { muted: true };
        let mut v1 = InputInfoSourcesV1::default();
        v1.sw_microphone = MUTED_MIC;

        let serialized_v1 = v1.serialize_to();
        let current = InputInfoSources::try_deserialize_from(&serialized_v1)
            .expect("deserialization should succeed");
        let mut expected_input_state = InputState::new();
        expected_input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            DeviceState::MUTED,
        );
        expected_input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::HARDWARE,
            DeviceState::AVAILABLE,
        );
        assert_eq!(current.input_device_state, expected_input_state);
    }

    #[fuchsia::test]
    fn test_input_migration_v1_to_v2() {
        const MUTED_MIC: Microphone = Microphone { muted: true };
        let mut v1 = InputInfoSourcesV1::default();
        v1.sw_microphone = MUTED_MIC;

        let serialized_v1 = v1.serialize_to();
        let v2 = InputInfoSourcesV2::try_deserialize_from(&serialized_v1)
            .expect("deserialization should succeed");

        assert_eq!(v2.hw_microphone, Microphone { muted: false });
        assert_eq!(v2.sw_microphone, MUTED_MIC);
        assert_eq!(v2.input_device_state, InputState::new());
    }

    #[fuchsia::test]
    fn test_input_migration_v2_to_current() {
        const DEFAULT_CAMERA_NAME: &str = "camera";
        const MUTED_MIC: Microphone = Microphone { muted: true };
        let mut v2 = InputInfoSourcesV2::default();
        v2.input_device_state.set_source_state(
            InputDeviceType::CAMERA,
            DEFAULT_CAMERA_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            DeviceState::AVAILABLE,
        );
        v2.input_device_state.set_source_state(
            InputDeviceType::CAMERA,
            DEFAULT_CAMERA_NAME.to_string(),
            DeviceStateSource::HARDWARE,
            DeviceState::MUTED,
        );
        v2.sw_microphone = MUTED_MIC;

        let serialized_v2 = v2.serialize_to();
        let current = InputInfoSources::try_deserialize_from(&serialized_v2)
            .expect("deserialization should succeed");
        let mut expected_input_state = InputState::new();

        expected_input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            DeviceState::MUTED,
        );
        expected_input_state.set_source_state(
            InputDeviceType::MICROPHONE,
            DEFAULT_MIC_NAME.to_string(),
            DeviceStateSource::HARDWARE,
            DeviceState::AVAILABLE,
        );
        expected_input_state.set_source_state(
            InputDeviceType::CAMERA,
            DEFAULT_CAMERA_NAME.to_string(),
            DeviceStateSource::SOFTWARE,
            DeviceState::AVAILABLE,
        );
        expected_input_state.set_source_state(
            InputDeviceType::CAMERA,
            DEFAULT_CAMERA_NAME.to_string(),
            DeviceStateSource::HARDWARE,
            DeviceState::MUTED,
        );

        assert_eq!(current.input_device_state, expected_input_state);
    }

    #[fasync::run_until_stalled(test)]
    async fn test_camera_error_on_restore() {
        let message_hub = service::MessageHub::create_hub();

        // Create a fake storage receptor used to receive and respond to storage messages.
        let (_, mut storage_receptor) = message_hub
            .create(service::message::MessengerType::Addressable(Address::Storage))
            .await
            .expect("Unable to create agent messenger");

        // Spawn a task that mimics the storage agent by responding to read/write calls.
        fasync::Task::local(async move {
            loop {
                if let Ok((payload, message_client)) = storage_receptor.next_payload().await {
                    if let Ok(StoragePayload::Request(storage_request)) =
                        StoragePayload::try_from(payload)
                    {
                        match storage_request {
                            StorageRequest::Read(_, _) => {
                                // Just respond with the default value as we're not testing storage.
                                let _ = message_client.reply(service::Payload::Storage(
                                    StoragePayload::Response(StorageResponse::Read(
                                        InputInfoSources::default().into(),
                                    )),
                                ));
                            }
                            StorageRequest::Write(_, _) => {
                                // Just respond with Unchanged as we're not testing storage.
                                let _ = message_client.reply(service::Payload::Storage(
                                    StoragePayload::Response(StorageResponse::Write(Ok(
                                        UpdateState::Unchanged,
                                    ))),
                                ));
                            }
                        }
                    }
                }
            }
        })
        .detach();

        let client_proxy = create_proxy(message_hub).await;

        let controller = InputController::create_with_config(
            client_proxy,
            InputConfiguration {
                devices: vec![InputDeviceConfiguration {
                    device_name: DEFAULT_CAMERA_NAME.to_string(),
                    device_type: InputDeviceType::CAMERA,
                    source_states: vec![SourceState {
                        source: DeviceStateSource::SOFTWARE,
                        state: 0,
                    }],
                    mutable_toggle_state: 0,
                }],
            },
        )
        .expect("Should have controller");

        // Restore should pass.
        let result = controller.handle(Request::Restore).await;
        assert_eq!(result, Some(Ok(None)));

        // But the camera state should show an error.
        let result = controller.handle(Request::Get).await;
        let Some(Ok(Some(SettingInfo::Input(input_info)))) = result else {
            panic!("Expected Input response. Got {result:?}");
        };
        let camera_state = input_info
            .input_device_state
            .get_state(InputDeviceType::CAMERA, DEFAULT_CAMERA_NAME.to_string())
            .unwrap();
        assert!(camera_state.has_state(DeviceState::ERROR));
    }

    #[fasync::run_until_stalled(test)]
    async fn test_controller_creation_with_default_config() {
        use crate::handler::setting_handler::persist::controller::CreateWith;

        let message_hub = service::MessageHub::create_hub();
        let client_proxy = create_proxy(message_hub).await;
        let config_logger = InspectConfigLogger::new(component::inspector().root());
        let default_setting = DefaultSetting::new(
            Some(InputConfiguration::default()),
            "/config/data/input_device_config.json",
            Rc::new(std::sync::Mutex::new(config_logger)),
        );
        let _controller = InputController::create_with(
            client_proxy,
            Rc::new(std::sync::Mutex::new(default_setting)),
        )
        .expect("Should have controller");
    }

    async fn create_proxy(message_hub: service::message::Delegate) -> ClientProxy {
        // Create the messenger that the client proxy uses to send messages.
        let (controller_messenger, _) = message_hub
            .create(service::message::MessengerType::Unbound)
            .await
            .expect("Unable to create agent messenger");

        // Note that no camera service is registered, to mimic scenarios where devices do not
        // have a functioning camera service.
        let service_registry = ServiceRegistry::create();

        let service_context =
            ServiceContext::new(Some(ServiceRegistry::serve(service_registry)), None);

        // This isn't actually the signature for the notifier, but it's unused in this test, so just
        // provide the signature of its own messenger to the client proxy.
        let signature = controller_messenger.get_signature();

        ClientProxy::new(
            Rc::new(ClientImpl::for_test(
                Default::default(),
                controller_messenger,
                signature,
                Rc::new(service_context),
                SettingType::Input,
            )),
            SettingType::Input,
        )
        .await
    }
}
