// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::addr_of_mut;

use munge::munge;

use crate::decoder::InternalHandleDecoder;
use crate::encoder::InternalHandleEncoder;
use crate::{
    Decode, DecodeError, Decoder, DecoderExt as _, Encode, EncodeError, Encoder, EncoderExt as _,
    Slot, WireU16, WireU32, ZeroPadding, CHUNK_SIZE,
};

#[derive(Clone, Copy)]
#[repr(C)]
struct Encoded {
    maybe_num_bytes: WireU32,
    num_handles: WireU16,
    flags: WireU16,
}

const INLINE_SIZE: usize = 4;

/// A FIDL envelope
#[repr(C, align(8))]
pub union WireEnvelope {
    zero: [u8; 8],
    encoded: Encoded,
    decoded_inline: [MaybeUninit<u8>; INLINE_SIZE],
    decoded_out_of_line: *mut (),
}

unsafe impl ZeroPadding for WireEnvelope {
    fn zero_padding(_: &mut MaybeUninit<Self>) {}
}

impl WireEnvelope {
    const IS_INLINE_BIT: u16 = 1;

    /// Encodes a zero envelope into a slot.
    #[inline]
    pub fn encode_zero(out: &mut MaybeUninit<Self>) {
        out.write(WireEnvelope { zero: [0; 8] });
    }

    /// Encodes a `'static` value into an envelope with an encoder.
    #[inline]
    pub fn encode_value_static<E: InternalHandleEncoder + ?Sized, T: Encode<E>>(
        value: &mut T,
        encoder: &mut E,
        out: &mut MaybeUninit<Self>,
    ) -> Result<(), EncodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    maybe_num_bytes,
                    num_handles,
                    flags,
                },
            } = out;
        }

        let handles_before = encoder.__internal_handle_count();

        let encoded_size = size_of::<T::Encoded>();
        if encoded_size <= INLINE_SIZE {
            // If the encoded inline value is less than 4 bytes long, we need to zero out the part
            // that won't get written over
            unsafe {
                maybe_num_bytes
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(encoded_size)
                    .write_bytes(0, INLINE_SIZE - encoded_size);
            }
        } else {
            return Err(EncodeError::ExpectedInline(encoded_size));
        }

        let value_out = unsafe { &mut *maybe_num_bytes.as_mut_ptr().cast() };
        T::Encoded::zero_padding(value_out);
        value.encode(encoder, value_out)?;

        flags.write(WireU16(Self::IS_INLINE_BIT));

        let handle_count = (encoder.__internal_handle_count() - handles_before).try_into().unwrap();
        num_handles.write(WireU16(handle_count));

        Ok(())
    }

    /// Encodes a value into an envelope with an encoder.
    #[inline]
    pub fn encode_value<E: Encoder + ?Sized, T: Encode<E>>(
        value: &mut T,
        encoder: &mut E,
        out: &mut MaybeUninit<Self>,
    ) -> Result<(), EncodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    maybe_num_bytes,
                    num_handles,
                    flags,
                },
            } = out;
        }

        let handles_before = encoder.__internal_handle_count();

        let encoded_size = size_of::<T::Encoded>();
        if encoded_size <= INLINE_SIZE {
            // If the encoded inline value is less than 4 bytes long, we need to zero out the part
            // that won't get written over
            unsafe {
                maybe_num_bytes
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(encoded_size)
                    .write_bytes(0, INLINE_SIZE - encoded_size);
            }
            let value_out = unsafe { &mut *maybe_num_bytes.as_mut_ptr().cast() };
            T::Encoded::zero_padding(value_out);
            value.encode(encoder, value_out)?;
            flags.write(WireU16(Self::IS_INLINE_BIT));
        } else {
            let bytes_before = encoder.bytes_written();

            encoder.encode_next(value)?;

            let bytes_count = (encoder.bytes_written() - bytes_before).try_into().unwrap();
            maybe_num_bytes.write(WireU32(bytes_count));
            flags.write(WireU16(0));
        }

        let handle_count = (encoder.__internal_handle_count() - handles_before).try_into().unwrap();
        num_handles.write(WireU16(handle_count));

        Ok(())
    }

    /// Returns the zero envelope.
    #[inline]
    pub fn zero() -> Self {
        Self { zero: [0; 8] }
    }

    /// Returns whether a envelope slot is encoded as zero.
    #[inline]
    pub fn is_encoded_zero(slot: Slot<'_, Self>) -> bool {
        munge!(let Self { zero } = slot);
        *zero == [0; 8]
    }

    /// Returns whether an envelope is zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        unsafe { self.zero == [0; 8] }
    }

    #[inline]
    fn out_of_line_chunks(
        maybe_num_bytes: Slot<'_, WireU32>,
        flags: Slot<'_, WireU16>,
    ) -> Result<Option<usize>, DecodeError> {
        if **flags & Self::IS_INLINE_BIT == 0 {
            let num_bytes = **maybe_num_bytes;
            if num_bytes as usize % CHUNK_SIZE != 0 {
                return Err(DecodeError::InvalidEnvelopeSize(num_bytes));
            }
            if num_bytes <= INLINE_SIZE as u32 {
                return Err(DecodeError::OutOfLineValueTooSmall(num_bytes));
            }
            Ok(Some(num_bytes as usize / CHUNK_SIZE))
        } else {
            Ok(None)
        }
    }

    /// Decodes and discards a static type in an envelope.
    #[inline]
    pub fn decode_unknown_static<D: InternalHandleDecoder + ?Sized>(
        slot: Slot<'_, Self>,
        decoder: &mut D,
    ) -> Result<(), DecodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    maybe_num_bytes,
                    num_handles,
                    flags,
                },
            } = slot;
        }

        if let Some(count) = Self::out_of_line_chunks(maybe_num_bytes, flags)? {
            return Err(DecodeError::ExpectedInline(count * CHUNK_SIZE));
        }

        decoder.__internal_take_handles(**num_handles as usize)?;

        Ok(())
    }

    /// Decodes and discards an unknown value in an envelope.
    #[inline]
    pub fn decode_unknown<D: Decoder + ?Sized>(
        slot: Slot<'_, Self>,
        mut decoder: &mut D,
    ) -> Result<(), DecodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    maybe_num_bytes,
                    num_handles,
                    flags,
                },
            } = slot;
        }

        if let Some(count) = Self::out_of_line_chunks(maybe_num_bytes, flags)? {
            decoder.take_chunks(count)?;
        }

        decoder.__internal_take_handles(**num_handles as usize)?;

        Ok(())
    }

    /// Decodes a value of a known type from an envelope.
    #[inline]
    pub fn decode_as_static<D: InternalHandleDecoder + ?Sized, T: Decode<D>>(
        mut slot: Slot<'_, Self>,
        decoder: &mut D,
    ) -> Result<(), DecodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    maybe_num_bytes,
                    num_handles,
                    flags,
                },
             } = slot.as_mut();
        }

        let handles_before = decoder.__internal_handles_remaining();
        let num_handles = **num_handles as usize;

        if let Some(count) = Self::out_of_line_chunks(maybe_num_bytes, flags)? {
            return Err(DecodeError::ExpectedInline(count * CHUNK_SIZE));
        }

        // Decode inline value
        if size_of::<T>() > INLINE_SIZE {
            return Err(DecodeError::InlineValueTooBig(size_of::<T>()));
        }
        munge!(let Self { mut decoded_inline } = slot);
        let mut slot = unsafe { Slot::<T>::new_unchecked(decoded_inline.as_mut_ptr().cast()) };
        T::decode(slot.as_mut(), decoder)?;

        let handles_consumed = handles_before - decoder.__internal_handles_remaining();
        if handles_consumed != num_handles {
            return Err(DecodeError::IncorrectNumberOfHandlesConsumed {
                expected: num_handles,
                actual: handles_consumed,
            });
        }

        Ok(())
    }

    /// Decodes a value of a known type from an envelope.
    #[inline]
    pub fn decode_as<D: Decoder + ?Sized, T: Decode<D>>(
        mut slot: Slot<'_, Self>,
        mut decoder: &mut D,
    ) -> Result<(), DecodeError> {
        munge! {
            let Self {
                encoded: Encoded {
                    mut maybe_num_bytes,
                    num_handles,
                    flags,
                },
             } = slot.as_mut();
        }

        let handles_before = decoder.__internal_handles_remaining();
        let num_handles = **num_handles as usize;

        let out_of_line_chunks = Self::out_of_line_chunks(maybe_num_bytes.as_mut(), flags)?;
        if let Some(_count) = out_of_line_chunks {
            // Decode out-of-line value
            // TODO: set cap on decoder to make sure that the envelope doesn't decode more bytes
            // than it claims that it will
            let mut value_slot = decoder.take_slot::<T>()?;
            let value_ptr = value_slot.as_mut_ptr();
            T::decode(value_slot, decoder)?;

            munge!(let Self { mut decoded_out_of_line } = slot);
            // SAFETY: Identical to `ptr.write(value_ptr.cast())`, but raw
            // pointers don't currently implement `IntoBytes`.
            unsafe { decoded_out_of_line.as_mut_ptr().write(value_ptr.cast()) };
        } else {
            // Decode inline value
            if size_of::<T>() > INLINE_SIZE {
                return Err(DecodeError::InlineValueTooBig(size_of::<T>()));
            }
            munge!(let Self { mut decoded_inline } = slot);
            let mut slot = unsafe { Slot::<T>::new_unchecked(decoded_inline.as_mut_ptr().cast()) };
            T::decode(slot.as_mut(), decoder)?;
        }

        let handles_consumed = handles_before - decoder.__internal_handles_remaining();
        if handles_consumed != num_handles {
            return Err(DecodeError::IncorrectNumberOfHandlesConsumed {
                expected: num_handles,
                actual: handles_consumed,
            });
        }

        Ok(())
    }

    #[inline]
    unsafe fn as_ptr<T>(this: *mut Self) -> *mut T {
        if size_of::<T>() <= INLINE_SIZE {
            let inline = unsafe { addr_of_mut!((*this).decoded_inline) };
            inline.cast()
        } else {
            unsafe { (*this).decoded_out_of_line.cast() }
        }
    }

    /// Returns a reference to the contained `T`.
    ///
    /// # Safety
    ///
    /// The envelope must have been successfully decoded as a `T`.
    #[inline]
    pub unsafe fn deref_unchecked<T>(&self) -> &T {
        let ptr = unsafe { Self::as_ptr::<T>((self as *const Self).cast_mut()).cast_const() };
        unsafe { &*ptr }
    }

    /// Clones the envelope, assuming that it contains an inline `T`.
    ///
    /// # Safety
    ///
    /// The envelope must have been successfully decoded as a `T`.
    #[inline]
    pub unsafe fn clone_unchecked<T: Clone>(&self) -> Self {
        debug_assert_eq!(size_of::<T>(), INLINE_SIZE);

        union ClonedToDecodedInline<T> {
            cloned: ManuallyDrop<T>,
            decoded_inline: [MaybeUninit<u8>; INLINE_SIZE],
        }

        let cloned = unsafe { self.deref_unchecked::<T>().clone() };
        unsafe {
            Self {
                decoded_inline: ClonedToDecodedInline { cloned: ManuallyDrop::new(cloned) }
                    .decoded_inline,
            }
        }
    }
}
