// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uapi;
use bitflags::bitflags;

bitflags! {
    // Used for both IPv4 and IPv6.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IptIpFlags {
    V4(IptIpFlagsV4),
    V6(IptIpFlagsV6),
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct IptIpFlagsV4: u32 {
        const Fragment = uapi::IPT_F_FRAG;
        const Goto = uapi::IPT_F_GOTO;
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct IptIpFlagsV6: u32 {
        const Protocol = uapi::IP6T_F_PROTO;
        const TOS = uapi::IP6T_F_TOS;
        const Goto = uapi::IP6T_F_GOTO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn all_known_bits_same_as_mask() {
        assert_eq!(IptIpInverseFlags::all().bits(), uapi::IPT_INV_MASK);
        assert_eq!(IptIpFlagsV4::all().bits(), uapi::IPT_F_MASK);
        assert_eq!(IptIpFlagsV6::all().bits(), uapi::IP6T_F_MASK);
    }
}
