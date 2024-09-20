// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Proivdes a placeholder handle type to use in generated code examples.

use core::mem::forget;
use core::num::NonZeroU32;

/// A placeholder raw handle type.
pub type RawHandle = NonZeroU32;

/// A placeholder handle.
#[derive(Debug, Eq, PartialEq)]
pub struct Handle {
    raw: RawHandle,
}

impl Handle {
    /// Construct a placeholder handle with the given ID.
    pub const fn from_raw(raw: RawHandle) -> Self {
        Handle { raw }
    }

    /// Consumes the handle, returning the underlying raw handle.
    pub fn into_raw(self) -> RawHandle {
        let result = self.raw;
        forget(self);
        result
    }

    /// Returns the raw handle underlying this handle.
    pub fn as_raw(&self) -> RawHandle {
        self.raw
    }
}

impl Drop for Handle {
    fn drop(&mut self) {}
}
