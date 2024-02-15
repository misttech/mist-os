// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context as _, Error};
use fidl::endpoints::Proxy;
use fidl_fuchsia_bluetooth_snoop::{PacketType, SnoopPacket as FidlSnoopPacket, Timestamp};
use fidl_fuchsia_hardware_bluetooth::VendorMarker as HardwareVendorMarker;
use fidl_fuchsia_io::DirectoryProxy;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, Channel, DurationNum, MessageBuf};
use futures::Stream;
use std::{
    marker::Unpin,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use crate::bounded_queue::{CreatedAt, SizeOf};

/// A wrapper type for the bitmask representing flags in the snoop channel protocol.
struct HciFlags(u8);

impl HciFlags {
    const IS_RECEIVED: u8 = 0b100;
    const PACKET_TYPE_CMD: u8 = 0b00;
    const PACKET_TYPE_EVENT: u8 = 0b01;
    // Included for completeness. Used in tests
    #[allow(dead_code)]
    const PACKET_TYPE_DATA: u8 = 0b10;

    /// Does the packet represent an HCI event sent from the controller to the host?
    fn is_received(&self) -> bool {
        self.0 & HciFlags::IS_RECEIVED == HciFlags::IS_RECEIVED
    }
    /// Returns the packet type.
    fn hci_packet_type(&self) -> PacketType {
        match self.0 & 0b11 {
            HciFlags::PACKET_TYPE_CMD => PacketType::Cmd,
            HciFlags::PACKET_TYPE_EVENT => PacketType::Event,
            _ => PacketType::Data,
        }
    }
}

pub struct SnoopPacket {
    pub is_received: bool,
    pub type_: PacketType,
    // Clock monotonic timestamp.
    // Not exposed in the public API so that it is not used without
    // a ClockTransformation being applied.
    timestamp: zx::Time,
    pub original_len: usize,
    pub payload: Vec<u8>,
}

impl SnoopPacket {
    pub fn new(
        is_received: bool,
        type_: PacketType,
        monotonic_timestamp: zx::Time,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            is_received,
            type_,
            timestamp: monotonic_timestamp,
            original_len: payload.len(),
            payload,
        }
    }

    pub fn timestamp_parts(&self, clock_xform: Option<&zx::ClockTransformation>) -> (i64, i32) {
        let nanos = if let Some(xform) = clock_xform {
            xform.apply(self.timestamp)
        } else {
            self.timestamp
        }
        .into_nanos();
        let seconds = nanos / 1_000_000_000;
        let nanos = nanos % 1_000_000_000;
        (seconds, nanos as i32)
    }

    /// Create a FidlSnoopPacket. Use `clock` to transform the timestamp
    /// to the correct synthetic Clock.
    pub fn to_fidl(&self, clock_xform: Option<&zx::ClockTransformation>) -> FidlSnoopPacket {
        let (seconds, subsec_nanos) = self.timestamp_parts(clock_xform);
        FidlSnoopPacket {
            is_received: self.is_received,
            type_: self.type_,
            timestamp: Timestamp { subsec_nanos: subsec_nanos as u32, seconds: seconds as u64 },
            original_len: self.original_len as u32,
            payload: self.payload.clone(),
        }
    }
}

impl SizeOf for SnoopPacket {
    fn size_of(&self) -> usize {
        std::mem::size_of::<Self>() + self.payload.len()
    }
}

impl CreatedAt for SnoopPacket {
    fn created_at(&self) -> Duration {
        let (secs, nanos) = self.timestamp_parts(None);
        Duration::new(secs as u64, nanos as u32)
    }
}

/// A Snooper provides a `Stream` associated with the snoop channel for a single HCI device. This
/// stream can be polled for `SnoopPacket`s coming off the channel.
#[derive(Debug)]
pub(crate) struct Snooper {
    pub device_name: String,
    pub chan: fuchsia_async::Channel,
}

impl Snooper {
    /// Create a new snooper from a device path. This opens a new snoop channel, returning an error
    /// if the devices doesn't exist or the channel cannot be created.
    pub fn new(dir: &DirectoryProxy, path: &str) -> Result<Snooper, Error> {
        let vendor = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
            HardwareVendorMarker,
        >(dir, path)
        .context("failed to open bt-hci device")?;

        let client_end_vendor = vendor.into_client_end().unwrap();
        let vendor_sync = client_end_vendor.into_sync_proxy();

        let client_end = vendor_sync
            .open_hci(fasync::Time::after(3.seconds()).into())?
            .map_err(|e| format_err!("Failed opening the HCI with {e:?}"))?;
        let sync_hci = client_end.into_sync_proxy();

        let (ours, theirs) = Channel::create();
        let res = sync_hci
            .open_snoop_channel(theirs, fasync::Time::after(3.seconds()).into())
            .context("open snoop channel request")?
            .map_err(|e| format_err!("Failed with error: {}", e));
        match res {
            Ok(()) => Snooper::from_channel(ours, path),
            Err(e) => Err(format_err!("Failed to open snoop channel {}", e)),
        }
    }

    /// Take a channel and wrap it in a `Snooper`.
    pub fn from_channel(chan: Channel, path: &str) -> Result<Snooper, Error> {
        let chan = fasync::Channel::from_channel(chan);

        let device_name = path.to_owned();
        Ok(Snooper { device_name, chan })
    }

    /// Parse a raw byte buffer and return a SnoopPacket. Returns `None` if the buffer does not
    /// contain a valid packet.
    pub fn build_pkt(buf: MessageBuf) -> Option<SnoopPacket> {
        if buf.bytes().is_empty() {
            return None;
        }
        let flags = HciFlags(buf.bytes()[0]);
        let time = zx::Time::get_monotonic();
        let payload = buf.bytes()[1..].to_vec();
        Some(SnoopPacket::new(flags.is_received(), flags.hci_packet_type(), time, payload))
    }
}

impl Unpin for Snooper {}
impl Stream for Snooper {
    type Item = (String, SnoopPacket);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buf = MessageBuf::new();
        match self.chan.recv_from(cx, &mut buf) {
            Poll::Ready(_t) => {
                let item = Snooper::build_pkt(buf).map(|pkt| (self.device_name.clone(), pkt));
                Poll::Ready(item)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fuchsia_async as fasync;
    use fuchsia_zircon::Channel;
    use futures::StreamExt;
    use std::task::Poll;

    #[test]
    fn test_from_channel() {
        let _exec = fasync::TestExecutor::new();
        let (channel, _) = Channel::create();
        let snooper = Snooper::from_channel(channel, "c").unwrap();
        assert_eq!(snooper.device_name, "c");
    }

    #[test]
    fn test_from_directory_proxy_timeout() {
        let _exec = fasync::TestExecutor::new();
        let (proxy, _requests) =
            fidl::endpoints::create_proxy_and_stream::<fidl_fuchsia_io::DirectoryMarker>().unwrap();
        let _ = Snooper::new(&proxy, "foo").expect_err("should have timed out");
    }

    #[test]
    fn test_build_pkt() {
        let flags = HciFlags::IS_RECEIVED;
        let buf = MessageBuf::new_with(vec![flags], vec![]);
        let pkt = Snooper::build_pkt(buf).unwrap();
        assert!(pkt.is_received);
        assert!(pkt.payload.is_empty());
        assert!(pkt.timestamp.into_nanos() > 0);
        assert_eq!(pkt.type_, PacketType::Cmd);

        let flags = HciFlags::PACKET_TYPE_DATA;
        let buf = MessageBuf::new_with(vec![flags, 0, 1, 2], vec![]);
        let pkt = Snooper::build_pkt(buf).unwrap();
        assert!(!pkt.is_received);
        assert_eq!(pkt.payload, vec![0, 1, 2]);
        assert_eq!(pkt.type_, PacketType::Data);
    }

    #[test]
    fn test_snoop_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (tx, rx) = Channel::create();
        let mut snooper = Snooper::from_channel(rx, "c").unwrap();
        let flags = HciFlags::IS_RECEIVED | HciFlags::PACKET_TYPE_EVENT;
        tx.write(&[flags, 0, 1, 2], &mut vec![]).unwrap();
        tx.write(&[0, 3, 4, 5], &mut vec![]).unwrap();

        let item_1 = exec.run_until_stalled(&mut snooper.next());
        let item_2 = exec.run_until_stalled(&mut snooper.next());
        assert!(item_1.is_ready());
        assert!(item_2.is_ready());
        match (item_1, item_2) {
            (Poll::Ready(item_1), Poll::Ready(item_2)) => {
                assert_eq!(item_1.unwrap().1.payload, vec![0, 1, 2]);
                assert_eq!(item_2.unwrap().1.payload, vec![3, 4, 5]);
            }
            _ => panic!("failed to build both packets 1 and 2 from snoop stream"),
        }

        let item_3 = exec.run_until_stalled(&mut snooper.next());
        assert!(item_3.is_pending());
    }
}
