// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device queues.

mod fifo;
pub(super) mod rx;

use alloc::collections::VecDeque;

const MAX_RX_QUEUED_PACKETS: usize = 10000;
const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReceiveQueueFullError<T>(pub T);

/// The state used to dequeue and handle packets from the device queue.
pub(crate) struct DequeueState<Meta, Buffer> {
    dequed_packets: VecDeque<(Meta, Buffer)>,
}

impl<Meta, Buffer> Default for DequeueState<Meta, Buffer> {
    fn default() -> DequeueState<Meta, Buffer> {
        DequeueState {
            /// Make sure we can dequeue up to `MAX_BATCH_SIZE` packets without
            /// needing to reallocate.
            dequed_packets: VecDeque::with_capacity(MAX_BATCH_SIZE),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum EnqueueResult {
    QueueWasPreviouslyEmpty,
    QueuePreviouslyHadPackets,
}

#[cfg_attr(test, derive(Debug))]
enum DequeueResult {
    MorePacketsStillQueued,
    NoMorePacketsLeft,
}
