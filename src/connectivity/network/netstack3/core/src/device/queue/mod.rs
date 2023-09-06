// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device queues.

mod fifo;
pub(super) mod rx;
pub mod tx;

use alloc::collections::VecDeque;

use crate::device::DeviceSendFrameError;

/// The maximum number of elements that can be in the RX queue.
const MAX_RX_QUEUED_LEN: usize = 10000;
/// The maximum number of elements that can be in the TX queue.
const MAX_TX_QUEUED_LEN: usize = 10000;
const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReceiveQueueFullError<T>(pub T);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TransmitQueueFrameError<S> {
    NoQueue(DeviceSendFrameError<()>),
    QueueFull(S),
    SerializeError(S),
}

/// The state used to dequeue and handle frames from the device queue.
pub(crate) struct DequeueState<Meta, Buffer> {
    dequeued_frames: VecDeque<(Meta, Buffer)>,
}

impl<Meta, Buffer> Default for DequeueState<Meta, Buffer> {
    fn default() -> DequeueState<Meta, Buffer> {
        DequeueState {
            // Make sure we can dequeue up to `MAX_BATCH_SIZE` frames without
            // needing to reallocate.
            dequeued_frames: VecDeque::with_capacity(MAX_BATCH_SIZE),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum EnqueueResult {
    QueueWasPreviouslyEmpty,
    QueuePreviouslyWasOccupied,
}

#[cfg_attr(test, derive(Debug))]
enum DequeueResult {
    MoreStillQueued,
    NoMoreLeft,
}
