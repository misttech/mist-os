// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;

use munge::munge;

use crate::{DecodeError, Slot, WireU64};

/// A raw FIDL pointer
#[repr(C, align(8))]
pub union WirePointer<T> {
    encoded: WireU64,
    decoded: *mut T,
}

impl<T> WirePointer<T> {
    /// Returns whether the wire pointer was encoded present.
    pub fn is_encoded_present(slot: Slot<'_, Self>) -> Result<bool, DecodeError> {
        munge!(let Self { encoded } = slot);
        match **encoded {
            0 => Ok(false),
            u64::MAX => Ok(true),
            x => Err(DecodeError::InvalidPointerPresence(x)),
        }
    }

    /// Encodes that a pointer is present in an output.
    pub fn encode_present(out: &mut MaybeUninit<Self>) {
        munge!(let Self { encoded } = out);
        encoded.write(WireU64(u64::MAX));
    }

    /// Encodes that a pointer is absent in a slot.
    pub fn encode_absent(out: &mut MaybeUninit<Self>) {
        munge!(let Self { encoded } = out);
        encoded.write(WireU64(0));
    }

    /// Sets the decoded value of the pointer.
    pub fn set_decoded(slot: Slot<'_, Self>, ptr: *mut T) {
        munge!(let Self { mut decoded } = slot);
        // SAFETY: Identical to `decoded.write(ptr.into_raw())`, but raw
        // pointers don't currently implement `IntoBytes`.
        unsafe {
            *decoded.as_mut_ptr() = ptr;
        }
    }

    /// Returns the underlying pointer.
    pub fn as_ptr(&self) -> *mut T {
        unsafe { self.decoded }
    }
}
