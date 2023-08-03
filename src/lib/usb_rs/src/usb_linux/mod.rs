// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod usbdevice_fs;
use usbdevice_fs::*;
mod discovery;
pub use discovery::{wait_for_devices, DeviceStream};

use futures::future::poll_fn;
use futures::task::AtomicWaker;
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::thread;
use zerocopy::Ref;

use crate::{
    DeviceDescriptor, Endpoint, EndpointDescriptor, EndpointDirection, EndpointType, Error,
    InterfaceDescriptor, Result,
};

#[derive(Debug)]
pub struct DeviceHandleInner(String);

impl DeviceHandleInner {
    pub(crate) fn new(hdl: String) -> DeviceHandleInner {
        DeviceHandleInner(hdl)
    }

    pub fn debug_name(&self) -> String {
        self.0.clone()
    }

    pub fn scan_interfaces(
        &self,
        f: impl Fn(&DeviceDescriptor, &InterfaceDescriptor) -> bool,
    ) -> Result<Interface> {
        // The endpoint descriptor comes in two lengths. We only have the struct for the larger one, and
        // the assumption from C land is we'd just let the end of it hang off the end of the buffer and
        // not touch the extra fields when we don't have them. To make this vibe with Rust we'll just
        // make the buffer a touch longer so this is always safe. For the scorekeepers, this should be
        // == 2.
        const OVERRUN: usize = (USB_DT_ENDPOINT_AUDIO_SIZE - USB_DT_ENDPOINT_SIZE) as usize;

        let mut file = match OpenOptions::new().read(true).write(true).open(&self.0) {
            Ok(file) => file,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::PermissionDenied {
                    return Err(err.into());
                }
                OpenOptions::new().read(true).open(&self.0)?
            }
        };

        let mut descriptor_buf = [0u8; 4096 + OVERRUN];
        // The usbdevfs API suggests that one read will always return the whole descriptor.
        let descriptor_length = file.read(&mut descriptor_buf[..4096])?;
        let mut descriptor_buf = &descriptor_buf[..descriptor_length + OVERRUN];

        let mut iface = Option::<InterfaceDescriptor>::None;
        let mut device = Option::<DeviceDescriptor>::None;

        while descriptor_buf.len() >= std::mem::size_of::<usb_descriptor_header>() + OVERRUN {
            let (header, _) = Ref::<_, usb_descriptor_header>::new_from_prefix(descriptor_buf)
                .ok_or(Error::MalformedDescriptor)?;
            let length = header.bLength as usize;
            if length > descriptor_buf.len() {
                return Err(Error::MalformedDescriptor);
            }

            if device.is_none() {
                if header.bDescriptorType == USB_DT_DEVICE as u8 {
                    let (descriptor, _) =
                        Ref::<_, usb_device_descriptor>::new_from_prefix(descriptor_buf)
                            .ok_or(Error::MalformedDescriptor)?;

                    device = Some(DeviceDescriptor {
                        vendor: u16::from_le(descriptor.idVendor),
                        product: u16::from_le(descriptor.idProduct),
                        class: descriptor.bDeviceClass,
                        subclass: descriptor.bDeviceSubClass,
                        protocol: descriptor.bDeviceProtocol,
                    });
                }
            } else if header.bDescriptorType == USB_DT_DEVICE as u8 {
                return Err(Error::MalformedDescriptor);
            }

            let device = device.as_ref().ok_or(Error::MalformedDescriptor)?;

            if header.bDescriptorType == USB_DT_ENDPOINT as u8 {
                let Some(iface) = iface.as_mut() else {
                    return Err(Error::MalformedDescriptor);
                };

                let (descriptor, _) =
                    Ref::<_, usb_endpoint_descriptor>::new_from_prefix(descriptor_buf)
                        .ok_or(Error::MalformedDescriptor)?;

                iface.add_endpoint(&*descriptor);
            } else if header.bDescriptorType == USB_DT_INTERFACE as u8 {
                let (descriptor, _) =
                    Ref::<_, usb_interface_descriptor>::new_from_prefix(descriptor_buf)
                        .ok_or(Error::MalformedDescriptor)?;

                if let Some(iface) = iface.replace(InterfaceDescriptor::from_ch9(&*descriptor)) {
                    if f(&device, &iface) {
                        return Interface::new(file, iface, None);
                    }
                }
            }

            descriptor_buf = &descriptor_buf[length..];
        }

        if let Some(iface) = iface {
            let device = device.as_ref().ok_or(Error::MalformedDescriptor)?;
            if f(&device, &iface) {
                return Interface::new(file, iface, None);
            }
        }

        Err(Error::InterfaceNotFound)
    }
}

/// Wrapper around the Linux URB, which is a structure used to communicate about a transaction to
/// the kernel.
struct Urb {
    urb: UnsafeCell<usbdevfs_urb>,
    waker: AtomicWaker,
    refs: AtomicU8,
}

impl Urb {
    /// Construct a new Urb value.
    const fn new() -> Self {
        Urb {
            urb: UnsafeCell::new(usbdevfs_urb {
                type_: 0,
                endpoint: 0,
                status: 0,
                flags: 0,
                buffer: std::ptr::null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                __bindgen_anon_1: usbdevfs_urb__bindgen_ty_1 { stream_id: 0 },
                error_count: 0,
                signr: 0,
                usercontext: std::ptr::null_mut(),
                // Zero-sized trailing field. The allocation pattern will need to be more
                // sophisticated here if we ever need to write to this.
                iso_frame_desc: usbdevice_fs::__IncompleteArrayField::new(),
            }),
            waker: AtomicWaker::new(),
            refs: AtomicU8::new(0),
        }
    }
}

// SAFETY: The Linux data structure which is causing these to not be auto-derived is designed to be
// shared with kernelspace concurrently.
unsafe impl Send for Urb {}
unsafe impl Sync for Urb {}

/// We need to send void pointers across threads in a few places. This can be used to prevent us
/// from ripping `Send` off of a bunch of our futures in the process.
struct SafePointer(*mut libc::c_void);

// SAFETY: The constructor of the data structure is responsible for ensuring the pointer is safe to
// use in this way.
unsafe impl Send for SafePointer {}
unsafe impl Sync for SafePointer {}

/// Thin wrapper around [`libc::ioctl`] that gives us a rusty error report.
macro_rules! ioctl {
    ($($args:tt)*) => {
        loop {
            if libc::ioctl($($args)*) < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    break Err(err)
                }
            } else {
                break Ok(());
            }
        }
    };
}

/// Allocatable pinned Urbs.
struct UrbQueue {
    /// Indices of Urbs in [`InterfaceInner`] that are free.
    free_urbs: VecDeque<usize>,
    /// For things waiting for free Urbs.
    wakers: Vec<Waker>,
}

impl UrbQueue {
    /// Construct a new UrbQueue.
    fn new(urbs: &[Urb]) -> Self {
        let mut free_urbs = VecDeque::new();

        for idx in 0..urbs.len() {
            free_urbs.push_front(idx);
        }

        UrbQueue { free_urbs, wakers: Vec::new() }
    }
}

/// Reference to an allocated Urb that will release it when dropped.
struct UrbRef<'a>(&'a InterfaceInner, usize);

impl Drop for UrbRef<'_> {
    fn drop(&mut self) {
        if self.refs.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.0.free_urb_by_id(self.1);
        }
    }
}

impl std::ops::Deref for UrbRef<'_> {
    type Target = Urb;

    fn deref(&self) -> &Self::Target {
        &self.0.urbs[self.1]
    }
}

/// Test stubs for creating a fake interface.
trait IoctlStub: Send + Sync {
    /// Stub for USBDEVFS_SUBMITURB
    fn submit(&self, fd: RawFd, urb: *mut usbdevfs_urb) -> Result<(), std::io::Error>;
    /// Stub for USBDEVFS_CLAIMINTERFACE
    fn claim_interface(&self, fd: RawFd, iface: *mut u32) -> Result<(), std::io::Error>;
    /// Stub for USBDEVFS_SETINTERFACE
    fn set_interface(
        &self,
        fd: RawFd,
        set_struct: *mut usbdevfs_setinterface,
    ) -> Result<(), std::io::Error>;
    /// Stub for USBDEVFS_REAPURB
    fn reap_urb(&self, fd: RawFd, urb_ptr: *mut *mut usbdevfs_urb) -> Result<(), std::io::Error>;
}

/// Internal state of [`Interface`]
pub struct InterfaceInner {
    /// The open device file for the USB device. The descriptor within will be configured to refer
    /// to the specific interface we want already.
    file: File,

    /// Queue of Urbs to allocate from.
    urb_queue: Mutex<UrbQueue>,

    /// Urbs,
    urbs: Pin<Box<[Urb]>>,

    /// Number of Urbs that are waiting to be reaped. Needs to be wide enough to fit the length of
    /// `urbs` to avoid overflow issues.
    pending_urbs: AtomicU8,

    /// Join handle for the thread which reaps our Urbs after submission,
    reaper_thread: thread::JoinHandle<()>,

    /// Stub interface for testing.
    stubs: Option<Arc<dyn IoctlStub>>,
}

impl InterfaceInner {
    /// Allocate a Urb from the pool.
    async fn alloc_urb(&self) -> UrbRef<'_> {
        poll_fn(move |ctx| {
            let mut queue = self.urb_queue.lock().unwrap();

            if let Some(got) = queue.free_urbs.pop_back() {
                self.urbs[got].refs.store(1, Ordering::Relaxed);
                return Poll::Ready(UrbRef(self, got));
            }

            queue.wakers.push(ctx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// Free a Urb back into the pool.
    fn free_urb_by_id(&self, id: usize) {
        let mut queue = self.urb_queue.lock().unwrap();
        queue.free_urbs.push_front(id);
        for waker in queue.wakers.drain(..) {
            waker.wake();
        }
    }

    /// Submit a Urb transaction
    async fn submit_urb(
        self: &Arc<Self>,
        address: u8,
        ty: u8,
        buf: SafePointer,
        len: libc::c_int,
    ) -> Result<usize> {
        let urb = self.alloc_urb().await;

        {
            // SAFETY: The allocation semantics should guarantee we're the only holder of this Urb,
            // and that nobody else has handed it to the kernel. We have scoped the reference so it
            // will drop before we hand this pointer to the kernel below, and we will leak a pointer
            // to Self so our pool of Urbs will not be deallocated.
            let urb_inner = unsafe { &mut *urb.urb.get() };

            urb_inner.type_ = ty;
            urb_inner.endpoint = address;
            urb_inner.buffer = buf.0;
            urb_inner.buffer_length = len;
            urb_inner.status = -1;

            let got = urb.refs.fetch_add(1, Ordering::Relaxed);
            debug_assert!(got == 1);
        }

        // Leak self so that our Urbs will not be free'd prematurely.
        let _ = Arc::into_raw(Arc::clone(self));
        self.pending_urbs.fetch_add(1, Ordering::Release);
        self.reaper_thread.thread().unpark();

        if let Some(stubs) = self.stubs.as_ref() {
            stubs.submit(self.file.as_raw_fd(), urb.urb.get())?;
        } else {
            // SAFETY: The pointer is held by the kernel until released by USBDEVFS_REAPURB later.
            // The explanation above explains why we know that pointer will last.
            unsafe {
                ioctl!(self.file.as_raw_fd(), USBDEVFS_SUBMITURB, urb.urb.get())?;
            }
        }

        poll_fn(|ctx| {
            urb.waker.register(ctx.waker());
            if urb.refs.load(Ordering::Relaxed) != 1 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;

        // SAFETY: As above. We should have the Urb back from the kernel and it is ours alone by
        // allocation.
        let (status, actual_length) = unsafe {
            let urb = &*urb.urb.get();
            (urb.status, urb.actual_length)
        };

        if status == 0 {
            Ok(actual_length as usize)
        } else {
            Err(std::io::Error::from_raw_os_error(-status).into())
        }
    }
}

/// Represents a single open USB interface.
pub struct Interface {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptors for all the endpoints this interface supports.
    endpoints: Vec<EndpointDescriptor>,
}

impl Interface {
    /// Create an interface object. The `file` should be the opened block device file, and the
    /// `descriptor` should be the descriptor read out of the descriptor data. `stubs` is used for
    /// testing and should be `None` outside of unit tests.
    fn new(
        file: File,
        descriptor: InterfaceDescriptor,
        stubs: Option<Arc<dyn IoctlStub>>,
    ) -> Result<Self> {
        let fd = file.as_raw_fd();

        let mut iface = usbdevfs_setinterface {
            interface: descriptor.id as u32,
            altsetting: descriptor.alternate as u32,
        };

        if let Some(stubs) = stubs.as_ref() {
            stubs.claim_interface(fd, &mut iface.interface as *mut libc::c_uint)?;
            stubs.set_interface(fd, &mut iface as *mut usbdevfs_setinterface)?;
        } else {
            // SAFETY: These ioctls will only reference this memory for their own runtime, during which
            // they will block this thread, preserving scope. Their arguments have been checked to match
            // the documentation.
            unsafe {
                ioctl!(fd, USBDEVFS_CLAIMINTERFACE, &mut iface.interface as *mut libc::c_uint)?;
                ioctl!(fd, USBDEVFS_SETINTERFACE, &mut iface as *mut usbdevfs_setinterface)?;
            }
        }

        let urbs = Box::pin([
            Urb::new(),
            Urb::new(),
            Urb::new(),
            Urb::new(),
            Urb::new(),
            Urb::new(),
            Urb::new(),
            Urb::new(),
        ]);
        let urb_queue = Mutex::new(UrbQueue::new(&*urbs));
        let pending_urbs = AtomicU8::new(0);
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);

        let reaper_thread_stubs = stubs.clone();
        let reaper_thread = thread::spawn(move || {
            let inner: std::sync::Weak<InterfaceInner> =
                receiver.recv().expect("Never got inner value!");

            while let Some(inner) = inner.upgrade() {
                if inner.pending_urbs.fetch_sub(1, Ordering::Acquire) == 0 {
                    inner.pending_urbs.fetch_add(1, Ordering::Release);
                    thread::park();
                    continue;
                }

                let mut out_ptr = std::ptr::null_mut::<usbdevfs_urb>();

                let fd = inner.file.as_raw_fd();

                // SAFETY: Before Urbs are submitted we leak a pointer to inner to protect their
                // allocations. This reclaims that leaked pointer. Because we checked `pending_urbs`
                // we know such a pointer existed. This is also how we know `fd` won't be closed
                // (and potentially reopened given how Unix file descriptors work) before the ioctl.
                unsafe { Arc::decrement_strong_count(&*inner) };

                // SAFETY: This should block until it no longer holds referencees to the data
                // we're passing it.
                let err = if let Some(stubs) = reaper_thread_stubs.as_ref() {
                    stubs.reap_urb(fd, &mut out_ptr)
                } else {
                    unsafe { ioctl!(fd, USBDEVFS_REAPURB, &mut out_ptr) }
                };

                if err.is_err() {
                    // TODO: Log something?
                    continue;
                }

                let Some((id, urb)) = inner.urbs.iter().enumerate().find(
                        |(_, urb)| std::ptr::eq(urb.urb.get(), out_ptr)
                    ) else {
                        panic!("Reap'd URB we did not sow!");
                    };

                let count = urb.refs.fetch_sub(1, Ordering::Relaxed);
                urb.waker.wake();
                if count == 1 {
                    inner.free_urb_by_id(id);
                }
            }
        });

        let inner =
            Arc::new(InterfaceInner { file, urb_queue, urbs, pending_urbs, reaper_thread, stubs });
        sender.send(Arc::downgrade(&inner)).expect("Reaper thread disappeared immediately!");

        Ok(Interface { inner, endpoints: descriptor.endpoints })
    }

    /// Iterate all the endpoints available for this device.
    pub fn endpoints(&self) -> impl std::iter::Iterator<Item = Endpoint> + '_ {
        self.endpoints.iter().cloned().map(|descriptor| match descriptor.ty {
            EndpointType::Bulk => {
                if descriptor.direction() == EndpointDirection::In {
                    Endpoint::BulkIn(BulkInEndpoint { inner: Arc::clone(&self.inner), descriptor })
                } else {
                    Endpoint::BulkOut(BulkOutEndpoint {
                        inner: Arc::clone(&self.inner),
                        descriptor,
                    })
                }
            }
            EndpointType::Isochronous => Endpoint::Isochronous(IsochronousEndpoint {
                inner: Arc::clone(&self.inner),
                descriptor,
            }),
            EndpointType::Interrupt => Endpoint::Interrupt(InterruptEndpoint {
                inner: Arc::clone(&self.inner),
                descriptor,
            }),
            EndpointType::Control => {
                Endpoint::Control(ControlEndpoint { inner: Arc::clone(&self.inner), descriptor })
            }
        })
    }
}

/// A bulk USB in endpoint. This is a live endpoint that can be read from.
pub struct BulkInEndpoint {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptor for this endpoint.
    descriptor: EndpointDescriptor,
}

impl BulkInEndpoint {
    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let fut = self.inner.submit_urb(
            self.descriptor.address,
            USBDEVFS_URB_TYPE_BULK as u8,
            SafePointer(buf.as_mut_ptr().cast::<libc::c_void>()),
            buf.len().try_into().map_err(|_| Error::BufferTooBig(buf.len()))?,
        );

        fut.await
    }
}

/// A bulk USB out endpoint. This is a live endpoint that can be written to.
pub struct BulkOutEndpoint {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptor for this endpoint.
    descriptor: EndpointDescriptor,
}

impl BulkOutEndpoint {
    pub async fn write(&self, buf: &[u8]) -> Result<()> {
        let fut = self.inner.submit_urb(
            self.descriptor.address,
            USBDEVFS_URB_TYPE_BULK as u8,
            SafePointer(buf.as_ptr().cast::<libc::c_void>().cast_mut()),
            buf.len().try_into().map_err(|_| Error::BufferTooBig(buf.len()))?,
        );

        fut.await.and_then(|x| {
            if x == buf.len() {
                Ok(())
            } else {
                Err(Error::ShortWrite(buf.len(), x))
            }
        })
    }
}

#[allow(unused)]
pub struct IsochronousEndpoint {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptor for this endpoint.
    descriptor: EndpointDescriptor,
}

#[allow(unused)]
pub struct InterruptEndpoint {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptor for this endpoint.
    descriptor: EndpointDescriptor,
}

#[allow(unused)]
pub struct ControlEndpoint {
    /// Internal interface state.
    inner: Arc<InterfaceInner>,

    /// Descriptor for this endpoint.
    descriptor: EndpointDescriptor,
}

impl InterfaceDescriptor {
    /// Turn a raw ch9 interface descriptor into a more rusty [`InterfaceDescriptor`]
    fn from_ch9(descriptor: &usb_interface_descriptor) -> Self {
        InterfaceDescriptor {
            id: descriptor.bInterfaceNumber,
            class: descriptor.bInterfaceClass,
            subclass: descriptor.bInterfaceSubClass,
            protocol: descriptor.bInterfaceProtocol,
            alternate: descriptor.bAlternateSetting,
            endpoints: Vec::new(),
        }
    }

    /// Add an endpoint to this descriptor's information given the raw ch9 endpoint descriptor.
    fn add_endpoint(&mut self, endpoint: &usb_endpoint_descriptor) {
        let ty = endpoint.bmAttributes as u32 & USB_ENDPOINT_XFERTYPE_MASK;
        let ty = match ty {
            USB_ENDPOINT_XFER_BULK => EndpointType::Bulk,
            USB_ENDPOINT_XFER_INT => EndpointType::Interrupt,
            USB_ENDPOINT_XFER_ISOC => EndpointType::Isochronous,
            USB_ENDPOINT_XFER_CONTROL => EndpointType::Control,
            _ => unreachable!("All bit patterns should be covered!"),
        };

        let address = endpoint.bEndpointAddress;

        self.endpoints.push(EndpointDescriptor { ty, address })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::USB_ENDPOINT_DIR_MASK;
    use std::collections::HashMap;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;

    #[derive(Clone)]
    enum EndpointBuffer {
        Data(VecDeque<Box<[u8]>>),
        WaitingReaders(VecDeque<*mut usbdevfs_urb>),
    }

    impl Default for EndpointBuffer {
        fn default() -> Self {
            EndpointBuffer::Data(VecDeque::new())
        }
    }

    fn write_from_target_with_urb(
        data: &[u8],
        urb_ptr: *mut usbdevfs_urb,
        reap_sender: &Sender<*mut usbdevfs_urb>,
    ) -> bool {
        // SAFETY: The crate under test should never pass us an invalid pointer.
        let urb = unsafe { urb_ptr.as_mut().unwrap() };

        let status = if usize::try_from(urb.buffer_length).unwrap() < data.len() {
            -libc::E2BIG
        } else {
            // SAFETY: The crate under test should never pass us an invalid pointer.
            unsafe {
                std::slice::from_raw_parts_mut(urb.buffer.cast::<u8>(), data.len())
                    .copy_from_slice(&data);
            }

            urb.actual_length = data.len().try_into().unwrap();
            0
        };
        urb.status = status;
        reap_sender.send(urb_ptr).unwrap();
        status == 0
    }

    struct FakeDev {
        descriptor: InterfaceDescriptor,
        reap_receiver: Receiver<*mut usbdevfs_urb>,
        reap_sender: Sender<*mut usbdevfs_urb>,
        claimed: AtomicU8,
        set: AtomicU8,
        endpoint_buffers: Mutex<HashMap<u8, EndpointBuffer>>,
    }

    impl FakeDev {
        fn times_claimed(&self) -> u8 {
            self.claimed.load(Ordering::Relaxed)
        }
        fn times_interface_set(&self) -> u8 {
            self.set.load(Ordering::Relaxed)
        }
        fn endpoint_read_from_target(&self, address: u8) -> Option<Box<[u8]>> {
            let mut buffers = self.endpoint_buffers.lock().unwrap();
            buffers.get_mut(&address).and_then(|x| {
                let EndpointBuffer::Data(x) = x else {
                        panic!("Target read from buffer with host reads waiting");
                    };
                x.pop_back()
            })
        }
        fn endpoint_write_from_target(&self, address: u8, data: &[u8]) {
            let mut buffers = self.endpoint_buffers.lock().unwrap();
            let buffer = buffers.entry(address).or_default();
            let queue = match buffer {
                EndpointBuffer::Data(data) => data,
                EndpointBuffer::WaitingReaders(readers) => {
                    while let Some(urb_ptr) = readers.pop_front() {
                        if write_from_target_with_urb(data, urb_ptr, &self.reap_sender) {
                            return;
                        }
                    }
                    *buffer = EndpointBuffer::Data(VecDeque::new());
                    let EndpointBuffer::Data(got) = buffer else {
                        unreachable!();
                    };
                    got
                }
            };

            queue.push_back(Box::from(data));
        }
    }

    struct FakeUSBEnv {
        fake_devs: Mutex<HashMap<RawFd, Arc<FakeDev>>>,
    }

    impl FakeUSBEnv {
        fn new_fake_dev_file(&self, descriptor: InterfaceDescriptor) -> File {
            let ret = tempfile::tempfile().unwrap();
            let (reap_sender, reap_receiver) = channel();
            let fake_dev = FakeDev {
                descriptor,
                reap_sender,
                reap_receiver,
                claimed: AtomicU8::new(0),
                set: AtomicU8::new(0),
                endpoint_buffers: Mutex::new(HashMap::new()),
            };
            self.fake_devs.lock().unwrap().insert(ret.as_raw_fd(), Arc::new(fake_dev));
            ret
        }

        fn new_fake_dev(self: &Arc<Self>, descriptor: InterfaceDescriptor) -> Interface {
            Interface::new(
                self.new_fake_dev_file(descriptor.clone()),
                descriptor,
                Some(Arc::clone(self) as Arc<dyn IoctlStub>),
            )
            .unwrap()
        }

        fn new() -> Arc<Self> {
            Arc::new(FakeUSBEnv { fake_devs: Mutex::new(HashMap::new()) })
        }

        fn get_dev(&self, fd: RawFd) -> Result<Arc<FakeDev>, std::io::Error> {
            self.fake_devs
                .lock()
                .unwrap()
                .get(&fd)
                .map(Arc::clone)
                .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENODEV))
        }
    }

    // SAFETY: It's just complaining about the urb pointers. It's fine for the same reasons it's
    // always fine.
    unsafe impl Send for FakeUSBEnv {}
    unsafe impl Sync for FakeUSBEnv {}

    impl IoctlStub for FakeUSBEnv {
        fn claim_interface(&self, fd: RawFd, iface: *mut u32) -> Result<(), std::io::Error> {
            // SAFETY: The crate under test should never pass us an invalid pointer.
            let iface = unsafe { *iface };
            let dev = self.get_dev(fd)?;
            if dev.descriptor.id as u32 != iface {
                return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
            }
            let _ = dev.claimed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn reap_urb(
            &self,
            fd: std::os::fd::RawFd,
            urb_ptr: *mut *mut usbdevfs_urb,
        ) -> Result<(), std::io::Error> {
            // SAFETY: The crate under test should never pass us an invalid pointer.
            let urb_ptr = unsafe { urb_ptr.as_mut().unwrap() };

            *urb_ptr = self.get_dev(fd)?.reap_receiver.recv().unwrap();
            Ok(())
        }

        fn set_interface(
            &self,
            fd: std::os::fd::RawFd,
            set_struct: *mut usbdevfs_setinterface,
        ) -> Result<(), std::io::Error> {
            // SAFETY: The crate under test should never pass us an invalid pointer.
            let set_struct = unsafe { *set_struct };
            let dev = self.get_dev(fd)?;
            if dev.descriptor.id as u32 != set_struct.interface {
                return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
            }
            if dev.descriptor.alternate as u32 != set_struct.altsetting {
                return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
            }
            let _ = dev.set.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn submit(
            &self,
            fd: std::os::fd::RawFd,
            urb: *mut usbdevfs_urb,
        ) -> Result<(), std::io::Error> {
            let urb_ptr = urb;
            // SAFETY: The crate under test should never pass us an invalid pointer.
            let urb = unsafe { urb.as_mut().unwrap() };
            let dev = self.get_dev(fd)?;

            assert_eq!(urb.type_, USBDEVFS_URB_TYPE_BULK as u8);
            let endpoint =
                dev.descriptor.endpoints.iter().find(|x| x.address == urb.endpoint).unwrap();

            let mut buffers = dev.endpoint_buffers.lock().unwrap();
            let buffer = buffers.entry(endpoint.address).or_default();

            match endpoint.direction() {
                EndpointDirection::Out => {
                    // SAFETY: The crate under test should never pass us an invalid pointer.
                    let data = unsafe {
                        std::slice::from_raw_parts(
                            urb.buffer.cast::<u8>(),
                            urb.buffer_length.try_into().unwrap(),
                        )
                    };
                    let EndpointBuffer::Data(buffer) = buffer else {
                        panic!("Readers waiting on Out endpoint")
                    };
                    buffer.push_back(Box::from(data));
                    urb.actual_length = urb.buffer_length;
                    urb.status = 0;
                    dev.reap_sender.send(urb_ptr).unwrap();
                }
                EndpointDirection::In => {
                    let waiting_readers = match buffer {
                        EndpointBuffer::WaitingReaders(readers) => readers,
                        EndpointBuffer::Data(queue) => {
                            if let Some(data) = queue.front() {
                                if write_from_target_with_urb(data, urb_ptr, &dev.reap_sender) {
                                    let _ = queue.pop_front();
                                }
                                return Ok(());
                            }
                            *buffer = EndpointBuffer::WaitingReaders(VecDeque::new());
                            let EndpointBuffer::WaitingReaders(got) = buffer else {
                                unreachable!()
                            };
                            got
                        }
                    };
                    waiting_readers.push_back(urb_ptr);
                }
            }
            Ok(())
        }
    }

    #[fuchsia::test]
    async fn simple_bulk() {
        let env = FakeUSBEnv::new();
        let descriptor = InterfaceDescriptor {
            id: 0,
            class: 0xff,
            subclass: 0x42,
            protocol: 22,
            alternate: 64,
            endpoints: vec![
                EndpointDescriptor { ty: EndpointType::Bulk, address: 0 | USB_ENDPOINT_DIR_MASK },
                EndpointDescriptor { ty: EndpointType::Bulk, address: 1 },
            ],
        };
        let iface = env.new_fake_dev(descriptor);
        let fd = iface.inner.file.as_raw_fd();
        assert_eq!(1, env.get_dev(fd).unwrap().times_claimed());
        assert_eq!(1, env.get_dev(fd).unwrap().times_interface_set());

        let mut eps = iface.endpoints().collect::<Vec<_>>();
        assert_eq!(2, eps.len());
        let a = eps.pop().unwrap();
        let b = eps.pop().unwrap();

        let (i, o) = match (a, b) {
            (Endpoint::BulkIn(a), Endpoint::BulkOut(b)) => (a, b),
            (Endpoint::BulkOut(a), Endpoint::BulkIn(b)) => (b, a),
            _ => panic!("Wrong endpoint types!"),
        };

        assert_eq!(0 | USB_ENDPOINT_DIR_MASK, i.descriptor.address);
        assert_eq!(1, o.descriptor.address);

        env.get_dev(fd).unwrap().endpoint_write_from_target(0 | USB_ENDPOINT_DIR_MASK, b"Wango!");
        let mut buf = [0u8; 8];
        let len = i.read(&mut buf).await.unwrap();
        assert_eq!(b"Wango!".len(), len);
        assert_eq!(b"Wango!", &buf[..len]);

        o.write(b"Bango!").await.unwrap();
        let data = env.get_dev(fd).unwrap().endpoint_read_from_target(1).unwrap();
        assert_eq!(b"Bango!", &*data);

        assert_eq!(1, env.get_dev(fd).unwrap().times_claimed());
        assert_eq!(1, env.get_dev(fd).unwrap().times_interface_set());
    }

    #[fuchsia::test]
    async fn stumpy_read() {
        let env = FakeUSBEnv::new();
        let descriptor = InterfaceDescriptor {
            id: 0,
            class: 0xff,
            subclass: 0x42,
            protocol: 22,
            alternate: 64,
            endpoints: vec![EndpointDescriptor {
                ty: EndpointType::Bulk,
                address: 0 | USB_ENDPOINT_DIR_MASK,
            }],
        };
        let iface = env.new_fake_dev(descriptor);
        let fd = iface.inner.file.as_raw_fd();
        assert_eq!(1, env.get_dev(fd).unwrap().times_claimed());
        assert_eq!(1, env.get_dev(fd).unwrap().times_interface_set());

        let mut eps = iface.endpoints().collect::<Vec<_>>();
        assert_eq!(1, eps.len());

        let i = match eps.pop().unwrap() {
            Endpoint::BulkIn(a) => a,
            _ => panic!("Wrong endpoint type!"),
        };

        assert_eq!(0 | USB_ENDPOINT_DIR_MASK, i.descriptor.address);

        env.get_dev(fd).unwrap().endpoint_write_from_target(0 | USB_ENDPOINT_DIR_MASK, b"Wango!");

        let mut buf = [0u8; 2];
        let err = i.read(&mut buf).await.unwrap_err();
        let err = match err {
            Error::IOError(err) => err,
            other => panic!("Unexpected error! {other:?}"),
        };
        assert_eq!(Some(libc::E2BIG), err.raw_os_error());

        let mut buf = [0u8; 8];
        let len = i.read(&mut buf).await.unwrap();
        assert_eq!(b"Wango!".len(), len);
        assert_eq!(b"Wango!", &buf[..len]);

        assert_eq!(1, env.get_dev(fd).unwrap().times_claimed());
        assert_eq!(1, env.get_dev(fd).unwrap().times_interface_set());
    }
}
