// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::cell::Cell;
use core::fmt;
use core::mem::{replace, ManuallyDrop};

use zx::sys::{zx_handle_t, ZX_HANDLE_INVALID};
use zx::{Handle, HandleBased as _};

use crate::fuchsia::{HandleDecoder, HandleEncoder};
use crate::{
    munge, u32_le, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError,
    EncodeOption, Slot, TakeFrom,
};

/// A Zircon handle.
#[repr(C, align(4))]
pub union WireHandle {
    encoded: u32_le,
    decoded: ManuallyDrop<Cell<zx_handle_t>>,
}

impl Drop for WireHandle {
    fn drop(&mut self) {
        drop(self.take());
    }
}

impl WireHandle {
    /// Encodes a handle as present in a slot.
    pub fn set_encoded_present(slot: Slot<'_, Self>) {
        munge!(let Self { mut encoded } = slot);
        *encoded = u32_le::from_native(u32::MAX);
    }

    /// Returns whether the underlying `zx_handle_t` is invalid.
    pub fn is_invalid(&self) -> bool {
        self.as_raw_handle() == ZX_HANDLE_INVALID
    }

    /// Takes the handle, if any, leaving an invalid handle in its place.
    pub fn take(&self) -> Handle {
        let raw = unsafe { self.decoded.replace(ZX_HANDLE_INVALID) };
        unsafe { Handle::from_raw(raw) }
    }

    /// Returns the underlying [`zx_handle_t`].
    #[inline]
    pub fn as_raw_handle(&self) -> zx_handle_t {
        unsafe { self.decoded.get() }
    }
}

impl fmt::Debug for WireHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_raw_handle().fmt(f)
    }
}

unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireHandle {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { encoded } = slot.as_mut());

        match encoded.to_native() {
            0 => (),
            u32::MAX => {
                let handle = decoder.take_handle()?;
                munge!(let Self { mut decoded } = slot);
                // SAFETY: `Cell` has no uninit bytes, even though it doesn't implement `IntoBytes`.
                unsafe {
                    decoded.as_mut_ptr().write(ManuallyDrop::new(Cell::new(handle.into_raw())));
                }
            }
            e => return Err(DecodeError::InvalidHandlePresence(e)),
        }
        Ok(())
    }
}

impl TakeFrom<WireHandle> for Handle {
    fn take_from(from: &WireHandle) -> Self {
        from.take()
    }
}

/// An optional Zircon handle.
#[derive(Debug)]
#[repr(transparent)]
pub struct WireOptionalHandle {
    handle: WireHandle,
}

impl WireOptionalHandle {
    /// Encodes a handle as present in a slot.
    pub fn set_encoded_present(slot: Slot<'_, Self>) {
        munge!(let Self { handle } = slot);
        WireHandle::set_encoded_present(handle);
    }

    /// Encodes a handle as absent in a slot.
    pub fn set_encoded_absent(slot: Slot<'_, Self>) {
        munge!(let Self { handle: WireHandle { mut encoded } } = slot);
        *encoded = u32_le::from_native(ZX_HANDLE_INVALID);
    }

    /// Returns whether a handle is present.
    pub fn is_some(&self) -> bool {
        !self.handle.is_invalid()
    }

    /// Returns whether a handle is absent.
    pub fn is_none(&self) -> bool {
        self.handle.is_invalid()
    }

    /// Takes the handle, if any, leaving an invalid handle in its place.
    pub fn take(&self) -> Option<Handle> {
        self.is_some().then(|| self.handle.take())
    }

    /// Returns the underlying [`zx_handle_t`], if any.
    #[inline]
    pub fn as_raw_handle(&self) -> Option<zx_handle_t> {
        self.is_some().then(|| self.handle.as_raw_handle())
    }
}

impl Encodable for Handle {
    type Encoded<'buf> = WireHandle;
}

impl<E: HandleEncoder + ?Sized> Encode<E> for Handle {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        if self.is_invalid() {
            Err(EncodeError::InvalidRequiredHandle)
        } else {
            let handle = replace(self, Handle::invalid());
            encoder.push_handle(handle)?;
            WireHandle::set_encoded_present(slot);
            Ok(())
        }
    }
}

impl EncodableOption for Handle {
    type EncodedOption<'buf> = WireOptionalHandle;
}

impl<E: HandleEncoder + ?Sized> EncodeOption<E> for Handle {
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
    ) -> Result<(), EncodeError> {
        if let Some(handle) = this {
            let handle = replace(handle, Handle::invalid());
            encoder.push_handle(handle)?;
            WireOptionalHandle::set_encoded_present(slot);
        } else {
            WireOptionalHandle::set_encoded_absent(slot);
        }
        Ok(())
    }
}

unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireOptionalHandle {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { handle } = slot.as_mut());
        WireHandle::decode(handle, decoder)
    }
}

impl TakeFrom<WireOptionalHandle> for Option<Handle> {
    fn take_from(from: &WireOptionalHandle) -> Self {
        from.take()
    }
}
