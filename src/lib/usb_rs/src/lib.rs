// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::stream::Stream;
use thiserror::Error;

#[cfg_attr(target_os = "linux", path = "usb_linux/mod.rs")]
#[cfg_attr(target_os = "macos", path = "usb_osx/mod.rs")]
mod usb_plat;

pub use usb_plat::{
    BulkInEndpoint, BulkOutEndpoint, ControlEndpoint, Interface, InterruptEndpoint,
    IsochronousEndpoint,
};

/// Selects the bit in USB endpoint addresses that tells us whether it is an in or and out endpoint.
pub(crate) const USB_ENDPOINT_DIR_MASK: u8 = 0x80;

pub struct DeviceHandle(usb_plat::DeviceHandleInner);

impl From<usb_plat::DeviceHandleInner> for DeviceHandle {
    fn from(inner: usb_plat::DeviceHandleInner) -> DeviceHandle {
        DeviceHandle(inner)
    }
}

impl DeviceHandle {
    /// A printable name for this device.
    pub fn debug_name(&self) -> String {
        self.0.debug_name()
    }

    /// Given a path to a USB device, scan each interface available on the device. Each interface's
    /// descriptor is passed to the given callback, and the first descriptor for which the callback
    /// returns `true` will be opened and returned.
    pub fn scan_interfaces(
        &self,
        f: impl Fn(&DeviceDescriptor, &InterfaceDescriptor) -> bool,
    ) -> Result<Interface> {
        self.0.scan_interfaces(f)
    }
}

impl std::fmt::Debug for DeviceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_tuple("DeviceHandle").field(&self.debug_name()).finish()
    }
}

/// Device discovery events. See `wait_for_devices`.
#[derive(Debug)]
pub enum DeviceEvent {
    /// Indicates a new USB device has been plugged in.
    Added(DeviceHandle),
    /// Indicates a USB device has been unplugged.
    Removed(DeviceHandle),
}

/// Errors emitted by USB operations.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not write all data (had {0} wrote {1})")]
    ShortWrite(usize, usize),
    #[error("Buffer of size {0} too large for USB API")]
    BufferTooBig(usize),
    #[error("Malformed descriptor table")]
    MalformedDescriptor,
    #[error("Could not find appropriate interface")]
    InterfaceNotFound,
    #[error("IO Error: {0:?}")]
    IOError(#[from] std::io::Error),
    #[error("Discovered device with malformed name: {0}")]
    BadDeviceName(String),
    #[error("Error watching device folder: {0:?}")]
    NotifyError(#[from] notify::Error),
}

/// Descriptive information about a USB device.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DeviceDescriptor {
    pub vendor: u16,
    pub product: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// Type of an endpoint on a USB interface.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EndpointType {
    Control,
    Interrupt,
    Isochronous,
    Bulk,
}

/// Direction an Endpoint flows on a USB interface.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EndpointDirection {
    In,
    Out,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InterfaceDescriptor {
    pub id: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub alternate: u8,
    pub endpoints: Vec<EndpointDescriptor>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct EndpointDescriptor {
    pub ty: EndpointType,
    pub address: u8,
}

impl EndpointDescriptor {
    pub fn direction(&self) -> EndpointDirection {
        if (self.address & USB_ENDPOINT_DIR_MASK) != 0 {
            EndpointDirection::In
        } else {
            EndpointDirection::Out
        }
    }
}

/// A USB endpoint. Wraps the four types of endpoint for easy carry.
pub enum Endpoint {
    BulkIn(BulkInEndpoint),
    BulkOut(BulkOutEndpoint),
    Isochronous(IsochronousEndpoint),
    Interrupt(InterruptEndpoint),
    Control(ControlEndpoint),
}

/// Waits for USB devices to appear on the bus.
pub fn wait_for_devices(
    notify_added: bool,
    notify_removed: bool,
) -> Result<impl Stream<Item = Result<DeviceEvent>>> {
    usb_plat::wait_for_devices(notify_added, notify_removed)
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
