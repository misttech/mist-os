// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

mod snapshot;
pub use snapshot::{Allocation, Snapshot};

mod streamer;
pub use streamer::Streamer;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Missing expected field {} in {}", .field, .container)]
    MissingField { container: &'static str, field: &'static str },
    #[error("SnapshotReceiver stream ended unexpectedly")]
    UnexpectedEndOfStream,
    #[error("SnapshotReceiver stream contains an unknown element type")]
    UnexpectedElementType,
    #[error("SnapshotReceiver stream contains multiple {} elements with the same unique ID",
        .element_type)]
    ConflictingElement { element_type: &'static str },
    #[error("Zircon error: {}", .0)]
    ZxError(#[from] fuchsia_zircon_status::Status),
    #[error("FIDL error: {}", .0)]
    FidlError(#[from] fidl::Error),
}
