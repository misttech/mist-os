// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to queued multicast packets.

use alloc::collections::{btree_map, BTreeMap};
use alloc::vec::Vec;
use arrayvec::ArrayVec;
use core::time::Duration;
use derivative::Derivative;
use net_types::ip::{Ip, IpVersionMarker};
use netstack3_base::{
    CoreTimerContext, Instant as _, StrongDeviceIdentifier as _, WeakDeviceIdentifier,
};
use packet_formats::ip::IpPacket;
use todo_unused::todo_unused;
use zerocopy::ByteSlice;

use crate::internal::multicast_forwarding::{
    MulticastForwardingBindingsContext, MulticastForwardingBindingsTypes,
    MulticastForwardingTimerId,
};
use crate::multicast_forwarding::MulticastRouteKey;
use crate::IpLayerIpExt;

/// The number of packets that the stack is willing to queue for a given
/// [`MulticastRouteKey`] while waiting for an applicable route to be installed.
///
/// This value is consistent with the defaults on both Netstack2 and Linux.
pub(crate) const PACKET_QUEUE_LEN: usize = 3;

/// The amount of time the stack is willing to queue a packet while waiting
/// for an applicable route to be installed.
///
/// This value is consistent with the defaults on both Netstack2 and Linux.
const PENDING_ROUTE_EXPIRATION: Duration = Duration::from_secs(10);

/// The minimum amount of time after a garbage-collection run across the
/// [`MulticastForwardingPendingPackets`] table that the stack will wait before
/// performing another garbage-collection.
///
/// This value is consistent with the defaults on both Netstack2 and Linux.
const PENDING_ROUTE_GC_PERIOD: Duration = Duration::from_secs(10);

/// A table of pending multicast packets that have not yet been forwarded.
///
/// Packets are placed in this table when, during forwarding, there is no route
/// in the [`MulticastRouteTable`] via which to forward them. If/when such a
/// route is installed, the packets stored here can be forwarded accordingly.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct MulticastForwardingPendingPackets<
    I: IpLayerIpExt,
    D: WeakDeviceIdentifier,
    BT: MulticastForwardingBindingsTypes,
> {
    table: BTreeMap<MulticastRouteKey<I>, PacketQueue<I, D, BT>>,
    /// Periodically triggers invocations of [`Self::run_garbage_collection`].
    ///
    /// All interactions with the `gc_timer` must uphold the invariant that the
    /// timer is not scheduled if [`Self::table`] is empty.
    ///
    /// Note: When [`Self`] is held by [`MulticastForwardingEnabledState`], it
    /// is lock protected, which prevents method calls on it from racing. E.g.
    /// no overlapping calls to [`Self::try_queue_packet`], [`Self::remove`],
    /// or [`Self::run_garbage_collection`].
    gc_timer: BT::Timer,
}

impl<I: IpLayerIpExt, D: WeakDeviceIdentifier, BC: MulticastForwardingBindingsContext>
    MulticastForwardingPendingPackets<I, D, BC>
{
    pub(crate) fn new<CC>(bindings_ctx: &mut BC) -> Self
    where
        CC: CoreTimerContext<MulticastForwardingTimerId<I>, BC>,
    {
        Self {
            table: Default::default(),
            gc_timer: CC::new_timer(
                bindings_ctx,
                MulticastForwardingTimerId::PendingPacketsGc(IpVersionMarker::<I>::new()),
            ),
        }
    }

    /// Attempt to queue the packet in the pending_table.
    ///
    /// If the table becomes newly occupied, the GC timer is scheduled.
    #[todo_unused("https://fxbug.dev/353328975")]
    pub(crate) fn try_queue_packet<B>(
        &mut self,
        bindings_ctx: &mut BC,
        key: MulticastRouteKey<I>,
        packet: &I::Packet<B>,
        dev: &D::Strong,
    ) -> QueuePacketOutcome
    where
        B: ByteSlice,
    {
        let was_empty = self.table.is_empty();
        let outcome = match self.table.entry(key) {
            btree_map::Entry::Vacant(entry) => {
                let queue = entry.insert(PacketQueue::new(bindings_ctx));
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
        };

        // If the table is newly non-empty, schedule the GC. The timer must not
        // already be scheduled (given the invariants on `gc_timer`).
        if was_empty && !self.table.is_empty() {
            assert!(bindings_ctx
                .schedule_timer(PENDING_ROUTE_GC_PERIOD, &mut self.gc_timer)
                .is_none());
        }

        outcome
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) fn contains(&self, key: &MulticastRouteKey<I>) -> bool {
        self.table.contains_key(key)
    }

    /// Remove the key from the pending table, returning its queue of packets.
    ///
    /// If the table becomes newly empty, the GC timer is canceled.
    pub(crate) fn remove(
        &mut self,
        key: &MulticastRouteKey<I>,
        bindings_ctx: &mut BC,
    ) -> Option<PacketQueue<I, D, BC>> {
        let was_empty = self.table.is_empty();
        let queue = self.table.remove(key);

        // If the table is newly empty, cancel the GC. Note, we don't assert on
        // the previous state of the timer, because it's possible cancelation
        // will race with the timer firing.
        if !was_empty && self.table.is_empty() {
            let _: Option<BC::Instant> = bindings_ctx.cancel_timer(&mut self.gc_timer);
        }

        queue
    }

    /// Removes expired [`PacketQueue`] entries from [`Self`].
    pub(crate) fn run_garbage_collection(&mut self, bindings_ctx: &mut BC) {
        let now = bindings_ctx.now();
        self.table.retain(|_key, packet_queue| packet_queue.expires_at > now);

        // If the table is still not empty, reschedule the GC. Note that we
        // don't assert on the previous state of the timer, because it's
        // possible that starting GC raced with a new timer being scheduled.
        if !self.table.is_empty() {
            let _: Option<BC::Instant> =
                bindings_ctx.schedule_timer(PENDING_ROUTE_GC_PERIOD, &mut self.gc_timer);
        }
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
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct PacketQueue<I: Ip, D: WeakDeviceIdentifier, BT: MulticastForwardingBindingsTypes> {
    queue: ArrayVec<QueuedPacket<I, D>, PACKET_QUEUE_LEN>,
    /// The time after which the PacketQueue is allowed to be garbage collected.
    expires_at: BT::Instant,
}

impl<I: Ip, D: WeakDeviceIdentifier, BC: MulticastForwardingBindingsContext> PacketQueue<I, D, BC> {
    fn new(bindings_ctx: &mut BC) -> Self {
        Self {
            queue: Default::default(),
            expires_at: bindings_ctx.now().add(PENDING_ROUTE_EXPIRATION),
        }
    }

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
    use netstack3_base::testutil::{
        FakeInstant, FakeTimerCtxExt, FakeWeakDeviceId, MultipleDevicesId,
    };
    use netstack3_base::{InstantContext, StrongDeviceIdentifier, TimerContext};
    use packet::ParseBuffer;
    use static_assertions::const_assert;

    use crate::internal::multicast_forwarding;
    use crate::internal::multicast_forwarding::testutil::{
        FakeBindingsCtx, FakeCoreCtx, TestIpExt,
    };

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

        let mut bindings_ctx = FakeBindingsCtx::<I>::default();

        let mut pending_table =
            MulticastForwardingPendingPackets::<
                I,
                <MultipleDevicesId as StrongDeviceIdentifier>::Weak,
                _,
            >::new::<FakeCoreCtx<I, MultipleDevicesId>>(&mut bindings_ctx);

        // The first packet gets a new queue.
        assert_eq!(
            pending_table.try_queue_packet(&mut bindings_ctx, key1.clone(), &packet, &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );
        // The second - Nth packets uses the existing queue.
        for _ in 1..PACKET_QUEUE_LEN {
            assert_eq!(
                pending_table.try_queue_packet(&mut bindings_ctx, key1.clone(), &packet, &DEV),
                QueuePacketOutcome::QueuedInExistingQueue
            );
        }
        // The Nth +1 packet is rejected.
        assert_eq!(
            pending_table.try_queue_packet(&mut bindings_ctx, key1.clone(), &packet, &DEV),
            QueuePacketOutcome::ExistingQueueFull
        );

        // A packet with a different key gets a new queue.
        assert_eq!(
            pending_table.try_queue_packet(&mut bindings_ctx, key2.clone(), &packet, &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );

        // Based on the calls above, `key1` should have a full queue, `key2`
        // should have a queue with only 1 packet, and `key3` shouldn't have
        // a queue.
        let expected_packet = QueuedPacket::new(&DEV, &packet);
        let queue =
            pending_table.remove(&key1, &mut bindings_ctx).expect("key1 should have a queue");
        assert_eq!(queue.queue.len(), PACKET_QUEUE_LEN);
        for packet in queue.queue.as_slice() {
            assert_eq!(packet, &expected_packet);
        }

        let queue =
            pending_table.remove(&key2, &mut bindings_ctx).expect("key2 should have a queue");
        let packet = assert_matches!(&queue.queue[..], [p] => p);
        assert_eq!(packet, &expected_packet);

        assert_matches!(pending_table.remove(&key3, &mut bindings_ctx), None);
    }

    /// Helper to observe the next scheduled GC for the core_ctx pending table.
    fn next_gc_time<I: TestIpExt>(
        core_ctx: &mut FakeCoreCtx<I, MultipleDevicesId>,
        bindings_ctx: &mut FakeBindingsCtx<I>,
    ) -> Option<FakeInstant> {
        multicast_forwarding::testutil::with_pending_table(core_ctx, |pending_table| {
            bindings_ctx.scheduled_instant(&mut pending_table.gc_timer)
        })
    }

    /// Helper to queue packet in the core_ctx pending table.
    fn try_queue_packet<I: TestIpExt>(
        core_ctx: &mut FakeCoreCtx<I, MultipleDevicesId>,
        bindings_ctx: &mut FakeBindingsCtx<I>,
        key: MulticastRouteKey<I>,
        dev: &MultipleDevicesId,
    ) -> QueuePacketOutcome {
        let buf =
            multicast_forwarding::testutil::new_ip_packet_buf::<I>(key.src_addr(), key.dst_addr());
        let mut buf_ref = buf.as_ref();
        let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");
        multicast_forwarding::testutil::with_pending_table(core_ctx, |pending_table| {
            pending_table.try_queue_packet(bindings_ctx, key, &packet, dev)
        })
    }

    /// Helper to remove a packet queue in the core_ctx pending table.
    fn remove_packet_queue<I: TestIpExt>(
        core_ctx: &mut FakeCoreCtx<I, MultipleDevicesId>,
        bindings_ctx: &mut FakeBindingsCtx<I>,
        key: &MulticastRouteKey<I>,
    ) -> Option<PacketQueue<I, FakeWeakDeviceId<MultipleDevicesId>, FakeBindingsCtx<I>>> {
        multicast_forwarding::testutil::with_pending_table(core_ctx, |pending_table| {
            pending_table.remove(key, bindings_ctx)
        })
    }

    /// Helper to trigger the GC.
    fn run_gc<I: TestIpExt>(
        core_ctx: &mut FakeCoreCtx<I, MultipleDevicesId>,
        bindings_ctx: &mut FakeBindingsCtx<I>,
    ) {
        assert_matches!(
            &bindings_ctx.trigger_timers_until_instant(bindings_ctx.now(), core_ctx)[..],
            [MulticastForwardingTimerId::PendingPacketsGc(_)]
        );
    }

    #[ip_test(I)]
    fn garbage_collection<I: TestIpExt>() {
        const DEV: MultipleDevicesId = MultipleDevicesId::A;
        let key1 = MulticastRouteKey::<I>::new(I::SRC1, I::DST1).unwrap();
        let key2 = MulticastRouteKey::<I>::new(I::SRC2, I::DST2).unwrap();

        let mut api = multicast_forwarding::testutil::new_api();
        assert!(api.enable());
        let (core_ctx, bindings_ctx) = api.contexts();

        // NB: As written, the test requires that
        //  1. `PENDING_ROUTE_GC_PERIOD` >= `PENDING_ROUTE_EXPIRATION`, and
        //  2. `PENDING_ROUTE_EXPIRATION > 0`.
        // If the values are ever changed such that that is not true, the test
        // will need to be re-written.
        const_assert!(PENDING_ROUTE_GC_PERIOD.checked_sub(PENDING_ROUTE_EXPIRATION).is_some());
        const_assert!(!PENDING_ROUTE_EXPIRATION.is_zero());

        // The GC shouldn't be scheduled with an empty table.
        assert!(next_gc_time(core_ctx, bindings_ctx).is_none());

        // Queue a packet, and expect the GC to be scheduled.
        let expected_first_gc = bindings_ctx.now() + PENDING_ROUTE_GC_PERIOD;
        assert_eq!(
            try_queue_packet(core_ctx, bindings_ctx, key1.clone(), &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );
        assert_eq!(next_gc_time(core_ctx, bindings_ctx), Some(expected_first_gc));

        // Sleep until we're ready to GC, and then queue a second packet under a
        // new key. Expect that the GC timer is still scheduled for the original
        // instant.
        bindings_ctx.timers.instant.sleep(PENDING_ROUTE_GC_PERIOD);
        assert_eq!(
            try_queue_packet(core_ctx, bindings_ctx, key2.clone(), &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );
        assert_eq!(next_gc_time(core_ctx, bindings_ctx), Some(expected_first_gc));

        // Run the GC, and verify that it was rescheduled after the fact
        // (because `key2` still exists in the table).
        run_gc(core_ctx, bindings_ctx);
        let expected_second_gc = bindings_ctx.timers.instant.now() + PENDING_ROUTE_GC_PERIOD;
        assert_eq!(next_gc_time(core_ctx, bindings_ctx), Some(expected_second_gc));

        // Verify that `key1` was removed, but `key2` remains.
        assert_matches!(remove_packet_queue(core_ctx, bindings_ctx, &key1), None);
        assert_matches!(remove_packet_queue(core_ctx, bindings_ctx, &key2), Some(_));

        // Now that we've explicitly removed `key2`, the table is empty and the
        // GC should have been canceled.
        assert!(next_gc_time(core_ctx, bindings_ctx).is_none());

        // Finally, verify that if the GC clears the table, it doesn't
        // reschedule itself.
        assert_eq!(
            try_queue_packet(core_ctx, bindings_ctx, key1.clone(), &DEV),
            QueuePacketOutcome::QueuedInNewQueue
        );
        assert_eq!(next_gc_time(core_ctx, bindings_ctx), Some(expected_second_gc));
        bindings_ctx.timers.instant.sleep(PENDING_ROUTE_GC_PERIOD);
        run_gc(core_ctx, bindings_ctx);
        assert_matches!(remove_packet_queue(core_ctx, bindings_ctx, &key1), None);
        assert!(next_gc_time(core_ctx, bindings_ctx).is_none());
    }
}
