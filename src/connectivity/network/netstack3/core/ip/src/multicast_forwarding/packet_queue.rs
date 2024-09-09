// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to queued multicast packets.

use alloc::collections::{btree_map, BTreeMap};
use alloc::vec::Vec;
use arrayvec::ArrayVec;
use derivative::Derivative;
use net_types::ip::{Ip, IpVersionMarker};
use netstack3_base::{StrongDeviceIdentifier as _, WeakDeviceIdentifier};
use packet_formats::ip::IpPacket;
use todo_unused::todo_unused;
use zerocopy::ByteSlice;

use crate::multicast_forwarding::MulticastRouteKey;
use crate::IpLayerIpExt;

/// The number of packets that the stack is willing to queue for a given
/// [`MulticastRouteKey`] while waiting for an applicable route to be installed.
///
/// This value is consistent with the defaults on both Netstack2 and Linux.
pub(crate) const PACKET_QUEUE_LEN: usize = 3;

/// A table of pending multicast packets that have not yet been forwarded.
///
/// Packets are placed in this table when, during forwarding, there is no route
/// in the [`MulticastRouteTable`] via which to forward them. If/when such a
/// route is installed, the packets stored here can be forwarded accordingly.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct MulticastForwardingPendingPackets<I: IpLayerIpExt, D: WeakDeviceIdentifier> {
    table: BTreeMap<MulticastRouteKey<I>, PacketQueue<I, D>>,
}

impl<I: IpLayerIpExt, D: WeakDeviceIdentifier> MulticastForwardingPendingPackets<I, D> {
    /// Attempt to queue the packet in the pending_table.
    #[todo_unused("https://fxbug.dev/353328975")]
    pub(crate) fn try_queue_packet<B: ByteSlice>(
        &mut self,
        key: MulticastRouteKey<I>,
        packet: &I::Packet<B>,
        dev: &D::Strong,
    ) -> QueuePacketOutcome {
        match self.table.entry(key) {
            btree_map::Entry::Vacant(entry) => {
                let queue = entry.insert(PacketQueue::default());
                queue
                    .try_push(|| QueuedPacket::new(dev, packet))
                    .expect("newly instantiated queue must have capacity");
                QueuePacketOutcome::QueuedInNewQueue
            }
            btree_map::Entry::Occupied(mut entry) => {
                match entry.get_mut().try_push(|| QueuedPacket::new(dev, packet)) {
                    Ok(()) => QueuePacketOutcome::QueuedInExistingQueue,
                    Err(PacketQueueFullError) => QueuePacketOutcome::ExistingQueueFull,
                }
            }
        }
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) fn contains(&self, key: &MulticastRouteKey<I>) -> bool {
        self.table.contains_key(key)
    }

    pub(crate) fn remove(&mut self, key: &MulticastRouteKey<I>) -> Option<PacketQueue<I, D>> {
        self.table.remove(key)
    }
}

/// Possible outcomes from calling [`MulticastForwardingPendingPackets::try_queue_packet`].
#[derive(Debug, PartialEq)]
pub(crate) enum QueuePacketOutcome {
    /// The packet was successfully queued. There was no existing
    /// [`PacketQueue`] for the given route key, so a new one was instantiated.
    QueuedInNewQueue,
    /// The packet was successfully queued. It was added onto an existing
    /// [`PacketQueue`] for the given route key.
    QueuedInExistingQueue,
    /// The packet was not queued. There was an existing [`PacketQueue`] for the
    /// given route key, but that queue was full.
    ExistingQueueFull,
}

/// A queue of multicast packets that are pending the installation of a route.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct PacketQueue<I: Ip, D: WeakDeviceIdentifier> {
    queue: ArrayVec<QueuedPacket<I, D>, PACKET_QUEUE_LEN>,
}

impl<I: Ip, D: WeakDeviceIdentifier> PacketQueue<I, D> {
    /// Try to push a packet into the queue, returning an error when full.
    ///
    /// Note: the packet is taken as a builder closure, because constructing the
    /// packet is an expensive operation (requiring a `Vec` allocation). By
    /// taking a closure we can defer construction until we're certain the queue
    /// has the free space to hold it.
    fn try_push(
        &mut self,
        packet_builder: impl FnOnce() -> QueuedPacket<I, D>,
    ) -> Result<(), PacketQueueFullError> {
        if self.queue.is_full() {
            return Err(PacketQueueFullError);
        }
        self.queue.push(packet_builder());
        Ok(())
    }
}

#[derive(Debug)]
struct PacketQueueFullError;

/// An individual multicast packet that's queued.
///
/// This type acts as a witness that the bytes held in `packet` constitute a
/// valid [`I::Packet`]. Any attempt to parse the bytes back into an instance of
/// [`I::Packet`] is infallible.
#[derive(Debug, PartialEq)]
pub struct QueuedPacket<I: Ip, D: WeakDeviceIdentifier> {
    /// The device on which the packet arrived.
    // TODO(https://fxbug.dev/353328975): Use this field to send queued packets.
    #[allow(unused)]
    device: D,
    /// The packet's raw bytes.
    // TODO(https://fxbug.dev/353328975): Use this field to send queued packets.
    #[allow(unused)]
    packet_bytes: Vec<u8>,
    /// The IP Version of `packet`.
    _version_marker: IpVersionMarker<I>,
}

impl<I: IpLayerIpExt, D: WeakDeviceIdentifier> QueuedPacket<I, D> {
    fn new<B: ByteSlice>(device: &D::Strong, packet: &I::Packet<B>) -> Self {
        QueuedPacket {
            device: device.downgrade(),
            packet_bytes: packet.to_vec(),
            _version_marker: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use netstack3_base::testutil::MultipleDevicesId;
    use netstack3_base::StrongDeviceIdentifier;
    use packet::ParseBuffer;

    use crate::internal::multicast_forwarding;
    use crate::internal::multicast_forwarding::testutil::TestIpExt;

    #[ip_test(I)]
    fn queue_packet<I: TestIpExt>() {
        const DEV: MultipleDevicesId = MultipleDevicesId::A;
        let key1 = MulticastRouteKey::new(I::SRC1, I::DST1).unwrap();
        let key2 = MulticastRouteKey::new(I::SRC2, I::DST2).unwrap();
        let key3 = MulticastRouteKey::new(I::SRC1, I::DST2).unwrap();

        // NB: technically the packet's addresses only match `key1`, but for the
        // sake of this test that doesn't cause problems.
        let buf = multicast_forwarding::testutil::new_ip_packet_buf::<I>(I::SRC1, I::DST1);
        let mut buf_ref = buf.as_ref();
        let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");

        let mut pending_table = MulticastForwardingPendingPackets::<
            I,
            <MultipleDevicesId as StrongDeviceIdentifier>::Weak,
        >::default();

        // The first packet gets a new queue.
        assert_eq!(
            pending_table.try_queue_packet(key1.clone(), &packet, &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );
        // The second - Nth packets uses the existing queue.
        for _ in 1..PACKET_QUEUE_LEN {
            assert_eq!(
                pending_table.try_queue_packet(key1.clone(), &packet, &DEV),
                QueuePacketOutcome::QueuedInExistingQueue
            );
        }
        // The Nth +1 packet is rejected.
        assert_eq!(
            pending_table.try_queue_packet(key1.clone(), &packet, &DEV),
            QueuePacketOutcome::ExistingQueueFull
        );

        // A packet with a different key gets a new queue.
        assert_eq!(
            pending_table.try_queue_packet(key2.clone(), &packet, &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );

        // Based on the calls above, `key1` should have a full queue, `key2`
        // should have a queue with only 1 packet, and `key3` shouldn't have
        // a queue.
        let expected_packet = QueuedPacket::new(&DEV, &packet);
        let queue = pending_table.remove(&key1).expect("key1 should have a queue");
        assert_eq!(queue.queue.len(), PACKET_QUEUE_LEN);
        for packet in queue.queue.as_slice() {
            assert_eq!(packet, &expected_packet);
        }

        let queue = pending_table.remove(&key2).expect("key2 should have a queue");
        let packet = assert_matches!(&queue.queue[..], [p] => p);
        assert_eq!(packet, &expected_packet);

        assert_matches!(pending_table.remove(&key3), None);
    }
}
