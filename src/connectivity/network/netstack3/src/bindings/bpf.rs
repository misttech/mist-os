// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ebpf::{
    link_program, BpfProgramContext, BpfValue, CbpfConfig, DataWidth, EbpfInstruction, EbpfProgram,
    Packet, ProgramArgument, Type, VerifiedEbpfProgram,
};
use ebpf_api::{
    __sk_buff, SKF_AD_OFF, SKF_AD_PROTOCOL, SKF_LL_OFF, SKF_NET_OFF, SK_BUF_TYPE,
    SOCKET_FILTER_CBPF_CONFIG,
};
use fidl_fuchsia_posix_socket_packet as fppacket;
use netstack3_core::device_socket::Frame;
use std::collections::HashMap;
use zerocopy::FromBytes;

// Packet buffer representation used for BPF filters.
#[repr(C)]
struct IpPacketForBpf<'a> {
    // This field must be first. eBPF programs will access it directly.
    sk_buff: __sk_buff,

    kind: fppacket::Kind,
    frame: Frame<&'a [u8]>,
    raw: &'a [u8],
}

impl Packet for &'_ IpPacketForBpf<'_> {
    fn load<'a>(&self, offset: i32, width: DataWidth) -> Option<BpfValue> {
        // cBPF Socket Filters use non-negative offset to access packet content.
        // Negative offsets are handler as follows as follows:
        //   SKF_AD_OFF (-0x1000) - Auxiliary info that may be outside of the packet.
        //      Currently only SKF_AD_PROTOCOL is implemented.
        //   SKF_NET_OFF (-0x100000) - Packet content relative to the IP header.
        //   SKF_LL_OFF (-0x200000) - Packet content relative to the link-level header.
        let (offset, slice) = if offset >= 0 {
            (
                offset,
                match self.kind {
                    fppacket::Kind::Network => self.frame.into_body(),
                    fppacket::Kind::Link => self.raw,
                },
            )
        } else if offset >= SKF_AD_OFF {
            if offset == SKF_AD_OFF + SKF_AD_PROTOCOL {
                return Some(self.frame.protocol().unwrap_or(0).into());
            } else {
                log::info!(
                    "cBPF program tried to access unimplemented SKF_AD_OFF offset: {}",
                    offset - SKF_AD_OFF
                );
                return None;
            }
        } else if offset >= SKF_NET_OFF {
            // Access network level packet.
            (offset - SKF_NET_OFF, self.frame.into_body())
        } else if offset >= SKF_LL_OFF {
            // Access link-level packet.
            (offset - SKF_LL_OFF, self.raw)
        } else {
            return None;
        };

        let offset = offset.try_into().unwrap();

        if offset >= slice.len() {
            return None;
        }

        // The packet is stored in network byte order, so multi-byte loads need to fix endianness.
        // Potentially this could be handled in the cBPF converter but then it would need to be
        // disabled from seccomp filter, which always runs in the host byte order.
        let slice = &slice[offset..];
        match width {
            DataWidth::U8 => u8::read_from_prefix(slice).ok().map(|(v, _)| v.into()),
            DataWidth::U16 => zerocopy::U16::<zerocopy::NetworkEndian>::read_from_prefix(slice)
                .ok()
                .map(|(v, _)| v.get().into()),
            DataWidth::U32 => zerocopy::U32::<zerocopy::NetworkEndian>::read_from_prefix(slice)
                .ok()
                .map(|(v, _)| v.get().into()),
            DataWidth::U64 => zerocopy::U64::<zerocopy::NetworkEndian>::read_from_prefix(slice)
                .ok()
                .map(|(v, _)| v.get().into()),
        }
    }
}

impl ProgramArgument for &'_ IpPacketForBpf<'_> {
    fn get_type() -> &'static Type {
        &*SK_BUF_TYPE
    }
}

struct SocketFilterContext {}

impl BpfProgramContext for SocketFilterContext {
    type RunContext<'a> = ();
    type Packet<'a> = &'a IpPacketForBpf<'a>;
    const CBPF_CONFIG: &'static CbpfConfig = &SOCKET_FILTER_CBPF_CONFIG;
}

#[derive(Debug)]
pub(crate) struct SocketFilterProgram {
    program: EbpfProgram<SocketFilterContext>,
}

pub(crate) enum SocketFilterResult {
    // If the packet is accepted it may need to trimmed to the specified size.
    Accept(usize),
    Reject,
}

impl SocketFilterProgram {
    pub(crate) fn new(code: Vec<u64>) -> Self {
        // Convert the `code` to `EbpfInstruction`.
        // SAFETY:  This is safe because `EbpfInstruction` is 64 bits.
        let code = unsafe {
            let mut code = std::mem::ManuallyDrop::new(code);
            Vec::from_raw_parts(
                code.as_mut_ptr() as *mut EbpfInstruction,
                code.len(),
                code.capacity(),
            )
        };

        // TODO(https://fxbug.dev/370043219) Currently we assume that the code has been verified.
        // This is safe because fuchsia.posix.socket.packet is routed only to Starnix,
        // but that may change in the future. We need a better mechanism for permissions & BPF
        // verification.
        let program =
            VerifiedEbpfProgram::from_verified_code(code, SocketFilterContext::get_arg_types());
        let program = link_program::<SocketFilterContext>(&program, &[], &[], HashMap::new())
            .expect("Failed to link SocketFilter program");

        Self { program }
    }

    pub(crate) fn run(
        &self,
        kind: fppacket::Kind,
        frame: Frame<&[u8]>,
        raw: &[u8],
    ) -> SocketFilterResult {
        let packet_size = match kind {
            fppacket::Kind::Network => frame.into_body().len(),
            fppacket::Kind::Link => raw.len(),
        };

        let mut packet = IpPacketForBpf {
            sk_buff: __sk_buff { len: packet_size.try_into().unwrap(), ..Default::default() },
            kind,
            frame,
            raw,
        };

        let result = self.program.run(&mut (), &mut packet);
        match result {
            0 => SocketFilterResult::Reject,
            n => SocketFilterResult::Accept(n.try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ebpf::Packet;
    use ebpf_api::SKF_AD_MAX;
    use netstack3_core::device_socket::SentFrame;
    use packet::ParsablePacket;
    use packet_formats::ethernet::EthernetFrameLengthCheck;

    struct TestData;
    impl TestData {
        const PROTO: u16 = 0x08AB;
        const BUFFER: &'static [u8] = &[
            0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, // Dest MAC
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, // Source MAC
            0x08, 0xAB, // EtherType
            0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x3A, 0x4B, // Packet body
        ];
        const BODY_POSITION: i32 = 14;

        /// Creates an EthernetFrame with the values specified above.
        fn frame() -> Frame<&'static [u8]> {
            let mut buffer_view = Self::BUFFER;
            Frame::Sent(SentFrame::Ethernet(
                packet_formats::ethernet::EthernetFrame::parse(
                    &mut buffer_view,
                    EthernetFrameLengthCheck::NoCheck,
                )
                .unwrap()
                .into(),
            ))
        }
    }

    fn packet_load(packet: &IpPacketForBpf<'_>, offset: i32, width: DataWidth) -> Option<u64> {
        packet.load(offset, width).map(|v| v.as_u64())
    }

    // Test loading Ethernet header at the specified base offset.
    fn test_ll_header_load(packet: &IpPacketForBpf<'_>, base: i32) {
        assert_eq!(packet_load(packet, base, DataWidth::U8), Some(0x06));
        assert_eq!(packet_load(packet, base, DataWidth::U16), Some(0x0607));
        assert_eq!(packet_load(packet, base, DataWidth::U32), Some(0x06070809));
        assert_eq!(packet_load(packet, base, DataWidth::U64), Some(0x060708090A0B0001));

        // Loads past the Ethernet header load the packet body.
        assert_eq!(packet_load(packet, base + 8, DataWidth::U8), Some(0x02));
        assert_eq!(packet_load(packet, base + 8, DataWidth::U16), Some(0x0203));
        assert_eq!(packet_load(packet, base + 8, DataWidth::U32), Some(0x02030405));
        assert_eq!(packet_load(packet, base + 8, DataWidth::U64), Some(0x0203040508AB2122));
    }

    // Test loading packet body at the specified base offset.
    fn test_packet_body_load(packet: &IpPacketForBpf<'_>, base: i32) {
        assert_eq!(packet_load(packet, base, DataWidth::U64), Some(0x212223242526273A));
        assert_eq!(packet_load(packet, base, DataWidth::U8), Some(0x21));
        assert_eq!(packet_load(packet, base, DataWidth::U16), Some(0x2122));
        assert_eq!(packet_load(packet, base, DataWidth::U32), Some(0x21222324));
        assert_eq!(packet_load(packet, base, DataWidth::U64), Some(0x212223242526273A));

        assert_eq!(packet_load(packet, base + 6, DataWidth::U8), Some(0x27));
        assert_eq!(packet_load(packet, base + 6, DataWidth::U16), Some(0x273A));
        assert_eq!(packet_load(packet, base + 6, DataWidth::U32), None);
        assert_eq!(packet_load(packet, base + 6, DataWidth::U64), None);

        assert_eq!(packet_load(packet, base + 9, DataWidth::U8), None);
        assert_eq!(packet_load(packet, base + 9, DataWidth::U16), None);
        assert_eq!(packet_load(packet, base + 9, DataWidth::U32), None);
        assert_eq!(packet_load(packet, base + 9, DataWidth::U64), None);
    }

    #[test]
    fn network_level_packet() {
        let packet = IpPacketForBpf {
            sk_buff: Default::default(),
            kind: fppacket::Kind::Network,
            frame: TestData::frame(),
            raw: TestData::BUFFER,
        };

        test_packet_body_load(&packet, 0);

        assert_eq!(packet_load(&packet, i32::MAX, DataWidth::U8), None);
        assert_eq!(packet_load(&packet, i32::MAX, DataWidth::U16), None);
        assert_eq!(packet_load(&packet, i32::MAX, DataWidth::U32), None);
        assert_eq!(packet_load(&packet, i32::MAX, DataWidth::U64), None);
    }

    #[test]
    fn link_level_packet() {
        let packet = IpPacketForBpf {
            sk_buff: Default::default(),
            kind: fppacket::Kind::Link,
            frame: TestData::frame(),
            raw: TestData::BUFFER,
        };

        test_ll_header_load(&packet, 0);
        test_packet_body_load(&packet, TestData::BODY_POSITION);
    }

    #[test]
    fn negative_offsets() {
        let packet = IpPacketForBpf {
            sk_buff: Default::default(),
            kind: fppacket::Kind::Link,
            frame: TestData::frame(),
            raw: TestData::BUFFER,
        };

        // Loads from SKF_AD_OFF + SKF_AD_PROTOCOL load EtherType, ignoring data width.
        assert_eq!(
            packet_load(&packet, SKF_AD_OFF + SKF_AD_PROTOCOL, DataWidth::U8),
            Some(TestData::PROTO as u64)
        );
        assert_eq!(
            packet_load(&packet, SKF_AD_OFF + SKF_AD_PROTOCOL, DataWidth::U16),
            Some(TestData::PROTO as u64)
        );
        assert_eq!(
            packet_load(&packet, SKF_AD_OFF + SKF_AD_PROTOCOL, DataWidth::U32),
            Some(TestData::PROTO as u64)
        );

        // SKF_AD_MAX is the max offset that can be used with SKF_AD_OFF.
        assert_eq!(packet_load(&packet, SKF_AD_OFF + SKF_AD_MAX, DataWidth::U16), None);
        assert_eq!(packet_load(&packet, SKF_AD_OFF + SKF_AD_MAX + 1, DataWidth::U16), None);

        // SKF_LL_OFF can be used to load the packet starting from the LL header.
        test_ll_header_load(&packet, SKF_LL_OFF);
        test_packet_body_load(&packet, SKF_LL_OFF + TestData::BODY_POSITION);

        // Loasds with offset = SKF_NET_OFF+n load the packet starting from the
        // packet body (Network-level header).
        test_packet_body_load(&packet, SKF_NET_OFF);

        // Loads below `SKF_LL_OFF` should always fail.
        assert_eq!(packet_load(&packet, SKF_LL_OFF - 1, DataWidth::U16), None);
        assert_eq!(packet_load(&packet, SKF_LL_OFF - 8, DataWidth::U16), None);
        assert_eq!(packet_load(&packet, i32::MIN, DataWidth::U16), None);
    }
}
