// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

/// Errors that can be produced while encoding FIDL messages.
#[derive(Error, Debug)]
pub enum Error {
    /// An encoded union had an unknown ordinal
    #[error("cannot encode unknown union ordinal of {0}")]
    UnknownUnionOrdinal(usize),
}
