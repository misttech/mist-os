// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Marker traits with blanket implementations.
//!
//! Traits in this module exist to be exported as markers to bindings without
//! exposing the internal traits directly.

/// A marker for extensions to IP types.
pub trait IpExt:
    crate::ip::IpExt + crate::ip::icmp::IcmpIpExt + crate::transport::tcp::socket::DualStackIpExt
{
}

impl<O> IpExt for O where
    O: crate::ip::IpExt
        + crate::ip::icmp::IcmpIpExt
        + crate::transport::tcp::socket::DualStackIpExt
{
}
