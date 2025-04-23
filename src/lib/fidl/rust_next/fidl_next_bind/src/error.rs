// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

use fidl_next_codec::{EncodeError, DecodeError};

/// Error returned by TryFrom on a strict enum if none of the members match the supplied value.
#[derive(Debug)]
pub struct UnknownStrictEnumMemberError(i128);

impl UnknownStrictEnumMemberError {
    /// Create a new error given an unknown value.
    pub fn new(unknown_value: i128) -> Self {
        Self(unknown_value)
    }
}

impl core::fmt::Display for UnknownStrictEnumMemberError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Strict enum doesn't have a member with value: {}", self.0)
    }
}

impl core::error::Error for UnknownStrictEnumMemberError {}

/// An encoding, decoding, or transport FIDL error.
#[derive(Error, Debug)]
pub enum Error<E> {
    /// A FIDL encoding error.
    #[error("encoding error: {0}")]
    Encode(#[from] EncodeError),
    /// A FIDL decoding error.
    #[error("decoding error: {0}")]
    Decode(#[from] DecodeError),
    /// A FIDL transport error.
    #[error("transport error: {0}")]
    Transport(E),
}
