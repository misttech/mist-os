// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    DeviceDescriptor, EndpointDescriptor, EndpointType, Error, InterfaceDescriptor, Result,
};
use std::io::Error as IOError;
use std::sync::{Arc, Condvar, Mutex};

use super::iokit_usb;

// SAFETY: These are all stubs around constant getters to get around limitations in bindgen. There
// are no safety concerns with any of these.
#[allow(non_snake_case)]
pub fn GetIOUSBDeviceUserClientTypeID() -> iokit_usb::CFUUIDRef {
    unsafe { iokit_usb::GetIOUSBDeviceUserClientTypeID() }
}
#[allow(non_snake_case)]
pub fn GetIOUSBInterfaceUserClientTypeID() -> iokit_usb::CFUUIDRef {
    unsafe { iokit_usb::GetIOUSBInterfaceUserClientTypeID() }
}
#[allow(non_snake_case)]
fn GetIOUSBDeviceInterfaceID500() -> iokit_usb::CFUUIDRef {
    unsafe { iokit_usb::GetIOUSBDeviceInterfaceID500() }
}
#[allow(non_snake_case)]
fn GetIOUSBInterfaceInterfaceID500() -> iokit_usb::CFUUIDRef {
    unsafe { iokit_usb::GetIOUSBInterfaceInterfaceID500() }
}
#[allow(non_snake_case)]
fn GetIOCFPlugInInterfaceID() -> iokit_usb::CFUUIDRef {
    unsafe { iokit_usb::GetIOCFPlugInInterfaceID() }
}

/// Turns a kern_return_t into an error.
fn kern_return_check(error: iokit_usb::kern_return_t) -> Result<()> {
    if error == 0 {
        Ok(())
    } else {
        Err(Error::IOError(IOError::new(
            std::io::ErrorKind::Other,
            format!("kern_return_t({error})"),
        )))
    }
}

/// Turns an IOReturn into an error.
pub fn ioreturn_check(error: iokit_usb::kern_return_t) -> Result<()> {
    if error == 0 {
        Ok(())
    } else {
        Err(Error::IOError(IOError::new(std::io::ErrorKind::Other, format!("IOReturn({error})"))))
    }
}

/// Wraps a pointer to CFRunLoopSource
struct RunLoopSourceRef(iokit_usb::CFRunLoopSourceRef);

impl Drop for RunLoopSourceRef {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::CFRelease(self.0.cast());
        }
    }
}

impl Clone for RunLoopSourceRef {
    fn clone(&self) -> RunLoopSourceRef {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::CFRetain(self.0.cast());
        }

        RunLoopSourceRef(self.0)
    }
}

// SAFETY: The whole point of these objects is to be able to signal them from another thread.
unsafe impl Send for RunLoopSourceRef {}
unsafe impl Sync for RunLoopSourceRef {}

/// Wraps a pointer to CFRunLoop
pub struct RunLoop {
    handle: iokit_usb::CFRunLoopRef,
    source_ref: RunLoopSourceRef,
}

// SAFETY: The only thing that doesn't have this already here is handle, which is a CoreFoundation
// object, and thus should be thread safe.
unsafe impl Send for RunLoop {}
unsafe impl Sync for RunLoop {}

impl RunLoop {
    /// Constructs a new run loop. This effectively spawns a thread that we can dispatch
    /// CoreFoundation events on.
    pub fn new() -> RunLoop {
        #[derive(Copy, Clone)]
        struct Safety(iokit_usb::CFRunLoopRef);

        // SAFETY: Referring to these pointers from another thread is the only reason to have them.
        unsafe impl Send for Safety {}

        let pair = Arc::new((Mutex::new(None), Condvar::new()));
        let thread_side = Arc::clone(&pair);

        extern "C" fn do_nothing(_ptr: *mut libc::c_void) {}

        // If the run loop has no source it will die immediately, so we have to add one. Later,
        // invalidating this source will release the loop.
        //
        // SAFETY: This operation should always be safe. We're passing it no pointers so it won't
        // retain anything that might go out of scope. The CFRunLoopSourceContext struct is copied
        // immediately, so a temporary pointer is fine.
        let dummy_source = unsafe {
            iokit_usb::CFRunLoopSourceCreate(
                iokit_usb::kCFAllocatorDefault,
                0,
                &mut iokit_usb::CFRunLoopSourceContext {
                    version: 0,
                    info: std::ptr::null_mut(),
                    retain: None,
                    release: None,
                    copyDescription: None,
                    equal: None,
                    hash: None,
                    schedule: None,
                    cancel: None,
                    perform: Some(do_nothing),
                },
            )
        };
        let dummy_source = RunLoopSourceRef(dummy_source);
        let source_ref = dummy_source.clone();

        std::thread::spawn(move || {
            // SAFETY: CoreFoundation manages run loop state automatically so this should always be
            // safe. dummy_source will be refcounted by CoreFoundation so we can drop our own ref
            // immediately.
            unsafe {
                let (handle_out, cvar) = &*thread_side;
                *handle_out.lock().unwrap() = Some(Safety(iokit_usb::CFRunLoopGetCurrent()));
                cvar.notify_one();
                iokit_usb::CFRunLoopAddSource(
                    iokit_usb::CFRunLoopGetCurrent(),
                    dummy_source.0,
                    iokit_usb::kCFRunLoopDefaultMode,
                );
                std::mem::drop(dummy_source);
                iokit_usb::CFRunLoopRun();
            }
        });

        let (handle_out, cvar) = &*pair;
        let mut handle_out = handle_out.lock().unwrap();
        let handle = loop {
            let Some(handle) = *handle_out else {
                handle_out = cvar.wait(handle_out).unwrap();
                continue;
            };

            break handle.0;
        };

        RunLoop { handle, source_ref }
    }
}

impl Drop for RunLoop {
    fn drop(&mut self) {
        // SAFETY: Our source should be valid by construction and is internally refcounted by
        // CoreFoundation, so it should be in scope.
        unsafe {
            iokit_usb::CFRunLoopSourceInvalidate(self.source_ref.0);
        }
    }
}

/// Safe wrapper around a Mach port
pub struct MachPort(iokit_usb::mach_port_t);

impl MachPort {
    /// Creates a new IO master port, which is used to derive handles to other IO components.
    pub fn new_master() -> Result<Self> {
        let mut master_port = 0;
        // SAFETY: We pass a constant explicitly designed for use with this call, and a pointer
        // which points directly to this stack frame and is intended as an output value.
        let err = unsafe { iokit_usb::IOMasterPort(iokit_usb::MACH_PORT_NULL, &mut master_port) };

        kern_return_check(err)?;
        Self::from_raw_port(master_port)
    }

    /// Wrap an unadorned mach_port_t.
    fn from_raw_port(port: iokit_usb::mach_port_t) -> Result<Self> {
        if port != 0 {
            Ok(MachPort(port))
        } else {
            Err(Error::IOError(IOError::new(
                std::io::ErrorKind::Other,
                format!("Could not create port"),
            )))
        }
    }

    /// Derives a new IO Notification port from this master port.
    pub fn new_notify(&self) -> NotifyPort {
        // SAFETY: We know the value we're passing is valid because this object is here to ensure
        // its lifetime. The new object we are creating does not capture the lifetime of the
        // argument passed; documentation examples explicitly show the mach_port_t being destroyed
        // while the NotifyPort is still in use.
        unsafe { NotifyPort(iokit_usb::IONotificationPortCreate(self.0)) }
    }
}

impl Drop for MachPort {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::mach_port_deallocate(iokit_usb::mach_task_self_, self.0);
        }
    }
}

/// Wraps an IO notification port.
pub struct NotifyPort(iokit_usb::IONotificationPortRef);

// SAFETY: These are CoreFoundation objects which are supposed to be thread-safe.
unsafe impl Send for NotifyPort {}
unsafe impl Sync for NotifyPort {}

impl NotifyPort {
    /// Tells the given run loop to dispatch notifications from this port.
    pub fn add_source_to_run_loop(&self, run_loop: &RunLoop) {
        // SAFETY: We know the port is valid by construction of this object. The documentation
        // mentions no failure modes for this function.
        let source =
            RunLoopSourceRef(unsafe { iokit_usb::IONotificationPortGetRunLoopSource(self.0) });

        // SAFETY: We are passing only constants designed specifically for this function, and the
        // source we were just given. Run loops are automatically created and managed so it's
        // generally safe to interact with them at any time. Run loop sources are refcounted by
        // CoreFoundation so it's safe to drop our ref after using it here.
        unsafe {
            iokit_usb::CFRunLoopAddSource(
                run_loop.handle,
                source.0,
                iokit_usb::kCFRunLoopDefaultMode,
            );
        }
    }

    /// Call `IOServiceAddMatchingNotification` for this port.
    pub fn add_matching_notification(
        &self,
        match_type: &'static [u8],
        matching_dict: MatchingDict,
        callback: Option<extern "C" fn(*mut libc::c_void, iokit_usb::io_iterator_t)>,
        data_ptr: *mut libc::c_void,
    ) -> Result<()> {
        let mut iter = 0;
        let callback = callback.map(|x| x as unsafe extern "C" fn(_, _));

        unsafe {
            kern_return_check(iokit_usb::IOServiceAddMatchingNotification(
                self.0,
                match_type.as_ptr().cast_mut().cast(),
                matching_dict.into_raw(),
                callback,
                data_ptr,
                &mut iter,
            ))?;
            if let Some(callback) = callback {
                callback(data_ptr, iter);
            }
        }

        Ok(())
    }
}

impl Drop for NotifyPort {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::IONotificationPortDestroy(self.0);
        }
    }
}

/// Wrapper around a matching dict, which is a structure containing device properties which we can
/// use to search for devices.
pub struct MatchingDict(std::ptr::NonNull<iokit_usb::__CFDictionary>);

impl MatchingDict {
    /// Create a new matching dict. These are basically balls of metadata about what sorts of
    /// devices we want to enumerate. Cloning this struct will produce a new reference to the same
    /// dict.
    pub fn new_usb() -> Result<Self> {
        // SAFETY: We're calling this function passing it a pointer that is a global constant
        // specifically prescribed for being passed to this function.
        let ptr = unsafe {
            iokit_usb::IOServiceMatching(iokit_usb::kIOUSBDeviceClassName.as_ptr().cast())
        };
        Ok(MatchingDict(std::ptr::NonNull::new(ptr).ok_or_else(|| {
            Error::IOError(IOError::new(
                std::io::ErrorKind::Other,
                format!("Could not create matching dict"),
            ))
        })?))
    }

    /// Get the raw pointer to the underlying matching dict. Destroys this wrapper object without
    /// consuming the reference it represents.
    fn into_raw(self) -> iokit_usb::CFMutableDictionaryRef {
        let ret = self.0.as_ptr();
        std::mem::forget(self);
        ret
    }
}

impl Drop for MatchingDict {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::CFRelease(self.0.as_ptr().cast());
        }
    }
}

impl Clone for MatchingDict {
    fn clone(&self) -> MatchingDict {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        MatchingDict(unsafe {
            std::ptr::NonNull::new(iokit_usb::CFRetain(self.0.as_ptr().cast()).cast_mut().cast())
                .expect("CFRetain returned Null!")
        })
    }
}

/// Wrapper around version 500 of IOKit's USB Interface object
pub struct InterfaceInterface500(*mut *mut iokit_usb::IOUSBInterfaceInterface500);

// SAFETY: CoreFoundation objects are generally thread safe.
unsafe impl Send for InterfaceInterface500 {}
unsafe impl Sync for InterfaceInterface500 {}

impl InterfaceInterface500 {
    /// SAFETY: Pointer passed to the function must be valid.
    unsafe fn new(
        ptr: *mut *mut iokit_usb::IOUSBInterfaceInterface500,
    ) -> Result<InterfaceInterface500> {
        assert!(!ptr.is_null(), "Tried to construct InterfaceInterface500 with null ptr");

        ioreturn_check(((**ptr).USBInterfaceOpen.unwrap())(ptr.cast()))?;

        Ok(InterfaceInterface500(ptr))
    }

    /// Call WritePipeAsync for this interface.
    pub fn read_pipe_async(
        &self,
        buf: &mut [u8],
        idx: u8,
        callback: Option<extern "C" fn(*mut libc::c_void, iokit_usb::IOReturn, *mut libc::c_void)>,
        callback_data: *mut libc::c_void,
    ) -> Result<()> {
        let callback = callback.map(|x| x as unsafe extern "C" fn(_, _, _));

        // SAFETY: Pointer should be valid by construction.
        unsafe {
            ioreturn_check(((**self.0).ReadPipeAsync.unwrap())(
                self.0.cast(),
                idx,
                buf.as_mut_ptr().cast(),
                buf.len().try_into().expect("Impossibly large USB transaction"),
                callback,
                callback_data,
            ))
        }
    }

    /// Call WritePipeAsync for this interface.
    pub fn write_pipe_async(
        &self,
        buf: &[u8],
        idx: u8,
        callback: Option<extern "C" fn(*mut libc::c_void, iokit_usb::IOReturn, *mut libc::c_void)>,
        callback_data: *mut libc::c_void,
    ) -> Result<()> {
        let callback = callback.map(|x| x as unsafe extern "C" fn(_, _, _));

        // SAFETY: Pointer should be valid by construction.
        unsafe {
            ioreturn_check(((**self.0).WritePipeAsync.unwrap())(
                self.0.cast(),
                idx,
                buf.as_ptr().cast_mut().cast(),
                buf.len().try_into().expect("Impossibly large USB transaction"),
                callback,
                callback_data,
            ))
        }
    }

    /// Derive a crate::InterfaceDescriptor for this interface.
    pub fn descriptor(&self) -> Result<InterfaceDescriptor> {
        let mut ret = InterfaceDescriptor {
            id: 0,
            class: 0,
            subclass: 0,
            protocol: 0,
            alternate: 0,
            endpoints: Vec::new(),
        };
        let mut num_endpoints = 0;

        // SAFETY: This object should guarantee the validity of the pointer.
        unsafe {
            ioreturn_check(((**self.0).GetInterfaceNumber.unwrap())(self.0.cast(), &mut ret.id))?;
            ioreturn_check(((**self.0).GetInterfaceClass.unwrap())(self.0.cast(), &mut ret.class))?;
            ioreturn_check(((**self.0).GetInterfaceSubClass.unwrap())(
                self.0.cast(),
                &mut ret.subclass,
            ))?;
            ioreturn_check(((**self.0).GetInterfaceProtocol.unwrap())(
                self.0.cast(),
                &mut ret.protocol,
            ))?;
            ioreturn_check(((**self.0).GetInterfaceProtocol.unwrap())(
                self.0.cast(),
                &mut ret.protocol,
            ))?;
            ioreturn_check(((**self.0).GetAlternateSetting.unwrap())(
                self.0.cast(),
                &mut ret.alternate,
            ))?;
            ioreturn_check(((**self.0).GetNumEndpoints.unwrap())(
                self.0.cast(),
                &mut num_endpoints,
            ))?;
        }

        ret.endpoints.reserve_exact(num_endpoints as usize);
        for endpoint_num in 1..=num_endpoints {
            let mut transfer_type = 0;
            let mut address = 0;
            let mut direction = 0;

            // SAFETY: Same as above.
            unsafe {
                ioreturn_check(((**self.0).GetPipeProperties.unwrap())(
                    self.0.cast(),
                    endpoint_num,
                    &mut direction,
                    &mut address,
                    &mut transfer_type,
                    &mut 0,
                    &mut 0,
                ))?;
            }

            let ty = match transfer_type as u32 {
                iokit_usb::kUSBBulk => EndpointType::Bulk,
                iokit_usb::kUSBControl => EndpointType::Control,
                iokit_usb::kUSBIsoc => EndpointType::Isochronous,
                iokit_usb::kUSBInterrupt => EndpointType::Interrupt,
                _ => return Err(Error::MalformedDescriptor),
            };

            let address = if u32::from(direction) == iokit_usb::kUSBIn {
                address | crate::USB_ENDPOINT_DIR_MASK
            } else {
                address
            };

            ret.endpoints.push(EndpointDescriptor { ty, address });
        }

        Ok(ret)
    }

    /// Use the given event loop to listen for events from this interface.
    pub fn register_with_event_loop(&self, run_loop: &RunLoop) -> Result<()> {
        let mut source = std::ptr::null_mut();

        // SAFETY: Pointer should be valid by construction. RunLoop sources are refcounted by
        // CoreFoundation so it's safe to drop the one we create here.
        unsafe {
            ioreturn_check(((**self.0).CreateInterfaceAsyncEventSource.unwrap())(
                self.0.cast(),
                &mut source,
            ))?;
            let source = RunLoopSourceRef(source);
            iokit_usb::CFRunLoopAddSource(
                run_loop.handle,
                source.0,
                iokit_usb::kCFRunLoopDefaultMode,
            );
        }
        Ok(())
    }
}

impl QueryableInterface for Result<InterfaceInterface500> {
    fn uuid() -> iokit_usb::CFUUIDRef {
        GetIOUSBInterfaceInterfaceID500()
    }

    unsafe fn get(ptr: *mut libc::c_void) -> Result<InterfaceInterface500> {
        InterfaceInterface500::new(ptr.cast())
    }
}

impl Drop for InterfaceInterface500 {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            ((**self.0).USBInterfaceClose.unwrap())(self.0.cast());
            ((**self.0).Release.unwrap())(self.0.cast());
        }
    }
}

/// Wrapper around version 500 of IOKit's USB Device.
pub struct DeviceInterface500(*mut *mut iokit_usb::IOUSBDeviceInterface500);

impl DeviceInterface500 {
    /// Derive a crate::DeviceDescriptor from this device.
    pub fn descriptor(&self) -> DeviceDescriptor {
        let mut ret =
            DeviceDescriptor { vendor: 0, product: 0, class: 0, subclass: 0, protocol: 0 };

        // SAFETY: If our contained pointer is valid, as this object should ensure, all these should
        // be safe uses.
        unsafe {
            ((**self.0).GetDeviceVendor.unwrap())(self.0.cast(), &mut ret.vendor);
            ((**self.0).GetDeviceProduct.unwrap())(self.0.cast(), &mut ret.product);
            ((**self.0).GetDeviceClass.unwrap())(self.0.cast(), &mut ret.class);
            ((**self.0).GetDeviceSubClass.unwrap())(self.0.cast(), &mut ret.subclass);
            ((**self.0).GetDeviceProtocol.unwrap())(self.0.cast(), &mut ret.protocol);
        }

        ret
    }

    /// Iterate the interfaces exposed by this device.
    pub fn iter_interfaces(&self) -> Result<impl Iterator<Item = IOService>> {
        let mut io_iter = 0;
        let mut request = iokit_usb::IOUSBFindInterfaceRequest::default();
        request.bInterfaceClass = iokit_usb::kIOUSBFindInterfaceDontCare as u16;
        request.bInterfaceSubClass = iokit_usb::kIOUSBFindInterfaceDontCare as u16;
        request.bInterfaceProtocol = iokit_usb::kIOUSBFindInterfaceDontCare as u16;
        request.bAlternateSetting = iokit_usb::kIOUSBFindInterfaceDontCare as u16;

        // SAFETY: This object assures the validity of self.0 and the two other pointers are outputs
        // so local stack locations should be fine.
        unsafe {
            ioreturn_check(((**self.0).CreateInterfaceIterator.unwrap())(
                self.0.cast(),
                &mut request,
                &mut io_iter,
            ))?;
        }

        // SAFETY: We just received this iterator so it should be valid.
        Ok(std::iter::from_fn(move || {
            Some(unsafe { iokit_usb::IOIteratorNext(io_iter) }).filter(|&x| x != 0)
        })
        .map(IOService))
    }
}

impl QueryableInterface for DeviceInterface500 {
    fn uuid() -> iokit_usb::CFUUIDRef {
        GetIOUSBDeviceInterfaceID500()
    }

    unsafe fn get(ptr: *mut libc::c_void) -> DeviceInterface500 {
        DeviceInterface500(ptr.cast())
    }
}

impl Drop for DeviceInterface500 {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            ((**self.0).Release.unwrap())(self.0.cast());
        }
    }
}

/// Wrapper around IOKit's io_service_t
pub struct IOService(iokit_usb::io_service_t);

impl IOService {
    /// Construct from a raw io_service_t
    pub fn from_raw(raw: iokit_usb::io_service_t) -> IOService {
        IOService(raw)
    }
}

impl std::fmt::Debug for IOService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let mut buf = [0u8; std::mem::size_of::<iokit_usb::io_name_t>()];

        // SAFETY: The io_name_t type is an array that is supposed to be big enough.
        if let Err(e) = unsafe {
            kern_return_check(iokit_usb::IORegistryEntryGetName(self.0, buf.as_mut_ptr().cast()))
        } {
            write!(f, "<Name unknown ({e:?})>")
        } else {
            let buf = buf.split(|&x| x == 0).next().unwrap_or(&[]);
            let string = String::from_utf8_lossy(buf);
            f.write_str(string.as_ref())
        }
    }
}

impl Clone for IOService {
    fn clone(&self) -> IOService {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::IOObjectRetain(self.0);
        }

        IOService(self.0)
    }
}

impl Drop for IOService {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            iokit_usb::IOObjectRelease(self.0);
        }
    }
}

/// Marks an object that can be returned from a PlugIn interface.
pub trait QueryableInterface {
    /// The UUID passed to the Query Interface call to get an object of this type.
    fn uuid() -> iokit_usb::CFUUIDRef;

    /// Construct an object of this type;
    /// SAFETY: The pointer must be valid and returned by an IOKit plugin's QueryInterface call.
    unsafe fn get(ptr: *mut libc::c_void) -> Self;
}

/// Wrapper around IOKit's PlugIn Interface
pub struct PlugInInterface(*mut *mut iokit_usb::IOCFPlugInInterface);

impl PlugInInterface {
    /// Create a new plugin interface for the given service.
    pub fn new(obj: IOService, ty: iokit_usb::CFUUIDRef) -> Result<PlugInInterface> {
        let mut plugin_interface = std::ptr::null_mut();

        // SAFETY: This object should guarantee that the handle is valid. All the other arguments
        // are constants or stack pointers known to be used only as immediate outputs.
        unsafe {
            kern_return_check(iokit_usb::IOCreatePlugInInterfaceForService(
                obj.0,
                ty,
                GetIOCFPlugInInterfaceID(),
                &mut plugin_interface,
                &mut 0,
            ))?;
        }

        if plugin_interface.is_null() {
            return Err(Error::IOError(IOError::new(
                std::io::ErrorKind::Other,
                format!("Could not create plugin interface"),
            )));
        }

        Ok(PlugInInterface(plugin_interface))
    }

    /// Query this plugin interface for a specific interface.
    pub fn query_interface<T: QueryableInterface>(self) -> Result<T> {
        let mut ptr = std::ptr::null_mut();

        // SAFETY: The passed-in object should be valid by construction of Self, and the other
        // parameter is an output pointer which should be fine as a stack pointer.
        let res = unsafe {
            ((**self.0).QueryInterface.unwrap())(
                self.0.cast(),
                iokit_usb::CFUUIDGetUUIDBytes(T::uuid()),
                &mut ptr,
            )
        };

        std::mem::drop(self);

        if res != 0 || ptr.is_null() {
            Err(Error::IOError(IOError::new(
                std::io::ErrorKind::Other,
                format!("Could not get CoreFoundation Interface HRESULT: {res}"),
            )))
        } else {
            // SAFETY: As required, this pointer is non-null and returned from QueryInterface.
            unsafe { Ok(T::get(ptr)) }
        }
    }
}

impl Drop for PlugInInterface {
    fn drop(&mut self) {
        // SAFETY: This object's purpose is to keep this value alive and ensure its scope, so this
        // should always be valid.
        unsafe {
            ((**self.0).Release.unwrap())(self.0.cast());
        }
    }
}
