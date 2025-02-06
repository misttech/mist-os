// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia-specific extensions to the FIDL codec.

mod channel;
mod handle;

use zx::Handle;

use crate::decoder::InternalHandleDecoder;
use crate::encoder::InternalHandleEncoder;
use crate::{DecodeError, EncodeError};

pub use self::channel::*;
pub use self::handle::*;
pub use zx;

/// A decoder which support Zircon handles.
pub trait HandleDecoder: InternalHandleDecoder {
    /// Takes the next handle from the decoder.
    fn take_handle(&mut self) -> Result<Handle, DecodeError>;

    /// Returns the number of handles remaining in the decoder.
    fn handles_remaining(&mut self) -> usize;
}

impl<T: HandleDecoder> HandleDecoder for &mut T {
    fn take_handle(&mut self) -> Result<Handle, DecodeError> {
        T::take_handle(self)
    }

    fn handles_remaining(&mut self) -> usize {
        T::handles_remaining(self)
    }
}

/// An encoder which supports Zircon handles.
pub trait HandleEncoder: InternalHandleEncoder {
    /// Pushes a handle into the encoder.
    fn push_handle(&mut self, handle: Handle) -> Result<(), EncodeError>;

    /// Returns the number of handles added to the encoder.
    fn handles_pushed(&self) -> usize;
}

impl<T: HandleEncoder> HandleEncoder for &mut T {
    fn push_handle(&mut self, handle: Handle) -> Result<(), EncodeError> {
        T::push_handle(self, handle)
    }

    fn handles_pushed(&self) -> usize {
        T::handles_pushed(self)
    }
}
