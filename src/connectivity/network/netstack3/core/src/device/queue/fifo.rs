// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FiFo device queue.

use alloc::collections::VecDeque;

use derivative::Derivative;
use packet::{GrowBufferMut, ParseBuffer, Serializer};

use crate::device::queue::{
    DequeueResult, EnqueueResult, ReceiveQueueFullError, TransmitQueueFrameError,
    MAX_RX_QUEUED_LEN, MAX_TX_QUEUED_LEN,
};

/// A FiFo (First In, First Out) queue.
///
/// If the queue is full, no new entries will be accepted.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct Queue<Meta, Buffer> {
    items: VecDeque<(Meta, Buffer)>,
}

impl<Meta, Buffer> Queue<Meta, Buffer> {
    pub(crate) fn requeue_items(&mut self, source: &mut VecDeque<(Meta, Buffer)>) {
        while let Some(p) = source.pop_back() {
            self.items.push_front(p);
        }
    }

    /// Dequeues items from this queue and pushes them to the back of the
    /// sink.
    ///
    /// Note that this method takes an explicit `max_batch_size` argument
    /// because the `VecDeque`'s capacity (via `VecDequeue::capacity`) may be
    /// larger than some specified maximum batch size. Note that
    /// [`VecDeque::with_capcity`] may allocate more capacity than specified.
    pub(super) fn dequeue_into(
        &mut self,
        sink: &mut VecDeque<(Meta, Buffer)>,
        max_batch_size: usize,
    ) -> DequeueResult {
        for _ in 0..max_batch_size {
            match self.items.pop_front() {
                Some(p) => sink.push_back(p),
                // No more items.
                None => break,
            }
        }

        if self.items.is_empty() {
            DequeueResult::NoMoreLeft
        } else {
            DequeueResult::MoreStillQueued
        }
    }
}

impl<Meta, Buffer: ParseBuffer> Queue<Meta, Buffer> {
    /// Attempts to add the RX frame to the queue.
    pub(super) fn queue_rx_frame(
        &mut self,
        meta: Meta,
        body: Buffer,
    ) -> Result<EnqueueResult, ReceiveQueueFullError<(Meta, Buffer)>> {
        let Self { items } = self;

        let len = items.len();
        if len == MAX_RX_QUEUED_LEN {
            return Err(ReceiveQueueFullError((meta, body)));
        }

        items.push_back((meta, body));

        Ok(if len == 0 {
            EnqueueResult::QueueWasPreviouslyEmpty
        } else {
            EnqueueResult::QueuePreviouslyWasOccupied
        })
    }
}

impl<Meta, Buffer: GrowBufferMut> Queue<Meta, Buffer> {
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
        let Self { items } = self;

        let len = items.len();
        if len == MAX_TX_QUEUED_LEN {
            return Err(TransmitQueueFrameError::QueueFull(body));
        }

        items.push_back((meta, get_buffer(body)?));

        Ok(if len == 0 {
            EnqueueResult::QueueWasPreviouslyEmpty
        } else {
            EnqueueResult::QueuePreviouslyWasOccupied
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use packet::Buf;

    #[test]
    fn max_elements() {
        let mut fifo = Queue::default();

        let mut res = Ok(EnqueueResult::QueueWasPreviouslyEmpty);
        for i in 0..MAX_RX_QUEUED_LEN {
            let body = Buf::new([i as u8], ..);
            assert_eq!(fifo.queue_rx_frame((), body), res);

            // The result we expect after the first frame is enqueued.
            res = Ok(EnqueueResult::QueuePreviouslyWasOccupied);
        }

        let frames =
            (0..MAX_RX_QUEUED_LEN).map(|i| ((), Buf::new([i as u8], ..))).collect::<VecDeque<_>>();
        assert_eq!(fifo.items, frames);

        let body = Buf::new([131], ..);
        assert_eq!(fifo.queue_rx_frame((), body.clone()), Err(ReceiveQueueFullError(((), body))));
        assert_eq!(fifo.items, frames);
    }
}
