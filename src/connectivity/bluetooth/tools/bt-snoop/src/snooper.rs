// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context as _, Error};
use fidl::endpoints::{ClientEnd, Proxy};
use fidl_fuchsia_bluetooth_snoop::{PacketFormat, SnoopPacket as FidlSnoopPacket};
use fidl_fuchsia_hardware_bluetooth::{
    PacketDirection as Direction, SnoopEvent, SnoopEventStream, SnoopMarker,
    SnoopOnObservePacketRequest, SnoopPacket as HardwareSnoopPacket, SnoopProxy,
    VendorMarker as HardwareVendorMarker,
};
use fidl_fuchsia_io::DirectoryProxy;
use fuchsia_async as fasync;

use futures::{ready, Stream, StreamExt};
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::warn;

use crate::bounded_queue::{CreatedAt, SizeOf};

pub struct SnoopPacket {
    pub is_received: bool,
    pub format: PacketFormat,
    pub timestamp: zx::MonotonicInstant,
    pub original_len: usize,
    pub payload: Vec<u8>,
}

impl SnoopPacket {
    pub fn new(
        is_received: bool,
        format: PacketFormat,
        timestamp: zx::MonotonicInstant,
        payload: Vec<u8>,
    ) -> Self {
        Self { is_received, format, timestamp, original_len: payload.len(), payload }
    }

    /// Create a FidlSnoopPacket
    pub fn to_fidl(&self) -> FidlSnoopPacket {
        FidlSnoopPacket {
            is_received: Some(self.is_received),
            format: Some(self.format),
            timestamp: Some(self.timestamp.into_nanos()),
            length: Some(self.original_len as u32),
            data: Some(self.payload.clone()),
            ..Default::default()
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
        Duration::from_nanos(self.timestamp.into_nanos() as u64)
    }
}

/// A Snooper provides a `Stream` associated with the snoop channel for a single HCI device. This
/// stream can be polled for packets.
pub(crate) struct Snooper {
    pub device_name: String,
    pub proxy: SnoopProxy,
    pub event_stream: SnoopEventStream,
    pub is_terminated: bool,
}

impl fmt::Debug for Snooper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snooper")
            .field("device_name", &self.device_name)
            .field("is_terminated", &self.is_terminated)
            .finish_non_exhaustive()
    }
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

        let snoop_client = vendor_sync
            .open_snoop(
                fasync::MonotonicInstant::after(zx::MonotonicDuration::from_seconds(3)).into(),
            )?
            .map_err(|e| format_err!("Failed opening Snoop with {e:?}"))?;

        Ok(Snooper::from_client(snoop_client, path))
    }

    pub fn from_client(client: ClientEnd<SnoopMarker>, path: &str) -> Snooper {
        let device_name = path.to_owned();
        let proxy = client.into_proxy();
        let event_stream = proxy.take_event_stream();
        Snooper { device_name, proxy, event_stream, is_terminated: false }
    }
}

impl TryFrom<SnoopOnObservePacketRequest> for SnoopPacket {
    type Error = Error;

    fn try_from(value: SnoopOnObservePacketRequest) -> Result<Self, Self::Error> {
        let time = zx::MonotonicInstant::get();
        let SnoopOnObservePacketRequest {
            packet: Some(packet), direction: Some(direction), ..
        } = value
        else {
            return Err(format_err!("Missing required fields"));
        };
        let packet_format;
        let buf = match packet {
            HardwareSnoopPacket::Event(buf) => {
                packet_format = PacketFormat::Event;
                buf
            }
            HardwareSnoopPacket::Command(buf) => {
                packet_format = PacketFormat::Command;
                buf
            }
            HardwareSnoopPacket::Acl(buf) => {
                packet_format = PacketFormat::AclData;
                buf
            }
            HardwareSnoopPacket::Sco(buf) => {
                packet_format = PacketFormat::SynchronousData;
                buf
            }
            HardwareSnoopPacket::Iso(buf) => {
                packet_format = PacketFormat::IsoData;
                buf
            }
            _ => return Err(format_err!("Unknown packet type")),
        };
        let is_received = direction == Direction::ControllerToHost;
        return Ok(SnoopPacket::new(is_received, packet_format, time, buf));
    }
}

impl Stream for Snooper {
    type Item = (String, SnoopPacket);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.is_terminated {
            return Poll::Ready(None);
        }
        loop {
            let result = ready!(self.event_stream.poll_next_unpin(cx)).transpose();
            if let Err(e) = result {
                warn!("error polling SnoopRequestStream: {e:?}");
                self.is_terminated = true;
                return Poll::Ready(None);
            }
            let Ok(Some(req)) = result else {
                self.is_terminated = true;
                return Poll::Ready(None);
            };

            match req {
                SnoopEvent::OnObservePacket { payload } => {
                    let Some(sequence) = payload.sequence else {
                        warn!("ObservePacket missing sequence number");
                        continue;
                    };
                    let result = self.proxy.acknowledge_packets(sequence);
                    if let Err(err) = result {
                        warn!("acknowledge_packets error: {:?}", err);
                        self.is_terminated = true;
                        return Poll::Ready(None);
                    }
                    let packet_result = SnoopPacket::try_from(payload);
                    match packet_result {
                        Ok(packet) => {
                            return Poll::Ready(Some((self.device_name.clone(), packet)));
                        }
                        Err(err) => {
                            warn!("ObservePacket parse error: {:?}", err);
                            self.is_terminated = true;
                            return Poll::Ready(None);
                        }
                    }
                }
                _ => continue,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_hardware_bluetooth::SnoopRequest;
    use fuchsia_async as fasync;
    use futures::StreamExt;

    #[test]
    fn test_from_proxy() {
        let _exec = fasync::TestExecutor::new();
        let (client, _stream) = fidl::endpoints::create_request_stream::<SnoopMarker>();
        let snooper = Snooper::from_client(client, "c");
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
    fn test_try_from() {
        let req = SnoopOnObservePacketRequest {
            sequence: Some(0),
            direction: Some(Direction::ControllerToHost),
            packet: Some(HardwareSnoopPacket::Event(vec![])),
            ..Default::default()
        };
        let pkt = SnoopPacket::try_from(req).unwrap();
        assert!(pkt.is_received);
        assert!(pkt.payload.is_empty());
        assert!(pkt.timestamp.into_nanos() > 0);
        assert_eq!(pkt.format, PacketFormat::Event);

        let req = SnoopOnObservePacketRequest {
            sequence: Some(0),
            direction: Some(Direction::HostToController),
            packet: Some(HardwareSnoopPacket::Acl(vec![0, 1, 2])),
            ..Default::default()
        };
        let pkt = SnoopPacket::try_from(req).unwrap();
        assert!(!pkt.is_received);
        assert_eq!(pkt.payload, vec![0, 1, 2]);
        assert_eq!(pkt.format, PacketFormat::AclData);
    }

    #[test]
    fn test_try_from_missing_payload() {
        let req = SnoopOnObservePacketRequest {
            sequence: Some(0),
            direction: Some(Direction::ControllerToHost),
            packet: None,
            ..Default::default()
        };
        let result = SnoopPacket::try_from(req);
        assert!(result.is_err());
    }

    #[test]
    fn test_try_from_missing_direction() {
        let req = SnoopOnObservePacketRequest {
            sequence: Some(0),
            direction: None,
            packet: Some(HardwareSnoopPacket::Event(vec![0, 1, 2])),
            ..Default::default()
        };
        let result = SnoopPacket::try_from(req);
        assert!(result.is_err());
    }

    #[test]
    fn test_snoop_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (snoop_client, mut req_stream) =
            fidl::endpoints::create_request_stream::<SnoopMarker>();
        let snoop_control = req_stream.control_handle();
        let mut snooper = Snooper::from_client(snoop_client, "c");
        let req_0 = SnoopOnObservePacketRequest {
            sequence: Some(0),
            direction: Some(Direction::ControllerToHost),
            packet: Some(HardwareSnoopPacket::Event(vec![0, 1, 2])),
            ..Default::default()
        };
        let req_1 = SnoopOnObservePacketRequest {
            sequence: Some(1),
            direction: Some(Direction::HostToController),
            packet: Some(HardwareSnoopPacket::Command(vec![3, 4, 5])),
            ..Default::default()
        };
        snoop_control.send_on_observe_packet(&req_0).unwrap();
        snoop_control.send_on_observe_packet(&req_1).unwrap();

        let item_1 = exec.run_until_stalled(&mut snooper.next());
        assert!(item_1.is_ready());
        let item_2 = exec.run_until_stalled(&mut snooper.next());
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

        let req_1 = exec.run_until_stalled(&mut req_stream.next());
        assert!(req_1.is_ready());
        match req_1 {
            Poll::Ready(Some(Ok(SnoopRequest::AcknowledgePackets { sequence, .. }))) => {
                assert_eq!(sequence, 0);
            }
            _ => panic!("failed to send OnAcknowledgePackets"),
        }
        let req_2 = exec.run_until_stalled(&mut req_stream.next());
        assert!(req_2.is_ready());
        match req_2 {
            Poll::Ready(Some(Ok(SnoopRequest::AcknowledgePackets { sequence, .. }))) => {
                assert_eq!(sequence, 1);
            }
            _ => panic!("failed to send OnAcknowledgePackets"),
        }
        let req_3 = exec.run_until_stalled(&mut req_stream.next());
        assert!(req_3.is_pending());
    }

    #[test]
    fn test_snoop_stream_missing_sequence() {
        let mut exec = fasync::TestExecutor::new();
        let (snoop_client, snoop_stream) = fidl::endpoints::create_request_stream::<SnoopMarker>();
        let snoop_control = snoop_stream.control_handle();
        let mut snooper = Snooper::from_client(snoop_client, "c");

        let req_0 = SnoopOnObservePacketRequest {
            sequence: None, // Missing sequence!
            direction: Some(Direction::ControllerToHost),
            packet: Some(HardwareSnoopPacket::Event(vec![0, 1, 2])),
            ..Default::default()
        };
        snoop_control.send_on_observe_packet(&req_0).unwrap();
        let item = exec.run_until_stalled(&mut snooper.next());
        assert!(item.is_pending());

        let req_1 = SnoopOnObservePacketRequest {
            sequence: Some(1),
            direction: Some(Direction::HostToController),
            packet: Some(HardwareSnoopPacket::Command(vec![3, 4, 5])),
            ..Default::default()
        };
        snoop_control.send_on_observe_packet(&req_1).unwrap();
        let item = exec.run_until_stalled(&mut snooper.next());
        assert!(item.is_ready());
    }

    #[test]
    fn test_snoop_stream_lifecycle() {
        let mut exec = fasync::TestExecutor::new();
        let (snoop_client, snoop_server) = fidl::endpoints::create_endpoints::<SnoopMarker>();
        let mut snooper = Snooper::from_client(snoop_client, "c");

        let item = exec.run_until_stalled(&mut snooper.next());
        assert!(item.is_pending());

        drop(snoop_server);
        let item = exec.run_until_stalled(&mut snooper.next());
        let Poll::Ready(None) = item else {
            panic!("Expected None");
        };
    }
}
