// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;

use core::fmt::{Debug, Formatter};
use num_derive::FromPrimitive;

/// Represents a NAT64 Translator State.
///
/// Functional equivalent of [`otsys::otNat64State`](crate::otsys::otNat64State).
#[derive(Debug, Copy, Clone, Eq, Ord, PartialOrd, PartialEq, FromPrimitive)]
pub enum Nat64State {
    /// Functional equivalent of [`otsys::OT_NAT64_STATE_DISABLED`](crate::otsys::OT_NAT64_STATE_DISABLED).
    Disabled = OT_NAT64_STATE_DISABLED as isize,

    /// Functional equivalent of [`otsys::OT_NAT64_STATE_NOT_RUNNING`](crate::otsys::OT_NAT64_STATE_NOT_RUNNING).
    NotRunning = OT_NAT64_STATE_NOT_RUNNING as isize,

    /// Functional equivalent of [`otsys::OT_NAT64_STATE_IDLE`](crate::otsys::OT_NAT64_STATE_IDLE).
    Idle = OT_NAT64_STATE_IDLE as isize,

    /// Functional equivalent of [`otsys::OT_NAT64_STATE_ACTIVE`](crate::otsys::OT_NAT64_STATE_ACTIVE).
    Active = OT_NAT64_STATE_ACTIVE as isize,
}

impl From<otNat64State> for Nat64State {
    fn from(x: otNat64State) -> Self {
        use num::FromPrimitive;
        Self::from_u32(x).unwrap_or_else(|| panic!("Unknown otNat64State value: {x}"))
    }
}

impl From<Nat64State> for otNat64State {
    fn from(x: Nat64State) -> Self {
        x as otNat64State
    }
}

/// Data type representing an IPv4 CIDR.
///
/// Functional equivalent of [`otsys::otIp4Cidr`](crate::otsys::otIp4Cidr).
#[derive(Default, Clone, Copy)]
#[repr(transparent)]
pub struct Ip4Cidr(pub otIp4Cidr);

impl_ot_castable!(Ip4Cidr, otIp4Cidr);

impl Debug for Ip4Cidr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "IpAddr:{:?}, PrefixLength:{}", self.get_address_bytes(), self.get_length())
    }
}

impl PartialEq for Ip4Cidr {
    fn eq(&self, other: &Ip4Cidr) -> bool {
        if (self.get_length() != other.get_length())
            || (self.get_address_bytes() != other.get_address_bytes())
        {
            return false;
        }
        true
    }
}

impl Eq for Ip4Cidr {}

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
    pub fn get_address_bytes(&self) -> [u8; 4] {
        unsafe { self.0.mAddress.mFields.m8 }
    }

    /// Get the length of IPv4 CIDR
    pub fn get_length(&self) -> u8 {
        self.0.mLength
    }
}
