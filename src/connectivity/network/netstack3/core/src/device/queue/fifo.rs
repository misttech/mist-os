// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FiFo device queue.

use alloc::collections::VecDeque;

use derivative::Derivative;
use packet::{ParseBuffer, Serializer, TargetBuffer};

use crate::device::queue::{
    DequeueResult, EnqueueResult, ReceiveQueueFullError, TransmitQueueFrameError,
    MAX_RX_QUEUED_PACKETS, MAX_TX_QUEUED_FRAMES,
};

/// A FiFo (First In, First Out) queue of packets.
///
/// If the queue is full, no new packets will be accepted.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct Queue<Meta, Buffer> {
    packets: VecDeque<(Meta, Buffer)>,
}

impl<Meta, Buffer> Queue<Meta, Buffer> {
    pub(crate) fn requeue_packets(&mut self, source: &mut VecDeque<(Meta, Buffer)>) {
        while let Some(p) = source.pop_back() {
            self.packets.push_front(p);
        }
    }

    /// Dequeues packets from this queue and pushes them to the back of the
    /// sink.
    ///
    /// Note that this method takes an explicit `max_batch_size` argument
    /// because the `VecDeque`'s capacity (via `VecDequeue::capacity`) may be
    /// larger than some specified maximum batch size. Note that
    /// [`VecDeque::with_capcity`] may allocate more capacity than specified.
    pub(super) fn dequeue_packets_into(
        &mut self,
        sink: &mut VecDeque<(Meta, Buffer)>,
        max_batch_size: usize,
    ) -> DequeueResult {
        for _ in 0..max_batch_size {
            match self.packets.pop_front() {
                Some(p) => sink.push_back(p),
                // No more packets.
                None => break,
            }
        }

        if self.packets.is_empty() {
            DequeueResult::NoMorePacketsLeft
        } else {
            DequeueResult::MorePacketsStillQueued
        }
    }
}

impl<Meta, Buffer: ParseBuffer> Queue<Meta, Buffer> {
    /// Attempts to add the RX packet to the queue.
    pub(super) fn queue_rx_packet(
        &mut self,
        meta: Meta,
        body: Buffer,
    ) -> Result<EnqueueResult, ReceiveQueueFullError<(Meta, Buffer)>> {
        let Self { packets } = self;

        let len = packets.len();
        if len == MAX_RX_QUEUED_PACKETS {
            return Err(ReceiveQueueFullError((meta, body)));
        }

        packets.push_back((meta, body));

        Ok(if len == 0 {
            EnqueueResult::QueueWasPreviouslyEmpty
        } else {
            EnqueueResult::QueuePreviouslyHadPackets
        })
    }
}

impl<Meta, Buffer: TargetBuffer> Queue<Meta, Buffer> {
    /// Attempts to add the tx frame to the queue.
    pub(crate) fn queue_tx_frame<
        S: Serializer,
        F: FnOnce(S) -> Result<Buffer, TransmitQueueFrameError<S>>,
    >(
        &mut self,
        meta: Meta,
        body: S,
        get_buffer: F,
    ) -> Result<EnqueueResult, TransmitQueueFrameError<S>> {
        let Self { packets } = self;

        let len = packets.len();
        if len == MAX_TX_QUEUED_FRAMES {
            return Err(TransmitQueueFrameError::QueueFull(body));
        }

        packets.push_back((meta, get_buffer(body)?));

        Ok(if len == 0 {
            EnqueueResult::QueueWasPreviouslyEmpty
        } else {
            EnqueueResult::QueuePreviouslyHadPackets
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use packet::Buf;

    #[test]
    fn max_mackets() {
        let mut fifo = Queue::default();

        let mut res = Ok(EnqueueResult::QueueWasPreviouslyEmpty);
        for i in 0..MAX_RX_QUEUED_PACKETS {
            let body = Buf::new([i as u8], ..);
            assert_eq!(fifo.queue_rx_packet((), body), res);

            // The result we expect after the first packet is enqueued.
            res = Ok(EnqueueResult::QueuePreviouslyHadPackets);
        }

        let packets = (0..MAX_RX_QUEUED_PACKETS)
            .into_iter()
            .map(|i| ((), Buf::new([i as u8], ..)))
            .collect::<VecDeque<_>>();
        assert_eq!(fifo.packets, packets);

        let body = Buf::new([131], ..);
        assert_eq!(fifo.queue_rx_packet((), body.clone()), Err(ReceiveQueueFullError(((), body))));
        assert_eq!(fifo.packets, packets);
    }
}
