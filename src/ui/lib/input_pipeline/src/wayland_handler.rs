// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    input_device::{InputDeviceDescriptor, InputDeviceEvent, InputEvent, UnhandledInputEvent},
    input_handler::{InputHandlerStatus, UnhandledInputHandler},
    keyboard_binding::KeyboardEvent,
};
use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_input::KeymapId;
use fidl_fuchsia_input_wayland::{KeymapRequestStream, KeymapWatchResponder};
use fuchsia_inspect;
use fuchsia_trace;
use fuchsia_zircon as zx;
use futures::StreamExt;
use keymaps;
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

pub type NotifyFn = Box<dyn Fn(&KeymapId, KeymapWatchResponder) -> bool>;
pub type KeymapGet = HangingGet<KeymapId, KeymapWatchResponder, NotifyFn>;

/// Serves `fuchsia.input.wayland/Keymap`.
pub struct WaylandHandler {
    /// The last known used keymap.
    keymap_id: RefCell<KeymapId>,
    /// Hanging get handler for propagating client keymap requests.
    hanging_get: RefCell<KeymapGet>,
    /// The inventory of this handler's Inspect status.
    _inspect_status: InputHandlerStatus,
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

/// A function that maybe produces new VMOs on each call.
type VmoFactory = Box<dyn Fn(&KeymapId) -> Result<zx::Vmo>>;

/// Produces a function that gets the VMO content from a file.
fn default_vmo_factory() -> VmoFactory {
    Box::new(move |keymap_id: &KeymapId| {
        let filename = match keymap_id {
            KeymapId::UsQwerty => "/pkg/data/keymap.xkb",
            // This will be different files once we have multiple keymaps.
            _ => "/pkg/data/keymap.xkb",
        };
        let mut file = std::fs::File::open(filename)
            .with_context(|| format!("while opening: {:?}", filename))?;
        let keymap_len = file
            .metadata()
            .with_context(|| format!("while getting metadata for: {:?}", filename))?
            .len();

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).with_context(|| format!("while reading: {:?}", filename))?;
        let vmo = zx::Vmo::create(keymap_len)
            .with_context(|| format!("while creating VMO from: {:?}", filename))?;
        vmo.write(&buffer, 0)?;
        Ok(vmo)
    })
}

fn keymap_update_fn(vmo_factory: VmoFactory) -> NotifyFn {
    Box::new(move |state: &KeymapId, responder: KeymapWatchResponder| {
        tracing::debug!("new_state: {:?}", state);

        let vmo = match vmo_factory(state) {
            Err(e) => {
                tracing::error!("could not load keymap: {:?}", &e);
                return false;
            }
            Ok(vmo) => vmo,
        };

        if let Err(e) = responder.send(vmo) {
            tracing::error!(
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
    pub fn new(input_handlers_node: &fuchsia_inspect::Node) -> Rc<Self> {
        WaylandHandler::new_with_get_vmo_factory(default_vmo_factory(), input_handlers_node)
    }

    fn new_with_get_vmo_factory(
        vmo_factory: VmoFactory,
        input_handlers_node: &fuchsia_inspect::Node,
    ) -> Rc<Self> {
        let inspect_status = InputHandlerStatus::new(
            input_handlers_node,
            "wayland_handler",
            /* generates_events */ false,
        );
        Rc::new(Self {
            keymap_id: RefCell::new(KeymapId::UsQwerty),
            hanging_get: RefCell::new(KeymapGet::new(
                KeymapId::UsQwerty,
                keymap_update_fn(vmo_factory),
            )),
            _inspect_status: inspect_status,
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
        if keymap_id != *self.keymap_id.borrow() {
            self.update_keymap(keymap_id);
        }
        UnhandledInputEvent {
            device_event: InputDeviceEvent::Keyboard(event),
            device_descriptor,
            event_time,
            trace_id,
        }
    }

    /// Set the keymap value to `keymap_id`.
    pub(crate) fn update_keymap(self: &Rc<Self>, keymap_id: KeymapId) {
        *self.keymap_id.borrow_mut() = keymap_id;
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
    use fuchsia_inspect;

    fn get_unhandled_input_event(event: KeyboardEvent) -> UnhandledInputEvent {
        UnhandledInputEvent {
            device_event: InputDeviceEvent::Keyboard(event),
            event_time: zx::Time::from_nanos(42),
            device_descriptor: InputDeviceDescriptor::Fake,
            trace_id: None,
        }
    }

    /// Produces a VMO which is sized according to the passed in state, as a
    /// fake for tests.
    fn fake_vmo_factory() -> VmoFactory {
        Box::new(|state: &KeymapId| {
            zx::Vmo::create(state.into_primitive() as u64).map_err(|e| e.into())
        })
    }

    const KEYMAP_XKB_SIZE_BYTES: u64 = 43264;

    // Tests actual loading of an existing keymap file. Loads it from storage, and
    // compares VMO size to the file's known good size. This is a bit brittle, but
    // keymap files seldom change. Assuming that's the case, checking the size is
    // good enough.
    #[fasync::run_singlethreaded(test)]
    async fn get_magic_initial() {
        let (proxy, stream) = endpoints::create_proxy_and_stream::<KeymapMarker>().unwrap();
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");
        let handler = WaylandHandler::new(&test_node);
        let _task = fasync::Task::local(handler.handle_keymap_watch_stream_fn(stream));

        let keymap_vmo: zx::Vmo = proxy.watch().await.unwrap();
        assert_eq!(keymap_vmo.get_content_size().unwrap(), KEYMAP_XKB_SIZE_BYTES);
    }

    // Tests initial hanging get notification by installing a fake update fn that
    // will produce fake VMOs with sizes that correspond to the KeymapId enum values.
    #[fasync::run_singlethreaded(test)]
    async fn get_initial() {
        let (proxy, stream) = endpoints::create_proxy_and_stream::<KeymapMarker>().unwrap();
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");
        let handler = WaylandHandler::new_with_get_vmo_factory(fake_vmo_factory(), &test_node);
        let _task = fasync::Task::local(handler.handle_keymap_watch_stream_fn(stream));

        let keymap_vmo: zx::Vmo = proxy.watch().await.unwrap();
        assert_eq!(
            keymap_vmo.get_content_size().unwrap(),
            KeymapId::UsQwerty.into_primitive() as u64
        );
    }

    // Same as above, except we force an update and test that our listener received the
    // update.
    #[fasync::run_singlethreaded(test)]
    async fn get_modified() {
        let (proxy, stream) = endpoints::create_proxy_and_stream::<KeymapMarker>().unwrap();
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");
        let handler = WaylandHandler::new_with_get_vmo_factory(fake_vmo_factory(), &test_node);
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

    #[fuchsia::test]
    fn wayland_handler_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let fake_handlers_node = inspector.root().create_child("input_handlers_node");
        let _handler = WaylandHandler::new(&fake_handlers_node);
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_handlers_node: {
                wayland_handler: {
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
}
