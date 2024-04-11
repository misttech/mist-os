// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{parse_fidl_keyboard_event_to_linux_input_event, InputFile};
use fidl_fuchsia_ui_input::MediaButtonsEvent;
use fidl_fuchsia_ui_input3 as fuiinput;
use fidl_fuchsia_ui_pointer::{
    EventPhase as FidlEventPhase, TouchEvent as FidlTouchEvent, TouchResponse as FidlTouchResponse,
    TouchResponseType, {self as fuipointer},
};
use fidl_fuchsia_ui_policy as fuipolicy;
use fidl_fuchsia_ui_views as fuiviews;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::StreamExt as _;
use starnix_core::{
    device::{kobject::DeviceMetadata, DeviceMode, DeviceOps},
    fs::sysfs::DeviceDirectory,
    task::{CurrentTask, Kernel},
    vfs::{FileOps, FsNode, FsString},
};
use starnix_logging::log_warn;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex};
use starnix_uapi as uapi;
use starnix_uapi::{
    device_type::{DeviceType, INPUT_MAJOR},
    errors::Errno,
    input_id,
    open_flags::OpenFlags,
    time::timeval_from_time,
    timeval,
    vfs::FdEvents,
    BUS_VIRTUAL,
};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Weak,
};

// Per https://www.linuxjournal.com/article/6429, the bus type should be populated with a
// sensible value, but other fields may not be.
const INPUT_ID: input_id =
    input_id { bustype: BUS_VIRTUAL as u16, product: 0, vendor: 0, version: 0 };

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

    open_files: Arc<Mutex<Vec<Weak<InputFile>>>>,

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

    pub fn register<L>(self: Arc<Self>, locked: &mut Locked<'_, L>, system_task: &CurrentTask)
    where
        L: LockBefore<FileOpsCore>,
    {
        let kernel = system_task.kernel();
        let registry = &kernel.device_registry;

        let input_class = registry.get_or_create_class("input".into(), registry.virtual_bus());
        let device_id = get_next_device_id();
        registry.add_and_register_device(
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

    /// Starts reading events from the Fuchsia input system, and making those events available
    /// to the guest system.
    ///
    /// This method *should* be called as soon as possible after the `TouchSourceProxy` has been
    /// registered with the `TouchSource` server, as the server expects `TouchSource` clients to
    /// consume events in a timely manner.
    ///
    /// # Parameters
    /// * `touch_source_proxy`: a connection to the Fuchsia input system, which will provide
    ///   touch events associated with the Fuchsia `View` created by Starnix.
    pub fn start_touch_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        touch_source_proxy: fuipointer::TouchSourceSynchronousProxy,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            let mut previous_event_disposition = vec![];
            loop {
                let query_res =
                    touch_source_proxy.watch(&previous_event_disposition, zx::Time::INFINITE);
                match query_res {
                    Ok(touch_events) => {
                        let num_received_events: u64 = touch_events.len().try_into().unwrap();
                        previous_event_disposition =
                            touch_events.iter().map(make_response_for_fidl_event).collect();
                        let converted_events =
                            touch_events.iter().filter_map(parse_fidl_touch_event);
                        let num_converted_events: u64 =
                            converted_events.clone().count().try_into().unwrap();
                        // `flat`: each FIDL event yields a `Vec<uapi::input_event>`,
                        // but the end result should be a single vector of UAPI events,
                        // not a `Vec<Vec<uapi::input_event>>`.
                        let new_events = converted_events.flat_map(make_uapi_events);
                        let num_generated_events: u64 =
                            new_events.clone().count().try_into().unwrap();

                        let mut files = slf.open_files.lock();
                        let filtered_files: Vec<Arc<InputFile>> =
                            files.drain(..).flat_map(|f| f.upgrade()).collect();
                        for file in filtered_files {
                            let mut inner = file.inner.lock();
                            match &inner.inspect_status {
                                Some(inspect_status) => {
                                    inspect_status.count_received_events(num_received_events);
                                    inspect_status.count_converted_events(num_converted_events);
                                    inspect_status.count_generated_events(num_generated_events);
                                }
                                None => (),
                            }
                            // TODO(https://fxbug.dev/42075438): Reading from an `InputFile` should
                            // not provide access to events that occurred before the file was
                            // opened.
                            inner.events.extend(new_events.clone());
                            // TODO(https://fxbug.dev/42075439): Skip notify if `inner.events`
                            // is empty.
                            inner.waiters.notify_fd_events(FdEvents::POLLIN);
                            files.push(Arc::downgrade(&file));
                        }
                    }
                    Err(e) => {
                        log_warn!("error {:?} reading from TouchSourceProxy; input is stopped", e);
                        break;
                    }
                };
            }
        });
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
                if keyboard.add_listener(view_ref, keyboard_listener, zx::Time::INFINITE).is_err() {
                    log_warn!("Could not register keyboard listener");
                }
                while let Some(Ok(request)) = event_stream.next().await {
                    match request {
                        fuiinput::KeyboardListenerRequest::OnKeyEvent { event, responder } => {
                            let new_events = parse_fidl_keyboard_event_to_linux_input_event(&event);

                            let mut files = slf.open_files.lock();
                            let filtered_files: Vec<Arc<InputFile>> =
                                files.drain(..).flat_map(|f| f.upgrade()).collect();
                            for file in filtered_files {
                                let mut inner = file.inner.lock();
                                inner.events.extend(new_events.clone());
                                inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                files.push(Arc::downgrade(&file));
                            }

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
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                // Create and register a listener to listen for MediaButtonsEvents from Input Pipeline.
                let (listener, mut listener_stream) = fidl::endpoints::create_request_stream::<
                    fuipolicy::MediaButtonsListenerMarker,
                >()
                .unwrap();
                if let Err(e) = registry_proxy.register_listener(listener, zx::Time::INFINITE) {
                    log_warn!("Failed to register media buttons listener: {:?}", e);
                }
                while let Some(Ok(request)) = listener_stream.next().await {
                    match request {
                        fuipolicy::MediaButtonsListenerRequest::OnEvent { event, responder } => {
                            let new_events = parse_fidl_button_event(&event);

                            let mut files = slf.open_files.lock();
                            let filtered_files: Vec<Arc<InputFile>> =
                                files.drain(..).flat_map(|f| f.upgrade()).collect();
                            for file in filtered_files {
                                let mut inner = file.inner.lock();
                                inner.events.extend(new_events.clone());
                                inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                files.push(Arc::downgrade(&file));
                            }

                            responder.send().expect("media buttons responder failed to respond");
                        }
                        _ => {}
                    }
                }
            })
        });
    }

    #[cfg(test)]
    fn open_test(
        &self,
        current_task: &CurrentTask,
    ) -> Result<starnix_core::vfs::FileHandle, Errno> {
        let input_file = match self.device_type {
            InputDeviceType::Touch(display_width, display_height) => {
                let file = Arc::new(InputFile::new_touch(
                    INPUT_ID,
                    display_width,
                    display_height,
                    Some(self.input_files_node.create_child("touch_file")),
                ));
                file
            }
            InputDeviceType::Keyboard => Arc::new(InputFile::new_keyboard(INPUT_ID, None)),
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
                    INPUT_ID,
                    display_width,
                    display_height,
                    Some(self.input_files_node.create_child("touch_file")),
                ));
                file
            }
            InputDeviceType::Keyboard => Arc::new(InputFile::new_keyboard(
                INPUT_ID,
                Some(self.input_files_node.create_child("keyboard_file")),
            )),
        };
        self.open_files.lock().push(Arc::downgrade(&input_file));
        Ok(Box::new(input_file))
    }
}

/// get_next_device_id() returns the current value of NEXT_DEVICE_ID for next
/// available device id, and increase the NEXT_DEVICE_ID for next call.
pub fn get_next_device_id() -> u32 {
    // Maintain an atomic increment number as device_id, used as X in
    // /dev/input/eventX.
    static NEXT_DEVICE_ID: AtomicU32 = AtomicU32::new(0);

    NEXT_DEVICE_ID.fetch_add(1, Ordering::SeqCst)
}

#[derive(Copy, Clone)]
// This module's representation of the information from `fuipointer::EventPhase`.
enum PhaseChange {
    Added,
    Removed,
}

// This module's representation of the information from `fuipointer::TouchEvent`.
struct TouchEvent {
    time: timeval,
    // Change in the contact state, if any. The phase change for a FIDL event reporting a move,
    // for example, will have `None` here.
    phase_change: Option<PhaseChange>,
    pos_x: f32,
    pos_y: f32,
}

/// Returns `Some` if the FIDL event included a `timestamp`, and a `pointer_sample` which includes
/// both `position_in_viewport` and `phase`. Returns `None` otherwise.
fn parse_fidl_touch_event(fidl_event: &FidlTouchEvent) -> Option<TouchEvent> {
    match fidl_event {
        FidlTouchEvent {
            timestamp: Some(time_nanos),
            pointer_sample:
                Some(fuipointer::TouchPointerSample {
                    position_in_viewport: Some([x, y]),
                    phase: Some(phase),
                    ..
                }),
            ..
        } => Some(TouchEvent {
            time: timeval_from_time(zx::Time::from_nanos(*time_nanos)),
            phase_change: phase_change_from_fidl_phase(phase),
            pos_x: *x,
            pos_y: *y,
        }),
        _ => None, // TODO(https://fxbug.dev/42075446): Add some inspect counters of ignored events.
    }
}

fn parse_fidl_button_event(fidl_event: &MediaButtonsEvent) -> Vec<uapi::input_event> {
    let time = timeval_from_time(zx::Time::get_monotonic());
    let mut events = vec![];
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };
    match fidl_event {
        &MediaButtonsEvent { power: Some(true), .. } => {
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::KEY_POWER as u16,
                value: 1,
            };
            events.push(key_event);
            events.push(sync_event);
        }
        // Power button is released
        &MediaButtonsEvent { power: Some(false), .. } => {
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::KEY_POWER as u16,
                value: 0,
            };
            events.push(key_event);
            events.push(sync_event);
        }
        _ => {}
    }

    events
}

/// Returns a FIDL response for `fidl_event`.
fn make_response_for_fidl_event(fidl_event: &FidlTouchEvent) -> FidlTouchResponse {
    match fidl_event {
        FidlTouchEvent { pointer_sample: Some(_), .. } => FidlTouchResponse {
            response_type: Some(TouchResponseType::Yes), // Event consumed by Starnix.
            trace_flow_id: fidl_event.trace_flow_id,
            ..Default::default()
        },
        _ => FidlTouchResponse::default(),
    }
}

/// Returns a sequence of UAPI input events which represent the data in `event`.
fn make_uapi_events(event: TouchEvent) -> Vec<uapi::input_event> {
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time: event.time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };
    let mut events = vec![];
    events.extend(make_contact_state_uapi_events(&event).iter().flatten());
    events.extend(make_position_uapi_events(&event));
    events.push(sync_event);
    events
}

/// Returns a sequence of UAPI input events representing `event`'s change in contact state,
/// if any.
fn make_contact_state_uapi_events(event: &TouchEvent) -> Option<[uapi::input_event; 2]> {
    event.phase_change.map(|phase_change| {
        let value = match phase_change {
            PhaseChange::Added => 1,
            PhaseChange::Removed => 0,
        };
        [
            uapi::input_event {
                time: event.time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOUCH as u16,
                value,
            },
            // TODO(https://fxbug.dev/42075449): Reporting `BTN_TOOL_FINGER` here could cause some
            // programs to interpret this device as a touchpad, rather than a touchscreen. Also,
            // this isn't suitable if `InputFile` (eventually) supports multi-touch mode.
            // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
            uapi::input_event {
                time: event.time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOOL_FINGER as u16,
                value,
            },
        ]
    })
}

/// Returns a sequence of UAPI input events representing `event`'s contact location.
fn make_position_uapi_events(event: &TouchEvent) -> [uapi::input_event; 2] {
    [
        // The X coordinate.
        uapi::input_event {
            time: event.time,
            type_: uapi::EV_ABS as u16,
            code: uapi::ABS_X as u16,
            value: event.pos_x as i32,
        },
        // The Y coordinate.
        uapi::input_event {
            time: event.time,
            type_: uapi::EV_ABS as u16,
            code: uapi::ABS_Y as u16,
            value: event.pos_y as i32,
        },
    ]
}

/// Returns `Some` phase change if `fidl_phase` reports a change in contact state.
/// Returns `None` otherwise.
fn phase_change_from_fidl_phase(fidl_phase: &FidlEventPhase) -> Option<PhaseChange> {
    match fidl_phase {
        FidlEventPhase::Add => Some(PhaseChange::Added),
        FidlEventPhase::Change => None, // `Change` indicates position change only
        FidlEventPhase::Remove => Some(PhaseChange::Removed),
        // TODO(https://fxbug.dev/42075450): Figure out whether this is correct.
        FidlEventPhase::Cancel => None,
    }
}

#[cfg(test)]
mod test {
    #![allow(clippy::unused_unit)] // for compatibility with `test_case`

    use super::*;
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use fidl_fuchsia_ui_pointer as fuipointer;
    use fidl_fuchsia_ui_policy as fuipolicy;
    use fuchsia_zircon as zx;
    use fuipointer::{
        EventPhase, TouchEvent, TouchPointerSample, TouchResponse, TouchSourceMarker,
        TouchSourceRequest,
    };
    use starnix_core::{
        task::{EventHandler, Waiter},
        testing::{create_kernel_and_task, create_kernel_task_and_unlocked},
        vfs::{buffers::VecOutputBuffer, FileHandle},
    };
    use starnix_uapi::errors::EAGAIN;
    use test_case::test_case;
    use test_util::assert_near;
    use zerocopy::FromBytes as _; // for `read_from()`

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

        let (touch_source_proxy, touch_source_stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");

        input_device.start_touch_relay(current_task.kernel(), touch_source_proxy);

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
        let input_device = InputDevice::new_keyboard(inspector.root());
        let input_file = input_device.open_test(current_task).expect("Failed to create input file");
        let (device_registry_proxy, device_listener_stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<fuipolicy::DeviceListenerRegistryMarker>()
                .expect("Failed to create DeviceListenerRegistry proxy and stream.");
        input_device.start_button_relay(current_task.kernel(), device_registry_proxy);
        (input_device, input_file, device_listener_stream)
    }

    fn make_touch_event() -> fuipointer::TouchEvent {
        // Default to `Change`, because that has the fewest side effects.
        make_touch_event_with_phase(EventPhase::Change)
    }

    fn make_touch_event_with_phase(phase: EventPhase) -> fuipointer::TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([0.0, 0.0]),
                phase: Some(phase),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_event_with_coords(x: f32, y: f32) -> fuipointer::TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([x, y]),
                // Default to `Change`, because that has the fewest side effects.
                phase: Some(EventPhase::Change),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_pointer_sample() -> TouchPointerSample {
        TouchPointerSample {
            position_in_viewport: Some([0.0, 0.0]),
            // Default to `Change`, because that has the fewest side effects.
            phase: Some(EventPhase::Change),
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
                    uapi::input_event::read_from(Vec::from(event_bytes).as_slice())
                        .ok_or(anyhow!("failed to read input_event from buffer")),
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
        touch_event: TouchEvent,
    ) -> Vec<TouchResponse> {
        match request_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                responder.send(&[touch_event]).expect("failure sending Watch reply");
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
        let (_kernel, current_task) = create_kernel_and_task();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().for_each(|waiter| {
            input_file.wait_async(&current_task, waiter, FdEvents::POLLIN, EventHandler::None);
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::Time::ZERO), Err(_));

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_eq!(waiter1.wait_until(&current_task, zx::Time::ZERO), Ok(()));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));
    }

    #[::fuchsia::test]
    async fn notifies_blocked_waiter_of_new_data() {
        // Set up resources.
        let (kernel, current_task) = create_kernel_and_task();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter = Waiter::new();

        // Ask `input_file` to notify `waiter` when data is available to read.
        input_file.wait_async(&current_task, &waiter, FdEvents::POLLIN, EventHandler::None);

        let mut waiter_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |_, task| waiter.wait(&task));
        assert!(futures::poll!(&mut waiter_thread).is_pending());

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`.
        //
        // TODO(https://fxbug.dev/42075452): Without this, `relay_thread` gets stuck `await`-ing
        // the reply to its first request. Figure out why that happens, and remove this second
        // reply.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;
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
        let (_kernel, current_task) = create_kernel_and_task();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        // Ask `input_file` to notify `waiter` when data is available to read.
        let waitkeys = [&waiter1, &waiter2]
            .iter()
            .map(|waiter| {
                input_file
                    .wait_async(&current_task, waiter, FdEvents::POLLIN, EventHandler::None)
                    .expect("wait_async")
            })
            .collect::<Vec<_>>();

        // Cancel wait for `waiter1`.
        waitkeys.into_iter().next().expect("failed to get first waitkey").cancel();

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;
        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));
    }

    #[::fuchsia::test]
    async fn query_events() {
        // Set up resources.
        let (_kernel, current_task) = create_kernel_and_task();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Check initial expectation.
        assert_eq!(
            input_file.query_events(&current_task).expect("query_events"),
            FdEvents::empty(),
            "events should be empty before data arrives"
        );

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Check post-watch expectation.
        assert_eq!(
            input_file.query_events(&current_task).expect("query_events"),
            FdEvents::POLLIN | FdEvents::POLLRDNORM,
            "events should be POLLIN after data arrives"
        );
    }

    #[test_case(EventPhase::Add, uapi::BTN_TOUCH => Some(1);
        "add_yields_btn_touch_true")]
    #[test_case(EventPhase::Change, uapi::BTN_TOUCH => None;
        "change_yields_no_btn_touch_event")]
    #[test_case(EventPhase::Remove, uapi::BTN_TOUCH => Some(0);
        "remove_yields_btn_touch_false")]
    #[test_case(EventPhase::Add, uapi::BTN_TOOL_FINGER => Some(1);
        "add_yields_btn_tool_finger_true")]
    #[test_case(EventPhase::Change, uapi::BTN_TOOL_FINGER => None;
        "change_yields_no_btn_tool_finger_event")]
    #[test_case(EventPhase::Remove, uapi::BTN_TOOL_FINGER => Some(0);
        "remove_yields_btn_tool_finger_false")]
    #[::fuchsia::test]
    async fn translates_event_phase_to_expected_evkey_events(
        fidl_phase: EventPhase,
        event_code: u32,
    ) -> Option<i32> {
        let event_code = event_code as u16;

        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(fidl_phase),
        )
        .await;

        // Wait for another `Watch`.
        // Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, TouchEvent::default()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        // Report the `value` of the first `event.code` event, or None if there wasn't one.
        events.into_iter().find_map(|event| {
            if event.type_ == uapi::EV_KEY as u16 && event.code == event_code {
                Some(event.value)
            } else {
                None
            }
        })
    }

    // Per https://www.kernel.org/doc/Documentation/input/event-codes.rst,
    // 1. "BTN_TOUCH must be the first evdev code emitted".
    // 2. "EV_SYN, isused to separate input events"
    #[test_case((uapi::EV_KEY, uapi::BTN_TOUCH), 0; "btn_touch_is_first")]
    #[test_case((uapi::EV_SYN, uapi::SYN_REPORT), -1; "syn_report_is_last")]
    #[::fuchsia::test]
    async fn event_sequence_is_correct((event_type, event_code): (u32, u32), position: isize) {
        let event_type = event_type as u16;
        let event_code = event_code as u16;

        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (_input_device, input_file, mut touch_source_stream) = start_touch_input(&current_task);

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(EventPhase::Add),
        )
        .await;

        // Wait for another `Watch.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        // Check that the expected event type and code are in the expected position.
        let position = if position >= 0 {
            position.unsigned_abs()
        } else {
            events.iter().len() - position.unsigned_abs()
        };
        assert_matches!(
            events.get(position),
            Some(&uapi::input_event { type_, code, ..}) if type_ == event_type && code == event_code,
            "did not find type={}, code={} at position {}; `events` is {:?}",
            event_type,
            event_code,
            position,
            events
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

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event_with_coords(x, y))
            .await;

        // Wait for another `Watch`.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);

        // Check that the reported positions are within the acceptable error. The acceptable
        // error is chosen to allow either rounding or truncation.
        const ACCEPTABLE_ERROR: f32 = 1.0;
        let actual_x = events
            .iter()
            .find(|event| event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_X as u16)
            .unwrap_or_else(|| panic!("did not find `ABS_X` event in {:?}", events))
            .value;
        let actual_y = events
            .iter()
            .find(|event| event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_Y as u16)
            .unwrap_or_else(|| panic!("did not find `ABS_Y` event in {:?}", events))
            .value;
        assert_near!(x, actual_x as f32, ACCEPTABLE_ERROR);
        assert_near!(y, actual_y as f32, ACCEPTABLE_ERROR);
    }

    // Per the FIDL documentation for `TouchSource::Watch()`:
    //
    // > non-sample events should return an empty |TouchResponse| table to the
    // > server
    #[test_case(TouchEvent {
        timestamp: Some(0),
        pointer_sample: Some(make_touch_pointer_sample()),
        ..Default::default()
      } => matches Some(TouchResponse { response_type: Some(_), ..});
      "event_with_sample_yields_some_response_type")]
    #[test_case(TouchEvent::default() => matches Some(TouchResponse { response_type: None, ..});
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
        answer_next_watch_request(&mut touch_source_stream, event).await;

        // Get response to `event`.
        let responses =
            answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

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
    async fn sends_button_events() {
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
    fn touch_input_file_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let _touch_file = InputFile::new_touch(
            INPUT_ID,
            720,  /* screen height */
            1200, /* screen width */
            Some(inspector.root().create_child("touch_input_file")),
        );

        assert_data_tree!(inspector, root: {
            touch_input_file: {
                fidl_events_received_count: 0u64,
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
                .send(&vec![TouchEvent::default(); 2])
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
                        make_touch_event_with_phase(EventPhase::Add),
                        make_touch_event(),
                        make_touch_event(),
                        make_touch_event(),
                        make_touch_event_with_phase(EventPhase::Remove),
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
                    fidl_events_converted_count: 5u64,
                    uapi_events_generated_count: 19u64,
                    uapi_events_read_count: 19u64,
                },
            }
        });
    }
}
