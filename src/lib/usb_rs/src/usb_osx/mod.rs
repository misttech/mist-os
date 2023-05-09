// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    DeviceDescriptor, DeviceEvent, DeviceHandle, Endpoint, EndpointDescriptor, EndpointDirection,
    EndpointType, Error, InterfaceDescriptor, Result,
};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::future::poll_fn;
use futures::task::AtomicWaker;
use futures::Stream;
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{ready, Context, Poll};

mod iokit_usb;
mod iokit_wrappers;

use iokit_wrappers::{
    ioreturn_check, DeviceInterface500, GetIOUSBDeviceUserClientTypeID,
    GetIOUSBInterfaceUserClientTypeID, IOService, InterfaceInterface500, MachPort, MatchingDict,
    NotifyPort, PlugInInterface, RunLoop,
};

pub struct DeviceHandleInner(IOService, Arc<RunLoop>);

impl DeviceHandleInner {
    pub fn debug_name(&self) -> String {
        format!("{:?}", self.0)
    }

    pub fn scan_interfaces(
        &self,
        f: impl Fn(&DeviceDescriptor, &InterfaceDescriptor) -> bool,
    ) -> Result<Interface> {
        let dev: DeviceInterface500 =
            PlugInInterface::new(self.0.clone(), GetIOUSBDeviceUserClientTypeID())?
                .query_interface()?;
        let device_descriptor = dev.descriptor();

        for iface in dev.iter_interfaces()? {
            let iface = PlugInInterface::new(iface, GetIOUSBInterfaceUserClientTypeID())?
                .query_interface::<Result<InterfaceInterface500>>()??;

            let Ok(interface_descriptor) = iface.descriptor() else {
                // TODO: Log something?
                continue;
            };

            if f(&device_descriptor, &interface_descriptor) {
                iface.register_with_event_loop(&self.1)?;
                return Ok(Interface {
                    inner: Arc::new(iface),
                    _run_loop: Arc::clone(&self.1),
                    endpoint_descriptors: interface_descriptor.endpoints,
                });
            }
        }

        Err(Error::InterfaceNotFound)
    }
}

/// Data passed to IOKit callbacks which drive `DeviceStream`.
struct CallbackData {
    event_sender: UnboundedSender<DeviceEvent>,
    run_loop: Weak<RunLoop>,
}

fn handle_event(
    refcon: *mut ::std::os::raw::c_void,
    iterator: iokit_usb::io_iterator_t,
    f: impl Fn(DeviceHandle) -> DeviceEvent,
) {
    // SAFETY: This is the standard callback data pattern. We registered a pointer with this type in
    // wait_for_devices.
    let weak_data = unsafe { Weak::from_raw(refcon.cast::<CallbackData>()) };
    let data = weak_data.upgrade();

    // Leak the weak reference so that this callback will still work next time around.
    // TODO: This means the control block leaks forever. Should probably find a way to have this
    // callback cancel itself on the last go.
    std::mem::forget(weak_data);

    let Some(data) = data else {
        return;
    };

    let Some(run_loop) = data.run_loop.upgrade() else {
        return;
    };

    // SAFETY: Documentation says IOIteratorNext should be resilient to invalid iterator values, so
    // should be no way for this to fail.
    let iter = std::iter::from_fn(move || unsafe { Some(iokit_usb::IOIteratorNext(iterator)) })
        .take_while(|&x| x != 0);

    for device in iter {
        let _ = data.event_sender.unbounded_send(f(DeviceHandleInner(
            IOService::from_raw(device),
            Arc::clone(&run_loop),
        )
        .into()));
    }
}

extern "C" fn added(refcon: *mut ::std::os::raw::c_void, iterator: iokit_usb::io_iterator_t) {
    handle_event(refcon, iterator, DeviceEvent::Added)
}

extern "C" fn removed(refcon: *mut ::std::os::raw::c_void, iterator: iokit_usb::io_iterator_t) {
    handle_event(refcon, iterator, DeviceEvent::Removed)
}

/// Waits for USB devices to appear on the bus.
pub fn wait_for_devices(notify_added: bool, notify_removed: bool) -> Result<DeviceStream> {
    let (event_sender, receiver) = unbounded();
    let run_loop = Arc::new(RunLoop::new());
    let callback_data =
        Arc::new(CallbackData { event_sender, run_loop: Arc::downgrade(&run_loop) });
    let master_port = MachPort::new_master()?;

    let matching_dict = MatchingDict::new_usb()?;
    let notify_port = master_port.new_notify();
    notify_port.add_source_to_run_loop(&run_loop);

    if notify_added {
        let data_ptr = Arc::downgrade(&callback_data).into_raw().cast_mut().cast();
        notify_port.add_matching_notification(
            iokit_usb::kIOFirstMatchNotification,
            matching_dict.clone(),
            Some(added),
            data_ptr,
        )?;
    }

    if notify_removed {
        let data_ptr = Arc::downgrade(&callback_data).into_raw().cast_mut().cast();
        notify_port.add_matching_notification(
            iokit_usb::kIOTerminatedNotification,
            matching_dict.clone(),
            Some(removed),
            data_ptr,
        )?;
    }

    Ok(DeviceStream {
        _notify_port: notify_port,
        _callback_data: callback_data,
        _run_loop: run_loop,
        receiver,
    })
}

pub struct DeviceStream {
    _notify_port: NotifyPort,
    _run_loop: Arc<RunLoop>,
    _callback_data: Arc<CallbackData>,
    receiver: UnboundedReceiver<DeviceEvent>,
}

impl Stream for DeviceStream {
    type Item = Result<DeviceEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(ready!(Pin::new(&mut self.receiver).poll_next(cx)).map(Ok))
    }
}

pub struct Interface {
    inner: Arc<InterfaceInterface500>,
    _run_loop: Arc<RunLoop>,
    endpoint_descriptors: Vec<EndpointDescriptor>,
}

impl Interface {
    pub fn endpoints(&self) -> impl std::iter::Iterator<Item = Endpoint> + '_ {
        self.endpoint_descriptors.iter().enumerate().map(|(i, ep)| {
            // 1-based index
            let idx = i + 1;

            match (ep.ty, ep.direction()) {
                (EndpointType::Bulk, EndpointDirection::In) => {
                    Endpoint::BulkIn(BulkInEndpoint { inner: Arc::clone(&self.inner), idx })
                }
                (EndpointType::Bulk, EndpointDirection::Out) => {
                    Endpoint::BulkOut(BulkOutEndpoint { inner: Arc::clone(&self.inner), idx })
                }
                (EndpointType::Control, _) => {
                    Endpoint::Control(ControlEndpoint(Arc::clone(&self.inner)))
                }
                (EndpointType::Interrupt, _) => {
                    Endpoint::Interrupt(InterruptEndpoint(Arc::clone(&self.inner)))
                }
                (EndpointType::Isochronous, _) => {
                    Endpoint::Isochronous(IsochronousEndpoint(Arc::clone(&self.inner)))
                }
            }
        })
    }
}

/// A landing pad for turning IOKit callbacks into futures waking up. Contains a waker and a
/// refcount for determining when the OS callbacks still might need this object.
struct CallbackResult {
    waker: AtomicWaker,
    refcount: AtomicU8,
    result: UnsafeCell<Option<Result<usize>>>,
}

/// A queue of references to CallbackResults in our pool which are available for use.
static FREE_CALLBACK_RESULTS: Mutex<VecDeque<&'static mut CallbackResult>> =
    Mutex::new(VecDeque::new());

impl CallbackResult {
    fn alloc() -> (CallbackResultRef, CallbackResultRef) {
        let item = {
            let mut queue = FREE_CALLBACK_RESULTS.lock().unwrap();
            if let Some(item) = queue.pop_front() {
                item
            } else {
                let mut allocated = Box::leak(
                    (0..8)
                        .map(|_| CallbackResult {
                            waker: AtomicWaker::new(),
                            refcount: AtomicU8::new(0),
                            result: UnsafeCell::new(None),
                        })
                        .collect::<Box<[_]>>(),
                )
                .iter_mut();
                let item = allocated.next().unwrap();
                allocated.for_each(|x| queue.push_back(x));
                item
            }
        };

        *item.result.get_mut() = None;
        item.refcount = AtomicU8::new(2);
        (CallbackResultRef(item), CallbackResultRef(item))
    }
}

/// Pointer to a `CallbackResult`. Decrements the internal refcount when dropped.
struct CallbackResultRef(*mut CallbackResult);

// SAFETY: CallbackResult is mostly atomic, except for the result field which we only manipulate
// when we know we have exclusive access.
unsafe impl Send for CallbackResultRef {}
unsafe impl Sync for CallbackResultRef {}

impl CallbackResultRef {
    fn take_ptr(mut self) -> *mut CallbackResult {
        let ret = self.0;
        self.0 = std::ptr::null_mut();
        ret
    }

    /// SAFETY: this function will mutably write to our result field without ensuring that we are
    /// the only reference, so we must know any other references will not touch that field in any
    /// way when we call this.
    unsafe fn complete(self, result: Result<usize>) {
        (*(*self.0).result.get() = Some(result));
    }

    async fn wait(self) -> Result<usize> {
        poll_fn(move |cx| {
            let this = &self;
            // SAFETY: The pointer should always be valid by construction.
            let count = unsafe {
                let this = this.0.as_ref().unwrap();
                this.waker.register(cx.waker());
                this.refcount.load(Ordering::Acquire)
            };
            if count > 1 {
                Poll::Pending
            } else {
                // SAFETY: The refcount is 1, so we're now the only reference, so we can take a
                // mutable reference to the internal pointer.
                let result = unsafe { (*(*this.0).result.get()).take() };
                Poll::Ready(result.expect("Callback destroyed without a reply"))
            }
        })
        .await
    }
}

impl Drop for CallbackResultRef {
    fn drop(&mut self) {
        // SAFETY: The pointer should always be valid by construction, unless take_ptr has nulled it
        // out.
        let Some(this) = (unsafe {self.0.as_ref()}) else {
            return;
        };
        let count = this.refcount.fetch_sub(1, Ordering::Release);
        if count == 1 {
            // SAFETY: Refcount is one so we're the only reference, and we can promote to mut.
            let this = unsafe { self.0.as_mut().unwrap() };
            // Make sure we're not holding an error value that's got strings or something
            // allocation-heavy inside.
            *this.result.get_mut() = None;
            FREE_CALLBACK_RESULTS.lock().unwrap().push_back(this);
        } else {
            this.waker.wake();
        }
    }
}

/// Callback invoked by bulk read/write calls made into IOKit. Completes the `CallbackResult` for
/// the request with a Result.
extern "C" fn iokit_callback(
    callback_res: *mut libc::c_void,
    res: iokit_usb::IOReturn,
    arg: *mut libc::c_void,
) {
    // SAFETY: complete writes to the internal value while other pointers exist. This is the
    // only place we should call it for this ref pair and the other ref should block until
    // our ref is destroyed before examining it, so we shouldn't violate mutability semantics.
    unsafe {
        CallbackResultRef(callback_res.cast()).complete(ioreturn_check(res).map(|_| arg as usize));
    }
}

pub struct BulkInEndpoint {
    inner: Arc<InterfaceInterface500>,
    idx: usize,
}

impl BulkInEndpoint {
    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let (res, callback_res) = CallbackResult::alloc();

        self.inner.read_pipe_async(
            buf,
            self.idx.try_into().expect("Corrupt endpoint index"),
            Some(iokit_callback),
            callback_res.take_ptr().cast(),
        )?;

        res.wait().await
    }
}

pub struct BulkOutEndpoint {
    inner: Arc<InterfaceInterface500>,
    idx: usize,
}

impl BulkOutEndpoint {
    pub async fn write(&self, buf: &[u8]) -> Result<()> {
        let (res, callback_res) = CallbackResult::alloc();

        self.inner.write_pipe_async(
            buf,
            self.idx.try_into().expect("Corrupt endpoint index"),
            Some(iokit_callback),
            callback_res.take_ptr().cast(),
        )?;

        let size = res.wait().await?;
        assert!(size <= buf.len(), "Wrote more than the buffer!");
        if size < buf.len() {
            Err(Error::ShortWrite(buf.len(), size))
        } else {
            Ok(())
        }
    }
}

pub struct ControlEndpoint(Arc<InterfaceInterface500>);
pub struct InterruptEndpoint(Arc<InterfaceInterface500>);
pub struct IsochronousEndpoint(Arc<InterfaceInterface500>);
