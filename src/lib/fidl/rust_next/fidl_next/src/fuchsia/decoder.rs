// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::zx::Handle;
use crate::DecodeError;

/// A decoder which support Zircon handles.
pub trait HandleDecoder {
    /// Takes the next handle from the decoder.
    fn take_handle(&mut self) -> Result<Handle, DecodeError>;

    /// Returns the number of handles remaining in the decoder.
    fn handles_remaining(&mut self) -> usize;
}
