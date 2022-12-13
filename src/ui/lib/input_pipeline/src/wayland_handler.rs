// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    input_device::{InputDeviceDescriptor, InputDeviceEvent, InputEvent, UnhandledInputEvent},
    input_handler::UnhandledInputHandler,
    keyboard_binding::KeyboardEvent,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_input::KeymapId;
use fidl_fuchsia_input_wayland::{KeymapRequestStream, KeymapWatchResponder};
use fuchsia_syslog::{fx_log_debug, fx_log_err};
use fuchsia_trace;
use fuchsia_zircon as zx;
use futures::StreamExt;
use keymaps;
use std::cell::RefCell;
use std::rc::Rc;

pub type NotifyFn = Box<dyn Fn(&KeymapId, KeymapWatchResponder) -> bool + Send>;
pub type KeymapGet = HangingGet<KeymapId, KeymapWatchResponder, NotifyFn>;

/// Serves `fuchsia.input.wayland/Keymap`.
pub struct WaylandHandler {
    /// Hanging get handler for propagating client keymap requests.
    hanging_get: RefCell<KeymapGet>,
}

#[async_trait(?Send)]
impl UnhandledInputHandler for WaylandHandler {
    async fn handle_unhandled_input_event(
        self: Rc<Self>,
        input_event: UnhandledInputEvent,
    ) -> Vec<InputEvent> {
        match input_event {
            UnhandledInputEvent {
                device_event: InputDeviceEvent::Keyboard(event),
                device_descriptor,
                event_time,
                trace_id,
            } => vec![InputEvent::from(self.process_keyboard_event(
                event,
                device_descriptor,
                event_time,
                trace_id,
            ))],
            // Pass other events unchanged.
            _ => vec![InputEvent::from(input_event)],
        }
    }
}

fn keymap_update_fn() -> NotifyFn {
    Box::new(|state, responder: KeymapWatchResponder| {
        fx_log_debug!("new_state: {:?}", state);

        // TODO(fxbug.dev/116283): Add something meaningful into this VMO.
        let vmo = zx::Vmo::create(state.into_primitive() as u64).unwrap();

        if let Err(e) = responder.send(vmo) {
            fx_log_err!(
                "fuchsia.input.wayland/Keymap.Watch: can not send update: {:?}, {:?}",
                state,
                &e
            );
            false
        } else {
            true
        }
    })
}

impl WaylandHandler {
    /// Creates a new instance of [WaylandHandler].
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            hanging_get: RefCell::new(KeymapGet::new(KeymapId::UsQwerty, keymap_update_fn())),
        })
    }

    /// Monitors the keymap for each passing keyboard event.
    fn process_keyboard_event(
        self: &Rc<Self>,
        event: KeyboardEvent,
        device_descriptor: InputDeviceDescriptor,
        event_time: zx::Time,
        trace_id: Option<fuchsia_trace::Id>,
    ) -> UnhandledInputEvent {
        let keymap_id = keymaps::into_keymap_id(&event.get_keymap().unwrap_or("".into()));
        self.update_keymap(keymap_id);
        UnhandledInputEvent {
            device_event: InputDeviceEvent::Keyboard(event),
            device_descriptor,
            event_time,
            trace_id,
        }
    }

    /// Set the keymap value to `keymap_id`.
    pub(crate) fn update_keymap(self: &Rc<Self>, keymap_id: KeymapId) {
        self.hanging_get.borrow().new_publisher().set(keymap_id);
    }

    /// Returns a function which allows responding to a call to
    /// `fuchsia.input.wayland/Keymap.Watch` as a hanging get call.
    ///
    /// The return value of this function *MUST* be submitted to a task to have
    /// it actually execute.
    #[must_use]
    pub fn handle_keymap_watch_stream_fn(
        self: &Rc<Self>,
        mut stream: KeymapRequestStream,
    ) -> impl futures::Future<Output = Result<()>> {
        let subscriber = self.hanging_get.borrow_mut().new_subscriber();

        async move {
            while let Some(request_result) = stream.next().await {
                let watcher = request_result?
                    .into_watch()
                    .ok_or(anyhow!("could not watch: fuchsia.input.wayland/Keymap.Watch"))?;
                subscriber.register(watcher)?;
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints;
    use fidl_fuchsia_input::Key;
    use fidl_fuchsia_input_wayland::KeymapMarker;
    use fidl_fuchsia_ui_input3::KeyEventType;
    use fuchsia_async as fasync;

    fn get_unhandled_input_event(event: KeyboardEvent) -> UnhandledInputEvent {
        UnhandledInputEvent {
            device_event: InputDeviceEvent::Keyboard(event),
            event_time: zx::Time::from_nanos(42),
            device_descriptor: InputDeviceDescriptor::Fake,
            trace_id: None,
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn get_initial() {
        let (proxy, stream) = endpoints::create_proxy_and_stream::<KeymapMarker>().unwrap();
        let handler = WaylandHandler::new();
        let _task = fasync::Task::local(handler.handle_keymap_watch_stream_fn(stream));

        let keymap_vmo: zx::Vmo = proxy.watch().await.unwrap();
        assert_eq!(
            keymap_vmo.get_content_size().unwrap(),
            KeymapId::UsQwerty.into_primitive() as u64
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn get_modified() {
        let (proxy, stream) = endpoints::create_proxy_and_stream::<KeymapMarker>().unwrap();
        let handler = WaylandHandler::new();
        let _task = fasync::Task::local(handler.handle_keymap_watch_stream_fn(stream));

        let event = get_unhandled_input_event(
            KeyboardEvent::new(Key::A, KeyEventType::Pressed)
                .into_with_keymap(Some("FR_AZERTY".into())),
        );

        let result = handler.clone().handle_unhandled_input_event(event).await;
        assert_eq!(result.len(), 1);

        let keymap_vmo: zx::Vmo = proxy.watch().await.unwrap();
        assert_eq!(
            keymap_vmo.get_content_size().unwrap(),
            KeymapId::FrAzerty.into_primitive() as u64
        );
    }
}
