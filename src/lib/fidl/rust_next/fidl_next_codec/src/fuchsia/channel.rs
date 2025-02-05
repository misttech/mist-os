// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::replace;

use crate::fuchsia::{HandleDecoder, HandleEncoder, WireHandle, WireOptionalHandle};
use crate::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Slot, TakeFrom,
};

use zx::sys::zx_handle_t;
use zx::{Channel, Handle};

/// A Zircon channel.
#[derive(Debug)]
#[repr(transparent)]
pub struct WireChannel {
    handle: WireHandle,
}

impl WireChannel {
    /// Encodes a channel as present in a slot.
    pub fn set_encoded_present(slot: Slot<'_, Self>) {
        munge!(let Self { handle } = slot);
        WireHandle::set_encoded_present(handle);
    }

    /// Returns whether the underlying `zx_handle_t` is invalid.
    pub fn is_invalid(&self) -> bool {
        self.handle.is_invalid()
    }

    /// Takes the channel, if any, leaving an invalid handle in its place.
    pub fn take(&mut self) -> Channel {
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
    fn take_from(from: &mut WireChannel) -> Self {
        from.take()
    }
}

/// An optional Zircon channel.
#[derive(Debug)]
#[repr(transparent)]
pub struct WireOptionalChannel {
    handle: WireOptionalHandle,
}

impl WireOptionalChannel {
    /// Encodes a channel as present in a slot.
    pub fn set_encoded_present(slot: Slot<'_, Self>) {
        munge!(let Self { handle } = slot);
        WireOptionalHandle::set_encoded_present(handle);
    }

    /// Encodes a channel as absent in a slot.
    pub fn set_encoded_absent(slot: Slot<'_, Self>) {
        munge!(let Self { handle } = slot);
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
    pub fn take(&mut self) -> Option<Channel> {
        self.handle.take().map(Channel::from)
    }

    /// Returns the underlying [`zx_handle_t`], if any.
    #[inline]
    pub fn as_raw_handle(&self) -> Option<zx_handle_t> {
        self.handle.as_raw_handle()
    }
}

impl Encodable for Channel {
    type Encoded<'buf> = WireChannel;
}

impl<E: HandleEncoder + ?Sized> Encode<E> for Channel {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        let channel = replace(self, Channel::from(Handle::invalid()));

        munge!(let WireChannel { handle } = slot);
        Handle::from(channel).encode(encoder, handle)
    }
}

impl EncodableOption for Channel {
    type EncodedOption<'buf> = WireOptionalChannel;
}

impl<E: HandleEncoder + ?Sized> EncodeOption<E> for Channel {
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
    ) -> Result<(), EncodeError> {
        let channel = this.map(|channel| replace(channel, Channel::from(Handle::invalid())));

        munge!(let WireOptionalChannel { handle } = slot);
        Handle::encode_option(channel.map(Handle::from).as_mut(), encoder, handle)
    }
}

unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireOptionalChannel {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { handle } = slot.as_mut());
        WireOptionalHandle::decode(handle, decoder)
    }
}

impl TakeFrom<WireOptionalChannel> for Option<Channel> {
    fn take_from(from: &mut WireOptionalChannel) -> Self {
        from.take()
    }
}
