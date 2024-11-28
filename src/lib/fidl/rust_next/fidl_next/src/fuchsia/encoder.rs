// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::zx::Handle;
use crate::EncodeError;

/// An encoder which supports Zircon handles.
pub trait HandleEncoder {
    /// Pushes a handle into the encoder.
    fn push_handle(&mut self, handle: Handle) -> Result<(), EncodeError>;

    /// Returns the number of handles added to the encoder.
    fn handles_pushed(&self) -> usize;
}
