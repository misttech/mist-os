// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceOps;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::lock::Mutex;
use crate::logging::*;
use crate::mm::MemoryAccessorExt;
use crate::syscalls::SyscallResult;
use crate::syscalls::SUCCESS;
use crate::task::{CurrentTask, EventHandler, WaitKey, WaitQueue, Waiter};
use crate::types::*;

use fidl::endpoints::Proxy as _; // for `on_closed()`
use fidl::handle::fuchsia_handles::Signals;
use fidl_fuchsia_ui_pointer::{self as fuipointer, TouchEvent, TouchResponse, TouchResponseType};
use fuchsia_async as fasync;
use futures::future::{self, Either};
use std::collections::VecDeque;
use std::sync::Arc;

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
    inner: Mutex<InputFileMutableState>,
}

// Mutable state of `InputFile`
struct InputFileMutableState {
    events: VecDeque<TouchEvent>,
    waiters: WaitQueue,
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
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
            }),
        })
    }

    /// Starts reading events from the Fuchsia input system, and making those events available
    /// to the guest system.
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
                    let query_fut =
                        touch_source_proxy.watch(&mut previous_event_disposition.into_iter());
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
                            previous_event_disposition = touch_events
                                .iter()
                                .map(|event| TouchResponse {
                                    response_type: Some(TouchResponseType::Yes),
                                    trace_flow_id: event.trace_flow_id,
                                    ..TouchResponse::EMPTY
                                })
                                .collect();
                            let mut inner = slf.inner.lock();
                            inner.events.extend(touch_events);
                            inner.waiters.notify_events(FdEvents::POLLIN);
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
            _ => {
                not_implemented!(current_task, "ioctl() {} on input device", request);
                error!(EOPNOTSUPP)
            }
        }
    }

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "read() on input device");
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "write() on input device");
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitKey {
        self.inner.lock().waiters.wait_async_mask(waiter, events.bits(), handler)
    }

    fn cancel_wait(&self, _current_task: &CurrentTask, waiter: &Waiter, key: WaitKey) {
        self.inner.lock().waiters.cancel_wait(waiter, key);
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

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::create_kernel_and_task;
    use assert_matches::assert_matches;
    use fuchsia_zircon as zx;
    use fuipointer::{TouchEvent, TouchSourceMarker, TouchSourceRequest};
    use futures::StreamExt as _; // for `next()`

    #[::fuchsia::test()]
    async fn initial_watch_request_has_empty_responses_arg() {
        // Set up resources.
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);

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
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let fake_touch_events = std::iter::repeat(TouchEvent::EMPTY);
        let relay_thread = input_file.start_relay(touch_source_proxy);

        // Reply to first `Watch` with two `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => responder
                .send(&mut fake_touch_events.clone().take(2).collect::<Vec<_>>().into_iter())
                .expect("failure sending Watch reply"),
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Verify second `Watch` has two elements in `responses`.
        // Then reply with five `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                assert_matches!(responses.as_slice(), [_, _]);
                responder
                    .send(&mut fake_touch_events.clone().take(5).collect::<Vec<_>>().into_iter())
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
        // Set up input resources.
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);

        // Set up waiter resources.
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task) = create_kernel_and_task();

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().enumerate().for_each(|(_i, waiter)| {
            input_file.wait_async(
                &FileObject::new(
                    Box::new(input_file.clone()),
                    // The input node doesn't really live at the root of the filesystem.
                    // But the test doesn't need to be 100% representative of production.
                    current_task
                        .lookup_path_from_root(b".")
                        .expect("failed to get namespace node for root"),
                    OpenFlags::empty(),
                ),
                &current_task,
                waiter,
                FdEvents::POLLIN,
                Box::new(|_| ()),
            );
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::Time::ZERO), Err(_));

        // Reply to first `Watch` request.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending first Watch reply");
            }
            unexpected_request => panic!("unexpected first request {:?}", unexpected_request),
        }

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending second Watch reply");
            }
            unexpected_request => panic!("unexpected second request {:?}", unexpected_request),
        }

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
        // Set up input resources.
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);

        // Set up waiter resources.
        let waiter = Waiter::new();
        let (_kernel, current_task) = create_kernel_and_task();

        // Ask `input_file` to notify `waiter` when data is available to read.
        input_file.wait_async(
            &FileObject::new(
                Box::new(input_file.clone()),
                // The input node doesn't really live at the root of the filesystem.
                // But the test doesn't need to be 100% representative of production.
                current_task
                    .lookup_path_from_root(b".")
                    .expect("failed to get namespace node for root"),
                OpenFlags::empty(),
            ),
            &current_task,
            &waiter,
            FdEvents::POLLIN,
            Box::new(|_| ()),
        );

        let waiter_thread = std::thread::spawn(move || waiter.wait(&current_task));
        assert!(!waiter_thread.is_finished());

        // Reply to first `Watch` request.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending first Watch reply");
            }
            unexpected_request => panic!("unexpected first request {:?}", unexpected_request),
        }

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        //
        // TODO(quiche): Without this, `relay_thread` gets stuck `await`-ing the reply to its
        // first request. Figure out why that happens, and remove this second reply.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending second Watch reply");
            }
            unexpected_request => panic!("unexpected second request {:?}", unexpected_request),
        }

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
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);

        // Set up waiter resources.
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task) = create_kernel_and_task();

        // Ask `input_file` to notify `waiter` when data is available to read.
        let waitkeys = [&waiter1, &waiter2]
            .iter()
            .enumerate()
            .map(|(_i, waiter)| {
                input_file.wait_async(
                    &FileObject::new(
                        Box::new(input_file.clone()),
                        // The input node doesn't really live at the root of the filesystem.
                        // But the test doesn't need to be 100% representative of production.
                        current_task
                            .lookup_path_from_root(b".")
                            .expect("failed to get namespace node for root"),
                        OpenFlags::empty(),
                    ),
                    &current_task,
                    waiter,
                    FdEvents::POLLIN,
                    Box::new(|_| ()),
                )
            })
            .collect::<Vec<_>>();

        // Cancel wait for `waiter1`.
        input_file.cancel_wait(
            &current_task,
            &waiter1,
            waitkeys.into_iter().next().expect("failed to get first waitkey"),
        );

        // Reply to first `Watch` request.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending first Watch reply");
            }
            unexpected_request => panic!("unexpected first request {:?}", unexpected_request),
        }

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending second Watch reply");
            }
            unexpected_request => panic!("unexpected second request {:?}", unexpected_request),
        }

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
        let input_file = InputFile::new();
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_relay(touch_source_proxy);
        let (_kernel, current_task) = create_kernel_and_task();

        // Check initial expectation.
        assert_eq!(
            input_file.query_events(&current_task),
            FdEvents::empty(),
            "events should be empty before data arrives"
        );

        // Reply to first `Watch` request.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending first Watch reply");
            }
            unexpected_request => panic!("unexpected first request {:?}", unexpected_request),
        }

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => {
                responder
                    .send(&mut vec![TouchEvent::EMPTY].into_iter())
                    .expect("failure sending second Watch reply");
            }
            unexpected_request => panic!("unexpected second request {:?}", unexpected_request),
        }

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
}
