// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;

use core::fmt::{Debug, Formatter};

/// Data type representing an IPv4 CIDR.
///
/// Functional equivalent of [`otsys::otIp4Cidr`](crate::otsys::otIp4Cidr).
#[derive(Default, Clone, Copy)]
#[repr(transparent)]
pub struct Ip4Cidr(pub otIp4Cidr);

impl_ot_castable!(Ip4Cidr, otIp4Cidr);

impl Debug for Ip4Cidr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "IpAddr:{:?}, PrefixLength:{}", self.get_address(), self.get_length())
    }
}

/// IPv4 address type
pub type Ip4Address = std::net::Ipv4Addr;

impl Ip4Cidr {
    /// create a new Ip4Cidr
    pub fn new(addr: [u8; 4], len: u8) -> Ip4Cidr {
        Ip4Cidr(otIp4Cidr {
            mAddress: otIp4Address { mFields: otIp4Address__bindgen_ty_1 { m8: addr } },
            mLength: len,
        })
    }

    /// Get the address of IPv4 CIDR
    pub fn get_address(&self) -> [u8; 4] {
        unsafe { self.0.mAddress.mFields.m8 }
    }

    /// Get the length of IPv4 CIDR
    pub fn get_length(&self) -> u8 {
        self.0.mLength
    }
}
