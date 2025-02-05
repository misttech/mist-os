// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod channel;
mod handle;

use zx::Handle;

use crate::{DecodeError, EncodeError};

pub use self::channel::*;
pub use self::handle::*;

/// A decoder which support Zircon handles.
pub trait HandleDecoder {
    /// Takes the next handle from the decoder.
    fn take_handle(&mut self) -> Result<Handle, DecodeError>;

    /// Returns the number of handles remaining in the decoder.
    fn handles_remaining(&mut self) -> usize;
}

/// An encoder which supports Zircon handles.
pub trait HandleEncoder {
    /// Pushes a handle into the encoder.
    fn push_handle(&mut self, handle: Handle) -> Result<(), EncodeError>;

    /// Returns the number of handles added to the encoder.
    fn handles_pushed(&self) -> usize;
}
