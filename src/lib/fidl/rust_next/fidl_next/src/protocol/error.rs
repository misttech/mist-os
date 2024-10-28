// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

use crate::DecodeError;

/// Errors that can be produced when decoding FIDL messages.
#[derive(Error, Debug)]
pub enum ProtocolError<E> {
    /// The dispatcher was stopped. This may be due to an error or because the channel was closed.
    #[error("the dispatcher was stopped")]
    DispatcherStopped,

    /// The underlying transport encountered an error.
    #[error("transport error: {0}")]
    TransportError(E),

    /// The endpoint received a message with an invalid protocol header.
    #[error("invalid message header")]
    InvalidMessageHeader(DecodeError),

    /// The endpoint received a response for a transaction which did not occur.
    #[error("unrequested response to txid {0}")]
    UnrequestedResponse(u32),

    /// The response from the server was of the wrong type.
    #[error(
        "the response from the server was the wrong type; expected ordinal {expected}, but got \
        ordinal {actual}"
    )]
    InvalidResponseOrdinal {
        /// The expected ordinal of the response
        expected: u64,
        /// The actual ordinal of the response
        actual: u64,
    },
}
