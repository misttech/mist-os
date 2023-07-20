// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::input_handler::{InputHandlerStatus, UnhandledInputHandler};
use crate::{autorepeater, input_device, metrics};
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_input as finput;
use fidl_fuchsia_settings as fsettings;
use fuchsia_async as fasync;
use fuchsia_inspect;
use futures::{TryFutureExt, TryStreamExt};
use metrics_registry::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The text settings handler instance. Refer to as `text_settings_handler::TextSettingsHandler`.
/// Its task is to decorate an input event with the keymap identifier.  The instance can
/// be freely cloned, each clone is thread-safely sharing data with others.
pub struct TextSettingsHandler {
    /// Stores the currently active keymap identifier, if present.  Wrapped
    /// in an refcell as it can be changed out of band through
    /// `fuchsia.input.keymap.Configuration/SetLayout`.
    keymap_id: RefCell<Option<finput::KeymapId>>,

    /// Stores the currently active autorepeat settings.
    autorepeat_settings: RefCell<Option<autorepeater::Settings>>,

    /// The inventory of this handler's Inspect status.
    pub inspect_status: InputHandlerStatus,

    /// The metrics logger.
    metrics_logger: metrics::MetricsLogger,
}

#[async_trait(?Send)]
impl UnhandledInputHandler for TextSettingsHandler {
    async fn handle_unhandled_input_event(
        self: Rc<Self>,
        unhandled_input_event: input_device::UnhandledInputEvent,
    ) -> Vec<input_device::InputEvent> {
        match unhandled_input_event.clone() {
            input_device::UnhandledInputEvent {
                device_event: input_device::InputDeviceEvent::Keyboard(mut event),
                device_descriptor,
                event_time,
                trace_id: _,
            } => {
                self.inspect_status
                    .count_received_event(input_device::InputEvent::from(unhandled_input_event));
                let keymap_id = self.get_keymap_name();
                tracing::debug!(
                    "text_settings_handler::Instance::handle_unhandled_input_event: keymap_id = {:?}",
                    &keymap_id
                );
                event = event
                    .into_with_keymap(keymap_id)
                    .into_with_autorepeat_settings(self.get_autorepeat_settings());
                vec![input_device::InputEvent {
                    device_event: input_device::InputDeviceEvent::Keyboard(event),
                    device_descriptor,
                    event_time,
                    handled: input_device::Handled::No,
                    trace_id: None,
                }]
            }
            // Pass a non-keyboard event through.
            _ => vec![input_device::InputEvent::from(unhandled_input_event)],
        }
    }
}

impl TextSettingsHandler {
    /// Creates a new text settings handler instance.
    ///
    /// `initial_*` contain the desired initial values to be served.  Usually
    /// you want the defaults.
    pub fn new(
        initial_keymap: Option<finput::KeymapId>,
        initial_autorepeat: Option<autorepeater::Settings>,
        input_handlers_node: &fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Rc<Self> {
        let inspect_status = InputHandlerStatus::new(
            input_handlers_node,
            "text_settings_handler",
            /* generates_events */ false,
        );
        Rc::new(Self {
            keymap_id: RefCell::new(initial_keymap),
            autorepeat_settings: RefCell::new(initial_autorepeat),
            inspect_status,
            metrics_logger,
        })
    }

    /// Processes requests for keymap change from `stream`.
    pub async fn process_keymap_configuration_from(
        self: &Rc<Self>,
        proxy: fsettings::KeyboardProxy,
    ) -> Result<(), Error> {
        let mut stream = HangingGetStream::new(proxy, fsettings::KeyboardProxy::watch);
        loop {
            match stream
                .try_next()
                .await
                .context("while waiting on fuchsia.settings.Keyboard/Watch")?
            {
                Some(fsettings::KeyboardSettings { keymap, autorepeat, .. }) => {
                    self.set_keymap_id(keymap);
                    self.set_autorepeat_settings(autorepeat.map(|e| e.into()));
                    tracing::info!("keymap ID set to: {:?}", self.get_keymap_id());
                }
                e => {
                    self.metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::TextSettingsHandlerExit,
                        std::format!("exiting - unexpected response: {:?}", e),
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    /// Starts reading events from the stream.  Does not block.
    pub fn serve(self: Rc<Self>, proxy: fsettings::KeyboardProxy) {
        let metrics_logger_clone = self.metrics_logger.clone();
        fasync::Task::local(
            async move { self.process_keymap_configuration_from(proxy).await }
                // In most tests, this message is not fatal. It indicates that keyboard
                // settings won't run, but that only means you can't configure keyboard
                // settings. If your tests does not need to change keymaps, or adjust
                // key autorepeat rates, these are not relevant.
                .unwrap_or_else(move |e: anyhow::Error| {
                    metrics_logger_clone.log_error(
                        InputPipelineErrorMetricDimensionEvent::TextSettingsHandlerCantRun,
                        std::format!("can't run: {:?}", e),
                    );
                }),
        )
        .detach();
    }

    fn set_keymap_id(self: &Rc<Self>, keymap_id: Option<finput::KeymapId>) {
        *(self.keymap_id.borrow_mut()) = keymap_id;
    }

    fn set_autorepeat_settings(self: &Rc<Self>, autorepeat: Option<autorepeater::Settings>) {
        *(self.autorepeat_settings.borrow_mut()) = autorepeat.map(|s| s.into());
    }

    /// Gets the currently active keymap ID.
    pub fn get_keymap_id(&self) -> Option<finput::KeymapId> {
        self.keymap_id.borrow().clone()
    }

    /// Gets the currently active autorepeat settings.
    pub fn get_autorepeat_settings(&self) -> Option<autorepeater::Settings> {
        self.autorepeat_settings.borrow().clone()
    }

    fn get_keymap_name(&self) -> Option<String> {
        // Maybe instead just pass in the keymap ID directly?
        match *self.keymap_id.borrow() {
            Some(id) => match id {
                finput::KeymapId::FrAzerty => Some("FR_AZERTY".to_owned()),
                finput::KeymapId::UsDvorak => Some("US_DVORAK".to_owned()),
                finput::KeymapId::UsColemak => Some("US_COLEMAK".to_owned()),
                finput::KeymapId::UsQwerty | finput::KeymapIdUnknown!() => {
                    Some("US_QWERTY".to_owned())
                }
            },
            None => Some("US_QWERTY".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::input_device;
    use crate::input_handler::InputHandler;
    use crate::keyboard_binding;
    use crate::testing_utilities;
    use fidl_fuchsia_input;
    use fidl_fuchsia_ui_input3;
    use fuchsia_async as fasync;
    use fuchsia_inspect;
    use fuchsia_zircon as zx;
    use pretty_assertions::assert_eq;
    use std::convert::TryFrom as _;

    fn input_event_from(
        keyboard_event: keyboard_binding::KeyboardEvent,
    ) -> input_device::InputEvent {
        testing_utilities::create_input_event(
            keyboard_event,
            &input_device::InputDeviceDescriptor::Fake,
            zx::Time::from_nanos(42),
            input_device::Handled::No,
        )
    }

    fn key_event_with_settings(
        keymap: Option<String>,
        settings: autorepeater::Settings,
    ) -> input_device::InputEvent {
        let keyboard_event = keyboard_binding::KeyboardEvent::new(
            fidl_fuchsia_input::Key::A,
            fidl_fuchsia_ui_input3::KeyEventType::Pressed,
        )
        .into_with_keymap(keymap)
        .into_with_autorepeat_settings(Some(settings));
        input_event_from(keyboard_event)
    }

    fn key_event(keymap: Option<String>) -> input_device::InputEvent {
        let keyboard_event = keyboard_binding::KeyboardEvent::new(
            fidl_fuchsia_input::Key::A,
            fidl_fuchsia_ui_input3::KeyEventType::Pressed,
        )
        .into_with_keymap(keymap);
        input_event_from(keyboard_event)
    }

    fn unhandled_key_event() -> input_device::UnhandledInputEvent {
        input_device::UnhandledInputEvent::try_from(key_event(None)).unwrap()
    }

    #[fasync::run_singlethreaded(test)]
    async fn keymap_id_setting() {
        #[derive(Debug)]
        struct Test {
            keymap_id: Option<finput::KeymapId>,
            expected: Option<String>,
        }
        let tests = vec![
            Test { keymap_id: None, expected: Some("US_QWERTY".to_owned()) },
            Test {
                keymap_id: Some(finput::KeymapId::UsQwerty),
                expected: Some("US_QWERTY".to_owned()),
            },
            Test {
                keymap_id: Some(finput::KeymapId::FrAzerty),
                expected: Some("FR_AZERTY".to_owned()),
            },
            Test {
                keymap_id: Some(finput::KeymapId::UsDvorak),
                expected: Some("US_DVORAK".to_owned()),
            },
            Test {
                keymap_id: Some(finput::KeymapId::UsColemak),
                expected: Some("US_COLEMAK".to_owned()),
            },
        ];
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");
        for test in tests {
            let handler = TextSettingsHandler::new(
                test.keymap_id.clone(),
                None,
                &test_node,
                metrics::MetricsLogger::default(),
            );
            let expected = key_event(test.expected.clone());
            let result = handler.handle_unhandled_input_event(unhandled_key_event()).await;
            assert_eq!(vec![expected], result, "for: {:?}", &test);
        }
    }

    fn serve_into(
        mut server_end: fsettings::KeyboardRequestStream,
        keymap: Option<finput::KeymapId>,
        autorepeat: Option<fsettings::Autorepeat>,
    ) {
        fasync::Task::local(async move {
            if let Ok(Some(fsettings::KeyboardRequest::Watch { responder, .. })) =
                server_end.try_next().await
            {
                let settings =
                    fsettings::KeyboardSettings { keymap, autorepeat, ..Default::default() };
                responder.send(&settings).expect("response sent");
            }
        })
        .detach();
    }

    #[fasync::run_singlethreaded(test)]
    async fn config_call_processing() {
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");
        let handler =
            TextSettingsHandler::new(None, None, &test_node, metrics::MetricsLogger::default());

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsettings::KeyboardMarker>().unwrap();

        // Serve a specific keyboard setting.
        serve_into(
            stream,
            Some(finput::KeymapId::FrAzerty),
            Some(fsettings::Autorepeat { delay: 43, period: 44 }),
        );

        // Start an asynchronous handler that processes keymap configuration calls
        // incoming from `server_end`.
        handler.clone().serve(proxy);

        // Setting the keymap with a hanging get that does not synchronize with the "main"
        // task of the handler inherently races with `handle_input_event`.  So, the only
        // way to test it correctly is to verify that we get a changed setting *eventually*
        // after asking the server to hand out the modified settings.  So, we loop with an
        // expectation that at some point the settings get applied.  To avoid a long timeout
        // we quit the loop if nothing happened after a generous amount of time.
        let deadline = fuchsia_async::Time::after(zx::Duration::from_seconds(5));
        let autorepeat: autorepeater::Settings = Default::default();
        loop {
            let result = handler.clone().handle_unhandled_input_event(unhandled_key_event()).await;
            let expected = key_event_with_settings(
                Some("FR_AZERTY".to_owned()),
                autorepeat
                    .clone()
                    .into_with_delay(zx::Duration::from_nanos(43))
                    .into_with_period(zx::Duration::from_nanos(44)),
            );
            if vec![expected] == result {
                break;
            }
            fuchsia_async::Timer::new(fuchsia_async::Time::after(zx::Duration::from_millis(10)))
                .await;
            let now = fuchsia_async::Time::now();
            assert!(now < deadline, "the settings did not get applied, was: {:?}", &result);
        }
    }

    #[fuchsia::test]
    fn text_settings_handler_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let fake_handlers_node = inspector.root().create_child("input_handlers_node");
        let _handler = TextSettingsHandler::new(
            None,
            None,
            &fake_handlers_node,
            metrics::MetricsLogger::default(),
        );
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_handlers_node: {
                text_settings_handler: {
                    events_received_count: 0u64,
                    events_handled_count: 0u64,
                    last_received_timestamp_ns: 0u64,
                    "fuchsia.inspect.Health": {
                        status: "STARTING_UP",
                        // Timestamp value is unpredictable and not relevant in this context,
                        // so we only assert that the property is present.
                        start_timestamp_nanos: fuchsia_inspect::AnyProperty
                    },
                }
            }
        });
    }

    #[fasync::run_singlethreaded(test)]
    async fn text_settings_handler_inspect_counts_events() {
        let inspector = fuchsia_inspect::Inspector::default();
        let fake_handlers_node = inspector.root().create_child("input_handlers_node");
        let text_settings_handler = TextSettingsHandler::new(
            None,
            None,
            &fake_handlers_node,
            metrics::MetricsLogger::default(),
        );
        let device_descriptor = input_device::InputDeviceDescriptor::Keyboard(
            keyboard_binding::KeyboardDeviceDescriptor {
                keys: vec![finput::Key::A, finput::Key::B],
                ..Default::default()
            },
        );
        let (_, event_time_u64) = testing_utilities::event_times();
        let input_events = vec![
            testing_utilities::create_keyboard_event_with_time(
                finput::Key::A,
                fidl_fuchsia_ui_input3::KeyEventType::Pressed,
                None,
                event_time_u64,
                &device_descriptor,
                /* keymap= */ None,
            ),
            // Should not count received events that have already been handled.
            testing_utilities::create_keyboard_event_with_handled(
                finput::Key::B,
                fidl_fuchsia_ui_input3::KeyEventType::Pressed,
                None,
                event_time_u64,
                &device_descriptor,
                /* keymap= */ None,
                /* key_meaning= */ None,
                input_device::Handled::Yes,
            ),
            testing_utilities::create_keyboard_event_with_time(
                finput::Key::A,
                fidl_fuchsia_ui_input3::KeyEventType::Released,
                None,
                event_time_u64,
                &device_descriptor,
                /* keymap= */ None,
            ),
            // Should not count non-keyboard input events.
            testing_utilities::create_fake_input_event(event_time_u64),
            testing_utilities::create_keyboard_event_with_time(
                finput::Key::B,
                fidl_fuchsia_ui_input3::KeyEventType::Pressed,
                None,
                event_time_u64,
                &device_descriptor,
                /* keymap= */ None,
            ),
        ];

        for input_event in input_events {
            let _ = text_settings_handler.clone().handle_input_event(input_event).await;
        }

        let last_event_timestamp: u64 = event_time_u64.into_nanos().try_into().unwrap();

        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_handlers_node: {
                text_settings_handler: {
                    events_received_count: 3u64,
                    events_handled_count: 0u64,
                    last_received_timestamp_ns: last_event_timestamp,
                    "fuchsia.inspect.Health": {
                        status: "STARTING_UP",
                        // Timestamp value is unpredictable and not relevant in this context,
                        // so we only assert that the property is present.
                        start_timestamp_nanos: fuchsia_inspect::AnyProperty
                    },
                }
            }
        });
    }
}
