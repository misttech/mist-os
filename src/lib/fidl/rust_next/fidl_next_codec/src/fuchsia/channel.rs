// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;

use crate::fuchsia::{HandleDecoder, HandleEncoder, WireHandle, WireOptionalHandle};
use crate::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Slot, TakeFrom, ZeroPadding,
};

use zx::sys::zx_handle_t;
use zx::{Channel, Handle};

/// A Zircon channel.
#[derive(Debug)]
#[repr(transparent)]
pub struct WireChannel {
    handle: WireHandle,
}

unsafe impl ZeroPadding for WireChannel {
    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { handle } = out);
        WireHandle::zero_padding(handle);
    }
}

impl WireChannel {
    /// Encodes a channel as present in an output.
    pub fn set_encoded_present(out: &mut MaybeUninit<Self>) {
        munge!(let Self { handle } = out);
        WireHandle::set_encoded_present(handle);
    }

    /// Returns whether the underlying `zx_handle_t` is invalid.
    pub fn is_invalid(&self) -> bool {
        self.handle.is_invalid()
    }

    /// Takes the channel, if any, leaving an invalid handle in its place.
    pub fn take(&self) -> Channel {
        self.handle.take().into()
    }

    /// Returns the underlying [`zx_handle_t`].
    #[inline]
    pub fn as_raw_handle(&self) -> zx_handle_t {
        self.handle.as_raw_handle()
    }
}

unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireChannel {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { handle } = slot.as_mut());
        WireHandle::decode(handle, decoder)
    }
}

impl TakeFrom<WireChannel> for Channel {
    fn take_from(from: &WireChannel) -> Self {
        from.take()
    }
}

/// An optional Zircon channel.
#[derive(Debug)]
#[repr(transparent)]
pub struct WireOptionalChannel {
    handle: WireOptionalHandle,
}

unsafe impl ZeroPadding for WireOptionalChannel {
    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { handle } = out);
        WireOptionalHandle::zero_padding(handle);
    }
}

impl WireOptionalChannel {
    /// Encodes a channel as present in an output.
    pub fn set_encoded_present(out: &mut MaybeUninit<Self>) {
        munge!(let Self { handle } = out);
        WireOptionalHandle::set_encoded_present(handle);
    }

    /// Encodes a channel as absent in an output.
    pub fn set_encoded_absent(out: &mut MaybeUninit<Self>) {
        munge!(let Self { handle } = out);
        WireOptionalHandle::set_encoded_absent(handle);
    }

    /// Returns whether a channel is present.
    pub fn is_some(&self) -> bool {
        !self.handle.is_some()
    }

    /// Returns whether a channel is absent.
    pub fn is_none(&self) -> bool {
        self.handle.is_none()
    }

    /// Takes the channel, if any, leaving an invalid channel in its place.
    pub fn take(&self) -> Option<Channel> {
        self.handle.take().map(Channel::from)
    }

    /// Returns the underlying [`zx_handle_t`], if any.
    #[inline]
    pub fn as_raw_handle(&self) -> Option<zx_handle_t> {
        self.handle.as_raw_handle()
    }
}

impl Encodable for Channel {
    type Encoded = WireChannel;
}

unsafe impl<E: HandleEncoder + ?Sized> Encode<E> for Channel {
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        munge!(let WireChannel { handle } = out);
        Handle::from(self).encode(encoder, handle)
    }
}

impl EncodableOption for Channel {
    type EncodedOption = WireOptionalChannel;
}

unsafe impl<E: HandleEncoder + ?Sized> EncodeOption<E> for Channel {
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        munge!(let WireOptionalChannel { handle } = out);
        Handle::encode_option(this.map(Handle::from), encoder, handle)
    }
}

unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireOptionalChannel {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { handle } = slot.as_mut());
        WireOptionalHandle::decode(handle, decoder)
    }
}

impl TakeFrom<WireOptionalChannel> for Option<Channel> {
    fn take_from(from: &WireOptionalChannel) -> Self {
        from.take()
    }
}
