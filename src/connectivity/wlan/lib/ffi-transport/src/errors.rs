// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("no resources (requested {requested_capacity} bytes)")]
    NoResources { requested_capacity: usize },
    #[error("invalid FfiBuffer: {0}")]
    InvalidFfiBuffer(InvalidFfiBuffer),
}

#[derive(Debug, Error)]
pub enum InvalidFfiBuffer {
    #[error("null ctx")]
    NullCtx,
    #[error("null data")]
    NullData,
    #[error("zero capacity")]
    ZeroCapacity,
}
