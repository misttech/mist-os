// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{FuchsiaTouchEventToLinuxTouchEventConverter, InputFile};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_ui_pointer::{
    TouchEvent as FidlTouchEvent, TouchPointerSample, TouchResponse as FidlTouchResponse,
    TouchResponseType, {self as fuipointer},
};
use fuchsia_async as fasync;
use starnix_core::power::{clear_wake_proxy_signal, create_proxy_for_wake_events};
use starnix_core::task::Kernel;
use starnix_logging::log_warn;
use starnix_sync::Mutex;
use starnix_uapi::uapi;
use starnix_uapi::vfs::FdEvents;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};

pub enum EventProxyMode {
    /// Don't proxy input events at all.
    None,

    /// Have the Starnix runner proxy events such that the container
    /// will wake up if events are received while the container is
    /// suspended.
    WakeContainer,
}

pub type OpenedFiles = Arc<Mutex<Vec<Weak<InputFile>>>>;

pub enum InputDeviceType {
    Touch(FuchsiaTouchEventToLinuxTouchEventConverter),
    Keyboard,
}

impl std::fmt::Display for InputDeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputDeviceType::Touch(_) => write!(f, "touch"),
            InputDeviceType::Keyboard => write!(f, "keyboard"),
        }
    }
}

pub struct DeviceState {
    device_type: InputDeviceType,
    open_files: OpenedFiles,
}

pub type DeviceId = u32;

pub const DEFAULT_TOUCH_DEVICE_ID: DeviceId = 0;
pub const DEFAULT_KEYBOARD_DEVICE_ID: DeviceId = 1;

pub struct InputEventsRelay {
    devices: Mutex<HashMap<DeviceId, DeviceState>>,
}

impl InputEventsRelay {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { devices: Mutex::new(HashMap::new()) })
    }

    pub fn add_touch_device(&self, device_id: DeviceId, open_files: OpenedFiles) {
        self.devices.lock().insert(
            device_id,
            DeviceState {
                device_type: InputDeviceType::Touch(
                    FuchsiaTouchEventToLinuxTouchEventConverter::create(),
                ),
                open_files: open_files,
            },
        );
    }

    pub fn add_keyboard_device(&self, device_id: DeviceId, open_files: OpenedFiles) {
        self.devices.lock().insert(
            device_id,
            DeviceState { device_type: InputDeviceType::Keyboard, open_files: open_files },
        );
    }

    pub fn remove_device(&self, device_id: DeviceId) {
        self.devices.lock().remove(&device_id);
    }

    pub fn start_relays(
        self: &Arc<Self>,
        kernel: &Kernel,
        event_proxy_mode: EventProxyMode,
        touch_source_client_end: ClientEnd<fuipointer::TouchSourceMarker>,
        default_touch_device_opened_files: OpenedFiles,
    ) {
        self.start_touch_relay(
            kernel,
            event_proxy_mode,
            touch_source_client_end,
            default_touch_device_opened_files,
        );
    }

    fn start_touch_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        event_proxy_mode: EventProxyMode,
        touch_source_client_end: ClientEnd<fuipointer::TouchSourceMarker>,
        default_touch_device_opened_files: OpenedFiles,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
            let mut default_touch_device = DeviceState {
                device_type: InputDeviceType::Touch(
                    FuchsiaTouchEventToLinuxTouchEventConverter::create(),
                ),
                open_files: default_touch_device_opened_files,
            };
            let (touch_source_proxy, resume_event) = match event_proxy_mode {
                EventProxyMode::WakeContainer => {
                    // Proxy the touch events through the Starnix runner. This allows touch events to
                    // wake the container when it is suspended.
                    let (touch_source_channel, resume_event) =
                    create_proxy_for_wake_events(touch_source_client_end.into_channel());
                    (
                        fuipointer::TouchSourceProxy::new(fidl::AsyncChannel::from_channel(
                            touch_source_channel,
                        )),
                        Some(resume_event),
                    )
                }
                EventProxyMode::None => (
                    touch_source_client_end.into_proxy().expect("Failed to create proxy"),
                    None,
                ),
            };
            let mut previous_event_disposition = vec![];
            loop {
                // Create the future to watch for the the next input events, but don't execute
                // it...
                let watch_future = touch_source_proxy.watch(&previous_event_disposition);

                // .. until the event that we passed to the runner has been cleared. This prevents
                // the container from suspending between calls to `watch`.
                resume_event.as_ref().map(clear_wake_proxy_signal);

                match watch_future.await {
                    Ok(touch_events) => {
                        // TODO(https://fxbug.dev/365571169): record received event count by device,
                        // and use `num_received_events` in another inspect node.
                        let num_received_events: u64 = touch_events.len().try_into().unwrap();

                        previous_event_disposition =
                            touch_events.iter().map(make_response_for_fidl_event).collect();

                        let mut num_converted_events: u64 = 0;
                        let mut num_ignored_events: u64 = 0;
                        let mut num_unexpected_events: u64 = 0;
                        let mut new_events: VecDeque<uapi::input_event> = VecDeque::new();

                        // 1 vec may contains events from different device.
                        let (events_by_device, ignored_events) =
                            group_touch_events_by_device_id(touch_events);
                        num_ignored_events += ignored_events;

                        for (device_id, events) in events_by_device {
                            let mut devs = slf.devices.lock();

                            let dev = devs.get_mut(&device_id).unwrap_or(&mut default_touch_device);

                            if let InputDeviceType::Touch(ref mut converter) = dev.device_type {
                                let (
                                    mut covered_events,
                                    num_converted,
                                    num_ignored,
                                    num_unexpected,
                                ) = converter.handle(events);

                                new_events.append(&mut covered_events);
                                num_converted_events += num_converted;
                                num_ignored_events += num_ignored;
                                num_unexpected_events += num_unexpected;
                            } else {
                                log_warn!("Non touch device received touch events: device_id = {}, device_type = {}", device_id, dev.device_type);
                                continue;
                            }

                            let mut files = dev.open_files.lock();

                            // TODO(https://fxbug.dev/364891772): we don't need to collect files here.
                            let filtered_files: Vec<Arc<InputFile>> =
                                files.drain(..).flat_map(|f| f.upgrade()).collect();
                            for file in filtered_files {
                                let mut inner = file.inner.lock();
                                match &inner.inspect_status {
                                    Some(inspect_status) => {
                                        inspect_status.count_received_events(num_received_events);
                                        inspect_status.count_ignored_events(num_ignored_events);
                                        inspect_status
                                            .count_unexpected_events(num_unexpected_events);
                                        inspect_status.count_converted_events(num_converted_events);
                                        inspect_status.count_generated_events(
                                            new_events.len().try_into().unwrap(),
                                        );
                                    }
                                    None => (),
                                }

                                if !new_events.is_empty() {
                                    // TODO(https://fxbug.dev/42075438): Reading from an `InputFile` should
                                    // not provide access to events that occurred before the file was
                                    // opened.
                                    inner.events.append(&mut new_events);
                                    inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                }

                                files.push(Arc::downgrade(&file));
                            }
                        }
                    }
                    Err(e) => {
                        log_warn!("error {:?} reading from TouchSourceProxy; input is stopped", e);
                        break;
                    }
                };
            }});
        });
    }
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

fn group_touch_events_by_device_id(
    events: Vec<FidlTouchEvent>,
) -> (HashMap<DeviceId, Vec<FidlTouchEvent>>, u64) {
    let mut events_by_device: HashMap<u32, Vec<FidlTouchEvent>> = HashMap::new();
    let mut ignored_events: u64 = 0;
    for e in events {
        match e {
            FidlTouchEvent {
                pointer_sample: Some(TouchPointerSample { interaction: Some(id), .. }),
                ..
            } => {
                events_by_device.entry(id.device_id).or_default().push(e);
            }
            _ => {
                ignored_events += 1;
            }
        }
    }

    (events_by_device, ignored_events)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::InputDevice;
    use anyhow::anyhow;
    use fuipointer::{
        EventPhase, TouchEvent, TouchInteractionId, TouchPointerSample, TouchResponse,
        TouchSourceMarker, TouchSourceRequest, TouchSourceRequestStream,
    };
    use futures::StreamExt as _;
    use starnix_core::task::CurrentTask;
    use starnix_core::testing::create_kernel_task_and_unlocked;
    use starnix_core::vfs::{FileHandle, FileObject, VecOutputBuffer};
    use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked};
    use starnix_uapi::errors::{Errno, EAGAIN};
    use starnix_uapi::input_id;
    use starnix_uapi::open_flags::OpenFlags;
    use zerocopy::FromBytes as _;

    const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

    fn start_touch_input_inspect_and_dimensions(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        x_max: i32,
        y_max: i32,
        inspector: &fuchsia_inspect::Inspector,
    ) -> (Arc<InputEventsRelay>, Arc<InputDevice>, FileHandle, TouchSourceRequestStream) {
        let input_device = InputDevice::new_touch(x_max, y_max, inspector.root());
        let input_file =
            input_device.open_test(locked, current_task).expect("Failed to create input file");

        let (touch_source_client_end, touch_source_stream) =
            fidl::endpoints::create_request_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");

        let relay = InputEventsRelay::new();
        relay.start_relays(
            &current_task.kernel(),
            EventProxyMode::None,
            touch_source_client_end,
            input_device.open_files.clone(),
        );

        (relay, input_device, input_file, touch_source_stream)
    }

    // Waits for a `Watch()` request to arrive on `request_stream`, and responds with
    // `touch_event`. Returns the arguments to the `Watch()` call.
    async fn answer_next_watch_request(
        request_stream: &mut TouchSourceRequestStream,
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

    fn make_empty_touch_event(device_id: u32) -> TouchEvent {
        TouchEvent {
            pointer_sample: Some(TouchPointerSample {
                interaction: Some(TouchInteractionId {
                    pointer_id: 0,
                    device_id,
                    interaction_id: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_event_with_phase_device_id(
        phase: EventPhase,
        pointer_id: u32,
        device_id: u32,
    ) -> TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([0.0, 0.0]),
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

    #[::fuchsia::test]
    async fn route_touch_event_by_device_id() {
        // Set up resources.
        let inspector = fuchsia_inspect::Inspector::default();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (input_relay, _input_device, input_file, mut touch_source_stream) =
            start_touch_input_inspect_and_dimensions(
                &mut locked,
                &current_task,
                700,
                1200,
                &inspector,
            );

        const DEVICE_ID: u32 = 10;

        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase_device_id(EventPhase::Add, 1, DEVICE_ID)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s.
        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_empty_touch_event(DEVICE_ID)],
        )
        .await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        // Default device receive events because no matched device.
        assert_ne!(events.len(), 0);

        // add a device, mock uinput.
        let open_files: OpenedFiles = Arc::new(Mutex::new(vec![]));
        input_relay.add_touch_device(DEVICE_ID, open_files.clone());
        let device_id_10_file = Arc::new(InputFile::new_touch(
            input_id { bustype: 0, vendor: 0, product: 0, version: 0 },
            1000,
            1000,
            None,
        ));
        open_files.lock().push(Arc::downgrade(&device_id_10_file));
        let device_id_10_file_object = FileObject::new(
            Box::new(device_id_10_file),
            current_task
                .lookup_path_from_root(&mut locked, ".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");

        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase_device_id(EventPhase::Add, 1, DEVICE_ID)],
        )
        .await;

        answer_next_watch_request(
            &mut touch_source_stream,
            vec![make_empty_touch_event(DEVICE_ID)],
        )
        .await;

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        // Default device should not receive events because they matched device id 10.
        assert_eq!(events.len(), 0);

        let events = read_uapi_events(&mut locked, &device_id_10_file_object, &current_task);
        // file of device id 10 should receive events.
        assert_ne!(events.len(), 0);
    }
}
