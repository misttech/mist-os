// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;

use crate::fuchsia::{HandleDecoder, HandleEncoder, WireHandle, WireOptionalHandle};
use crate::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    FromWire, FromWireOption, Slot, Wire,
};

use zx::sys::zx_handle_t;
use zx::Handle;

macro_rules! define_wire_handle_types {
    ($($wire:ident($wire_optional:ident): $natural:ident),* $(,)?) => { $(
        #[doc = concat!("A Zircon ", stringify!($natural), ".")]
        #[derive(Debug)]
        #[repr(transparent)]
        pub struct $wire {
            handle: WireHandle,
        }

        unsafe impl Wire for $wire {
            type Decoded<'de> = Self;

            #[inline]
            fn zero_padding(out: &mut MaybeUninit<Self>) {
                munge!(let Self { handle } = out);
                WireHandle::zero_padding(handle);
            }
        }

        impl $wire {
            #[doc = concat!("Encodes a ", stringify!($natural), " as present in an output.")]
            pub fn set_encoded_present(out: &mut MaybeUninit<Self>) {
                munge!(let Self { handle } = out);
                WireHandle::set_encoded_present(handle);
            }

            /// Returns whether the underlying `zx_handle_t` is invalid.
            pub fn is_invalid(&self) -> bool {
                self.handle.is_invalid()
            }

            /// Returns the underlying [`zx_handle_t`].
            #[inline]
            pub fn as_raw_handle(&self) -> zx_handle_t {
                self.handle.as_raw_handle()
            }
        }

        unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for $wire {
            fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
                munge!(let Self { handle } = slot.as_mut());
                WireHandle::decode(handle, decoder)
            }
        }

        #[doc = concat!("An optional Zircon ", stringify!($natural), ".")]
        #[derive(Debug)]
        #[repr(transparent)]
        pub struct $wire_optional {
            handle: WireOptionalHandle,
        }

        unsafe impl Wire for $wire_optional {
            type Decoded<'de> = Self;

            #[inline]
            fn zero_padding(out: &mut MaybeUninit<Self>) {
                munge!(let Self { handle } = out);
                WireOptionalHandle::zero_padding(handle);
            }
        }

        impl $wire_optional {
            #[doc = concat!("Encodes a ", stringify!($natural), " as present in an output.")]
            pub fn set_encoded_present(out: &mut MaybeUninit<Self>) {
                munge!(let Self { handle } = out);
                WireOptionalHandle::set_encoded_present(handle);
            }

            #[doc = concat!("Encodes a ", stringify!($natural), " as absent in an output.")]
            pub fn set_encoded_absent(out: &mut MaybeUninit<Self>) {
                munge!(let Self { handle } = out);
                WireOptionalHandle::set_encoded_absent(handle);
            }

            #[doc = concat!("Returns whether a ", stringify!($natural), " is present.")]
            pub fn is_some(&self) -> bool {
                !self.handle.is_some()
            }

            #[doc = concat!("Returns whether a ", stringify!($natural), " is absent.")]
            pub fn is_none(&self) -> bool {
                self.handle.is_none()
            }

            /// Returns the underlying [`zx_handle_t`], if any.
            #[inline]
            pub fn as_raw_handle(&self) -> Option<zx_handle_t> {
                self.handle.as_raw_handle()
            }
        }

        unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for $wire_optional {
            fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
                munge!(let Self { handle } = slot.as_mut());
                WireOptionalHandle::decode(handle, decoder)
            }
        }

        impl Encodable for zx::$natural {
            type Encoded = $wire;
        }

        unsafe impl<E: HandleEncoder + ?Sized> Encode<E> for zx::$natural {
            fn encode(
                self,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                munge!(let $wire { handle } = out);
                Handle::from(self).encode(encoder, handle)
            }
        }

        impl FromWire<$wire> for zx::$natural {
            fn from_wire(wire: $wire) -> Self {
                Handle::from_wire(wire.handle).into()
            }
        }

        impl EncodableOption for zx::$natural {
            type EncodedOption = $wire_optional;
        }

        unsafe impl<E: HandleEncoder + ?Sized> EncodeOption<E> for zx::$natural {
            fn encode_option(
                this: Option<Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                munge!(let $wire_optional { handle } = out);
                Handle::encode_option(this.map(Handle::from), encoder, handle)
            }
        }

        impl FromWireOption<$wire_optional> for zx::$natural {
            fn from_wire_option(wire: $wire_optional) -> Option<Self> {
                Handle::from_wire_option(wire.handle).map(zx::$natural::from)
            }
        }
    )* };
}

define_wire_handle_types! {
    WireProcess(WireOptionalProcess): Process,
    WireThread(WireOptionalThread): Thread,
    WireVmo(WireOptionalVmo): Vmo,
    WireChannel(WireOptionalChannel): Channel,
    WireEvent(WireOptionalEvent): Event,
    WirePort(WireOptionalPort): Port,
    WireInterrupt(WireOptionalInterrupt): Interrupt,
    WireSocket(WireOptionalSocket): Socket,
    WireResource(WireOptionalResource): Resource,
    WireEventPair(WireOptionalEventPair): EventPair,
    WireJob(WireOptionalJob): Job,
    WireVmar(WireOptionalVmar): Vmar,
    WireFifo(WireOptionalFifo): Fifo,
    WireGuest(WireOptionalGuest): Guest,
    WireVcpu(WireOptionalVcpu): Vcpu,
    WireTimer(WireOptionalTimer): Timer,
    WireIommu(WireOptionalIommu): Iommu,
    WireBti(WireOptionalBti): Bti,
    WireProfile(WireOptionalProfile): Profile,
    WirePmt(WireOptionalPmt): Pmt,
    WirePager(WireOptionalPager): Pager,
    WireException(WireOptionalException): Exception,
    WireClock(WireOptionalClock): Clock,
    WireStream(WireOptionalStream): Stream,
    WireIob(WireOptionalIob): Iob,
}
