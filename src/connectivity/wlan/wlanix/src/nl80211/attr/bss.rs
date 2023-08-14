// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use netlink_packet_utils::{
    byteorder::{ByteOrder, NativeEndian},
    nla::Nla,
    Emitable,
};
use std::mem::{size_of, size_of_val};

use crate::nl80211::constants::*;

#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Nl80211BssAttr {
    Bssid([u8; 6]),
    Frequency(u32),
    InformationElement(Vec<u8>),
    LastSeenBoottime(u64),
    SignalMbm(i32),
    Capability(u16),
    Status(u32),
    ChainSignal(Vec<ChainSignalAttr>),
}

impl Nla for Nl80211BssAttr {
    fn value_len(&self) -> usize {
        use Nl80211BssAttr::*;
        match self {
            Bssid(val) => size_of_val(val),
            Frequency(val) => size_of_val(val),
            InformationElement(val) => size_of_val(&val[..]),
            LastSeenBoottime(val) => size_of_val(val),
            SignalMbm(val) => size_of_val(val),
            Capability(val) => size_of_val(val),
            Status(val) => size_of_val(val),
            ChainSignal(val) => val.as_slice().buffer_len(),
        }
    }

    fn kind(&self) -> u16 {
        use Nl80211BssAttr::*;
        match self {
            Bssid(_) => NL80211_BSS_BSSID,
            Frequency(_) => NL80211_BSS_FREQUENCY,
            InformationElement(_) => NL80211_BSS_INFORMATION_ELEMENTS,
            LastSeenBoottime(_) => NL80211_BSS_LAST_SEEN_BOOTTIME,
            SignalMbm(_) => NL80211_BSS_SIGNAL_MBM,
            Capability(_) => NL80211_BSS_CAPABILITY,
            Status(_) => NL80211_BSS_STATUS,
            ChainSignal(_) => NL80211_BSS_CHAIN_SIGNAL,
        }
    }

    fn emit_value(&self, buffer: &mut [u8]) {
        use Nl80211BssAttr::*;
        match self {
            Bssid(val) => buffer.copy_from_slice(&val[..]),
            Frequency(val) => NativeEndian::write_u32(buffer, *val),
            InformationElement(val) => buffer.copy_from_slice(&val[..]),
            LastSeenBoottime(val) => NativeEndian::write_u64(buffer, *val),
            SignalMbm(val) => NativeEndian::write_i32(buffer, *val),
            Capability(val) => NativeEndian::write_u16(buffer, *val),
            Status(val) => NativeEndian::write_u32(buffer, *val),
            ChainSignal(val) => val.as_slice().emit(buffer),
        }
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ChainSignalAttr {
    pub id: u16,
    pub rssi: i8,
}

impl Nla for ChainSignalAttr {
    fn value_len(&self) -> usize {
        size_of_val(&self.rssi)
    }

    fn kind(&self) -> u16 {
        self.id
    }

    fn emit_value(&self, buffer: &mut [u8]) {
        buffer[0] = self.rssi as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bss_attrs() {
        let attrs = vec![
            Nl80211BssAttr::Bssid([1, 3, 3, 7, 4, 2]),
            Nl80211BssAttr::Frequency(0xaabb),
            Nl80211BssAttr::InformationElement(vec![22, 33, 44]),
            Nl80211BssAttr::LastSeenBoottime(0xccddeeff),
            Nl80211BssAttr::SignalMbm(0x0a0b),
            Nl80211BssAttr::Capability(0x1a1b),
            Nl80211BssAttr::Status(0x2a2b2c),
            Nl80211BssAttr::ChainSignal(vec![
                ChainSignalAttr { id: 99, rssi: -20 },
                ChainSignalAttr { id: 111, rssi: -40 },
            ]),
        ];

        let mut buffer = vec![0; attrs.as_slice().buffer_len()];
        attrs.as_slice().emit(&mut buffer[..]);

        let expected_buffer = vec![
            10, 0, // length
            1, 0, // kind: bssid
            1, 3, 3, 7, 4, 2, // value
            0, 0, // padding
            8, 0, // length
            2, 0, // kind: frequency
            0xbb, 0xaa, 0, 0, // value
            7, 0, // length
            6, 0, // kind: information element
            22, 33, 44, // value
            0,  // padding
            12, 0, // length
            15, 0, // kind: last seen boottime
            0xff, 0xee, 0xdd, 0xcc, 0, 0, 0, 0, // value
            8, 0, // length
            7, 0, // kind: signal mbm
            0x0b, 0x0a, 0, 0, // value
            6, 0, // length
            5, 0, // kind: capability
            0x1b, 0x1a, // value
            0, 0, // padding
            8, 0, // length
            9, 0, // kind: status
            0x2c, 0x2b, 0x2a, 0, // value
            20, 0, // length
            19, 0, // kind
            5, 0, 99, 0, 236, 0, 0, 0, // first chain
            5, 0, 111, 0, 216, 0, 0, 0, // second chain
        ];

        assert_eq!(buffer, expected_buffer);
    }
}
