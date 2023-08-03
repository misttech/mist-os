// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides standalone FIDL encoding and decoding.

use crate::encoding::{
    AtRestFlags, Context, Decode, Decoder, Depth, Encode, Encoder, GenericMessage,
    GenericMessageType, ResourceTypeMarker, TransactionHeader, TypeMarker, ValueTypeMarker,
    WireFormatVersion, MAGIC_NUMBER_INITIAL,
};
use crate::handle::{HandleDisposition, HandleInfo};
use crate::{Error, Result};

/// Marker trait implemented for FIDL non-resource structs, tables, and unions.
/// These can be used with the persistence API and standalone encoding/decoding API.
pub trait Persistable:
    TypeMarker<Owned = Self> + Decode<Self> + for<'a> ValueTypeMarker<Borrowed<'a> = &'a Self>
{
}

/// Marker trait implemented for FIDL resource structs, tables, and unions.
/// These can be used with the standalone encoding/decoding API, but not the persistence API.
pub trait Standalone:
    TypeMarker<Owned = Self> + Decode<Self> + for<'a> ResourceTypeMarker<Borrowed<'a> = &'a mut Self>
{
}

/// Header for RFC-0120 persistent FIDL messages.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct WireMetadata {
    /// Must be zero.
    disambiguator: u8,
    /// Magic number indicating the message's wire format. Two sides with
    /// different magic numbers are incompatible with each other.
    magic_number: u8,
    /// "At rest" flags set for this message. MUST NOT be validated by bindings.
    at_rest_flags: [u8; 2],
    /// Reserved bytes. Must be zero.
    reserved: [u8; 4],
}

impl WireMetadata {
    /// Creates a new `WireMetadata` with a specific context and magic number.
    #[inline]
    fn new_full(context: Context, magic_number: u8) -> Self {
        WireMetadata {
            disambiguator: 0,
            magic_number,
            at_rest_flags: context.at_rest_flags().into(),
            reserved: [0; 4],
        }
    }

    /// Returns the header's flags as an `AtRestFlags` value.
    #[inline]
    fn at_rest_flags(&self) -> AtRestFlags {
        AtRestFlags::from_bits_truncate(u16::from_le_bytes(self.at_rest_flags))
    }

    /// Returns the context to use for decoding the message body associated with
    /// this header. During migrations, this is dependent on `self.flags()` and
    /// controls dynamic behavior in the read path.
    #[inline]
    fn decoding_context(&self) -> Context {
        if self.at_rest_flags().contains(AtRestFlags::USE_V2_WIRE_FORMAT) {
            Context { wire_format_version: WireFormatVersion::V2 }
        } else {
            Context { wire_format_version: WireFormatVersion::V1 }
        }
    }

    /// Returns an error if this header has an incompatible magic number.
    #[inline]
    fn validate(&self) -> Result<()> {
        match self.magic_number {
            MAGIC_NUMBER_INITIAL => Ok(()),
            n => Err(Error::IncompatibleMagicNumber(n)),
        }
    }
}

/// The default context for persistent encoding.
#[inline]
fn default_persistent_encode_context() -> Context {
    Context { wire_format_version: WireFormatVersion::V2 }
}

/// Encodes a FIDL object to bytes following RFC-0120. This only works on
/// non-resource structs, tables, and unions. See `unpersist` for the reverse.
pub fn persist<T: Persistable>(body: &T) -> Result<Vec<u8>> {
    persist_with_context::<T>(body, default_persistent_encode_context())
}

// TODO(fxbug.dev/79584): Kept only for overnet, remove when possible.
#[doc(hidden)]
pub fn persist_with_context<T: ValueTypeMarker>(
    body: T::Borrowed<'_>,
    context: Context,
) -> Result<Vec<u8>> {
    let header = WireMetadata::new_full(context, MAGIC_NUMBER_INITIAL);
    let msg = GenericMessage { header, body };
    let mut combined_bytes = Vec::<u8>::new();
    let mut handles = Vec::<HandleDisposition<'static>>::new();
    Encoder::encode_with_context::<GenericMessageType<WireMetadata, T>>(
        context,
        &mut combined_bytes,
        &mut handles,
        msg,
    )?;
    debug_assert!(handles.is_empty(), "value type contains handles");
    Ok(combined_bytes)
}

/// Decodes a FIDL object from bytes following RFC-0120. Must be a non-resource
/// struct, table, or union. See `persist` for the reverse.
pub fn unpersist<T: Persistable>(bytes: &[u8]) -> Result<T> {
    // TODO(fxbug.dev/99738): Only accept the new header format.
    //
    // To soft-transition component manager's use of persistent FIDL, we
    // temporarily need to accept the old 16-byte header.
    //
    //       disambiguator
    //            | magic
    //            |  | flags
    //            |  |  / \  ( reserved )
    //     new:  00 MA FL FL  00 00 00 00
    //     idx:  0  1  2  3   4  5  6  7
    //     old:  00 00 00 00  FL FL FL MA  00 00 00 00  00 00 00 00
    //          ( txid gap )   \ | /   |  (      ordinal gap      )
    //                         flags  magic
    //
    // So bytes[7] is 0 for the new format and 1 for the old format.
    if bytes.len() < 8 {
        return Err(Error::InvalidHeader);
    }
    let header_len = match bytes[7] {
        0 => 8,
        MAGIC_NUMBER_INITIAL => 16,
        _ => return Err(Error::InvalidHeader),
    };
    if bytes.len() < header_len {
        return Err(Error::OutOfRange);
    }
    let (header_bytes, body_bytes) = bytes.split_at(header_len);
    let header = decode_wire_metadata(header_bytes)?;
    let mut output = T::new_empty();
    Decoder::decode_with_context::<T>(header.decoding_context(), body_bytes, &mut [], &mut output)?;
    Ok(output)
}

/// Encodes a FIDL object to bytes and wire metadata following RFC-0120. Must be
/// a non-resource struct, table, or union.
pub fn standalone_encode_value<T: Persistable>(body: &T) -> Result<(Vec<u8>, WireMetadata)> {
    // This helper is needed to convince rustc that &T implements Encode<T>.
    fn helper<T: ValueTypeMarker>(body: T::Borrowed<'_>) -> Result<(Vec<u8>, WireMetadata)> {
        let context = default_persistent_encode_context();
        let metadata = WireMetadata::new_full(context, MAGIC_NUMBER_INITIAL);
        let mut bytes = Vec::<u8>::new();
        let mut handles = Vec::<HandleDisposition<'static>>::new();
        Encoder::encode_with_context::<T>(context, &mut bytes, &mut handles, body)?;
        debug_assert!(handles.is_empty(), "value type contains handles");
        Ok((bytes, metadata))
    }
    helper::<T>(body)
}

/// Encodes a FIDL object to bytes, handles, and wire metadata following
/// RFC-0120. Must be a resource struct, table, or union.
pub fn standalone_encode_resource<T: Standalone>(
    mut body: T,
) -> Result<(Vec<u8>, Vec<HandleDisposition<'static>>, WireMetadata)> {
    // This helper is needed to convince rustc that &mut T implements Encode<T>.
    fn helper<T: ResourceTypeMarker>(
        body: T::Borrowed<'_>,
    ) -> Result<(Vec<u8>, Vec<HandleDisposition<'static>>, WireMetadata)> {
        let context = default_persistent_encode_context();
        let metadata = WireMetadata::new_full(context, MAGIC_NUMBER_INITIAL);
        let mut bytes = Vec::<u8>::new();
        let mut handles = Vec::<HandleDisposition<'static>>::new();
        Encoder::encode_with_context::<T>(context, &mut bytes, &mut handles, body)?;
        Ok((bytes, handles, metadata))
    }
    helper::<T>(&mut body)
}

/// Decodes a FIDL object from bytes and wire metadata following RFC-0120. Must
/// be a non-resource struct, table, or union.
pub fn standalone_decode_value<T: Persistable>(bytes: &[u8], metadata: &WireMetadata) -> Result<T> {
    let mut output = T::Owned::new_empty();
    Decoder::decode_with_context::<T>(metadata.decoding_context(), bytes, &mut [], &mut output)?;
    Ok(output)
}

/// Decodes a FIDL object from bytes, handles, and wire metadata following
/// RFC-0120. Must be a resource struct, table, or union.
pub fn standalone_decode_resource<T: Standalone>(
    bytes: &[u8],
    handles: &mut [HandleInfo],
    metadata: &WireMetadata,
) -> Result<T> {
    let mut output = T::Owned::new_empty();
    Decoder::decode_with_context::<T>(metadata.decoding_context(), bytes, handles, &mut output)?;
    Ok(output)
}

/// Decodes the persistently stored header from a message.
/// Returns the header and a reference to the tail of the message.
fn decode_wire_metadata(bytes: &[u8]) -> Result<WireMetadata> {
    let context = Context { wire_format_version: WireFormatVersion::V2 };
    let metadata = match bytes.len() {
        8 => {
            // New 8-byte format.
            let mut header = new_empty!(WireMetadata);
            Decoder::decode_with_context::<WireMetadata>(context, bytes, &mut [], &mut header)
                .map_err(|_| Error::InvalidHeader)?;
            header
        }
        // TODO(fxbug.dev/99738): Remove this.
        16 => {
            // Old 16-byte format that matches TransactionHeader.
            let mut header = new_empty!(TransactionHeader);
            Decoder::decode_with_context::<TransactionHeader>(context, bytes, &mut [], &mut header)
                .map_err(|_| Error::InvalidHeader)?;
            WireMetadata {
                disambiguator: 0,
                magic_number: header.magic_number,
                at_rest_flags: header.at_rest_flags,
                reserved: [0; 4],
            }
        }
        _ => return Err(Error::InvalidHeader),
    };
    metadata.validate()?;
    Ok(metadata)
}

unsafe impl TypeMarker for WireMetadata {
    type Owned = Self;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        1
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        8
    }
}

impl ValueTypeMarker for WireMetadata {
    type Borrowed<'a> = &'a Self;
    fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl Encode<WireMetadata> for &WireMetadata {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<WireMetadata>(offset);
        unsafe {
            let buf_ptr = encoder.buf.as_mut_ptr().add(offset);
            (buf_ptr as *mut WireMetadata).write_unaligned(*self);
        }
        Ok(())
    }
}

impl Decode<Self> for WireMetadata {
    #[inline(always)]
    fn new_empty() -> Self {
        Self { disambiguator: 0, magic_number: 0, at_rest_flags: [0; 2], reserved: [0; 4] }
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Self>(offset);
        unsafe {
            let buf_ptr = decoder.buf.as_ptr().add(offset);
            let obj_ptr = self as *mut WireMetadata;
            std::ptr::copy_nonoverlapping(buf_ptr, obj_ptr as *mut u8, 8);
        }
        Ok(())
    }
}
