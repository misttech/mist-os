// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;
use std::iter::FusedIterator;
use std::task::Poll;

use futures::future::FusedFuture;
use futures::task::AtomicWaker;
use zerocopy::{little_endian, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned};

/// The serializable enumeration of packet types that can be used over a usb vsock link. These
/// roughly correspond to the state machine described by the `fuchsia.hardware.vsock` fidl library.
#[repr(u8)]
#[derive(
    Debug,
    TryFromBytes,
    IntoBytes,
    KnownLayout,
    Immutable,
    Unaligned,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Clone,
    Copy,
)]
pub enum PacketType {
    /// Synchronizes the connection between host and device. Each side must send and receive a
    /// sync packet with the same payload before any other packet may be recognized on the usb
    /// connection. If this packet is received mid-stream, all connections must be considered
    /// reset to avoid data loss. It should also only ever be the last vsock packet in a given
    /// usb packet.
    Sync = b'S',
    /// Connect to a cid:port pair from a cid:port pair on the other side. The payload must be empty.
    Connect = b'C',
    /// Terminate or refuse a connection on a particular cid:port pair set. There must have been a
    /// previous [`PacketType::Connect`] request for this, and after this that particular set of
    /// pairs must be considered disconnected and no more [`PacketType::Data`] packets may be sent
    /// for it unless a new connection is initiated. The payload must be empty.
    Reset = b'R',
    /// Accepts a connection previously requested with [`PacketType::Connect`] on the given cid:port
    /// pair set. The payload may contain an initial data packet for the connection.
    Accept = b'A',
    /// A data packet for a particular cid:port pair set previously established with a [`PacketType::Connect`]
    /// and [`PacketType::Accept`] message. If all of the cid and port fields of the packet are
    /// zero, this is for a special data stream between the host and device that does not require
    /// an established connection.
    Data = b'D',
}

/// The packet header for a vsock packet passed over the usb vsock link. Each usb packet can contain
/// one or more packets, each of which must start with a valid header and correct payload length.
#[repr(C, packed(1))]
#[derive(
    Debug,
    TryFromBytes,
    IntoBytes,
    KnownLayout,
    Immutable,
    Unaligned,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Clone,
)]
pub struct Header {
    magic: [u8; 3],
    /// The type of this packet
    pub packet_type: PacketType,
    /// For Connect, Reset, Accept, and Data packets this represents the device side's address.
    /// Usually this will be a special value representing either that it is simply "the device",
    /// or zero along with the rest of the cid and port fields to indicate that it's a control stream
    /// packet. Must be zero for any other packet type.
    pub device_cid: little_endian::U32,
    /// For Connect, Reset, Accept, and Data packets this represents the host side's address.
    /// Usually this will be a special value representing either that it is simply "the host",
    /// or zero along with the rest of the cid and port fields to indicate that it's a control stream
    /// packet. Must be zero for any other packet type.
    pub host_cid: little_endian::U32,
    /// For Connect, Reset, Accept, and Data packets this represents the device side's port.
    /// This must be a valid positive value for any of those packet types, unless all of the cid and
    /// port fields are also zero, in which case it is a control stream packet. Must be zero for any
    /// other packet type.
    pub device_port: little_endian::U32,
    /// For Connect, Reset, Accept, and Data packets this represents the host side's port.
    /// This must be a valid positive value for any of those packet types, unless all of the cid and
    /// port fields are also zero, in which case it is a control stream packet. Must be zero for any
    /// other packet type.
    pub host_port: little_endian::U32,
    /// The length of the packet payload. This must be zero for any packet type other than Sync,
    /// Accept, or Data.
    pub payload_len: little_endian::U32,
}

impl Header {
    /// Helper constant for the size of a header on the wire
    pub const SIZE: usize = size_of::<Self>();
    const MAGIC: &'static [u8; 3] = b"ffx";

    /// Builds a new packet with correct magic value and packet type and all other fields
    /// initialized to zero.
    pub fn new(packet_type: PacketType) -> Self {
        let device_cid = 0.into();
        let host_cid = 0.into();
        let device_port = 0.into();
        let host_port = 0.into();
        let payload_len = 0.into();
        Header {
            magic: *Self::MAGIC,
            packet_type,
            device_cid,
            host_cid,
            device_port,
            host_port,
            payload_len,
        }
    }

    /// Gets the size of this packet on the wire with the header and a payload of length
    /// [`Self::payload_len`].
    pub fn packet_size(&self) -> usize {
        Packet::size_with_payload(self.payload_len.get() as usize)
    }
}

/// A typed reference to the contents of a packet in a buffer.
#[derive(Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Packet<'a> {
    /// The packet's header
    pub header: &'a Header,
    /// The packet's payload
    pub payload: &'a [u8],
}

impl<'a> Packet<'a> {
    /// The size of this packet according to its header (as [`Self::payload`] may have been
    /// over-allocated for the size of the packet).
    pub fn size(&self) -> usize {
        self.header.packet_size()
    }

    fn size_with_payload(payload_size: usize) -> usize {
        size_of::<Header>() + payload_size
    }

    fn parse_next(buf: &'a [u8]) -> Result<(Self, &'a [u8]), std::io::Error> {
        // split off and validate the header
        let Some((header, body)) = buf.split_at_checked(size_of::<Header>()) else {
            return Err(std::io::Error::other("insufficient data for last packet"));
        };
        let header = Header::try_ref_from_bytes(header).map_err(|err| {
            std::io::Error::other(format!("failed to parse usb vsock header: {err:?}"))
        })?;
        if header.magic != *Header::MAGIC {
            return Err(std::io::Error::other(format!("invalid magic bytes on usb vsock header")));
        }
        // validate the payload length
        let payload_len = Into::<u64>::into(header.payload_len) as usize;
        let body_len = body.len();
        if payload_len > body_len {
            return Err(std::io::Error::other(format!("payload length on usb vsock header ({payload_len}) was larger than available in buffer {body_len}")));
        }

        let (payload, remain) = body.split_at(payload_len);
        Ok((Packet { header, payload }, remain))
    }

    /// Writes the packet to a buffer when the buffer is known to be large enough to hold it. Note
    /// that the packet header's [`Header::payload_len`] must be correct before calling this, it
    /// does not use the size of [`Self::payload`] to decide how much of the payload buffer is
    /// valid.
    ///
    /// # Panics
    ///
    /// Panics if the buffer is not large enough for the packet.
    pub fn write_to_unchecked(&'a self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let (packet, remain) = buf.split_at_mut(self.size());
        self.header.write_to_prefix(packet).unwrap();
        self.payload.write_to_suffix(packet).unwrap();
        remain
    }
}

/// A typed mutable reference to the contents of a packet in a buffer.
#[derive(Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct PacketMut<'a> {
    /// The packet's header.
    pub header: &'a mut Header,
    /// The packet's payload.
    pub payload: &'a mut [u8],
}

impl<'a> PacketMut<'a> {
    /// Creates a new [`PacketMut`] inside the given buffer and initializes the header to the given
    /// [`PacketType`] before returning it. All other fields in the header will be zeroed, and the
    /// [`PacketMut::payload`] will be the remaining area of the buffer after the header.
    ///
    /// Use [`PacketMut::finish`] to validate and write the proper packet length and return the
    /// total size of the packet.
    ///
    /// # Panics
    ///
    /// The buffer must be large enough to hold at least a packet header, and this will panic if
    /// it's not.
    pub fn new_in(packet_type: PacketType, buf: &'a mut [u8]) -> Self {
        Header::new(packet_type)
            .write_to_prefix(buf)
            .expect("not enough room in buffer for packet header");
        let (header_bytes, payload) = buf.split_at_mut(Header::SIZE);
        let header = Header::try_mut_from_bytes(header_bytes).unwrap();
        PacketMut { header, payload }
    }

    /// Validates the correctness of the packet and returns the size of the packet within the
    /// original buffer.
    pub fn finish(self, payload_len: usize) -> Result<usize, PacketTooBigError> {
        if payload_len <= self.payload.len() {
            self.header.payload_len.set(u32::try_from(payload_len).map_err(|_| PacketTooBigError)?);
            Ok(Header::SIZE + payload_len)
        } else {
            Err(PacketTooBigError)
        }
    }
}

/// Reads a sequence of vsock packets from a given usb packet buffer
pub struct PacketStream<'a> {
    buf: Option<&'a [u8]>,
}

impl<'a> PacketStream<'a> {
    /// Creates a new [`PacketStream`] from the contents of `buf`. The returned stream will
    /// iterate over individual vsock packets.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf: Some(buf) }
    }
}

impl<'a> FusedIterator for PacketStream<'a> {}
impl<'a> Iterator for PacketStream<'a> {
    type Item = Result<Packet<'a>, std::io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        // return immediately if we've already returned `None` or `Some(Err)`
        let data = self.buf.take()?;

        // also return immediately if there's no more data in the buffer
        if data.len() == 0 {
            return None;
        }

        match Packet::parse_next(data) {
            Ok((header, rest)) => {
                // update our pointer for next time
                self.buf = Some(rest);
                Some(Ok(header))
            }
            Err(err) => Some(Err(err)),
        }
    }
}

/// Builds an aggregate usb packet out of vsock packets and gives readiness
/// notifications when there is room to add another packet or data available to send.
pub struct PacketBuilder<B> {
    buffer: B,
    offset: usize,
    space_waker: AtomicWaker,
    packet_waker: AtomicWaker,
}

/// the size of the packet would have been too large even if the buffer was empty
#[derive(Debug, Copy, Clone)]
pub struct PacketTooBigError;

impl<B> PacketBuilder<B> {
    /// Creates a new builder from `buffer`, which is a type that can be used as a mutable slice
    /// with space available for storing vsock packets. The `readable_notify` will have a message
    /// sent to it whenever a usb packet could be transmitted.
    pub fn new(buffer: B) -> Self {
        let offset = 0;
        let space_waker = AtomicWaker::default();
        let packet_waker = AtomicWaker::default();
        Self { buffer, offset, space_waker, packet_waker }
    }
}

impl<B> PacketBuilder<B>
where
    B: std::ops::DerefMut<Target = [u8]>,
{
    /// Gets the space currently available for another packet in the buffer
    pub fn available(&self) -> usize {
        self.buffer.len() - self.offset
    }

    /// Waits for there to be room to write a vsock packet of at least `sizeof(Header)+bytes`
    /// in the buffer. Use [`Self::write_vsock_packet`] to write the packet when
    /// space becomes available.
    ///
    /// # Panics
    ///
    /// Panics if the buffer this was built with isn't big enough to hold the packet
    /// even if it were empty.
    pub fn wait_for_space(&self, bytes: usize) -> SpaceAvailableFuture<'_, B> {
        let needed_space = Packet::size_with_payload(bytes);
        if needed_space <= self.buffer.len() {
            SpaceAvailableFuture { needed_space, builder: self }
        } else {
            panic!("Buffer too small for packet even when empty");
        }
    }

    /// Waits for a usb packet to be available in the buffer. Use [`Self::take_usb_packet`]
    /// to get the packet when this returns.
    pub fn wait_for_usb_packet(&self) -> UsbPacketFuture<'_, B> {
        UsbPacketFuture { builder: self }
    }

    /// Writes the given packet into the buffer. The packet and header must be able to fit
    /// within the buffer provided at creation time.
    pub fn write_vsock_packet(&mut self, packet: &Packet<'_>) -> Result<(), PacketTooBigError> {
        let packet_size = packet.size();
        if self.available() >= packet_size {
            packet.write_to_unchecked(&mut self.buffer[self.offset..]);
            self.offset += packet_size;
            self.packet_waker.wake();
            Ok(())
        } else {
            Err(PacketTooBigError)
        }
    }

    /// Takes the current usb packet, if there is one. The returned mutable slice
    /// will be only the data written to the buffer so far, and packet writing will be reset to the
    /// beginning of the buffer.
    pub fn take_usb_packet(&mut self) -> Option<&mut [u8]> {
        let written = self.offset;
        if written == 0 {
            return None;
        }
        self.offset = 0;
        self.space_waker.wake();
        Some(&mut self.buffer[0..written])
    }
}

/// The future implementation for [`PacketBuilder::wait_for_usb_packet`]
pub struct UsbPacketFuture<'a, B> {
    builder: &'a PacketBuilder<B>,
}
impl<'a, B> Unpin for UsbPacketFuture<'a, B> {}

impl<'a, B> Future for UsbPacketFuture<'a, B> {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.builder.offset == 0 {
            self.builder.packet_waker.register(cx.waker());
            Poll::Pending
        } else {
            self.builder.packet_waker.take();
            Poll::Ready(())
        }
    }
}

impl<'a, B> FusedFuture for UsbPacketFuture<'a, B> {
    /// This future is always pollable as long as the underlying builder is still valid.
    fn is_terminated(&self) -> bool {
        false
    }
}

/// The future implementation for [`PacketBuilder::wait_for_space`]
pub struct SpaceAvailableFuture<'a, B> {
    needed_space: usize,
    builder: &'a PacketBuilder<B>,
}
impl<'a, B> Unpin for SpaceAvailableFuture<'a, B> {}

impl<'a, B> Future for SpaceAvailableFuture<'a, B>
where
    B: std::ops::DerefMut<Target = [u8]>,
{
    type Output = Result<(), PacketTooBigError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.builder.available() < self.needed_space {
            self.builder.space_waker.register(cx.waker());
            Poll::Pending
        } else {
            self.builder.space_waker.take();
            Poll::Ready(Ok(()))
        }
    }
}

impl<'a, B> FusedFuture for SpaceAvailableFuture<'a, B>
where
    B: std::ops::DerefMut<Target = [u8]>,
{
    /// This future is always pollable as long as the underlying builder is still valid.
    fn is_terminated(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{poll, select};

    async fn assert_pending<F: Future>(fut: F) {
        let fut = std::pin::pin!(fut);
        if let Poll::Ready(_) = poll!(fut) {
            panic!("Future was ready when it shouldn't have been");
        }
    }

    #[fuchsia::test]
    async fn roundtrip_packet() {
        let payload = b"hello world!";
        let packet = Packet {
            payload,
            header: &Header {
                device_cid: 1.into(),
                host_cid: 2.into(),
                device_port: 3.into(),
                host_port: 4.into(),
                payload_len: little_endian::U32::from(payload.len() as u32),
                ..Header::new(PacketType::Data)
            },
        };
        let buffer = vec![0; packet.size()];
        let mut builder = PacketBuilder::new(buffer);
        // we should not be ready to pull a usb packet off yet
        assert_pending(builder.wait_for_usb_packet()).await;

        builder.wait_for_space(payload.len()).await.unwrap();
        builder.write_vsock_packet(&packet).unwrap();

        // we shouldn't have any space for another packet now
        assert_pending(builder.wait_for_space(1)).await;

        // but we should have a new usb packet available
        builder.wait_for_usb_packet().await;
        let buffer = builder.take_usb_packet().unwrap();

        // the packet we get back out should be the same one we put in
        let (read_packet, remain) = Packet::parse_next(buffer).unwrap();
        assert_eq!(packet, read_packet);
        assert!(remain.is_empty());

        // and now there shouldn't be a waiting usb packet but there should be room for a new one
        assert_pending(builder.wait_for_usb_packet()).await;
        builder.wait_for_space(1).await.unwrap();
    }

    #[fuchsia::test]
    async fn many_packets() {
        fn make_numbered_packet(num: u32) -> (Header, String) {
            let payload = format!("packet #{num}!");
            let header = Header {
                device_cid: num.into(),
                device_port: num.into(),
                host_cid: num.into(),
                host_port: num.into(),
                payload_len: little_endian::U32::from(payload.len() as u32),
                ..Header::new(PacketType::Data)
            };
            (header, payload)
        }
        const BUFFER_SIZE: usize = 256;
        let mut builder = PacketBuilder::new(vec![0; BUFFER_SIZE]);
        let mut packet_num = 0;
        let mut read_packet_num = 0;
        while read_packet_num < 1024 {
            let next_packet = make_numbered_packet(packet_num);
            let next_packet = Packet { header: &next_packet.0, payload: next_packet.1.as_ref() };
            select! {
                res = builder.wait_for_space(next_packet.size()) => {
                    res.expect("all packets should fit in buffer");
                    builder.write_vsock_packet(&next_packet).unwrap();
                    packet_num += 1;
                },
                _ = builder.wait_for_usb_packet() => {
                    let buffer = builder.take_usb_packet().unwrap();
                    let mut num_packets = 0;
                    for packet in PacketStream::new(&buffer) {
                        let packet_compare = make_numbered_packet(read_packet_num);
                        let packet_compare = Packet { header: &packet_compare.0, payload: &packet_compare.1.as_ref() };
                        assert_eq!(packet.unwrap(), packet_compare);
                        read_packet_num += 1;
                        num_packets += 1;
                    }
                    println!("Read {num_packets} vsock packets from usb packet buffer, had {count} bytes left", count = BUFFER_SIZE - buffer.len());
                }
            }
        }
        assert_eq!(packet_num, read_packet_num);
    }
}
