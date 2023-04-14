// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

mod usb_linux;

/// Selects the bit in USB endpoint addresses that tells us whether it is an in or and out endpoint.
pub(crate) const USB_ENDPOINT_DIR_MASK: u8 = 0x80;

/// Device discovery events. See `wait_for_devices`.
#[derive(Debug)]
pub enum DeviceEvent {
    /// Indicates a new USB device has been plugged in.
    Added(String),
    /// Indicates a USB device has been unplugged.
    Removed(String),
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

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub use usb_linux::{
    scan_interfaces_for_device, wait_for_devices, BulkInEndpoint, BulkOutEndpoint, ControlEndpoint,
    Endpoint, Interface, InterruptEndpoint, IsochronousEndpoint,
};
