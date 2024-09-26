// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::replace;

use munge::munge;

use crate::{decode, encode, u32_le, Decode, Encode, Handle, RawHandle, Slot, TakeFrom};

/// A FIDL Zircon handle
#[repr(C, align(4))]
pub union WireHandle {
    encoded: u32_le,
    decoded: u32,
}

impl Drop for WireHandle {
    fn drop(&mut self) {
        if let Some(raw_handle) = self.as_raw_handle() {
            drop(Handle::from_raw(raw_handle));
        }
    }
}

impl WireHandle {
    /// Encodes a handle as present in a slot.
    pub fn set_encoded_present(slot: Slot<'_, Self>) {
        munge!(let Self { mut encoded } = slot);
        *encoded = u32_le::from_native(u32::MAX);
    }

    /// Encodes a handle as absent in a slot.
    pub fn set_encoded_absent(slot: Slot<'_, Self>) {
        munge!(let Self { mut encoded } = slot);
        *encoded = u32_le::from_native(0);
    }

    /// Takes the handle, if any, leaving `None` in its place.
    pub fn take(&mut self) -> Option<Handle> {
        let handle = unsafe { replace(&mut self.decoded, 0) };
        RawHandle::new(handle).map(Handle::from_raw)
    }

    /// Returns the underlying `RawHandle`, if any.
    #[inline]
    pub fn as_raw_handle(&self) -> Option<RawHandle> {
        let raw_handle = unsafe { self.decoded };
        RawHandle::new(raw_handle)
    }
}

impl fmt::Debug for WireHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_raw_handle().fmt(f)
    }
}

unsafe impl Decode<'_> for WireHandle {
    fn decode(
        mut slot: Slot<'_, Self>,
        decoder: &mut decode::Decoder<'_>,
    ) -> Result<(), decode::Error> {
        munge!(let Self { encoded } = slot.as_mut());

        match encoded.to_native() {
            0 => (),
            u32::MAX => {
                let handle = decoder.take_handle()?;
                munge!(let Self { mut decoded } = slot);
                decoded.write(handle.into_raw().get());
            }
            e => return Err(decode::Error::InvalidHandlePresence(e)),
        }
        Ok(())
    }
}

impl Encode for Option<Handle> {
    type Encoded<'buf> = WireHandle;

    fn encode(
        &mut self,
        encoder: &mut encode::Encoder,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), encode::Error> {
        if let Some(handle) = self.take() {
            encoder.push_handle(handle);
            WireHandle::set_encoded_present(slot);
        } else {
            WireHandle::set_encoded_absent(slot);
        }
        Ok(())
    }
}

impl TakeFrom<WireHandle> for Option<Handle> {
    fn take_from(from: &mut WireHandle) -> Self {
        from.take()
    }
}
