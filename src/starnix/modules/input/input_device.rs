// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::input_event_relay::{DeviceId, EventProxyMode, OpenedFiles};
use crate::{parse_fidl_keyboard_event_to_linux_input_event, InputFile};
use fidl::endpoints::RequestStream;
use fidl_fuchsia_ui_input::MediaButtonsEvent;
use futures::StreamExt as _;
use starnix_core::device::kobject::DeviceMetadata;
use starnix_core::device::{DeviceMode, DeviceOps};
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::power::{clear_wake_proxy_signal, create_proxy_for_wake_events};
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::{FileOps, FsNode, FsString};
use starnix_logging::log_warn;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::device_type::{DeviceType, INPUT_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::time::timeval_from_time;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{input_id, BUS_VIRTUAL};
use std::sync::Arc;
use {
    fidl_fuchsia_ui_input3 as fuiinput, fidl_fuchsia_ui_policy as fuipolicy,
    fidl_fuchsia_ui_views as fuiviews, fuchsia_async as fasync, starnix_uapi as uapi, zx,
};

// Add a fuchsia-specific vendor ID. 0xf1ca is currently not allocated
// to any vendor in the USB spec.
//
// May not be zero, see below.
const FUCHSIA_VENDOR_ID: u16 = 0xfc1a;

// May not be zero, see below.
const FUCHSIA_TOUCH_PRODUCT_ID: u16 = 0x2;

// May not be zero, see below.
const FUCHSIA_KEYBOARD_PRODUCT_ID: u16 = 0x1;

// Touch and keyboard input IDs should be distinct.
// Per https://www.linuxjournal.com/article/6429, the bus type should be populated with a
// sensible value, but other fields may not be.
//
// While this may be the case for Linux itself, Android is not so relaxed.
// Devices with apparently-invalid vendor or product IDs don't get extra
// device configuration.  So we must make a minimum effort to present
// sensibly-looking product and vendor IDs.  Zero version only means that
// version-specific config files will not be applied.
//
// For background, see:
//
// * Allowable file locations:
//   https://source.android.com/docs/core/interaction/input/input-device-configuration-files#location
// * Android configuration selection code:
//   https://source.corp.google.com/h/googleplex-android/platform/superproject/main/+/main:frameworks/native/libs/input/InputDevice.cpp;l=60;drc=285211e60bff87fc5a9c9b4105a4b4ccb7edffaf
const TOUCH_INPUT_ID: input_id = input_id {
    bustype: BUS_VIRTUAL as u16,
    // Make sure that vendor ID and product ID at least seem plausible.  See
    // above for details.
    vendor: FUCHSIA_VENDOR_ID,
    product: FUCHSIA_TOUCH_PRODUCT_ID,
    // Version is OK to be zero, but config files named `Product_yyyy_Vendor_zzzz_Version_ttt.*`
    // will not work.
    version: 0,
};
const KEYBOARD_INPUT_ID: input_id = input_id {
    bustype: BUS_VIRTUAL as u16,
    // Make sure that vendor ID and product ID at least seem plausible.  See
    // above for details.
    vendor: FUCHSIA_VENDOR_ID,
    product: FUCHSIA_KEYBOARD_PRODUCT_ID,
    version: 1,
};

#[derive(Clone)]
enum InputDeviceType {
    // A touch device, containing (display width, display height).
    Touch(i32, i32),

    // A keyboard device.
    Keyboard,
}

#[derive(Clone)]
pub struct InputDevice {
    device_type: InputDeviceType,

    pub open_files: OpenedFiles,

    input_files_node: Arc<fuchsia_inspect::Node>,
}

impl InputDevice {
    pub fn new_touch(
        display_width: i32,
        display_height: i32,
        inspect_node: &fuchsia_inspect::Node,
    ) -> Arc<Self> {
        let input_files_node = Arc::new(inspect_node.create_child("touch_device"));
        Arc::new(InputDevice {
            device_type: InputDeviceType::Touch(display_width, display_height),
            open_files: Default::default(),
            input_files_node,
        })
    }

    pub fn new_keyboard(inspect_node: &fuchsia_inspect::Node) -> Arc<Self> {
        let input_files_node = Arc::new(inspect_node.create_child("keyboard_device"));
        Arc::new(InputDevice {
            device_type: InputDeviceType::Keyboard,
            open_files: Default::default(),
            input_files_node,
        })
    }

    pub fn register<L>(
        self: Arc<Self>,
        locked: &mut Locked<'_, L>,
        system_task: &CurrentTask,
        device_id: DeviceId,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        let kernel = system_task.kernel();
        let registry = &kernel.device_registry;

        let input_class = registry.objects.input_class();
        registry.register_device(
            locked,
            system_task,
            FsString::from(format!("event{}", device_id)).as_ref(),
            DeviceMetadata::new(
                format!("input/event{}", device_id).into(),
                DeviceType::new(INPUT_MAJOR, device_id),
                DeviceMode::Char,
            ),
            input_class,
            DeviceDirectory::new,
            self,
        );
    }

    pub fn start_keyboard_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        keyboard: fuiinput::KeyboardSynchronousProxy,
        view_ref: fuiviews::ViewRef,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let (keyboard_listener, mut event_stream) =
                    fidl::endpoints::create_request_stream::<fuiinput::KeyboardListenerMarker>()
                        .expect("Failed to create keyboard proxy and stream");
                if keyboard
                    .add_listener(view_ref, keyboard_listener, zx::MonotonicInstant::INFINITE)
                    .is_err()
                {
                    log_warn!("Could not register keyboard listener");
                }
                while let Some(Ok(request)) = event_stream.next().await {
                    match request {
                        fuiinput::KeyboardListenerRequest::OnKeyEvent { event, responder } => {
                            let new_events = parse_fidl_keyboard_event_to_linux_input_event(&event);
                            slf.open_files.lock().retain(|f| {
                                let Some(file) = f.upgrade() else {
                                    return false;
                                };

                                let mut inner = file.inner.lock();

                                if !new_events.is_empty() {
                                    inner.events.extend(new_events.clone());
                                    inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                }

                                return true;
                            });
                            responder.send(fuiinput::KeyEventStatus::Handled).expect("");
                        }
                    }
                }
            })
        });
    }

    pub fn start_button_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        registry_proxy: fuipolicy::DeviceListenerRegistrySynchronousProxy,
        event_proxy_mode: EventProxyMode,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let (remote_client, remote_server) =
                    fidl::endpoints::create_endpoints::<fuipolicy::MediaButtonsListenerMarker>();
                if let Err(e) =
                    registry_proxy.register_listener(remote_client, zx::MonotonicInstant::INFINITE)
                {
                    log_warn!("Failed to register media buttons listener: {:?}", e);
                    return;
                }

                let (local_listener_stream, local_resume_event) = match event_proxy_mode {
                    EventProxyMode::WakeContainer => {
                        let (local_channel, local_resume_event) =
                            create_proxy_for_wake_events(remote_server.into_channel());
                        let local_listener_stream =
                            fuipolicy::MediaButtonsListenerRequestStream::from_channel(
                                fidl::AsyncChannel::from_channel(local_channel),
                            );
                        (local_listener_stream, Some(local_resume_event))
                    }
                    EventProxyMode::None => {
                        (remote_server.into_stream().expect("Failed to create event stream"), None)
                    }
                };
                slf.button_relay_loop(local_listener_stream, local_resume_event).await;
            })
        });
    }

    async fn button_relay_loop(
        &self,
        mut local_listener_stream: fuipolicy::MediaButtonsListenerRequestStream,
        local_resume_event: Option<zx::EventPair>,
    ) {
        let mut power_was_pressed = false;
        let mut function_was_pressed = false;

        loop {
            let next_event_future = local_listener_stream.next();

            local_resume_event.as_ref().map(clear_wake_proxy_signal);

            match next_event_future.await {
                Some(Ok(fuipolicy::MediaButtonsListenerRequest::OnEvent { event, responder })) => {
                    let (new_events, power_is_pressed, function_is_pressed) =
                        parse_fidl_button_event(&event, power_was_pressed, function_was_pressed);
                    power_was_pressed = power_is_pressed;
                    function_was_pressed = function_is_pressed;

                    let (converted_events, ignored_events, generated_events) = match new_events
                        .len()
                    {
                        0 => (0u64, 1u64, 0u64),
                        len => {
                            if len % 2 == 1 {
                                log_warn!("unexpectedly received {} events: there should always be an even number of non-empty events.", len);
                            }
                            (1u64, 0u64, len as u64)
                        }
                    };

                    self.open_files.lock().retain(|f| {
                        let Some(file) = f.upgrade() else {
                            return false;
                        };

                        let mut inner = file.inner.lock();
                        match &inner.inspect_status {
                            Some(inspect_status) => {
                                inspect_status.count_received_events(1);
                                inspect_status.count_ignored_events(ignored_events);
                                inspect_status.count_converted_events(converted_events);
                                inspect_status.count_generated_events(generated_events);
                            }
                            None => (),
                        }

                        if !new_events.is_empty() {
                            inner.events.extend(new_events.clone());
                            inner.waiters.notify_fd_events(FdEvents::POLLIN);
                        }

                        return true;
                    });

                    responder.send().expect("media buttons responder failed to respond");
                }
                Some(Ok(_)) => { /* Ignore deprecated OnMediaButtonsEvent */ }
                Some(Err(e)) => {
                    log_warn!("Received an error while listening for events on MediaButtonsListener: {:?}", e);
                    break;
                }
                None => {
                    break;
                }
            }
        }

        log_warn!("MediaButtonsListener request stream has ended");
    }

    #[cfg(test)]
    pub fn open_test(
        &self,
        current_task: &CurrentTask,
    ) -> Result<starnix_core::vfs::FileHandle, Errno> {
        let input_file = match self.device_type {
            InputDeviceType::Touch(display_width, display_height) => {
                let file = Arc::new(InputFile::new_touch(
                    TOUCH_INPUT_ID,
                    display_width,
                    display_height,
                    Some(self.input_files_node.create_child("touch_file")),
                ));
                file
            }
            InputDeviceType::Keyboard => Arc::new(InputFile::new_keyboard(
                KEYBOARD_INPUT_ID,
                Some(self.input_files_node.create_child("keyboard_file")),
            )),
        };
        self.open_files.lock().push(Arc::downgrade(&input_file));
        let file_object = starnix_core::vfs::FileObject::new(
            Box::new(input_file),
            current_task
                .lookup_path_from_root(".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");
        Ok(file_object)
    }
}

impl DeviceOps for InputDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let input_file = match self.device_type {
            InputDeviceType::Touch(display_width, display_height) => {
                let file = Arc::new(InputFile::new_touch(
                    TOUCH_INPUT_ID,
                    display_width,
                    display_height,
                    Some(self.input_files_node.create_child("touch_file")),
                ));
                file
            }
            InputDeviceType::Keyboard => Arc::new(InputFile::new_keyboard(
                KEYBOARD_INPUT_ID,
                Some(self.input_files_node.create_child("keyboard_file")),
            )),
        };
        self.open_files.lock().push(Arc::downgrade(&input_file));
        Ok(Box::new(input_file))
    }
}

fn parse_fidl_button_event(
    fidl_event: &MediaButtonsEvent,
    power_was_pressed: bool,
    function_was_pressed: bool,
) -> (Vec<uapi::input_event>, bool /* power_is_pressed */, bool /* function_is_pressed */) {
    let time = timeval_from_time(zx::MonotonicInstant::get());
    let mut events = vec![];
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };

    let power_is_pressed = fidl_event.power.unwrap_or(false);
    let function_is_pressed = fidl_event.function.unwrap_or(false);
    for (then, now, key_code) in [
        (power_was_pressed, power_is_pressed, uapi::KEY_POWER),
        (function_was_pressed, function_is_pressed, uapi::KEY_SCREENSAVER),
    ] {
        // Button state changed. Send an event.
        if then != now {
            events.push(uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: key_code as u16,
                value: now as i32,
            });
            events.push(sync_event);
        }
    }

    (events, power_is_pressed, function_is_pressed)
}

#[cfg(test)]
mod test {
    #![allow(clippy::unused_unit)] // for compatibility with `test_case`

    use super::*;
    use crate::input_event_relay;
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use fuipointer::{
        EventPhase, TouchEvent, TouchInteractionId, TouchPointerSample, TouchResponse,
        TouchSourceMarker, TouchSourceRequest,
    };
    use pretty_assertions::assert_eq;
    use starnix_core::task::{EventHandler, Waiter};
    use starnix_core::testing::{create_kernel_and_task, create_kernel_task_and_unlocked};
    use starnix_core::vfs::buffers::VecOutputBuffer;
    use starnix_core::vfs::FileHandle;
    use starnix_uapi::errors::EAGAIN;
    use test_case::test_case;
    use test_util::assert_near;
    use zerocopy::FromBytes as _;
    use {fidl_fuchsia_ui_pointer as fuipointer, fidl_fuchsia_ui_policy as fuipolicy, zx}; // for `read_from()`

    const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

    fn start_touch_input(
        current_task: &CurrentTask,
    ) -> (Arc<InputDevice>, FileHandle, fuipointer::TouchSourceRequestStream) {
        let inspector = fuchsia_inspect::Inspector::default();
        start_touch_input_inspect_and_dimensions(current_task, 700, 1200, &inspector)
    }

    fn start_touch_input_inspect(
        current_task: &CurrentTask,
        inspector: &fuchsia_inspect::Inspector,
    ) -> (Arc<InputDevice>, FileHandle, fuipointer::TouchSourceRequestStream) {
        start_touch_input_inspect_and_dimensions(current_task, 700, 1200, &inspector)
    }

    fn start_touch_input_inspect_and_dimensions(
        current_task: &CurrentTask,
        x_max: i32,
        y_max: i32,
        inspector: &fuchsia_inspect::Inspector,
    ) -> (Arc<InputDevice>, FileHandle, fuipointer::TouchSourceRequestStream) {
        let input_device = InputDevice::new_touch(x_max, y_max, inspector.root());
        let input_file = input_device.open_test(current_task).expect("Failed to create input file");

        let (touch_source_client_end, touch_source_stream) =
            fidl::endpoints::create_request_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");

        let relay = input_event_relay::InputEventsRelay::new();
        relay.start_relays(
            &current_task.kernel(),
            EventProxyMode::None,
            touch_source_client_end,
            input_device.open_files.clone(),
        );

        (input_device, input_file, touch_source_stream)
    }

    fn start_keyboard_input(
        current_task: &CurrentTask,
    ) -> (Arc<InputDevice>, FileHandle, fuiinput::KeyboardRequestStream) {
        let inspector = fuchsia_inspect::Inspector::default();
        let input_device = InputDevice::new_keyboard(inspector.root());
        let input_file = input_device.open_test(current_task).expect("Failed to create input file");
        let (keyboard_proxy, keyboard_stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<fuiinput::KeyboardMarker>()
                .expect("failed to create Keyboard channel");
        let view_ref_pair =
            fuchsia_scenic::ViewRefPair::new().expect("Failed to create ViewRefPair");

        input_device.start_keyboard_relay(
            current_task.kernel(),
            keyboard_proxy,
            view_ref_pair.view_ref,
        );

        (input_device, input_file, keyboard_stream)
    }

    fn start_button_input(
        current_task: &CurrentTask,
    ) -> (Arc<InputDevice>, FileHandle, fuipolicy::DeviceListenerRegistryRequestStream) {
        let inspector = fuchsia_inspect::Inspector::default();
        start_button_input_inspect(current_task, &inspector)
    }

    fn start_button_input_inspect(
        current_task: &CurrentTask,
        inspector: &fuchsia_inspect::Inspector,
    ) -> (Arc<InputDevice>, FileHandle, fuipolicy::DeviceListenerRegistryRequestStream) {
        let input_device = InputDevice::new_keyboard(inspector.root());
        let input_file = input_device.open_test(current_task).expect("Failed to create input file");
        let (device_registry_proxy, device_listener_stream) =
        fidl::endpoints::create_sync_proxy_and_stream::<fuipolicy::DeviceListenerRegistryMarker>()
            .expect("Failed to create DeviceListenerRegistry proxy and stream.");
        input_device.start_button_relay(
            current_task.kernel(),
            device_registry_proxy,
            EventProxyMode::None,
        );
        (input_device, input_file, device_listener_stream)
    }

    fn make_touch_event(pointer_id: u32) -> fuipointer::TouchEvent {
        // Default to `Change`, because that has the fewest side effects.
        make_touch_event_with_phase(EventPhase::Change, pointer_id)
    }

    fn make_touch_event_with_phase(phase: EventPhase, pointer_id: u32) -> fuipointer::TouchEvent {
        make_touch_event_with_coords_phase(0.0, 0.0, phase, pointer_id)
    }

    fn make_touch_event_with_coords_phase(
        x: f32,
        y: f32,
        phase: EventPhase,
        pointer_id: u32,
    ) -> fuipointer::TouchEvent {
        make_touch_event_with_coords_phase_timestamp(x, y, phase, pointer_id, 0)
    }

    fn make_touch_event_with_coords(x: f32, y: f32, pointer_id: u32) -> fuipointer::TouchEvent {
        make_touch_event_with_coords_phase(x, y, EventPhase::Change, pointer_id)
    }

    fn make_touch_event_with_coords_phase_timestamp(
        x: f32,
        y: f32,
        phase: EventPhase,
        pointer_id: u32,
        time_nanos: i64,
    ) -> fuipointer::TouchEvent {
        make_touch_event_with_coords_phase_timestamp_device_id(
            x, y, phase, pointer_id, time_nanos, 0,
        )
    }

    fn make_empty_touch_event() -> fuipointer::TouchEvent {
        TouchEvent {
            pointer_sample: Some(TouchPointerSample {
                interaction: Some(TouchInteractionId {
                    pointer_id: 0,
                    device_id: 0,
                    interaction_id: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_event_with_coords_phase_timestamp_device_id(
        x: f32,
        y: f32,
        phase: EventPhase,
        pointer_id: u32,
        time_nanos: i64,
        device_id: u32,
    ) -> fuipointer::TouchEvent {
        TouchEvent {
            timestamp: Some(time_nanos),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([x, y]),
                // Default to `Change`, because that has the fewest side effects.
                phase: Some(phase),
                interaction: Some(TouchInteractionId { pointer_id, device_id, interaction_id: 0 }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn read_uapi_events<L>(
        locked: &mut Locked<'_, L>,
        file: &FileHandle,
        current_task: &CurrentTask,
    ) -> Vec<uapi::input_event>
    where
        L: LockBefore<FileOpsCore>,
    {
        std::iter::from_fn(|| {
            let mut locked = locked.cast_locked::<FileOpsCore>();
            let mut event_bytes = VecOutputBuffer::new(INPUT_EVENT_SIZE);
            match file.read(&mut locked, current_task, &mut event_bytes) {
                Ok(INPUT_EVENT_SIZE) => Some(
                    uapi::input_event::read_from_bytes(Vec::from(event_bytes).as_slice())
                        .map_err(|_| anyhow!("failed to read input_event from buffer")),
                ),
                Ok(other_size) => {
                    Some(Err(anyhow!("got {} bytes (expected {})", other_size, INPUT_EVENT_SIZE)))
                }
                Err(Errno { code: EAGAIN, .. }) => None,
                Err(other_error) => Some(Err(anyhow!("read failed: {:?}", other_error))),
            }
        })
        .enumerate()
        .map(|(i, read_res)| match read_res {
            Ok(event) => event,
            Err(e) => panic!("unexpected result {:?} on iteration {}", e, i),
        })
        .collect()
    }

    // Waits for a `Watch()` request to arrive on `request_stream`, and responds with
    // `touch_event`. Returns the arguments to the `Watch()` call.
    async fn answer_next_watch_request(
        request_stream: &mut fuipointer::TouchSourceRequestStream,
        touch_events: Vec<TouchEvent>,
    ) -> Vec<TouchResponse> {
        match request_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                responder.send(&touch_events).expect("failure sending Watch reply");
                responses
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }
    }

    #[::fuchsia::test()]
    async fn initial_watch_request_has_empty_responses_arg() {
        let (_kernel, current_task) = create_kernel_and_task();
        // Set up resources.
        let (_input_device, _input_file, mut touch_source_stream) =
            start_touch_input(&current_task);

        // Verify that the watch request has empty `responses`.
        assert_matches!(
            touch_source_stream.next().await,
            Some(Ok(TouchSourceRequest::Watch { responses, .. }))
                => assert_eq!(responses.as_slice(), [])
        );
    }

    #[::fuchsia::test]
    async fn later_watch_requests_have_responses_arg_matching_earlier_watch_replies() {
        // Set up resources.
        let (_kernel, current_task) = create_kernel_and_task();
        let (_input_device, _input_file, mut touch_source_stream) =
            start_touch_input(&current_task);

        // Reply to first `Watch` with two `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => responder
                .send(&vec![TouchEvent::default(); 2])
                .expect("failure sending Watch reply"),
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Verify second `Watch` has two elements in `responses`.
        // Then reply with five `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                assert_matches!(responses.as_slice(), [_, _]);
                responder
                    .send(&vec![TouchEvent::default(); 5])
                    .expect("failure sending Watch reply")
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Verify third `Watch` has five elements in `responses`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_, _, _, _, _]);
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }
    }

    #[::fuchsia::test]
    async fn notifies_polling_waiters_of_new_data() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().for_each(|waiter| {
            input_file.wait_async(
                &mut locked,
                &current_task,
                waiter,
                FdEvents::POLLIN,
                EventHandler::None,
            );
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;
        answer_next_watch_request(&mut touch_source_stream, vec![make_touch_event(1)]).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_eq!(waiter1.wait_until(&current_task, zx::MonotonicInstant::ZERO), Ok(()));
        assert_eq!(waiter2.wait_until(&current_task, zx::MonotonicInstant::ZERO), Ok(()));
    }

    #[::fuchsia::test]
    async fn notifies_blocked_waiter_of_new_data() {
        // Set up resources.
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter = Waiter::new();

        // Ask `input_file` to notify `waiter` when data is available to read.
        input_file.wait_async(
            &mut locked,
            &current_task,
            &waiter,
            FdEvents::POLLIN,
            EventHandler::None,
        );

        let mut waiter_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |_, task| waiter.wait(&task));
        assert!(futures::poll!(&mut waiter_thread).is_pending());

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch`.
        //
        // TODO(https://fxbug.dev/42075452): Without this, `relay_thread` gets stuck `await`-ing
        // the reply to its first request. Figure out why that happens, and remove this second
        // reply.
        answer_next_watch_request(&mut touch_source_stream, vec![make_touch_event(1)]).await;
    }

    #[::fuchsia::test]
    async fn does_not_notify_polling_waiters_without_new_data() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().for_each(|waiter| {
            input_file.wait_async(
                &mut locked,
                &current_task,
                waiter,
                FdEvents::POLLIN,
                EventHandler::None,
            );
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));

        // Reply to first `Watch` request with an empty set of events.
        answer_next_watch_request(&mut touch_source_stream, vec![]).await;

        // `InputFile` should be done processing the first reply. Since there
        // were no touch_events given, `InputFile` should not have notified the
        // interested waiters.
        assert_matches!(waiter1.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));
    }

    // Note: a user program may also want to be woken if events were already ready at the
    // time that the program called `epoll_wait()`. However, there's no test for that case
    // in this module, because:
    //
    // 1. Not all programs will want to be woken in such a case. In particular, some programs
    //    use "edge-triggered" mode instead of "level-tiggered" mode. For details on the
    //    two modes, see https://man7.org/linux/man-pages/man7/epoll.7.html.
    // 2. For programs using "level-triggered" mode, the relevant behavior is implemented in
    //    the `epoll` module, and verified by `epoll::tests::test_epoll_ready_then_wait()`.
    //
    // See also: the documentation for `FileOps::wait_async()`.

    #[::fuchsia::test]
    async fn honors_wait_cancellation() {
        // Set up input resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        // Ask `input_file` to notify `waiter` when data is available to read.
        let waitkeys = [&waiter1, &waiter2]
            .iter()
            .map(|waiter| {
                input_file
                    .wait_async(
                        &mut locked,
                        &current_task,
                        waiter,
                        FdEvents::POLLIN,
                        EventHandler::None,
                    )
                    .expect("wait_async")
            })
            .collect::<Vec<_>>();

        // Cancel wait for `waiter1`.
        waitkeys.into_iter().next().expect("failed to get first waitkey").cancel();

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;
        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![make_touch_event(1)]).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_matches!(waiter1.wait_until(&current_task, zx::MonotonicInstant::ZERO), Err(_));
        assert_eq!(waiter2.wait_until(&current_task, zx::MonotonicInstant::ZERO), Ok(()));
    }

    #[::fuchsia::test]
    async fn query_events() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Check initial expectation.
        assert_eq!(
            input_file.query_events(&mut locked, &current_task).expect("query_events"),
            FdEvents::empty(),
            "events should be empty before data arrives"
        );

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![make_touch_event(1)]).await;

        // Check post-watch expectation.
        assert_eq!(
            input_file.query_events(&mut locked, &current_task).expect("query_events"),
            FdEvents::POLLIN | FdEvents::POLLRDNORM,
            "events should be POLLIN after data arrives"
        );
    }

    fn make_uapi_input_event(ty: u32, code: u32, value: i32) -> uapi::input_event {
        make_uapi_input_event_with_timestamp(ty, code, value, 0)
    }

    fn make_uapi_input_event_with_timestamp(
        ty: u32,
        code: u32,
        value: i32,
        time_nanos: i64,
    ) -> uapi::input_event {
        uapi::input_event {
            time: timeval_from_time(zx::MonotonicInstant::from_nanos(time_nanos)),
            type_: ty as u16,
            code: code as u16,
            value,
        }
    }

    #[::fuchsia::test]
    async fn touch_event_ignored() {
        // Set up resources.
        let inspector = fuchsia_inspect::Inspector::default();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) =
            start_touch_input_inspect(&current_task, &inspector);

        // Touch add for pointer 1. This should be counted as a received event and a converted
        // event. It should also yield 6 generated events.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s. This should be counted as a received event and an ignored event.
        answer_next_watch_request(&mut touch_source_stream, vec![make_empty_touch_event()]).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(events.len(), 6);

        // Reply to `Watch` request of empty event. This should be counted as a received event and
        // an ignored event.
        answer_next_watch_request(&mut touch_source_stream, vec![make_empty_touch_event()]).await;

        // Wait for another `Watch`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_])
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events, vec![]);
        assert_data_tree!(inspector, root: {
            touch_device: {
                touch_file: {
                    fidl_events_received_count: 3u64,
                    fidl_events_ignored_count: 2u64,
                    fidl_events_unexpected_count: 0u64,
                    fidl_events_converted_count: 1u64,
                    uapi_events_generated_count: 6u64,
                    uapi_events_read_count: 6u64,
                },
            }
        });
    }

    #[test_case(make_touch_event_with_phase(EventPhase::Add, 1); "touch add for pointer already added")]
    #[test_case(make_touch_event_with_phase(EventPhase::Change, 2); "touch change for pointer not added")]
    #[test_case(make_touch_event_with_phase(EventPhase::Remove, 2); "touch remove for pointer not added")]
    #[test_case(make_touch_event_with_phase(EventPhase::Cancel, 1); "touch cancel")]
    #[::fuchsia::test]
    async fn touch_event_unexpected(event: TouchEvent) {
        // Set up resources.
        let inspector = fuchsia_inspect::Inspector::default();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) =
            start_touch_input_inspect(&current_task, &inspector);

        // Touch add for pointer 1. This should be counted as a received event and a converted
        // event. It should also yield 6 generated events.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s. This should be counted as a received event and an ignored event.
        answer_next_watch_request(&mut touch_source_stream, vec![make_empty_touch_event()]).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(events.len(), 6);

        // Reply to `Watch` request of given event. This should be counted as a received event and
        // an unexpected event.
        answer_next_watch_request(&mut touch_source_stream, vec![event]).await;

        // Wait for another `Watch`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_])
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events, vec![]);
        assert_data_tree!(inspector, root: {
            touch_device: {
                touch_file: {
                    fidl_events_received_count: 3u64,
                    fidl_events_ignored_count: 1u64,
                    fidl_events_unexpected_count: 1u64,
                    fidl_events_converted_count: 1u64,
                    uapi_events_generated_count: 6u64,
                    uapi_events_read_count: 6u64,
                },
            }
        });
    }

    #[::fuchsia::test]
    async fn translates_touch_add() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add for pointer 1.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, vec![make_empty_touch_event()]).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 0),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
    }

    #[::fuchsia::test]
    async fn translates_touch_change() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add for pointer 1.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(events.len(), 6);

        // Reply to touch change.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_coords(10.0, 20.0, 1)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
    }

    #[::fuchsia::test]
    async fn translates_touch_remove() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add for pointer 1.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(events.len(), 6);

        // Reply to touch change.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Remove, 1)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
    }

    #[::fuchsia::test]
    async fn multi_touch_event_sequence() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add for pointer 1.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(events.len(), 6);

        // Touch add for pointer 2.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![
                make_touch_event_with_coords(10.0, 20.0, 1),
                make_touch_event_with_phase(EventPhase::Add, 2),
            ],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 0),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );

        // Both pointers move.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![
                make_touch_event_with_coords(11.0, 21.0, 1),
                make_touch_event_with_coords(101.0, 201.0, 2),
            ],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 11),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 21),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 101),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 201),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );

        // Pointer 1 up.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![
                make_touch_event_with_phase(EventPhase::Remove, 1),
                make_touch_event_with_coords(101.0, 201.0, 2),
            ],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 101),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 201),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );

        // Pointer 2 up.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase(EventPhase::Remove, 2)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
    }

    #[::fuchsia::test]
    async fn multi_event_sequence_unsorted_in_one_watch() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add for pointer 1.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![
                make_touch_event_with_coords_phase_timestamp(
                    10.0,
                    20.0,
                    EventPhase::Change,
                    1,
                    100,
                ),
                make_touch_event_with_coords_phase_timestamp(0.0, 0.0, EventPhase::Add, 1, 1),
            ],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_eq!(
            events,
            vec![
                make_uapi_input_event_with_timestamp(uapi::EV_KEY, uapi::BTN_TOUCH, 1, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 0, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 0, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_SYN, uapi::SYN_REPORT, 0, 1),
                make_uapi_input_event_with_timestamp(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 100),
                make_uapi_input_event_with_timestamp(
                    uapi::EV_ABS,
                    uapi::ABS_MT_POSITION_X,
                    10,
                    100
                ),
                make_uapi_input_event_with_timestamp(
                    uapi::EV_ABS,
                    uapi::ABS_MT_POSITION_Y,
                    20,
                    100
                ),
                make_uapi_input_event_with_timestamp(uapi::EV_SYN, uapi::SYN_REPORT, 0, 100),
            ]
        );
    }

    #[test_case((0.0, 0.0); "origin")]
    #[test_case((100.7, 200.7); "above midpoint")]
    #[test_case((100.3, 200.3); "below midpoint")]
    #[test_case((100.5, 200.5); "midpoint")]
    #[::fuchsia::test]
    async fn sends_acceptable_coordinates((x, y): (f32, f32)) {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Touch add.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_coords_phase(x, y, EventPhase::Add, 1)],
        )
        .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        // Check that the reported positions are within the acceptable error. The acceptable
        // error is chosen to allow either rounding or truncation.
        const ACCEPTABLE_ERROR: f32 = 1.0;
        let actual_x = events
            .iter()
            .find(|event| {
                event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_MT_POSITION_X as u16
            })
            .unwrap_or_else(|| panic!("did not find `ABS_X` event in {:?}", events))
            .value;
        let actual_y = events
            .iter()
            .find(|event| {
                event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_MT_POSITION_Y as u16
            })
            .unwrap_or_else(|| panic!("did not find `ABS_Y` event in {:?}", events))
            .value;
        assert_near!(x, actual_x as f32, ACCEPTABLE_ERROR);
        assert_near!(y, actual_y as f32, ACCEPTABLE_ERROR);
    }

    // Per the FIDL documentation for `TouchSource::Watch()`:
    //
    // > non-sample events should return an empty |TouchResponse| table to the
    // > server
    #[test_case(
        make_touch_event_with_phase(EventPhase::Add, 2)
            => matches Some(TouchResponse { response_type: Some(_), ..});
        "event_with_sample_yields_some_response_type")]
    #[test_case(
        TouchEvent::default() => matches Some(TouchResponse { response_type: None, ..});
        "event_without_sample_yields_no_response_type")]
    #[::fuchsia::test]
    async fn sends_appropriate_reply_to_touch_source_server(
        event: TouchEvent,
    ) -> Option<TouchResponse> {
        // Set up resources.
        let (_kernel, current_task, _locked) = create_kernel_task_and_unlocked();
        let (_input_device, _input_file, mut touch_source_stream) =
            start_touch_input(&current_task);

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, vec![event]).await;

        // Get response to `event`.
        let responses =
            answer_next_watch_request(&mut touch_source_stream, vec![TouchEvent::default()]).await;

        // Return the value for `test_case` to match on.
        responses.get(0).cloned()
    }

    #[test_case(fidl_fuchsia_input::Key::Escape, uapi::KEY_POWER; "Esc maps to Power")]
    #[test_case(fidl_fuchsia_input::Key::A, uapi::KEY_A; "A maps to A")]
    #[::fuchsia::test]
    async fn sends_keyboard_events(fkey: fidl_fuchsia_input::Key, lkey: u32) {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_keyboard_device, keyboard_file, mut keyboard_stream) =
            start_keyboard_input(&current_task);

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fkey),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;
        std::mem::drop(keyboard_stream); // Close Zircon channel.
        std::mem::drop(keyboard_listener); // Close Zircon channel.
        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, lkey as u16);
    }

    #[::fuchsia::test]
    async fn skips_unknown_keyboard_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_keyboard_device, keyboard_file, mut keyboard_stream) =
            start_keyboard_input(&current_task);

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fidl_fuchsia_input::Key::AcRefresh),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;
        std::mem::drop(keyboard_stream); // Close Zircon channel.
        std::mem::drop(keyboard_listener); // Close Zircon channel.
        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        assert_eq!(events.len(), 0);
    }

    #[::fuchsia::test]
    async fn sends_power_button_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut device_listener_stream) =
            start_button_input(&current_task);

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let power_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_event).await;
        std::mem::drop(device_listener_stream); // Close Zircon channel.
        std::mem::drop(buttons_listener); // Close Zircon channel.

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
        assert_eq!(events[0].value, 1);
    }

    #[::fuchsia::test]
    async fn sends_function_button_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut device_listener_stream) =
            start_button_input(&current_task);

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let function_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(false),
            function: Some(true),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&function_event).await;
        std::mem::drop(device_listener_stream); // Close Zircon channel.
        std::mem::drop(buttons_listener); // Close Zircon channel.

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, uapi::KEY_SCREENSAVER as u16);
        assert_eq!(events[0].value, 1);
    }

    #[::fuchsia::test]
    async fn sends_overlapping_button_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut device_listener_stream) =
            start_button_input(&current_task);

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let power_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            ..Default::default()
        };

        let function_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(true),
            ..Default::default()
        };

        let function_release_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            ..Default::default()
        };

        let power_release_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(false),
            function: Some(false),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_event).await;
        let _ = buttons_listener.on_event(&function_event).await;
        let _ = buttons_listener.on_event(&function_release_event).await;
        let _ = buttons_listener.on_event(&power_release_event).await;
        std::mem::drop(device_listener_stream); // Close Zircon channel.
        std::mem::drop(buttons_listener); // Close Zircon channel.

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events.len(), 8);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
        assert_eq!(events[0].value, 1);
        assert_eq!(events[2].code, uapi::KEY_SCREENSAVER as u16);
        assert_eq!(events[2].value, 1);
        assert_eq!(events[4].code, uapi::KEY_SCREENSAVER as u16);
        assert_eq!(events[4].value, 0);
        assert_eq!(events[6].code, uapi::KEY_POWER as u16);
        assert_eq!(events[6].value, 0);
    }

    #[::fuchsia::test]
    async fn sends_simultaneous_button_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut device_listener_stream) =
            start_button_input(&current_task);

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let power_and_function_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(true),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_and_function_event).await;
        std::mem::drop(device_listener_stream); // Close Zircon channel.
        std::mem::drop(buttons_listener); // Close Zircon channel.

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
        assert_eq!(events[0].value, 1);
        assert_eq!(events[2].code, uapi::KEY_SCREENSAVER as u16);
        assert_eq!(events[2].value, 1);
    }

    #[::fuchsia::test]
    fn touch_input_file_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let _touch_file = InputFile::new_touch(
            TOUCH_INPUT_ID,
            720,  /* screen height */
            1200, /* screen width */
            Some(inspector.root().create_child("touch_input_file")),
        );

        assert_data_tree!(inspector, root: {
            touch_input_file: {
                fidl_events_received_count: 0u64,
                fidl_events_ignored_count: 0u64,
                fidl_events_unexpected_count: 0u64,
                fidl_events_converted_count: 0u64,
                uapi_events_generated_count: 0u64,
                uapi_events_read_count: 0u64,
            }
        });
    }

    #[::fuchsia::test]
    async fn touch_relay_updates_touch_inspect_status() {
        let inspector = fuchsia_inspect::Inspector::default();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) =
            start_touch_input_inspect(&current_task, &inspector);

        // Send 2 TouchEvents to proxy that should be counted as `received` by InputFile
        // A TouchEvent::default() has no pointer sample so these events should be discarded.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => responder
                .send(&vec![make_empty_touch_event(); 2])
                .expect("failure sending Watch reply"),
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Send 5 TouchEvents with pointer sample to proxy, these should be received and converted
        // Add/Remove events generate 5 uapi events each. Change events generate 3 uapi events each.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                assert_matches!(responses.as_slice(), [_, _]);
                responder
                    .send(&vec![
                        make_touch_event_with_coords_phase_timestamp(
                            0.0,
                            0.0,
                            EventPhase::Add,
                            1,
                            1,
                        ),
                        make_touch_event_with_coords_phase_timestamp(
                            0.0,
                            0.0,
                            EventPhase::Change,
                            1,
                            2,
                        ),
                        make_touch_event_with_coords_phase_timestamp(
                            0.0,
                            0.0,
                            EventPhase::Change,
                            1,
                            3,
                        ),
                        make_touch_event_with_coords_phase_timestamp(
                            0.0,
                            0.0,
                            EventPhase::Change,
                            1,
                            4,
                        ),
                        make_touch_event_with_coords_phase_timestamp(
                            0.0,
                            0.0,
                            EventPhase::Remove,
                            1,
                            5,
                        ),
                    ])
                    .expect("failure sending Watch reply");
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Wait for next `Watch` call and verify it has five elements in `responses`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_, _, _, _, _])
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        let _events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_data_tree!(inspector, root: {
            touch_device: {
                touch_file: {
                    fidl_events_received_count: 7u64,
                    fidl_events_ignored_count: 2u64,
                    fidl_events_unexpected_count: 0u64,
                    fidl_events_converted_count: 5u64,
                    uapi_events_generated_count: 22u64,
                    uapi_events_read_count: 22u64,
                },
            }
        });
    }

    #[::fuchsia::test]
    fn keyboard_input_file_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let _keyboard_file = InputFile::new_keyboard(
            KEYBOARD_INPUT_ID,
            Some(inspector.root().create_child("keyboard_input_file")),
        );

        assert_data_tree!(inspector, root: {
          keyboard_input_file: {
                fidl_events_received_count: 0u64,
                fidl_events_ignored_count: 0u64,
                fidl_events_unexpected_count: 0u64,
                fidl_events_converted_count: 0u64,
                uapi_events_generated_count: 0u64,
                uapi_events_read_count: 0u64,
            }
        });
    }

    #[::fuchsia::test]
    async fn button_relay_updates_keyboard_inspect_status() {
        let inspector = fuchsia_inspect::Inspector::default();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut device_listener_stream) =
            start_button_input_inspect(&current_task, &inspector);

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        // Each of these events should count toward received and converted.
        // They also generate 2 uapi events each.
        let power_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            ..Default::default()
        };

        let power_release_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(false),
            function: Some(false),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_event).await;
        let _ = buttons_listener.on_event(&power_release_event).await;

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
        assert_eq!(events[0].value, 1);
        assert_eq!(events[2].code, uapi::KEY_POWER as u16);
        assert_eq!(events[2].value, 0);

        let _events = read_uapi_events(&mut locked, &input_file, &current_task);

        assert_data_tree!(inspector, root: {
            keyboard_device: {
                keyboard_file: {
                    fidl_events_received_count: 2u64,
                    fidl_events_ignored_count: 0u64,
                    fidl_events_unexpected_count: 0u64,
                    fidl_events_converted_count: 2u64,
                    uapi_events_generated_count: 4u64,
                    uapi_events_read_count: 4u64,
                },
            }
        });
    }
}
