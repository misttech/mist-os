// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    parse_fidl_button_event, parse_fidl_keyboard_event_to_linux_input_event,
    FuchsiaTouchEventToLinuxTouchEventConverter, InputDeviceStatus, InputFile,
};
use fidl::endpoints::{ClientEnd, RequestStream};
use fidl_fuchsia_ui_input3::{
    KeyEventStatus, KeyboardListenerMarker, KeyboardListenerRequest, KeyboardSynchronousProxy,
};
use fidl_fuchsia_ui_pointer::{
    MouseEvent as FidlMouseEvent, MousePointerSample, TouchEvent as FidlTouchEvent,
    TouchPointerSample, TouchResponse as FidlTouchResponse, TouchResponseType,
    {self as fuipointer},
};
use futures::StreamExt as _;
use starnix_core::power::{clear_wake_proxy_signal, create_proxy_for_wake_events};
use starnix_core::task::Kernel;
use starnix_logging::log_warn;
use starnix_sync::Mutex;
use starnix_types::time::timeval_from_time;
use starnix_uapi::uapi;
use starnix_uapi::vfs::FdEvents;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use {
    fidl_fuchsia_ui_policy as fuipolicy, fidl_fuchsia_ui_views as fuiviews, fuchsia_async as fasync,
};

#[derive(Clone, Copy)]
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
    Mouse,
}

impl std::fmt::Display for InputDeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputDeviceType::Touch(_) => write!(f, "touch"),
            InputDeviceType::Keyboard => write!(f, "keyboard"),
            InputDeviceType::Mouse => write!(f, "mouse"),
        }
    }
}

pub struct DeviceState {
    device_type: InputDeviceType,
    open_files: OpenedFiles,
    inspect_status: Option<Arc<InputDeviceStatus>>,
}

pub type DeviceId = u32;

pub const DEFAULT_TOUCH_DEVICE_ID: DeviceId = 0;
pub const DEFAULT_KEYBOARD_DEVICE_ID: DeviceId = 1;
pub const DEFAULT_MOUSE_DEVICE_ID: DeviceId = 2;

pub struct InputEventsRelay {
    devices: Mutex<HashMap<DeviceId, DeviceState>>,
}

impl InputEventsRelay {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { devices: Mutex::new(HashMap::new()) })
    }

    pub fn add_touch_device(
        &self,
        device_id: DeviceId,
        open_files: OpenedFiles,
        inspect_status: Option<Arc<InputDeviceStatus>>,
    ) {
        self.devices.lock().insert(
            device_id,
            DeviceState {
                device_type: InputDeviceType::Touch(
                    FuchsiaTouchEventToLinuxTouchEventConverter::create(),
                ),
                open_files,
                inspect_status,
            },
        );
    }

    pub fn add_keyboard_device(
        &self,
        device_id: DeviceId,
        open_files: OpenedFiles,
        inspect_status: Option<Arc<InputDeviceStatus>>,
    ) {
        self.devices.lock().insert(
            device_id,
            DeviceState { device_type: InputDeviceType::Keyboard, open_files, inspect_status },
        );
    }

    pub fn remove_device(&self, device_id: DeviceId) {
        self.devices.lock().remove(&device_id);
    }

    // TODO(https://fxbug.dev/373844683): Use 1 thread for all relays instead of 1 thread
    // per type of device.
    // TODO(https://fxbug.dev/371602479): Use `fuchsia.ui.SupportedInputDevices` to create
    // relays.
    pub fn start_relays(
        self: &Arc<Self>,
        kernel: &Kernel,
        event_proxy_mode: EventProxyMode,
        touch_source_client_end: ClientEnd<fuipointer::TouchSourceMarker>,
        keyboard: KeyboardSynchronousProxy,
        mouse_source_client_end: ClientEnd<fuipointer::MouseSourceMarker>,
        view_ref: fuiviews::ViewRef,
        registry_proxy: fuipolicy::DeviceListenerRegistrySynchronousProxy,
        default_touch_device_opened_files: OpenedFiles,
        default_keyboard_device_opened_files: OpenedFiles,
        default_mouse_device_opened_files: OpenedFiles,
        default_touch_device_inspect: Option<Arc<InputDeviceStatus>>,
        default_keyboard_device_inspect: Option<Arc<InputDeviceStatus>>,
    ) {
        self.start_touch_relay(
            kernel,
            event_proxy_mode,
            touch_source_client_end,
            default_touch_device_opened_files,
            default_touch_device_inspect,
        );
        self.start_keyboard_relay(
            kernel,
            keyboard,
            view_ref,
            default_keyboard_device_opened_files.clone(),
            None,
        );
        self.start_button_relay(
            kernel,
            registry_proxy,
            event_proxy_mode,
            default_keyboard_device_opened_files,
            default_keyboard_device_inspect,
        );
        self.start_mouse_relay(
            kernel,
            event_proxy_mode,
            mouse_source_client_end,
            default_mouse_device_opened_files,
        );
    }

    fn start_touch_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        event_proxy_mode: EventProxyMode,
        touch_source_client_end: ClientEnd<fuipointer::TouchSourceMarker>,
        default_touch_device_opened_files: OpenedFiles,
        device_inspect_status: Option<Arc<InputDeviceStatus>>,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
            let mut default_touch_device = DeviceState {
                device_type: InputDeviceType::Touch(
                    FuchsiaTouchEventToLinuxTouchEventConverter::create(),
                ),
                open_files: default_touch_device_opened_files,
                inspect_status: device_inspect_status,
            };
            let (touch_source_proxy, resume_event) = match event_proxy_mode {
                EventProxyMode::WakeContainer => {
                    // Proxy the touch events through the Starnix runner. This allows touch events to
                    // wake the container when it is suspended.
                    let (touch_source_channel, resume_event) =
                    create_proxy_for_wake_events(touch_source_client_end.into_channel(), "touch".to_string());
                    (
                        fuipointer::TouchSourceProxy::new(fidl::AsyncChannel::from_channel(
                            touch_source_channel,
                        )),
                        Some(resume_event),
                    )
                }
                EventProxyMode::None => (
                    touch_source_client_end.into_proxy(),
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

                        let mut num_ignored_events: u64 = 0;

                        // 1 vec may contains events from different device.
                        let (events_by_device, ignored_events) =
                            group_touch_events_by_device_id(touch_events);
                        num_ignored_events += ignored_events;

                        for (device_id, events) in events_by_device {
                            let mut devs = slf.devices.lock();

                            let dev = devs.get_mut(&device_id).unwrap_or(&mut default_touch_device);

                            let mut num_converted_events: u64 = 0;
                            let mut num_unexpected_events: u64 = 0;
                            let mut new_events: VecDeque<uapi::input_event> = VecDeque::new();

                            let last_event_time_ns: i64;
                            if let InputDeviceType::Touch(ref mut converter) = dev.device_type {
                                let mut batch = converter.handle(events);
                                new_events.append(&mut batch.events);
                                num_converted_events += batch.count_converted_fidl_events;
                                num_ignored_events += batch.count_ignored_fidl_events;
                                num_unexpected_events += batch.count_unexpected_fidl_events;
                                last_event_time_ns = batch.last_event_time_ns;
                            } else {
                                log_warn!("Non touch device received touch events: device_id = {}, device_type = {}", device_id, dev.device_type);
                                continue;
                            }

                            if let Some(dev_inspect_status) = &dev.inspect_status {
                                dev_inspect_status.count_total_received_events(num_received_events);
                                dev_inspect_status.count_total_ignored_events(num_ignored_events);
                                dev_inspect_status.count_total_unexpected_events(num_unexpected_events);
                                dev_inspect_status.count_total_converted_events(num_converted_events);
                                dev_inspect_status.count_total_generated_events(
                                    new_events.len().try_into().unwrap(),
                                    last_event_time_ns,
                                );
                            } else {
                                log_warn!("unable to record inspect for device_id: {}, device_type: {}", device_id, dev.device_type);
                            }

                            dev.open_files.lock().retain(|f| {
                                let Some(file) = f.upgrade() else {
                                    log_warn!("Dropping input file for touch that failed to upgrade");
                                    return false;
                                };
                                match &file.inspect_status {
                                    Some(file_inspect_status) => {
                                        file_inspect_status.count_received_events(num_received_events);
                                        file_inspect_status.count_ignored_events(num_ignored_events);
                                        file_inspect_status.count_unexpected_events(num_unexpected_events);
                                        file_inspect_status.count_converted_events(num_converted_events);
                                    }
                                    None => {
                                      log_warn!("unable to record inspect within the input file")
                                    }
                                }
                                if !new_events.is_empty() {
                                    // TODO(https://fxbug.dev/42075438): Reading from an `InputFile` should
                                    // not provide access to events that occurred before the file was
                                    // opened.
                                    if let Some(file_inspect_status) = &file.inspect_status {
                                        file_inspect_status.count_generated_events(
                                            new_events.len().try_into().unwrap(),
                                            last_event_time_ns
                                        );
                                    }
                                    let mut inner = file.inner.lock();
                                    inner.events.extend(new_events.clone());
                                    inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                }

                                true
                            });
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

    fn start_keyboard_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        keyboard: KeyboardSynchronousProxy,
        view_ref: fuiviews::ViewRef,
        default_keyboard_device_opened_files: OpenedFiles,
        device_inspect_status: Option<Arc<InputDeviceStatus>>,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let mut default_keyboard_device = DeviceState {
                    device_type: InputDeviceType::Keyboard,
                    open_files: default_keyboard_device_opened_files,
                    inspect_status: device_inspect_status,
                };
                let (keyboard_listener, mut event_stream) =
                    fidl::endpoints::create_request_stream::<KeyboardListenerMarker>();
                if keyboard
                    .add_listener(view_ref, keyboard_listener, zx::MonotonicInstant::INFINITE)
                    .is_err()
                {
                    log_warn!("Could not register keyboard listener");
                }
                while let Some(Ok(request)) = event_stream.next().await {
                    match request {
                        KeyboardListenerRequest::OnKeyEvent { event, responder } => {
                            let new_events = parse_fidl_keyboard_event_to_linux_input_event(&event);

                            let mut devs = slf.devices.lock();

                            let dev = match event.device_id {
                                Some(device_id) => {
                                    devs.get_mut(&device_id).unwrap_or(&mut default_keyboard_device)
                                }
                                None => &default_keyboard_device,
                            };

                            dev.open_files.lock().retain(|f| {
                                let Some(file) = f.upgrade() else {
                                    log_warn!(
                                        "Dropping input file for keyboard that failed to upgrade"
                                    );
                                    return false;
                                };
                                let mut inner = file.inner.lock();

                                if !new_events.is_empty() {
                                    inner.events.extend(new_events.clone());
                                    inner.waiters.notify_fd_events(FdEvents::POLLIN);
                                }

                                true
                            });

                            responder.send(KeyEventStatus::Handled).expect("");
                        }
                    }
                }
            })
        });
    }

    fn start_button_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        registry_proxy: fuipolicy::DeviceListenerRegistrySynchronousProxy,
        event_proxy_mode: EventProxyMode,
        default_keyboard_device_opened_files: OpenedFiles,
        device_inspect_status: Option<Arc<InputDeviceStatus>>,
    ) {
        let slf: Arc<InputEventsRelay> = self.clone();
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
                        let (local_channel, local_resume_event) = create_proxy_for_wake_events(
                            remote_server.into_channel(),
                            "buttons".to_string(),
                        );
                        let local_listener_stream =
                            fuipolicy::MediaButtonsListenerRequestStream::from_channel(
                                fidl::AsyncChannel::from_channel(local_channel),
                            );
                        (local_listener_stream, Some(local_resume_event))
                    }
                    EventProxyMode::None => (remote_server.into_stream(), None),
                };
                slf.button_relay_loop(
                    local_listener_stream,
                    default_keyboard_device_opened_files,
                    device_inspect_status,
                    local_resume_event,
                )
                .await;
            })
        });
    }

    async fn button_relay_loop(
        &self,
        mut local_listener_stream: fuipolicy::MediaButtonsListenerRequestStream,
        default_keyboard_device_opened_files: OpenedFiles,
        device_inspect_status: Option<Arc<InputDeviceStatus>>,
        local_resume_event: Option<zx::EventPair>,
    ) {
        let mut power_was_pressed = false;
        let mut function_was_pressed = false;

        let mut default_keyboard_device = DeviceState {
            device_type: InputDeviceType::Keyboard,
            open_files: default_keyboard_device_opened_files,
            inspect_status: device_inspect_status,
        };

        loop {
            let next_event_future = local_listener_stream.next();

            local_resume_event.as_ref().map(clear_wake_proxy_signal);

            match next_event_future.await {
                Some(Ok(fuipolicy::MediaButtonsListenerRequest::OnEvent { event, responder })) => {
                    let batch =
                        parse_fidl_button_event(&event, power_was_pressed, function_was_pressed);

                    power_was_pressed = batch.power_is_pressed;
                    function_was_pressed = batch.function_is_pressed;

                    let (converted_events, ignored_events, generated_events) = match batch
                        .events
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

                    let mut devs = self.devices.lock();

                    let dev = match event.device_id {
                        Some(device_id) => {
                            devs.get_mut(&device_id).unwrap_or(&mut default_keyboard_device)
                        }
                        None => &default_keyboard_device,
                    };

                    if let Some(dev_inspect_status) = &dev.inspect_status {
                        dev_inspect_status.count_total_received_events(1);
                        dev_inspect_status.count_total_ignored_events(ignored_events);
                        dev_inspect_status.count_total_converted_events(converted_events);
                        dev_inspect_status.count_total_generated_events(
                            generated_events,
                            batch.event_time.into_nanos().try_into().unwrap(),
                        );
                    } else {
                        log_warn!("unable to record inspect for button device");
                    }

                    dev.open_files.lock().retain(|f| {
                        let Some(file) = f.upgrade() else {
                            log_warn!("Dropping input file for buttons that failed to upgrade");
                            return false;
                        };
                        match &file.inspect_status {
                            Some(file_inspect_status) => {
                                file_inspect_status.count_received_events(1);
                                file_inspect_status.count_ignored_events(ignored_events);
                                file_inspect_status.count_converted_events(converted_events);
                            }
                            None => {
                                log_warn!("unable to record inspect within the input file")
                            }
                        }
                        if !batch.events.is_empty() {
                            if let Some(file_inspect_status) = &file.inspect_status {
                                file_inspect_status.count_generated_events(
                                    generated_events,
                                    batch.event_time.into_nanos().try_into().unwrap(),
                                );
                            }
                            let mut inner = file.inner.lock();
                            inner.events.extend(batch.events.clone());
                            inner.waiters.notify_fd_events(FdEvents::POLLIN);
                        }

                        true
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

    fn start_mouse_relay(
        self: &Arc<Self>,
        kernel: &Kernel,
        event_proxy_mode: EventProxyMode,
        mouse_source_client_end: ClientEnd<fuipointer::MouseSourceMarker>,
        default_mouse_device_opened_files: OpenedFiles,
    ) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                slf.run_mouse_relay(
                    event_proxy_mode,
                    mouse_source_client_end,
                    default_mouse_device_opened_files,
                )
                .await;
            })
        });
    }

    async fn run_mouse_relay(
        self: Arc<Self>,
        event_proxy_mode: EventProxyMode,
        mouse_source_client_end: ClientEnd<fuipointer::MouseSourceMarker>,
        default_mouse_device_opened_files: Arc<Mutex<Vec<Weak<InputFile>>>>,
    ) {
        let default_mouse_device = DeviceState {
            device_type: InputDeviceType::Mouse,
            open_files: default_mouse_device_opened_files,
            inspect_status: None,
        };
        let (mouse_source_proxy, resume_event) = match event_proxy_mode {
            EventProxyMode::WakeContainer => {
                // Proxy the mouse events through the Starnix runner. This allows mouse events to
                // wake the container when it is suspended.
                let (mouse_source_channel, resume_event) = create_proxy_for_wake_events(
                    mouse_source_client_end.into_channel(),
                    "mouse".to_string(),
                );
                (
                    fuipointer::MouseSourceProxy::new(fidl::AsyncChannel::from_channel(
                        mouse_source_channel,
                    )),
                    Some(resume_event),
                )
            }
            EventProxyMode::None => (mouse_source_client_end.into_proxy(), None),
        };
        loop {
            // Create the future to watch for the the next input events, but don't execute
            // it...
            let event_future = mouse_source_proxy.watch();

            // .. until the event that we passed to the runner has been cleared. This prevents
            // the container from suspending between calls to `watch`.
            resume_event.as_ref().map(clear_wake_proxy_signal);

            match event_future.await {
                Ok(mouse_events) => {
                    let mut new_events: VecDeque<uapi::input_event> = VecDeque::new();
                    let mut last_event_time_ns = zx::MonotonicInstant::get();
                    for event in mouse_events {
                        match event {
                            FidlMouseEvent {
                                timestamp: Some(time),
                                pointer_sample:
                                    Some(MousePointerSample { scroll_v: Some(ticks), .. }),
                                ..
                            } => {
                                // Ensure this is a mouse wheel event, otherwise ignore.
                                if ticks != 0 {
                                    last_event_time_ns = zx::MonotonicInstant::from_nanos(time);
                                    new_events.push_back(uapi::input_event {
                                        time: timeval_from_time(last_event_time_ns),
                                        type_: uapi::EV_REL as u16,
                                        code: uapi::REL_WHEEL as u16,
                                        value: ticks as i32,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    if new_events.len() > 0 {
                        new_events.push_back(uapi::input_event {
                            // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
                            time: timeval_from_time(last_event_time_ns),
                            type_: uapi::EV_SYN as u16,
                            code: uapi::SYN_REPORT as u16,
                            value: 0,
                        });
                    }
                    default_mouse_device.open_files.lock().retain(|f| {
                        let Some(file) = f.upgrade() else {
                            log_warn!("Dropping input file for mouse that failed to upgrade");
                            return false;
                        };
                        if !new_events.is_empty() {
                            let mut inner = file.inner.lock();
                            inner.events.extend(new_events.clone());
                            inner.waiters.notify_fd_events(FdEvents::POLLIN);
                        }
                        true
                    });
                }
                Err(e) => {
                    log_warn!("error {:?} reading from MouseSourceProxy; input is stopped", e);
                    break;
                }
            }
        }
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
    use fidl_fuchsia_ui_input::MediaButtonsEvent;
    use fidl_fuchsia_ui_input3 as fuiinput;
    use fuipointer::{
        EventPhase, MouseEvent, TouchEvent, TouchInteractionId, TouchPointerSample, TouchResponse,
        TouchSourceMarker, TouchSourceRequest, TouchSourceRequestStream,
    };
    use starnix_core::task::CurrentTask;
    use starnix_core::testing::create_kernel_task_and_unlocked;
    use starnix_core::vfs::{FileHandle, FileObject, VecOutputBuffer};
    use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked};
    use starnix_types::time::timeval_from_time;
    use starnix_uapi::errors::{Errno, EAGAIN};
    use starnix_uapi::input_id;
    use starnix_uapi::open_flags::OpenFlags;
    use zerocopy::FromBytes as _;

    const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

    fn start_input_relays(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
    ) -> (
        Arc<InputEventsRelay>,
        Arc<InputDevice>,
        Arc<InputDevice>,
        Arc<InputDevice>,
        FileHandle,
        FileHandle,
        FileHandle,
        TouchSourceRequestStream,
        fuiinput::KeyboardRequestStream,
        fuipointer::MouseSourceRequestStream,
        fuipolicy::DeviceListenerRegistryRequestStream,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();

        let touch_device = InputDevice::new_touch(700, 1200, inspector.root());
        let touch_file =
            touch_device.open_test(locked, current_task).expect("Failed to create input file");

        let keyboard_device = InputDevice::new_keyboard(inspector.root());
        let keyboard_file =
            keyboard_device.open_test(locked, current_task).expect("Failed to create input file");

        let mouse_device = InputDevice::new_mouse(inspector.root());
        let mouse_file =
            mouse_device.open_test(locked, current_task).expect("Failed to create input file");

        let (touch_source_client_end, touch_source_stream) =
            fidl::endpoints::create_request_stream::<TouchSourceMarker>();
        let (mouse_source_client_end, mouse_stream) =
            fidl::endpoints::create_request_stream::<fuipointer::MouseSourceMarker>();
        let (keyboard_proxy, keyboard_stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<fuiinput::KeyboardMarker>();
        let view_ref_pair =
            fuchsia_scenic::ViewRefPair::new().expect("Failed to create ViewRefPair");
        let (device_registry_proxy, device_listener_stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<fuipolicy::DeviceListenerRegistryMarker>(
            );

        let relay = InputEventsRelay::new();
        relay.start_relays(
            &current_task.kernel(),
            EventProxyMode::None,
            touch_source_client_end,
            keyboard_proxy,
            mouse_source_client_end,
            view_ref_pair.view_ref,
            device_registry_proxy,
            touch_device.open_files.clone(),
            keyboard_device.open_files.clone(),
            mouse_device.open_files.clone(),
            Some(touch_device.inspect_status.clone()),
            Some(keyboard_device.inspect_status.clone()),
        );

        (
            relay,
            touch_device,
            keyboard_device,
            mouse_device,
            touch_file,
            keyboard_file,
            mouse_file,
            touch_source_stream,
            keyboard_stream,
            mouse_stream,
            device_listener_stream,
        )
    }

    // Waits for a `Watch()` request to arrive on `request_stream`, and responds with
    // `touch_event`. Returns the arguments to the `Watch()` call.
    async fn answer_next_touch_watch_request(
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

    // Waits for a `Watch()` request to arrive on `request_stream`, and responds with
    // `mouse_events`.
    async fn answer_next_mouse_watch_request(
        request_stream: &mut fuipointer::MouseSourceRequestStream,
        mouse_events: Vec<MouseEvent>,
    ) {
        match request_stream.next().await {
            Some(Ok(fuipointer::MouseSourceRequest::Watch { responder })) => {
                responder.send(&mouse_events).expect("failure sending Watch reply");
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
        make_touch_event_with_phase_device_id_position(phase, pointer_id, device_id, 0.0, 0.0)
    }

    fn make_touch_event_with_phase_device_id_position(
        phase: EventPhase,
        pointer_id: u32,
        device_id: u32,
        x: f32,
        y: f32,
    ) -> TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([x, y]),
                phase: Some(phase),
                interaction: Some(TouchInteractionId { pointer_id, device_id, interaction_id: 0 }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_mouse_wheel_event(scroll_v_ticks: i64, device_id: u32) -> MouseEvent {
        MouseEvent {
            timestamp: Some(0),
            pointer_sample: Some(MousePointerSample {
                device_id: Some(device_id),
                scroll_v: Some(scroll_v_ticks),
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

    fn create_test_touch_device(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        input_relay: Arc<InputEventsRelay>,
        device_id: u32,
    ) -> FileHandle {
        let open_files: OpenedFiles = Arc::new(Mutex::new(vec![]));
        input_relay.add_touch_device(device_id, open_files.clone(), None);
        let device_file = Arc::new(InputFile::new_touch(
            input_id { bustype: 0, vendor: 0, product: 0, version: 0 },
            1000,
            1000,
            None,
        ));
        open_files.lock().push(Arc::downgrade(&device_file));

        FileObject::new(
            &current_task,
            Box::new(device_file),
            current_task
                .lookup_path_from_root(locked, ".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed")
    }

    fn make_uapi_input_event(ty: u32, code: u32, value: i32) -> uapi::input_event {
        uapi::input_event {
            time: timeval_from_time(zx::MonotonicInstant::from_nanos(0)),
            type_: ty as u16,
            code: code as u16,
            value,
        }
    }

    #[::fuchsia::test]
    async fn route_touch_event_by_device_id() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            input_relay,
            _touch_device,
            _keyboard_device,
            _mouse_device,
            input_file,
            _keyboard_file,
            _mouse_file,
            mut touch_source_stream,
            _keyboard_stream,
            _mouse_source_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        const DEVICE_ID: u32 = 10;

        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase_device_id(EventPhase::Add, 1, DEVICE_ID)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s.
        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_empty_touch_event(DEVICE_ID)],
        )
        .await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        // Default device receive events because no matched device.
        assert_ne!(events.len(), 0);

        // add a device, mock uinput.
        let device_id_10_file =
            create_test_touch_device(&mut locked, &current_task, input_relay.clone(), DEVICE_ID);

        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase_device_id(EventPhase::Add, 1, DEVICE_ID)],
        )
        .await;

        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_empty_touch_event(DEVICE_ID)],
        )
        .await;

        let events = read_uapi_events(&mut locked, &input_file, &current_task);
        // Default device should not receive events because they matched device id 10.
        assert_eq!(events.len(), 0);

        let events = read_uapi_events(&mut locked, &device_id_10_file, &current_task);
        // file of device id 10 should receive events.
        assert_ne!(events.len(), 0);
    }

    #[::fuchsia::test]
    async fn route_touch_event_by_device_id_multi_device_events_in_one_sequence() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            input_relay,
            _touch_device,
            _keyboard_device,
            _mouse_device,
            _input_file,
            _keyboard_file,
            _mouse_file,
            mut touch_source_stream,
            _keyboard_stream,
            _mouse_source_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        const DEVICE_ID_10: u32 = 10;
        const DEVICE_ID_11: u32 = 11;

        let device_id_10_file =
            create_test_touch_device(&mut locked, &current_task, input_relay.clone(), DEVICE_ID_10);

        let device_id_11_file =
            create_test_touch_device(&mut locked, &current_task, input_relay.clone(), DEVICE_ID_11);

        // 2 pointer down on different touch device.
        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![
                make_touch_event_with_phase_device_id_position(
                    EventPhase::Add,
                    1,
                    DEVICE_ID_10,
                    10.0,
                    20.0,
                ),
                make_touch_event_with_phase_device_id_position(
                    EventPhase::Add,
                    2,
                    DEVICE_ID_11,
                    30.0,
                    40.0,
                ),
            ],
        )
        .await;

        answer_next_touch_watch_request(&mut touch_source_stream, vec![]).await;

        let events_10 = read_uapi_events(&mut locked, &device_id_10_file, &current_task);
        let events_11 = read_uapi_events(&mut locked, &device_id_11_file, &current_task);
        assert_eq!(events_10.len(), events_11.len());

        assert_eq!(
            events_10,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );

        assert_eq!(
            events_11,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 30),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 40),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
    }

    #[::fuchsia::test]
    async fn route_key_event_by_device_id() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            input_relay,
            _touch_device,
            _keyboard_device,
            _mouse_device,
            _touch_file,
            keyboard_file,
            _mouse_file,
            _touch_source_stream,
            mut keyboard_stream,
            _mouse_source_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        const DEVICE_ID: u32 = 10;

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy()
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fidl_fuchsia_input::Key::A),
            device_id: Some(DEVICE_ID),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;

        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        // Default device should receive events because no device device id is 10.
        assert_ne!(events.len(), 0);

        // add a device, mock uinput.
        let open_files: OpenedFiles = Arc::new(Mutex::new(vec![]));
        input_relay.add_keyboard_device(DEVICE_ID, open_files.clone(), None);
        let device_id_10_file = Arc::new(InputFile::new_keyboard(
            input_id { bustype: 0, vendor: 0, product: 0, version: 0 },
            None,
        ));
        open_files.lock().push(Arc::downgrade(&device_id_10_file));
        let device_id_10_file_object = FileObject::new(
            &current_task,
            Box::new(device_id_10_file),
            current_task
                .lookup_path_from_root(&mut locked, ".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");

        let _ = keyboard_listener.on_key_event(&key_event).await;

        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        // Default device should not receive events because they matched device id 10.
        assert_eq!(events.len(), 0);

        let events = read_uapi_events(&mut locked, &device_id_10_file_object, &current_task);
        // file of device id 10 should receive events.
        assert_ne!(events.len(), 0);

        std::mem::drop(keyboard_stream); // Close Zircon channel.
        std::mem::drop(keyboard_listener); // Close Zircon channel.
    }

    #[::fuchsia::test]
    async fn route_button_event_by_device_id() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            input_relay,
            _touch_device,
            _keyboard_device,
            _mouse_device,
            _touch_file,
            keyboard_file,
            _mouse_file,
            _touch_source_stream,
            _keyboard_stream,
            _mouse_source_stream,
            mut device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        const DEVICE_ID: u32 = 10;

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy()
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let power_pressed_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            device_id: Some(DEVICE_ID),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_pressed_event).await;

        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        // Default device should receive events because no device device id is 10.
        assert_ne!(events.len(), 0);

        // add a device, mock uinput.
        let open_files: OpenedFiles = Arc::new(Mutex::new(vec![]));
        input_relay.add_keyboard_device(DEVICE_ID, open_files.clone(), None);
        let device_id_10_file = Arc::new(InputFile::new_keyboard(
            input_id { bustype: 0, vendor: 0, product: 0, version: 0 },
            None,
        ));
        open_files.lock().push(Arc::downgrade(&device_id_10_file));
        let device_id_10_file_object = FileObject::new(
            &current_task,
            Box::new(device_id_10_file),
            current_task
                .lookup_path_from_root(&mut locked, ".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");

        let power_released_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(false),
            function: Some(false),
            device_id: Some(DEVICE_ID),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_released_event).await;

        let events = read_uapi_events(&mut locked, &keyboard_file, &current_task);
        // Default device should not receive events because they matched device id 10.
        assert_eq!(events.len(), 0);

        let events = read_uapi_events(&mut locked, &device_id_10_file_object, &current_task);
        // file of device id 10 should receive events.
        assert_ne!(events.len(), 0);

        std::mem::drop(buttons_listener); // Close Zircon channel.
        std::mem::drop(device_listener_stream); // Close Zircon channel.
    }

    #[::fuchsia::test]
    async fn touch_device_multi_reader() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            _input_relay,
            touch_device,
            _keyboard_device,
            _mouse_device,
            touch_reader1,
            _keyboard_file,
            _mouse_file,
            mut touch_source_stream,
            _keyboard_stream,
            _mouse_source_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        let touch_reader2 = touch_device
            .open_test(&mut locked, &current_task)
            .expect("Failed to create input file");

        const DEVICE_ID: u32 = 10;

        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_touch_event_with_phase_device_id(EventPhase::Add, 1, DEVICE_ID)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `TouchEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s.
        answer_next_touch_watch_request(
            &mut touch_source_stream,
            vec![make_empty_touch_event(DEVICE_ID)],
        )
        .await;

        // Consume all of the `uapi::input_event`s that are available.
        let events_from_reader1 = read_uapi_events(&mut locked, &touch_reader1, &current_task);
        let events_from_reader2 = read_uapi_events(&mut locked, &touch_reader2, &current_task);
        assert_ne!(events_from_reader1.len(), 0);
        assert_eq!(events_from_reader1.len(), events_from_reader2.len());
    }

    #[::fuchsia::test]
    async fn keyboard_device_multi_reader() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            _input_relay,
            _touch_device,
            keyboard_device,
            _mouse_device,
            _touch_file,
            keyboard_reader1,
            _mouse_file,
            _touch_source_stream,
            mut keyboard_stream,
            _mouse_source_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        let keyboard_reader2 = keyboard_device
            .open_test(&mut locked, &current_task)
            .expect("Failed to create input file");

        const DEVICE_ID: u32 = 10;

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy()
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fidl_fuchsia_input::Key::A),
            device_id: Some(DEVICE_ID),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events_from_reader1 = read_uapi_events(&mut locked, &keyboard_reader1, &current_task);
        let events_from_reader2 = read_uapi_events(&mut locked, &keyboard_reader2, &current_task);
        assert_ne!(events_from_reader1.len(), 0);
        assert_eq!(events_from_reader1.len(), events_from_reader2.len());
    }

    #[::fuchsia::test]
    async fn media_button_device_multi_reader() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            _input_relay,
            _touch_device,
            keyboard_device,
            _mouse_device,
            _touch_file,
            keyboard_reader1,
            _mouse_file,
            _touch_source_stream,
            _keyboard_stream,
            _mouse_source_stream,
            mut device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        let keyboard_reader2 = keyboard_device
            .open_test(&mut locked, &current_task)
            .expect("Failed to create input file");

        const DEVICE_ID: u32 = 10;

        let power_pressed_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            function: Some(false),
            device_id: Some(DEVICE_ID),
            ..Default::default()
        };

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy()
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let _ = buttons_listener.on_event(&power_pressed_event).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events_from_reader1 = read_uapi_events(&mut locked, &keyboard_reader1, &current_task);
        let events_from_reader2 = read_uapi_events(&mut locked, &keyboard_reader2, &current_task);
        assert_ne!(events_from_reader1.len(), 0);
        assert_eq!(events_from_reader1.len(), events_from_reader2.len());
    }

    #[::fuchsia::test]
    async fn mouse_device_multi_reader() {
        // Set up resources.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (
            _input_relay,
            _touch_device,
            _keyboard_device,
            mouse_device,
            _touch_file,
            _keyboard_file,
            mouse_reader1,
            _touch_stream,
            _keyboard_stream,
            mut mouse_stream,
            _device_listener_stream,
        ) = start_input_relays(&mut locked, &current_task);

        let mouse_reader2 = mouse_device
            .open_test(&mut locked, &current_task)
            .expect("Failed to create input file");

        const DEVICE_ID: u32 = 10;

        answer_next_mouse_watch_request(
            &mut mouse_stream,
            vec![make_mouse_wheel_event(1, DEVICE_ID)],
        )
        .await;

        // Wait for another `Watch` to ensure input_file done processing the first reply.
        // Use an empty `MouseEvent`, to minimize the chance that this event creates unexpected
        // `uapi::input_event`s.
        answer_next_mouse_watch_request(
            &mut mouse_stream,
            vec![make_mouse_wheel_event(0, DEVICE_ID)],
        )
        .await;

        // Consume all of the `uapi::input_event`s that are available.
        let events_from_reader1 = read_uapi_events(&mut locked, &mouse_reader1, &current_task);
        let events_from_reader2 = read_uapi_events(&mut locked, &mouse_reader2, &current_task);
        assert_ne!(events_from_reader1.len(), 0);
        assert_eq!(events_from_reader1.len(), events_from_reader2.len());
    }
}
