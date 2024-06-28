// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uapi;
use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct IptIpInverseFlags: u32 {
        const InputInterface = uapi::IPT_INV_VIA_IN;
        const OutputInterface = uapi::IPT_INV_VIA_OUT;
        const TOS = uapi::IPT_INV_TOS;
        const SourceIpAddress = uapi::IPT_INV_SRCIP;
        const DestinationIpAddress = uapi::IPT_INV_DSTIP;
        const Fragment = uapi::IPT_INV_FRAG;
        const Protocol = uapi::IPT_INV_PROTO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn all_known_bits_same_as_mask() {
        assert_eq!(IptIpInverseFlags::all().bits(), uapi::IPT_INV_MASK);
    }
}
