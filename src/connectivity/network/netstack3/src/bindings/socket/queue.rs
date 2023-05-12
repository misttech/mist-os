// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Queue for datagram-like sockets.

use std::collections::VecDeque;

use fidl_fuchsia_posix_socket as psocket;

use const_unwrap::const_unwrap_option;
use fuchsia_zircon::{self as zx, Peered as _};
use thiserror::Error;
use tracing::{error, trace};

// These values were picked to match Linux behavior.

/// Limits the total size of messages that can be queued for an application
/// socket to be read before we start dropping packets.
pub(crate) const MAX_OUTSTANDING_APPLICATION_MESSAGES_SIZE: usize = 4 * 1024 * 1024;
/// The default value for the amount of data that can be queued for an
/// application socket to be read before packets are dropped.
pub(crate) const DEFAULT_OUTSTANDING_APPLICATION_MESSAGES_SIZE: usize = 208 * 1024;
/// The minimum value for the amount of data that can be queued for an
/// application socket to be read before packets are dropped.
pub(crate) const MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE: usize = 256;

const ZXSIO_SIGNAL_INCOMING: zx::Signals =
    const_unwrap_option(zx::Signals::from_bits(psocket::SIGNAL_DATAGRAM_INCOMING));

#[derive(Copy, Clone, Debug, Error, Eq, PartialEq)]
#[error("application buffers are full")]
pub(crate) struct NoSpace;

#[derive(Debug)]
pub(crate) struct MessageQueue<M> {
    local_event: zx::EventPair,
    queue: AvailableMessageQueue<M>,
}

impl<M> MessageQueue<M> {
    pub(crate) fn new(local_event: zx::EventPair) -> Self {
        Self {
            local_event,
            queue: AvailableMessageQueue::new(DEFAULT_OUTSTANDING_APPLICATION_MESSAGES_SIZE),
        }
    }

    pub(crate) fn peek(&self) -> Option<&M> {
        let Self { queue, local_event: _ } = self;
        queue.peek()
    }

    pub(crate) fn pop(&mut self) -> Option<M>
    where
        M: BodyLen,
    {
        let Self { queue, local_event } = self;
        let message = queue.pop();
        if queue.is_empty() {
            if let Err(e) = local_event.signal_peer(ZXSIO_SIGNAL_INCOMING, zx::Signals::NONE) {
                error!("socket failed to signal peer: {:?}", e);
            }
        }
        message
    }

    pub(crate) fn receive(&mut self, message: impl BodyLen + Into<M>) {
        let Self { queue, local_event } = self;
        let body_len = message.body_len();
        match queue.push(message) {
            Err(NoSpace) => {
                trace!("dropping {}-byte packet because the receive queue is full", body_len)
            }
            Ok(()) => local_event
                .signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_INCOMING)
                .unwrap_or_else(|e| error!("signal peer failed: {:?}", e)),
        }
    }

    pub(crate) fn local_event(&self) -> &zx::EventPair {
        &self.local_event
    }

    pub(crate) fn max_available_messages_size(&self) -> usize {
        let Self { local_event: _, queue } = self;
        queue.max_available_messages_size
    }

    pub(crate) fn set_max_available_messages_size(&mut self, new_size: usize) {
        let Self { local_event: _, queue } = self;
        queue.max_available_messages_size = usize::max(
            usize::min(new_size, MAX_OUTSTANDING_APPLICATION_MESSAGES_SIZE),
            MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE,
        );
    }

    #[cfg(test)]
    pub(crate) fn available_messages(&self) -> impl ExactSizeIterator<Item = &M> {
        let Self {
            local_event: _,
            queue:
                AvailableMessageQueue {
                    available_messages,
                    available_messages_size: _,
                    max_available_messages_size: _,
                },
        } = self;
        available_messages.iter()
    }
}

#[derive(Debug)]
struct AvailableMessageQueue<M> {
    available_messages: VecDeque<M>,
    /// The total size of the contents of `available_messages`.
    available_messages_size: usize,
    /// The maximum allowed value for `available_messages_size`.
    max_available_messages_size: usize,
}

pub(crate) trait BodyLen {
    fn body_len(&self) -> usize;
}

impl<M> AvailableMessageQueue<M> {
    pub(crate) fn new(max_available_messages_size: usize) -> Self {
        Self {
            available_messages: Default::default(),
            available_messages_size: 0,
            max_available_messages_size,
        }
    }

    pub(crate) fn push(&mut self, message: impl BodyLen + Into<M>) -> Result<(), NoSpace> {
        let Self { available_messages, available_messages_size, max_available_messages_size } =
            self;

        // Respect the configured limit except if this would be the only message
        // in the buffer. This is compatible with Linux behavior.
        let len = message.body_len();
        if *available_messages_size + len > *max_available_messages_size
            && !available_messages.is_empty()
        {
            return Err(NoSpace);
        }

        available_messages.push_back(message.into());
        *available_messages_size += len;
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<M>
    where
        M: BodyLen,
    {
        let Self { available_messages, available_messages_size, max_available_messages_size: _ } =
            self;

        available_messages.pop_front().map(|msg| {
            *available_messages_size -= msg.body_len();
            msg
        })
    }

    pub(crate) fn peek(&self) -> Option<&M> {
        let Self { available_messages, available_messages_size: _, max_available_messages_size: _ } =
            self;
        available_messages.front()
    }

    pub(crate) fn is_empty(&self) -> bool {
        let Self { available_messages, available_messages_size: _, max_available_messages_size: _ } =
            self;
        available_messages.is_empty()
    }
}
