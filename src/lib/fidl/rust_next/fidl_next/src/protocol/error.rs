// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

use crate::DecodeError;

/// Errors that can be produced by FIDL client and server dispatchers.
#[derive(Error, Debug)]
pub enum DispatcherError<E> {
    /// The underlying transport encountered an error.
    #[error("the underlying transport encountered an error: {0}")]
    TransportError(E),

    /// The dispatcher received a message with an invalid protocol header.
    #[error("received a message with an invalid message header: {0}")]
    InvalidMessageHeader(DecodeError),

    /// The dispatcher received a response for a transaction which did not occur.
    #[error("received a response which did not correspond to a pending request: {0}")]
    UnrequestedResponse(u32),

    /// The dispatcher received a response with the wrong ordinal for the transaction.
    #[error(
        "received a response with the wrong ordinal for the transaction; expected ordinal \
        {expected}, but got ordinal {actual}"
    )]
    InvalidResponseOrdinal {
        /// The expected ordinal of the response
        expected: u64,
        /// The actual ordinal of the response
        actual: u64,
    },
}
