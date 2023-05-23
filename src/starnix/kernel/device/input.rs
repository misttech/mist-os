// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{
    framebuffer_server::{IMAGE_HEIGHT, IMAGE_WIDTH},
    DeviceOps,
};
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::lock::Mutex;
use crate::logging::*;
use crate::mm::MemoryAccessorExt;
use crate::syscalls::SyscallResult;
use crate::syscalls::SUCCESS;
use crate::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use crate::types::*;

use fidl::endpoints::Proxy as _; // for `on_closed()`
use fidl::handle::fuchsia_handles::Signals;
use fidl_fuchsia_ui_pointer::{
    self as fuipointer, EventPhase as FidlEventPhase, TouchEvent as FidlTouchEvent,
    TouchResponse as FidlTouchResponse, TouchResponseType,
};
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::future::{self, Either};
use std::collections::VecDeque;
use std::sync::Arc;
use zerocopy::AsBytes as _; // for `as_bytes()`

pub struct InputFile {
    driver_version: u32,
    input_id: uapi::input_id,
    supported_keys: BitSet<{ min_bytes(KEY_CNT) }>,
    supported_position_attributes: BitSet<{ min_bytes(ABS_CNT) }>, // ABSolute position
    supported_motion_attributes: BitSet<{ min_bytes(REL_CNT) }>,   // RELative motion
    supported_switches: BitSet<{ min_bytes(SW_CNT) }>,
    supported_leds: BitSet<{ min_bytes(LED_CNT) }>,
    supported_haptics: BitSet<{ min_bytes(FF_CNT) }>, // 'F'orce 'F'eedback
    supported_misc_features: BitSet<{ min_bytes(MSC_CNT) }>,
    properties: BitSet<{ min_bytes(INPUT_PROP_CNT) }>,
    x_axis_info: uapi::input_absinfo,
    y_axis_info: uapi::input_absinfo,
    inner: Mutex<InputFileMutableState>,
}

// Mutable state of `InputFile`
struct InputFileMutableState {
    events: VecDeque<uapi::input_event>,
    waiters: WaitQueue,
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

impl InputFile {
    // Per https://www.linuxjournal.com/article/6429, the driver version is 32-bits wide,
    // and interpreted as:
    // * [31-16]: version
    // * [15-08]: minor
    // * [07-00]: patch level
    const DRIVER_VERSION: u32 = 0;

    // Per https://www.linuxjournal.com/article/6429, the bus type should be populated with a
    // sensible value, but other fields may not be.
    const INPUT_ID: uapi::input_id =
        uapi::input_id { bustype: BUS_VIRTUAL as u16, product: 0, vendor: 0, version: 0 };

    /// Creates an `InputFile` instance suitable for emulating a touchscreen.
    pub fn new() -> Arc<Self> {
        // Fuchsia scales the position reported by the touch sensor to fit view coordinates.
        // Hence, the range of touch positions is exactly the same as the range of view
        // coordinates.
        Self::new_with_dimensions(IMAGE_WIDTH as u16, IMAGE_HEIGHT as u16)
    }

    // Create an `InputFile` with sensor axes having ranges of `(0, x_max)`, and `(0, y_max)`.
    //
    // Note: the parameters are u16 instead of something larger because:
    // * `uapi::maximum` is `i32`, so `u32` would allow out-of-range values.
    // * `u16` is sufficient for the displays and sensors used in practice.
    pub fn new_with_dimensions(x_max: u16, y_max: u16) -> Arc<Self> {
        Arc::new(Self {
            driver_version: Self::DRIVER_VERSION,
            input_id: Self::INPUT_ID,
            supported_keys: touch_key_attributes(),
            supported_position_attributes: touch_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: touch_properties(),
            x_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(x_max),
                // TODO(https://fxbug.dev/124595): `value` field should contain the most recent
                // X position.
                ..uapi::input_absinfo::default()
            },
            y_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(y_max),
                // TODO(https://fxbug.dev/124595): `value` field should contain the most recent
                // Y position.
                ..uapi::input_absinfo::default()
            },
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
            }),
        })
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
    pub fn start_relay(
        self: &Arc<Self>,
        touch_source_proxy: fuipointer::TouchSourceProxy,
    ) -> std::thread::JoinHandle<()> {
        let slf = self.clone();
        std::thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let mut previous_event_disposition = vec![];
                // TODO(https://fxbug.dev/123718): Remove `close_fut`.
                let mut close_fut = touch_source_proxy.on_closed();
                loop {
                    let query_fut = touch_source_proxy.watch(&previous_event_disposition);
                    let query_res = match future::select(close_fut, query_fut).await {
                        Either::Left((Ok(Signals::CHANNEL_PEER_CLOSED), _)) => {
                            log_warn!("TouchSource server closed connection; input is stopped");
                            break;
                        }
                        Either::Left((Ok(signals), _))
                            if signals
                                == (Signals::CHANNEL_PEER_CLOSED | Signals::CHANNEL_READABLE) =>
                        {
                            log_warn!(
                                "{} {}",
                                "TouchSource server closed connection and channel is readable",
                                "input dropped event(s) and stopped"
                            );
                            break;
                        }
                        Either::Left((on_closed_res, _)) => {
                            log_warn!(
                                "on_closed() resolved with unexpected value {:?}; input is stopped",
                                on_closed_res
                            );
                            break;
                        }
                        Either::Right((query_res, pending_close_fut)) => {
                            close_fut = pending_close_fut;
                            query_res
                        }
                    };
                    match query_res {
                        Ok(touch_events) => {
                            previous_event_disposition =
                                touch_events.iter().map(make_response_for_fidl_event).collect();
                            let new_events = touch_events
                                .iter()
                                .filter_map(parse_fidl_touch_event)
                                // `flat`: each FIDL event yields a `Vec<uapi::input_event>`,
                                // but the end result should be a single vector of UAPI events,
                                // not a `Vec<Vec<uapi::input_event>>`.
                                .flat_map(make_uapi_events);
                            let mut inner = slf.inner.lock();
                            // TODO(https://fxbug.dev/124597): Reading from an `InputFile` should
                            // not provide access to events that occurred before the file was
                            // opened.
                            inner.events.extend(new_events);
                            // TODO(https://fxbug.dev/124598): Skip notify if `inner.events`
                            // is empty.
                            inner.waiters.notify_fd_events(FdEvents::POLLIN);
                        }
                        Err(e) => {
                            log_warn!(
                                "error {:?} reading from TouchSourceProxy; input is stopped",
                                e
                            );
                            break;
                        }
                    };
                }
            })
        })
    }
}

impl DeviceOps for Arc<InputFile> {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Arc::clone(self)))
    }
}

impl FileOps for Arc<InputFile> {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::EVIOCGVERSION => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.driver_version)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGID => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.input_id)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_KEY => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_keys.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_ABS => {
                current_task.mm.write_object(
                    UserRef::new(user_addr),
                    &self.supported_position_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_REL => {
                current_task.mm.write_object(
                    UserRef::new(user_addr),
                    &self.supported_motion_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_SW => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_switches.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_LED => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_leds.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_FF => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_haptics.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_MSC => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_misc_features.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGPROP => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.properties.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_X => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.x_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_Y => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.y_axis_info)?;
                Ok(SUCCESS)
            }
            _ => {
                not_implemented!("ioctl() {} on input device", request);
                error!(EOPNOTSUPP)
            }
        }
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        let event = self.inner.lock().events.pop_front();
        match event {
            Some(event) => {
                // TODO(https://fxbug.dev/124600): Consider sending as many events as will fit
                // in `data`, instead of sending them one at a time.
                data.write_all(event.as_bytes())
            }
            // TODO(https://fxbug.dev/124602): `EAGAIN` is only permitted if the file is opened
            // with `O_NONBLOCK`. Figure out what to do if the file is opened without that flag.
            None => {
                log_info!("read() returning EAGAIN");
                error!(EAGAIN)
            }
        }
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        not_implemented!("write() on input device");
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.inner.lock().waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        if self.inner.lock().events.is_empty() {
            FdEvents::empty()
        } else {
            FdEvents::POLLIN
        }
    }
}

struct BitSet<const NUM_BYTES: usize> {
    bytes: [u8; NUM_BYTES],
}

impl<const NUM_BYTES: usize> BitSet<{ NUM_BYTES }> {
    const fn new() -> Self {
        Self { bytes: [0; NUM_BYTES] }
    }

    fn set(&mut self, bitnum: u32) {
        let bitnum = bitnum as usize;
        let byte = bitnum / 8;
        let bit = bitnum % 8;
        self.bytes[byte] |= 1 << bit;
    }
}

/// Returns the minimum number of bytes required to store `n_bits` bits.
const fn min_bytes(n_bits: u32) -> usize {
    ((n_bits as usize) + 7) / 8
}

/// Returns appropriate `KEY`-board related flags for a touchscreen device.
fn touch_key_attributes() -> BitSet<{ min_bytes(KEY_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(BTN_TOUCH);
    attrs.set(BTN_TOOL_FINGER);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a touchscreen device.
fn touch_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(ABS_X);
    attrs.set(ABS_Y);
    attrs
}

/// Returns appropriate `INPUT_PROP`-erties for a touchscreen device.
fn touch_properties() -> BitSet<{ min_bytes(INPUT_PROP_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(INPUT_PROP_DIRECT);
    attrs
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
        _ => None, // TODO(https://fxbug.dev/124603): Add some inspect counters of ignored events.
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
            // TODO(https://fxbug.dev/124606): Reporting `BTN_TOOL_FINGER` here could cause some
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
        // TODO(https://fxbug.dev/124607): Figure out whether this is correct.
        FidlEventPhase::Cancel => None,
    }
}

#[cfg(test)]
mod test {
    #![allow(clippy::unused_unit)] // for compatibility with `test_case`
    #![allow(clippy::manual_range_contains)] // for compatibility with `assert_near`

    use super::*;
    use crate::fs::buffers::VecOutputBuffer;
    use crate::task::Kernel;
    use crate::testing::{create_kernel_and_task, map_memory};
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use fuchsia_zircon as zx;
    use fuipointer::{
        EventPhase, TouchEvent, TouchPointerSample, TouchResponse, TouchSourceMarker,
        TouchSourceRequest,
    };
    use futures::StreamExt as _; // for `next()`
    use test_case::test_case;
    use test_util::assert_near;
    use zerocopy::FromBytes as _; // for `read_from()`

    const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

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

    fn start_input(
    ) -> (Arc<InputFile>, fuipointer::TouchSourceRequestStream, std::thread::JoinHandle<()>) {
        let input_file = InputFile::new();
        let (touch_source_proxy, touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);
        (input_file, touch_source_stream, relay_thread)
    }

    fn make_kernel_objects(file: Arc<InputFile>) -> (Arc<Kernel>, CurrentTask, Arc<FileObject>) {
        let (kernel, current_task) = create_kernel_and_task();
        let file_object = FileObject::new(
            Box::new(file),
            // The input node doesn't really live at the root of the filesystem.
            // But the test doesn't need to be 100% representative of production.
            current_task
                .lookup_path_from_root(b".")
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        );
        (kernel, current_task, file_object)
    }

    fn read_uapi_events(
        file: Arc<InputFile>,
        file_object: &FileObject,
        task: &CurrentTask,
    ) -> Vec<uapi::input_event> {
        std::iter::from_fn(|| {
            let mut event_bytes = VecOutputBuffer::new(INPUT_EVENT_SIZE);
            match file.read(file_object, task, 0, &mut event_bytes) {
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
        // Set up resources.
        let (_input_file, mut touch_source_stream, relay_thread) = start_input();

        // Verify that the watch request has empty `responses`.
        assert_matches!(
            touch_source_stream.next().await,
            Some(Ok(TouchSourceRequest::Watch { responses, .. }))
                => assert_eq!(responses.as_slice(), [])
        );

        // Cleanly tear down the client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("client thread failed"); // Wait for client thread to finish.
    }

    #[::fuchsia::test]
    async fn later_watch_requests_have_responses_arg_matching_earlier_watch_replies() {
        // Set up resources.
        let (_input_file, mut touch_source_stream, relay_thread) = start_input();

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

        // Cleanly tear down the client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("client thread failed"); // Wait for client thread to finish.
    }

    #[::fuchsia::test]
    async fn notifies_polling_waiters_of_new_data() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().for_each(|waiter| {
            input_file.wait_async(
                &file_object,
                &current_task,
                waiter,
                FdEvents::POLLIN,
                Box::new(|_| ()),
            );
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::Time::ZERO), Err(_));

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_eq!(waiter1.wait_until(&current_task, zx::Time::ZERO), Ok(()));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[::fuchsia::test]
    async fn notifies_blocked_waiter_of_new_data() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let waiter = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify `waiter` when data is available to read.
        input_file.wait_async(
            &file_object,
            &current_task,
            &waiter,
            FdEvents::POLLIN,
            Box::new(|_| ()),
        );

        let waiter_thread = std::thread::spawn(move || waiter.wait(&current_task));
        assert!(!waiter_thread.is_finished());

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        //
        // TODO(https://fxbug.dev/124609): Without this, `relay_thread` gets stuck `await`-ing
        // the reply to its first request. Figure out why that happens, and remove this second
        // reply.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Block until `waiter_thread` completes.
        waiter_thread.join().expect("join() failed").expect("wait() failed");

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
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
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify `waiter` when data is available to read.
        let waitkeys = [&waiter1, &waiter2]
            .iter()
            .map(|waiter| {
                input_file
                    .wait_async(
                        &file_object,
                        &current_task,
                        waiter,
                        FdEvents::POLLIN,
                        Box::new(|_| ()),
                    )
                    .expect("wait_async")
            })
            .collect::<Vec<_>>();

        // Cancel wait for `waiter1`.
        waitkeys.into_iter().next().expect("failed to get first waitkey").cancel();

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[::fuchsia::test]
    async fn query_events() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let (_kernel, current_task) = create_kernel_and_task();

        // Check initial expectation.
        assert_eq!(
            input_file.query_events(&current_task),
            FdEvents::empty(),
            "events should be empty before data arrives"
        );

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Check post-watch expectation.
        assert_eq!(
            input_file.query_events(&current_task),
            FdEvents::POLLIN,
            "events should be POLLIN after data arrives"
        );

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[test_case((1920, 1080), uapi::EVIOCGABS_X => (0, 1920))]
    #[test_case((1280, 1024), uapi::EVIOCGABS_Y => (0, 1024))]
    #[::fuchsia::test]
    async fn provides_correct_axis_ranges((x_max, y_max): (u16, u16), ioctl_op: u32) -> (i32, i32) {
        // Set up resources.
        let input_file = InputFile::new_with_dimensions(x_max, y_max);
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::input_absinfo>() as u64,
        );

        // Invoke ioctl() for axis details.
        input_file.ioctl(&file_object, &current_task, ioctl_op, address).expect("ioctl() failed");

        // Extract minimum and maximum fields for validation.
        let axis_info = current_task
            .mm
            .read_object::<uapi::input_absinfo>(UserRef::new(address))
            .expect("failed to read user memory");
        (axis_info.minimum, axis_info.maximum)
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
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(fidl_phase),
        )
        .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above. Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, TouchEvent::default()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.

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
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(EventPhase::Add),
        )
        .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

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

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[test_case((0.0, 0.0); "origin")]
    #[test_case((100.7, 200.7); "above midpoint")]
    #[test_case((100.3, 200.3); "below midpoint")]
    #[test_case((100.5, 200.5); "midpoint")]
    #[::fuchsia::test]
    async fn sends_acceptable_coordinates((x, y): (f32, f32)) {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event_with_coords(x, y))
            .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

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

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
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
        let (_input_file, mut touch_source_stream, relay_thread) = start_input();

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, event).await;

        // Get response to `event`.
        let responses =
            answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.

        // Return the value for `test_case` to match on.
        responses.get(0).cloned()
    }
}
