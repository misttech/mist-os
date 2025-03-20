// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![warn(missing_docs, unsafe_op_in_unsafe_fn)]
//! A transport-agnostic library for implementing a vsock bridge over a usb bulk device.

mod connection;
mod packet;

pub use connection::*;
pub use packet::*;

/// An address for a vsock packet transmitted over USB. Since this library does not implement
/// policy decisions, it includes all four components of a vsock address pair even though some
/// may not be appropriate for some situations.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Address {
    /// For Connect, Reset, Accept, and Data packets this represents the device side's address.
    /// Usually this will be a special value representing either that it is simply "the device",
    /// or zero along with the rest of the cid and port fields to indicate that it's a control stream
    /// packet. Must be zero for any other packet type.
    pub device_cid: u32,
    /// For Connect, Reset, Accept, and Data packets this represents the host side's address.
    /// Usually this will be a special value representing either that it is simply "the host",
    /// or zero along with the rest of the cid and port fields to indicate that it's a control stream
    /// packet. Must be zero for any other packet type.
    pub host_cid: u32,
    /// For Connect, Reset, Accept, and Data packets this represents the device side's port.
    /// This must be a valid positive value for any of those packet types, unless all of the cid and
    /// port fields are also zero, in which case it is a control stream packet. Must be zero for any
    /// other packet type.
    pub device_port: u32,
    /// For Connect, Reset, Accept, and Data packets this represents the host side's port.
    /// This must be a valid positive value for any of those packet types, unless all of the cid and
    /// port fields are also zero, in which case it is a control stream packet. Must be zero for any
    /// other packet type.
    pub host_port: u32,
}

impl Address {
    /// Returns true if all the fields of this address are zero (which usually means it's a control
    /// packet of some sort).
    pub fn is_zeros(&self) -> bool {
        *self == Self::default()
    }
}

impl From<&Header> for Address {
    fn from(header: &Header) -> Self {
        Self {
            device_cid: header.device_cid.get(),
            host_cid: header.host_cid.get(),
            device_port: header.device_port.get(),
            host_port: header.host_port.get(),
        }
    }
}
