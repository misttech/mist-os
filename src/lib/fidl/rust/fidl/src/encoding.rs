// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL encoding and decoding.

// TODO(fxbug.dev/118834): This file is too big. Split it into smaller files.

pub use static_assertions::const_assert_eq;

use {
    crate::endpoints::ProtocolMarker,
    crate::handle::{
        Handle, HandleBased, HandleDisposition, HandleInfo, HandleOp, ObjectType, Rights, Status,
    },
    crate::{Error, Result},
    bitflags::bitflags,
    fuchsia_zircon_status as zx_status, fuchsia_zircon_types as zx_types,
    std::{cell::RefCell, cell::RefMut, marker::PhantomData, mem, ptr, str, u32, u64},
};

////////////////////////////////////////////////////////////////////////////////
// Traits
////////////////////////////////////////////////////////////////////////////////

/// A FIDL type marker.
///
/// This trait is only used for compile time dispatch. For example, we can
/// parameterize code on `T: TypeMarker`, but we would never write `value: T`.
/// In fact, `T` is often a zero-sized struct. From the user's perspective,
/// `T::Owned` is the FIDL type's "Rust type". For example, for the FIDL type
/// `string:10`, `T` is `BoundedString<10>` and `T::Owned` is `String`.
///
/// For primitive types and user-defined types, `Self` is actually the same as
/// `Self::Owned`. For all others (strings, arrays, vectors, handles, endpoints,
/// optionals, error results), `Self` is a zero-sized struct that uses generics
/// to represent FIDL type information such as the element type or constraints.
///
/// # Safety
///
/// * Implementations of `encode_is_copy` must only return true if it is safe to
///   transmute from `*const Self::Owned` to `*const u8` and read `inline_size`
///   bytes starting from that address.
///
/// * Implementations of `decode_is_copy` must only return true if it is safe to
///   transmute from `*mut Self::Owned` to `*mut u8` and write `inline_size`
///   bytes starting at that address.
pub unsafe trait TypeMarker: 'static + Sized {
    /// The owned Rust type which this FIDL type decodes into.
    type Owned: Decode<Self>;

    /// Returns the minimum required alignment of the inline portion of the
    /// encoded object. It must be a (nonzero) power of two.
    fn inline_align(context: Context) -> usize;

    /// Returns the size of the inline portion of the encoded object, including
    /// padding for alignment. Must be a multiple of `inline_align`.
    fn inline_size(context: Context) -> usize;

    /// Returns true if the memory layout of `Self::Owned` matches the FIDL wire
    /// format and encoding requires no validation. When true, we can optimize
    /// encoding arrays and vectors of `Self::Owned` to a single memcpy.
    ///
    /// This can be true even when `decode_is_copy` is false. For example, bools
    /// require validation when decoding, but they do not require validation
    /// when encoding because Rust guarantees a bool is either 0x00 or 0x01.
    #[inline(always)]
    fn encode_is_copy() -> bool {
        false
    }

    /// Returns true if the memory layout of `Self::Owned` matches the FIDL wire
    /// format and decoding requires no validation. When true, we can optimize
    /// decoding arrays and vectors of `Self::Owned` to a single memcpy.
    #[inline(always)]
    fn decode_is_copy() -> bool {
        false
    }
}

/// A FIDL value type marker.
///
/// Value types are guaranteed to never contain handles. As a result, they can
/// be encoded by immutable reference (or by value for `Copy` types).
pub trait ValueTypeMarker: TypeMarker {
    /// The Rust type to use for encoding. This is a particular `Encode<Self>`
    /// type cheaply obtainable from `&Self::Owned`. There are three cases:
    ///
    /// - Special cases such as `&[T]` for vectors.
    /// - For primitives, bits, and enums, it is `Owned`.
    /// - Otherwise, it is `&Owned`.
    type Borrowed<'a>: Encode<Self>;

    /// Cheaply converts from `&Self::Owned` to `Self::Borrowed`.
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_>;
}

/// A FIDL resource type marker.
///
/// Resource types are allowed to contain handles. As a result, they must be
/// encoded by mutable reference so that handles can be zeroed out.
pub trait ResourceTypeMarker: TypeMarker {
    /// The Rust type to use for encoding. This is a particular `Encode<Self>`
    /// type cheaply obtainable from `&mut Self::Owned`. There are three cases:
    ///
    /// - Special cases such as `&mut [T]` for vectors.
    /// - When `Owned: HandleBased`, it is `Owned`.
    /// - Otherwise, it is `&mut Owned`.
    type Borrowed<'a>: Encode<Self>;

    /// Cheaply converts from `&mut Self::Owned` to `Self::Borrowed`. For
    /// `HandleBased` types this is "take" (it returns an owned handle and
    /// replaces `value` with `Handle::invalid`), and for all other types it is
    /// "borrow" (just converts from one reference to another).
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_>;
}

/// A Rust type that can be encoded as the FIDL type `T`.
///
/// # Safety
///
/// Implementations of `encode` must write every byte in
/// `encoder.buf[offset..offset + T::inline_size(encoder.context)]` unless
/// returning an `Err` value.
pub unsafe trait Encode<T: TypeMarker>: Sized {
    /// Encodes the object into the encoder's buffers. Any handles stored in the
    /// object are swapped for `Handle::INVALID`.
    ///
    /// Implementations that encode out-of-line objects must call `depth.increment()?`.
    ///
    /// # Safety
    ///
    /// Callers must ensure `offset` is a multiple of `T::inline_align` and
    /// `encoder.buf` has room for writing `T::inline_size` bytes at `offset`.
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()>;
}

/// A Rust type that can be decoded from the FIDL type `T`.
pub trait Decode<T: TypeMarker>: 'static + Sized {
    /// Creates a valid instance of `Self`. The specific value does not matter,
    /// since it will be overwritten by `decode`.
    // TODO(fxbug.dev/118783): Take context parameter to discourage using this.
    fn new_empty() -> Self;

    /// Decodes an object of type `T` from the decoder's buffers into `self`.
    ///
    /// Implementations must validate every byte in
    /// `decoder.buf[offset..offset + T::inline_size(decoder.context)]` unless
    /// returning an `Err` value. Implementations that decode out-of-line
    /// objects must call `depth.increment()?`.
    ///
    /// # Safety
    ///
    /// Callers must ensure `offset` is a multiple of `T::inline_align` and
    /// `decoder.buf` has room for reading `T::inline_size` bytes at `offset`.
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()>;
}

////////////////////////////////////////////////////////////////////////////////
// Constants
////////////////////////////////////////////////////////////////////////////////

/// The maximum recursion depth of encoding and decoding. Each pointer to an
/// out-of-line object counts as one step in the recursion depth.
pub const MAX_RECURSION: usize = 32;

/// The maximum number of handles allowed in a FIDL message. Note that this
/// number is one less for large messages for the time being. See
/// (fxbug.dev/117162) for progress, or to report problems caused by this
/// specific limitation.
pub const MAX_HANDLES: usize = 64;

/// Indicates that an optional value is present.
pub const ALLOC_PRESENT_U64: u64 = u64::MAX;
/// Indicates that an optional value is present.
pub const ALLOC_PRESENT_U32: u32 = u32::MAX;
/// Indicates that an optional value is absent.
pub const ALLOC_ABSENT_U64: u64 = 0;
/// Indicates that an optional value is absent.
pub const ALLOC_ABSENT_U32: u32 = 0;

/// Special ordinal signifying an epitaph message.
pub const EPITAPH_ORDINAL: u64 = 0xffffffffffffffffu64;

/// The current wire format magic number
pub const MAGIC_NUMBER_INITIAL: u8 = 1;

////////////////////////////////////////////////////////////////////////////////
// Helper functions
////////////////////////////////////////////////////////////////////////////////

/// Rounds `x` up if necessary so that it is a multiple of `align`.
///
/// Requires `align` to be a (nonzero) power of two.
#[doc(hidden)] // only exported for use in macros or generated code
#[inline(always)]
pub fn round_up_to_align(x: usize, align: usize) -> usize {
    debug_assert_ne!(align, 0);
    debug_assert_eq!(align & (align - 1), 0);
    // https://en.wikipedia.org/wiki/Data_structure_alignment#Computing_padding
    (x + align - 1) & !(align - 1)
}

/// Resize a vector without zeroing added bytes.
///
/// The type `T` must be `Copy`. This is not enforced in the type signature
/// because it is used in generic contexts where verifying this requires looking
/// at control flow. See `decode_vector` for an example.
///
/// # Safety
///
/// This is unsafe when `new_len > old_len` because it leaves new elements at
/// indices `old_len..new_len` uninitialized. The caller must overwrite all the
/// new elements before reading them. "Reading" includes any operation that
/// extends the vector, such as `push`, because this could reallocate the vector
/// and copy the uninitialized bytes.
///
/// FIDL conformance tests are used to validate that there are no uninitialized
/// bytes in the output across a range of types and values.
// TODO(fxbug.dev/124338): Fix safety issues, use MaybeUninit.
#[inline]
unsafe fn resize_vec_no_zeroing<T>(buf: &mut Vec<T>, new_len: usize) {
    if new_len > buf.capacity() {
        buf.reserve(new_len - buf.len());
    }
    // Safety:
    // - `new_len` must be less than or equal to `capacity()`:
    //   The if-statement above guarantees this.
    // - The elements at `old_len..new_len` must be initialized:
    //   They are purposely left uninitialized, making this function unsafe.
    buf.set_len(new_len);
}

/// Helper type for checking encoding/decoding recursion depth.
#[doc(hidden)] // only exported for use in macros or generated code
#[derive(Debug, Copy, Clone)]
#[repr(transparent)]
pub struct Depth(usize);

impl Depth {
    /// Increments the depth, and returns an error if it exceeds the limit.
    #[inline(always)]
    pub fn increment(&mut self) -> Result<()> {
        self.0 += 1;
        if self.0 > MAX_RECURSION {
            return Err(Error::MaxRecursionDepth);
        }
        Ok(())
    }

    /// Decrements the depth.
    #[inline(always)]
    pub fn decrement(&mut self) {
        self.0 -= 1;
    }
}

////////////////////////////////////////////////////////////////////////////////
// Helper macros
////////////////////////////////////////////////////////////////////////////////

/// Given `T: TypeMarker`, expands to a `T::Owned::new_empty` call.
#[doc(hidden)] // only exported for use in macros or generated code
#[macro_export]
macro_rules! new_empty {
    ($ty:ty) => {
        <<$ty as $crate::encoding::TypeMarker>::Owned as $crate::encoding::Decode<$ty>>::new_empty()
    };
}

/// Given `T: TypeMarker`, expands to a `T::Owned::decode` call.
#[doc(hidden)] // only exported for use in macros or generated code
#[macro_export]
macro_rules! decode {
    ($ty:ty, $out_value:expr, $decoder:expr, $offset:expr, $depth:expr) => {
        <<$ty as $crate::encoding::TypeMarker>::Owned as $crate::encoding::Decode<$ty>>::decode(
            $out_value, $decoder, $offset, $depth,
        )
    };
}

////////////////////////////////////////////////////////////////////////////////
// Wire format
////////////////////////////////////////////////////////////////////////////////

/// Wire format version to use during encode / decode.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum WireFormatVersion {
    /// V1 wire format
    V1,
    /// V2 wire format
    /// This includes the following:
    /// - RFC-0113: Efficient envelopes
    /// - RFC-0114: Inlining small values in FIDL envelopes
    V2,
}

/// Context for encoding and decoding.
///
/// This is currently empty. We keep it around to ease the implementation of
/// context-dependent behavior for future migrations.
///
/// WARNING: Do not construct this directly unless you know what you're doing.
/// FIDL uses `Context` to coordinate soft migrations, so improper uses of it
/// could result in ABI breakage.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// Wire format version to use when encoding / decoding.
    pub wire_format_version: WireFormatVersion,
}

impl Context {
    /// Returns the header flags to set when encoding with this context.
    #[inline]
    fn at_rest_flags(&self) -> AtRestFlags {
        match self.wire_format_version {
            WireFormatVersion::V1 => AtRestFlags::empty(),
            WireFormatVersion::V2 => AtRestFlags::USE_V2_WIRE_FORMAT,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// Encoder
////////////////////////////////////////////////////////////////////////////////

/// Encoding state
#[derive(Debug)]
pub struct Encoder<'a> {
    /// Encoding context.
    pub context: Context,

    /// Buffer to write output data into.
    pub buf: &'a mut Vec<u8>,

    /// Buffer to write output handles into.
    handles: &'a mut Vec<HandleDisposition<'static>>,
}

/// The default context for encoding.
#[inline]
fn default_encode_context() -> Context {
    Context { wire_format_version: WireFormatVersion::V2 }
}

/// The default context for persistent encoding.
#[inline]
fn default_persistent_encode_context() -> Context {
    Context { wire_format_version: WireFormatVersion::V2 }
}

impl<'a> Encoder<'a> {
    /// FIDL-encodes `x` into the provided data and handle buffers.
    #[inline]
    pub fn encode<T: TypeMarker>(
        buf: &'a mut Vec<u8>,
        handles: &'a mut Vec<HandleDisposition<'static>>,
        x: impl Encode<T>,
    ) -> Result<()> {
        let context = default_encode_context();
        Self::encode_with_context::<T>(context, buf, handles, x)
    }

    /// FIDL-encodes `x` into the provided data and handle buffers, using the
    /// specified encoding context.
    ///
    /// WARNING: Do not call this directly unless you know what you're doing.
    /// FIDL uses `Context` to coordinate soft migrations, so improper uses of
    /// this function could result in ABI breakage.
    #[inline]
    pub fn encode_with_context<T: TypeMarker>(
        context: Context,
        buf: &'a mut Vec<u8>,
        handles: &'a mut Vec<HandleDisposition<'static>>,
        x: impl Encode<T>,
    ) -> Result<()> {
        fn prepare_for_encoding<'a>(
            context: Context,
            buf: &'a mut Vec<u8>,
            handles: &'a mut Vec<HandleDisposition<'static>>,
            ty_inline_size: usize,
        ) -> Encoder<'a> {
            // An empty response can have size zero.
            // This if statement is needed to not break the padding write below.
            if ty_inline_size != 0 {
                let aligned_inline_size = round_up_to_align(ty_inline_size, 8);
                // Safety: The uninitialized elements are written by `x.encode`,
                // except for the trailing padding which is zeroed below.
                unsafe {
                    resize_vec_no_zeroing(buf, aligned_inline_size);

                    // Zero the last 8 bytes in the block to ensure padding bytes are zero.
                    let padding_ptr = buf.get_unchecked_mut(aligned_inline_size - 8) as *mut u8;
                    (padding_ptr as *mut u64).write_unaligned(0);
                }
            }
            handles.truncate(0);
            Encoder { buf, handles, context }
        }
        let mut encoder = prepare_for_encoding(context, buf, handles, T::inline_size(context));
        // Safety: We reserve `T::inline_size` bytes in `encoder.buf` above.
        unsafe { x.encode(&mut encoder, 0, Depth(0)) }
    }

    /// In debug mode only, asserts that there is enough room in the buffer to
    /// write an object of type `T` at `offset`.
    #[inline(always)]
    pub fn debug_check_bounds<T: TypeMarker>(&self, offset: usize) {
        debug_assert!(offset + T::inline_size(self.context) <= self.buf.len());
    }

    /// Encodes a primitive numeric type.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self.buf` has room for writing
    /// `T::inline_size` bytes as `offset`.
    #[inline(always)]
    pub unsafe fn write_num<T: numeric::Numeric>(&mut self, num: T, offset: usize) {
        debug_assert!(offset + mem::size_of::<T>() <= self.buf.len());
        // Safety: The caller ensures `offset` is valid for writing
        // sizeof(T) bytes. Transmuting to a same-or-wider
        // integer or float pointer is safe because we use `write_unaligned`.
        let ptr = self.buf.get_unchecked_mut(offset) as *mut u8;
        (ptr as *mut T).write_unaligned(num);
    }

    /// Returns an offset for writing `len` out-of-line bytes (must be nonzero).
    /// Takes care of zeroing the padding bytes if `len` is not a multiple of 8.
    /// The caller must also call `depth.increment()?`.
    #[inline]
    pub fn out_of_line_offset(&mut self, len: usize) -> usize {
        debug_assert!(len > 0);
        let new_offset = self.buf.len();
        let padded_len = round_up_to_align(len, 8);
        // Safety: The uninitialized elements are written by `f`, except the
        // trailing padding which is zeroed below.
        unsafe {
            // In order to zero bytes for padding, we assume that at least 8 bytes are in the
            // out-of-line block.
            debug_assert!(padded_len >= 8);

            let new_len = self.buf.len() + padded_len;
            resize_vec_no_zeroing(self.buf, new_len);

            // Zero the last 8 bytes in the block to ensure padding bytes are zero.
            let padding_ptr = self.buf.get_unchecked_mut(new_len - 8) as *mut u8;
            (padding_ptr as *mut u64).write_unaligned(0);
        }
        new_offset
    }

    /// Write padding at the specified offset.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self.buf` has room for writing `len` bytes
    /// as `offset`.
    #[inline(always)]
    pub unsafe fn padding(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        debug_assert!(offset + len <= self.buf.len());
        // Safety:
        // - The caller ensures `offset` is valid for writing `len` bytes.
        // - All u8 pointers are properly aligned.
        ptr::write_bytes(self.buf.as_mut_ptr().add(offset), 0, len);
    }
}

////////////////////////////////////////////////////////////////////////////////
// Decoder
////////////////////////////////////////////////////////////////////////////////

/// Decoding state
#[derive(Debug)]
pub struct Decoder<'a> {
    /// Decoding context.
    pub context: Context,

    /// Buffer from which to read data.
    pub buf: &'a [u8],

    /// Next out of line block in buf.
    next_out_of_line: usize,

    /// Buffer from which to read handles.
    handles: &'a mut [HandleInfo],

    /// Index of the next handle to read from the handle array
    next_handle: usize,
}

impl<'a> Decoder<'a> {
    /// Decodes a value of FIDL type `T` into the Rust type `T::Owned` from the
    /// provided data and handle buffers. Assumes the buffers came from inside a
    /// transaction message wrapped by `header`.
    #[inline]
    pub fn decode_into<T: TypeMarker>(
        header: &TransactionHeader,
        buf: &'a [u8],
        handles: &'a mut [HandleInfo],
        value: &mut T::Owned,
    ) -> Result<()> {
        Self::decode_with_context::<T>(header.decoding_context(), buf, handles, value)
    }

    /// Decodes a value of FIDL type `T` into the Rust type `T::Owned` from the
    /// provided data and handle buffers, using the specified context.
    ///
    /// WARNING: Do not call this directly unless you know what you're doing.
    /// FIDL uses `Context` to coordinate soft migrations, so improper uses of
    /// this function could result in ABI breakage.
    #[inline]
    pub fn decode_with_context<T: TypeMarker>(
        context: Context,
        buf: &'a [u8],
        handles: &'a mut [HandleInfo],
        value: &mut T::Owned,
    ) -> Result<()> {
        let inline_size = T::inline_size(context);
        let next_out_of_line = round_up_to_align(inline_size, 8);
        if next_out_of_line > buf.len() {
            return Err(Error::OutOfRange);
        }
        let mut decoder = Decoder { next_out_of_line, buf, handles, next_handle: 0, context };
        // Safety: buf.len() >= inline_size based on the check above.
        unsafe {
            value.decode(&mut decoder, 0, Depth(0))?;
        }
        // Safety: next_out_of_line <= buf.len() based on the check above.
        unsafe { decoder.post_decoding(inline_size, next_out_of_line) }
    }

    /// Checks for errors after decoding. This is a separate function to reduce
    /// binary bloat.
    ///
    /// # Safety
    ///
    /// Requires `padding_end <= self.buf.len()`.
    unsafe fn post_decoding(&self, padding_start: usize, padding_end: usize) -> Result<()> {
        if self.next_out_of_line < self.buf.len() {
            return Err(Error::ExtraBytes);
        }
        if self.next_handle < self.handles.len() {
            return Err(Error::ExtraHandles);
        }

        let padding = padding_end - padding_start;
        if padding > 0 {
            // Safety:
            // padding_end <= self.buf.len() is guaranteed by the caller.
            let last_u64 = unsafe {
                let last_u64_ptr = self.buf.get_unchecked(padding_end - 8) as *const u8;
                (last_u64_ptr as *const u64).read_unaligned()
            };
            // padding == 0 => mask == 0x0000000000000000
            // padding == 1 => mask == 0xff00000000000000
            // padding == 2 => mask == 0xffff000000000000
            // ...
            let mask = !(!0u64 >> (padding * 8));
            if last_u64 & mask != 0 {
                return Err(self.end_of_block_padding_error(padding_start, padding_end));
            }
        }

        Ok(())
    }

    /// The position of the next out of line block and the end of the current
    /// blocks.
    #[inline(always)]
    pub fn next_out_of_line(&self) -> usize {
        self.next_out_of_line
    }

    /// The number of handles that have not yet been consumed.
    #[inline(always)]
    pub fn remaining_handles(&self) -> usize {
        self.handles.len() - self.next_handle
    }

    /// In debug mode only, asserts that there is enough room in the buffer to
    /// read an object of type `T` at `offset`.
    #[inline(always)]
    pub fn debug_check_bounds<T: TypeMarker>(&self, offset: usize) {
        debug_assert!(offset + T::inline_size(self.context) <= self.buf.len());
    }

    /// Decodes a primitive numeric type. The caller must ensure that `self.buf`
    /// has room for reading `T::inline_size` bytes as `offset`.
    #[inline(always)]
    pub fn read_num<T: numeric::Numeric>(&mut self, offset: usize) -> T {
        debug_assert!(offset + mem::size_of::<T>() <= self.buf.len());
        // Safety: The caller ensures `offset` is valid for reading
        // sizeof(T) bytes. Transmuting to a same-or-wider
        // integer pointer is safe because we use `read_unaligned`.
        unsafe {
            let ptr = self.buf.get_unchecked(offset) as *const u8;
            (ptr as *const T).read_unaligned()
        }
    }

    /// Returns an offset for reading `len` out-of-line bytes. Validates padding
    /// bytes, which must be present if `len` is not a multiple of 8. The caller
    /// must call `self.depth.increment()?` before encoding the out-of-line
    /// object and `self.depth.decrement()` after.
    #[inline(always)]
    pub fn out_of_line_offset(&mut self, len: usize) -> Result<usize> {
        // Compute offsets for out of line block.
        let offset = self.next_out_of_line;
        let aligned_len = round_up_to_align(len, 8);
        self.next_out_of_line += aligned_len;
        if self.next_out_of_line > self.buf.len() {
            return Err(Error::OutOfRange);
        }

        // Validate padding bytes at the end of the block.
        // Safety:
        // - self.next_out_of_line <= self.buf.len() based on the if-statement above.
        // - If `len` is 0, `next_out_of_line` is unchanged and this will read
        //   the prior 8 bytes. This is valid because at least 8 inline bytes
        //   are always read before calling `out_of_line_offset`. The `mask` will
        //   be zero so the check will not fail.
        debug_assert!(self.next_out_of_line >= 8);
        let last_u64 = unsafe {
            let last_u64_ptr = self.buf.get_unchecked(self.next_out_of_line - 8) as *const u8;
            (last_u64_ptr as *const u64).read_unaligned()
        };
        let padding = aligned_len - len;
        // padding == 0 => mask == 0x0000000000000000
        // padding == 1 => mask == 0xff00000000000000
        // padding == 2 => mask == 0xffff000000000000
        // ...
        let mask = !(!0u64 >> (padding * 8));
        if last_u64 & mask != 0 {
            return Err(self.end_of_block_padding_error(offset + len, self.next_out_of_line));
        }

        Ok(offset)
    }

    /// Generates an error for bad padding bytes at the end of a block.
    /// Assumes it is already known that there is a nonzero padding byte.
    fn end_of_block_padding_error(&self, start: usize, end: usize) -> Error {
        for i in start..end {
            if self.buf[i] != 0 {
                return Error::NonZeroPadding { padding_start: start };
            }
        }
        // This should be unreachable because we only call this after finding
        // nonzero padding. Abort instead of panicking to save code size.
        std::process::abort();
    }

    /// Checks that the specified padding bytes are in fact zeroes. Like
    /// `Decode::decode`, the caller is responsible for bounds checks.
    #[inline]
    pub fn check_padding(&self, offset: usize, len: usize) -> Result<()> {
        if len == 0 {
            // Skip body (so it can be optimized out).
            return Ok(());
        }
        debug_assert!(offset + len <= self.buf.len());
        for i in offset..offset + len {
            // Safety: Caller guarantees offset..offset+len is in bounds.
            if unsafe { *self.buf.get_unchecked(i) } != 0 {
                return Err(Error::NonZeroPadding { padding_start: offset });
            }
        }
        Ok(())
    }

    /// Checks the padding of the inline value portion of an envelope. Like
    /// `Decode::decode`, the caller is responsible for bounds checks.
    ///
    /// Note: `check_padding` could be used instead, but doing so leads to long
    /// compilation times which is why this method exists.
    #[inline]
    pub fn check_inline_envelope_padding(
        &self,
        value_offset: usize,
        value_len: usize,
    ) -> Result<()> {
        // Safety: The caller ensures `value_offset` is valid for reading
        // `value_len` bytes.
        let valid_padding = unsafe {
            match value_len {
                1 => {
                    *self.buf.get_unchecked(value_offset + 1) == 0
                        && *self.buf.get_unchecked(value_offset + 2) == 0
                        && *self.buf.get_unchecked(value_offset + 3) == 0
                }
                2 => {
                    *self.buf.get_unchecked(value_offset + 2) == 0
                        && *self.buf.get_unchecked(value_offset + 3) == 0
                }
                3 => *self.buf.get_unchecked(value_offset + 3) == 0,
                4 => true,
                value_len => unreachable!("value_len={}", value_len),
            }
        };
        if valid_padding {
            Ok(())
        } else {
            Err(Error::NonZeroPadding { padding_start: value_offset + value_len })
        }
    }

    /// Take the next handle from the `handles` list.
    #[inline]
    pub fn take_next_handle(
        &mut self,
        expected_object_type: ObjectType,
        expected_rights: Rights,
    ) -> Result<Handle> {
        let Some(next_handle) = self.handles.get_mut(self.next_handle) else {
            return Err(Error::OutOfRange);
        };
        let handle_info = mem::replace(
            next_handle,
            HandleInfo {
                handle: Handle::invalid(),
                object_type: ObjectType::NONE,
                rights: Rights::NONE,
            },
        );
        let handle =
            self.consume_handle_info(handle_info, expected_object_type, expected_rights)?;
        self.next_handle += 1;
        Ok(handle)
    }

    /// Drops the next handle in the handle array.
    #[inline]
    pub fn drop_next_handle(&mut self) -> Result<()> {
        let Some(next_handle) = self.handles.get_mut(self.next_handle) else {
            return Err(Error::OutOfRange);
        };
        drop(mem::replace(
            next_handle,
            HandleInfo {
                handle: Handle::invalid(),
                object_type: ObjectType::NONE,
                rights: Rights::NONE,
            },
        ));
        self.next_handle += 1;
        Ok(())
    }

    fn consume_handle_info(
        &self,
        mut handle_info: HandleInfo,
        expected_object_type: ObjectType,
        expected_rights: Rights,
    ) -> Result<Handle> {
        let received_object_type = handle_info.object_type;
        if expected_object_type != ObjectType::NONE
            && received_object_type != ObjectType::NONE
            && expected_object_type != received_object_type
        {
            return Err(Error::IncorrectHandleSubtype {
                expected: expected_object_type,
                received: received_object_type,
            });
        }

        let received_rights = handle_info.rights;
        if expected_rights != Rights::SAME_RIGHTS
            && received_rights != Rights::SAME_RIGHTS
            && expected_rights != received_rights
        {
            if !received_rights.contains(expected_rights) {
                return Err(Error::MissingExpectedHandleRights {
                    missing_rights: expected_rights - received_rights,
                });
            }
            return match handle_info.handle.replace(expected_rights) {
                Ok(r) => Ok(r),
                Err(status) => Err(Error::HandleReplace(status)),
            };
        }
        Ok(mem::replace(&mut handle_info.handle, Handle::invalid()))
    }
}

////////////////////////////////////////////////////////////////////////////////
// Ambiguous types
////////////////////////////////////////////////////////////////////////////////

/// A fake FIDL type that can encode from and decode into any Rust type. This
/// exists solely to prevent the compiler from inferring `T: TypeMarker`,
/// allowing us to add new generic impls without source breakage. It also
/// improves error messages when no suitable `T: TypeMarker` exists, preventing
/// spurious guesses about what you should do (e.g. implement `HandleBased`).
pub struct Ambiguous1;

/// Like `Ambiguous1`. There needs to be two of these types so that the compiler
/// doesn't infer one of them and generate a call to the panicking methods.
pub struct Ambiguous2;

/// An uninhabited type used as owned and borrowed type for ambiguous markers.
/// Can be replaced by `!` once that is stable.
pub enum AmbiguousNever {}

macro_rules! impl_ambiguous {
    ($ambiguous:ident) => {
        unsafe impl TypeMarker for $ambiguous {
            type Owned = AmbiguousNever;

            fn inline_align(_context: Context) -> usize {
                panic!("reached code for fake ambiguous type");
            }

            fn inline_size(_context: Context) -> usize {
                panic!("reached code for fake ambiguous type");
            }
        }

        impl ValueTypeMarker for $ambiguous {
            type Borrowed<'a> = AmbiguousNever;
            fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
                match *value {}
            }
        }

        impl ResourceTypeMarker for $ambiguous {
            type Borrowed<'a> = AmbiguousNever;
            fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
                match *value {}
            }
        }

        unsafe impl<T> Encode<$ambiguous> for T {
            unsafe fn encode(
                self,
                _encoder: &mut Encoder<'_>,
                _offset: usize,
                _depth: Depth,
            ) -> Result<()> {
                panic!("reached code for fake ambiguous type");
            }
        }

        // TODO(fxbug.dev/118783): impl for `T: 'static` this once user code has
        // migrated off new_empty(), which is meant to be internal.
        impl Decode<$ambiguous> for AmbiguousNever {
            fn new_empty() -> Self {
                panic!("reached code for fake ambiguous type");
            }

            unsafe fn decode(
                &mut self,
                _decoder: &mut Decoder<'_>,
                _offset: usize,
                _depth: Depth,
            ) -> Result<()> {
                match *self {}
            }
        }
    };
}

impl_ambiguous!(Ambiguous1);
impl_ambiguous!(Ambiguous2);

////////////////////////////////////////////////////////////////////////////////
// Empty types
////////////////////////////////////////////////////////////////////////////////

/// A FIDL type representing an empty payload (0 bytes).
pub struct EmptyPayload;

unsafe impl TypeMarker for EmptyPayload {
    type Owned = ();

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        1
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        0
    }
}

unsafe impl Encode<EmptyPayload> for () {
    #[inline(always)]
    unsafe fn encode(
        self,
        _encoder: &mut Encoder<'_>,
        _offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        Ok(())
    }
}

impl Decode<EmptyPayload> for () {
    #[inline(always)]
    fn new_empty() -> Self {}

    #[inline(always)]
    unsafe fn decode(
        &mut self,
        _decoder: &mut Decoder<'_>,
        _offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        Ok(())
    }
}

/// The FIDL type used for an empty success variant in a result union. Result
/// unions occur in two-way methods that are flexible or that use error syntax.
pub struct EmptyStruct;

unsafe impl TypeMarker for EmptyStruct {
    type Owned = ();

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        1
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        1
    }
}

impl ValueTypeMarker for EmptyStruct {
    type Borrowed<'a> = ();
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        *value
    }
}

unsafe impl Encode<EmptyStruct> for () {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<EmptyStruct>(offset);
        encoder.write_num(0u8, offset);
        Ok(())
    }
}

impl Decode<EmptyStruct> for () {
    #[inline(always)]
    fn new_empty() -> Self {}

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<EmptyStruct>(offset);
        match decoder.read_num::<u8>(offset) {
            0 => Ok(()),
            _ => Err(Error::Invalid),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// Primitive types
////////////////////////////////////////////////////////////////////////////////

// Private module to prevent others from implementing `Numeric`.
mod numeric {
    use super::*;

    /// Marker trait for primitive numeric types.
    pub trait Numeric {}

    /// Implements `Numeric`, `TypeMarker`, `ValueTypeMarker`, `Encode`, and
    /// `Decode` for a primitive numeric type (integer or float).
    macro_rules! impl_numeric {
        ($numeric_ty:ty) => {
            impl Numeric for $numeric_ty {}

            unsafe impl TypeMarker for $numeric_ty {
                type Owned = $numeric_ty;

                #[inline(always)]
                fn inline_align(_context: Context) -> usize {
                    mem::align_of::<$numeric_ty>()
                }

                #[inline(always)]
                fn inline_size(_context: Context) -> usize {
                    mem::size_of::<$numeric_ty>()
                }

                #[inline(always)]
                fn encode_is_copy() -> bool {
                    true
                }

                #[inline(always)]
                fn decode_is_copy() -> bool {
                    true
                }
            }

            impl ValueTypeMarker for $numeric_ty {
                type Borrowed<'a> = $numeric_ty;

                #[inline(always)]
                fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
                    *value
                }
            }

            unsafe impl Encode<$numeric_ty> for $numeric_ty {
                #[inline(always)]
                unsafe fn encode(
                    self,
                    encoder: &mut Encoder<'_>,
                    offset: usize,
                    _depth: Depth,
                ) -> Result<()> {
                    encoder.debug_check_bounds::<$numeric_ty>(offset);
                    encoder.write_num::<$numeric_ty>(self, offset);
                    Ok(())
                }
            }

            impl Decode<$numeric_ty> for $numeric_ty {
                #[inline(always)]
                fn new_empty() -> Self {
                    0 as $numeric_ty
                }

                #[inline(always)]
                unsafe fn decode(
                    &mut self,
                    decoder: &mut Decoder<'_>,
                    offset: usize,
                    _depth: Depth,
                ) -> Result<()> {
                    decoder.debug_check_bounds::<$numeric_ty>(offset);
                    *self = decoder.read_num::<$numeric_ty>(offset);
                    Ok(())
                }
            }
        };
    }

    impl_numeric!(u8);
    impl_numeric!(u16);
    impl_numeric!(u32);
    impl_numeric!(u64);
    impl_numeric!(i8);
    impl_numeric!(i16);
    impl_numeric!(i32);
    impl_numeric!(i64);
    impl_numeric!(f32);
    impl_numeric!(f64);
}

unsafe impl TypeMarker for bool {
    type Owned = bool;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        mem::align_of::<bool>()
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        mem::size_of::<bool>()
    }

    #[inline(always)]
    fn encode_is_copy() -> bool {
        // Rust guarantees a bool is 0x00 or 0x01.
        // https://doc.rust-lang.org/reference/types/boolean.html
        true
    }

    #[inline(always)]
    fn decode_is_copy() -> bool {
        // Decoding isn't just a copy because we have to ensure it's 0x00 or 0x01.
        false
    }
}

impl ValueTypeMarker for bool {
    type Borrowed<'a> = bool;

    #[inline(always)]
    fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
        *value
    }
}

unsafe impl Encode<bool> for bool {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<bool>(offset);
        // From https://doc.rust-lang.org/std/primitive.bool.html: "If you
        // cast a bool into an integer, true will be 1 and false will be 0."
        encoder.write_num(self as u8, offset);
        Ok(())
    }
}

impl Decode<bool> for bool {
    #[inline(always)]
    fn new_empty() -> Self {
        false
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<bool>(offset);
        // Safety: The caller ensures `offset` is valid for reading 1 byte.
        *self = match unsafe { *decoder.buf.get_unchecked(offset) } {
            0 => false,
            1 => true,
            _ => return Err(Error::InvalidBoolean),
        };
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////
// Arrays
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type `array<T, N>`.
pub struct Array<T: TypeMarker, const N: usize>(PhantomData<T>);

unsafe impl<T: TypeMarker, const N: usize> TypeMarker for Array<T, N> {
    type Owned = [T::Owned; N];

    #[inline(always)]
    fn inline_align(context: Context) -> usize {
        T::inline_align(context)
    }

    #[inline(always)]
    fn inline_size(context: Context) -> usize {
        N * T::inline_size(context)
    }

    #[inline(always)]
    fn encode_is_copy() -> bool {
        T::encode_is_copy()
    }

    #[inline(always)]
    fn decode_is_copy() -> bool {
        T::decode_is_copy()
    }
}

impl<T: ValueTypeMarker, const N: usize> ValueTypeMarker for Array<T, N> {
    type Borrowed<'a> = &'a [T::Owned; N];
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value
    }
}

impl<T: ResourceTypeMarker, const N: usize> ResourceTypeMarker for Array<T, N> {
    type Borrowed<'a> = &'a mut [T::Owned; N];
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl<'a, T: ValueTypeMarker, const N: usize> Encode<Array<T, N>> for &'a [T::Owned; N] {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Array<T, N>>(offset);
        encode_array_value::<T>(self, encoder, offset, depth)
    }
}

unsafe impl<'a, T: ResourceTypeMarker, const N: usize> Encode<Array<T, N>>
    for &'a mut [T::Owned; N]
{
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Array<T, N>>(offset);
        encode_array_resource::<T>(self, encoder, offset, depth)
    }
}

impl<T: TypeMarker, const N: usize> Decode<Array<T, N>> for [T::Owned; N] {
    #[inline]
    fn new_empty() -> Self {
        let mut arr = mem::MaybeUninit::<[T::Owned; N]>::uninit();
        unsafe {
            let arr_ptr = arr.as_mut_ptr() as *mut T::Owned;
            for i in 0..N {
                ptr::write(arr_ptr.add(i), T::Owned::new_empty());
            }
            arr.assume_init()
        }
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Array<T, N>>(offset);
        decode_array::<T>(self, decoder, offset, depth)
    }
}

#[inline]
unsafe fn encode_array_value<T: ValueTypeMarker>(
    slice: &[T::Owned],
    encoder: &mut Encoder<'_>,
    offset: usize,
    depth: Depth,
) -> Result<()> {
    let stride = T::inline_size(encoder.context);
    let len = slice.len();
    if T::encode_is_copy() {
        debug_assert_eq!(stride, mem::size_of::<T::Owned>());
        // Safety:
        // - The caller ensures `offset` if valid for writing `stride` bytes
        //   (inline size of `T`) `len` times, i.e. `len * stride`.
        // - Since T::inline_size is the same as mem::size_of for simple
        //   copy types, `slice` also has exactly `len * stride` bytes.
        // - Rust guarantees `slice` and `encoder.buf` do not alias.
        unsafe {
            let src = slice.as_ptr() as *const u8;
            let dst: *mut u8 = encoder.buf.get_unchecked_mut(offset);
            ptr::copy_nonoverlapping(src, dst, len * stride);
        }
    } else {
        for i in 0..len {
            // Safety: `i` is in bounds since `len` is defined as `slice.len()`.
            let item = unsafe { slice.get_unchecked(i) };
            T::borrow(item).encode(encoder, offset + i * stride, depth)?;
        }
    }
    Ok(())
}

#[inline]
unsafe fn encode_array_resource<T: ResourceTypeMarker>(
    slice: &mut [T::Owned],
    encoder: &mut Encoder<'_>,
    offset: usize,
    depth: Depth,
) -> Result<()> {
    let stride = T::inline_size(encoder.context);
    let len = slice.len();
    if T::encode_is_copy() {
        debug_assert_eq!(stride, mem::size_of::<T::Owned>());
        // Safety:
        // - The caller ensures `offset` if valid for writing `stride` bytes
        //   (inline size of `T`) `len` times, i.e. `len * stride`.
        // - Since T::inline_size is the same as mem::size_of for simple
        //   copy types, `slice` also has exactly `len * stride` bytes.
        // - Rust guarantees `slice` and `encoder.buf` do not alias.
        unsafe {
            let src = slice.as_ptr() as *const u8;
            let dst: *mut u8 = encoder.buf.get_unchecked_mut(offset);
            ptr::copy_nonoverlapping(src, dst, len * stride);
        }
    } else {
        for i in 0..len {
            // Safety: `i` is in bounds since `len` is defined as `slice.len()`.
            let item = unsafe { slice.get_unchecked_mut(i) };
            T::take_or_borrow(item).encode(encoder, offset + i * stride, depth)?;
        }
    }
    Ok(())
}

#[inline]
unsafe fn decode_array<T: TypeMarker>(
    slice: &mut [T::Owned],
    decoder: &mut Decoder<'_>,
    offset: usize,
    depth: Depth,
) -> Result<()> {
    let stride = T::inline_size(decoder.context);
    let len = slice.len();
    if T::decode_is_copy() {
        debug_assert_eq!(stride, mem::size_of::<T::Owned>());
        // Safety:
        // - The caller ensures `offset` if valid for reading `stride` bytes
        //   (inline size of `T`) `len` times, i.e. `len * stride`.
        // - Since T::inline_size is the same as mem::size_of for simple copy
        //   types, `slice` also has exactly `len * stride` bytes.
        // - Rust guarantees `slice` and `decoder.buf` do not alias.
        unsafe {
            let src: *const u8 = decoder.buf.get_unchecked(offset);
            let dst = slice.as_mut_ptr() as *mut u8;
            ptr::copy_nonoverlapping(src, dst, len * stride);
        }
    } else {
        for i in 0..len {
            // Safety: `i` is in bounds since `len` is defined as `slice.len()`.
            let item = unsafe { slice.get_unchecked_mut(i) };
            item.decode(decoder, offset + i * stride, depth)?;
        }
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// Vectors
////////////////////////////////////////////////////////////////////////////////

/// The maximum vector bound, corresponding to the `MAX` constraint in FIDL.
pub const MAX_BOUND: usize = usize::MAX;

/// The FIDL type `vector<T>:N`.
pub struct Vector<T: TypeMarker, const N: usize>(PhantomData<T>);

/// The FIDL type `vector<T>` or `vector<T>:MAX`.
pub type UnboundedVector<T> = Vector<T, MAX_BOUND>;

unsafe impl<T: TypeMarker, const N: usize> TypeMarker for Vector<T, N> {
    type Owned = Vec<T::Owned>;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        8
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        16
    }
}

impl<T: ValueTypeMarker, const N: usize> ValueTypeMarker for Vector<T, N> {
    type Borrowed<'a> = &'a [T::Owned];
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value
    }
}

impl<T: ResourceTypeMarker, const N: usize> ResourceTypeMarker for Vector<T, N> {
    type Borrowed<'a> = &'a mut [T::Owned];
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl<'a, T: ValueTypeMarker, const N: usize> Encode<Vector<T, N>> for &'a [T::Owned] {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Vector<T, N>>(offset);
        encode_vector_value::<T>(self, N, check_vector_length, encoder, offset, depth)
    }
}

unsafe impl<'a, T: ResourceTypeMarker, const N: usize> Encode<Vector<T, N>>
    for &'a mut [T::Owned]
{
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Vector<T, N>>(offset);
        encode_vector_resource::<T>(self, N, encoder, offset, depth)
    }
}

impl<T: TypeMarker, const N: usize> Decode<Vector<T, N>> for Vec<T::Owned> {
    #[inline(always)]
    fn new_empty() -> Self {
        Vec::new()
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Vector<T, N>>(offset);
        decode_vector::<T>(self, N, decoder, offset, depth)
    }
}

#[inline]
unsafe fn encode_vector_value<T: ValueTypeMarker>(
    slice: &[T::Owned],
    max_length: usize,
    check_length: impl Fn(usize, usize) -> Result<()>,
    encoder: &mut Encoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    encoder.write_num(slice.len() as u64, offset);
    encoder.write_num(ALLOC_PRESENT_U64, offset + 8);
    // write_out_of_line must not be called with a zero-sized out-of-line block.
    if slice.is_empty() {
        return Ok(());
    }
    check_length(slice.len(), max_length)?;
    depth.increment()?;
    let bytes_len = slice.len() * T::inline_size(encoder.context);
    let offset = encoder.out_of_line_offset(bytes_len);
    encode_array_value::<T>(slice, encoder, offset, depth)
}

#[inline]
unsafe fn encode_vector_resource<T: ResourceTypeMarker>(
    slice: &mut [T::Owned],
    max_length: usize,
    encoder: &mut Encoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    encoder.write_num(slice.len() as u64, offset);
    encoder.write_num(ALLOC_PRESENT_U64, offset + 8);
    // write_out_of_line must not be called with a zero-sized out-of-line block.
    if slice.is_empty() {
        return Ok(());
    }
    check_vector_length(slice.len(), max_length)?;
    depth.increment()?;
    let bytes_len = slice.len() * T::inline_size(encoder.context);
    let offset = encoder.out_of_line_offset(bytes_len);
    encode_array_resource::<T>(slice, encoder, offset, depth)
}

#[inline]
unsafe fn decode_vector<T: TypeMarker>(
    vec: &mut Vec<T::Owned>,
    max_length: usize,
    decoder: &mut Decoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    let Some(len) = decode_vector_header(decoder, offset)? else {
        return Err(Error::NotNullable);
    };
    check_vector_length(len, max_length)?;
    depth.increment()?;
    let bytes_len = len * T::inline_size(decoder.context);
    let offset = decoder.out_of_line_offset(bytes_len)?;
    if T::decode_is_copy() {
        // Safety: The uninitialized elements are immediately written by
        // `decode_array`, which always succeeds in the simple copy case.
        unsafe {
            resize_vec_no_zeroing(vec, len);
        }
    } else {
        vec.resize_with(len, T::Owned::new_empty);
    }
    // Safety: `vec` has `len` elements based on the above code.
    decode_array::<T>(vec, decoder, offset, depth)?;
    Ok(())
}

/// Decodes and validates a 16-byte vector header. Returns `Some(len)` if
/// the vector is present (including empty vectors), otherwise `None`.
#[doc(hidden)] // only exported for use in macros or generated code
#[inline]
pub fn decode_vector_header(decoder: &mut Decoder<'_>, offset: usize) -> Result<Option<usize>> {
    let len = decoder.read_num::<u64>(offset) as usize;
    match decoder.read_num::<u64>(offset + 8) {
        ALLOC_PRESENT_U64 => {
            // Check that the length does not exceed `u32::MAX` (per RFC-0059)
            // nor the total size of the message (to avoid a huge allocation
            // when the message cannot possibly be valid).
            if len <= u32::MAX as usize && len <= decoder.buf.len() {
                Ok(Some(len))
            } else {
                Err(Error::OutOfRange)
            }
        }
        ALLOC_ABSENT_U64 => {
            if len == 0 {
                Ok(None)
            } else {
                Err(Error::UnexpectedNullRef)
            }
        }
        _ => Err(Error::InvalidPresenceIndicator),
    }
}

#[inline(always)]
fn check_vector_length(actual_length: usize, max_length: usize) -> Result<()> {
    if actual_length > max_length {
        return Err(Error::VectorTooLong { max_length, actual_length });
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// Strings
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type `string:N`.
pub struct BoundedString<const N: usize>;

/// The FIDL type `string` or `string:MAX`.
pub type UnboundedString = BoundedString<MAX_BOUND>;

unsafe impl<const N: usize> TypeMarker for BoundedString<N> {
    type Owned = String;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        8
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        16
    }
}

impl<const N: usize> ValueTypeMarker for BoundedString<N> {
    type Borrowed<'a> = &'a str;
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl<'a, const N: usize> Encode<BoundedString<N>> for &'a str {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<BoundedString<N>>(offset);
        encode_vector_value::<u8>(self.as_bytes(), N, check_string_length, encoder, offset, depth)
    }
}

impl<const N: usize> Decode<BoundedString<N>> for String {
    #[inline(always)]
    fn new_empty() -> Self {
        String::new()
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<BoundedString<N>>(offset);
        decode_string(self, N, decoder, offset, depth)
    }
}

#[inline]
fn decode_string(
    string: &mut String,
    max_length: usize,
    decoder: &mut Decoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    let Some(len) = decode_vector_header(decoder, offset)? else {
        return Err(Error::NotNullable);
    };
    check_string_length(len, max_length)?;
    depth.increment()?;
    let offset = decoder.out_of_line_offset(len)?;
    // Safety: `out_of_line_offset` does this bounds check.
    let bytes = unsafe { &decoder.buf.get_unchecked(offset..offset + len) };
    let utf8 = str::from_utf8(bytes).map_err(|_| Error::Utf8Error)?;
    let boxed_utf8: Box<str> = utf8.into();
    *string = boxed_utf8.into_string();
    Ok(())
}

#[inline(always)]
fn check_string_length(actual_bytes: usize, max_bytes: usize) -> Result<()> {
    if actual_bytes > max_bytes {
        return Err(Error::StringTooLong { max_bytes, actual_bytes });
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// Handles
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type `zx.Handle:<OBJECT_TYPE, RIGHTS>`, or a `client_end` or `server_end`.
pub struct HandleType<T: HandleBased, const OBJECT_TYPE: u32, const RIGHTS: u32>(PhantomData<T>);

/// An abbreviation of `HandleType` that for channels with default rights, used
/// for the FIDL types `client_end:P` and `server_end:P`.
pub type Endpoint<T> = HandleType<
    T,
    { crate::ObjectType::CHANNEL.into_raw() },
    { crate::Rights::CHANNEL_DEFAULT.bits() },
>;

unsafe impl<T: 'static + HandleBased, const OBJECT_TYPE: u32, const RIGHTS: u32> TypeMarker
    for HandleType<T, OBJECT_TYPE, RIGHTS>
{
    type Owned = T;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        4
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        4
    }
}

impl<T: 'static + HandleBased, const OBJECT_TYPE: u32, const RIGHTS: u32> ResourceTypeMarker
    for HandleType<T, OBJECT_TYPE, RIGHTS>
{
    type Borrowed<'a> = T;
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        mem::replace(value, Handle::invalid().into())
    }
}

unsafe impl<T: 'static + HandleBased, const OBJECT_TYPE: u32, const RIGHTS: u32>
    Encode<HandleType<T, OBJECT_TYPE, RIGHTS>> for T
{
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<HandleType<T, OBJECT_TYPE, RIGHTS>>(offset);
        encode_handle(
            self.into(),
            ObjectType::from_raw(OBJECT_TYPE),
            // Safety: bitflags does not require valid bits for safety. This
            // function is just marked unsafe for aesthetic reasons.
            // TODO(fxbug.dev/124335): Use `from_bits_retain` instead.
            unsafe { Rights::from_bits_unchecked(RIGHTS) },
            encoder,
            offset,
        )
    }
}

impl<T: 'static + HandleBased, const OBJECT_TYPE: u32, const RIGHTS: u32>
    Decode<HandleType<T, OBJECT_TYPE, RIGHTS>> for T
{
    #[inline(always)]
    fn new_empty() -> Self {
        Handle::invalid().into()
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<HandleType<T, OBJECT_TYPE, RIGHTS>>(offset);
        *self = decode_handle(
            ObjectType::from_raw(OBJECT_TYPE),
            // Safety: bitflags does not require valid bits for safety. This
            // function is just marked unsafe for aesthetic reasons.
            // TODO(fxbug.dev/124335): Use `from_bits_retain` instead.
            unsafe { Rights::from_bits_unchecked(RIGHTS) },
            decoder,
            offset,
        )?
        .into();
        Ok(())
    }
}

#[inline]
unsafe fn encode_handle(
    handle: Handle,
    object_type: ObjectType,
    rights: Rights,
    encoder: &mut Encoder<'_>,
    offset: usize,
) -> Result<()> {
    if handle.is_invalid() {
        return Err(Error::NotNullable);
    }
    encoder.write_num(ALLOC_PRESENT_U32, offset);
    encoder.handles.push(HandleDisposition {
        handle_op: HandleOp::Move(handle),
        object_type,
        rights,
        result: Status::OK,
    });
    Ok(())
}

#[inline]
unsafe fn decode_handle(
    object_type: ObjectType,
    rights: Rights,
    decoder: &mut Decoder<'_>,
    offset: usize,
) -> Result<Handle> {
    match decoder.read_num::<u32>(offset) {
        ALLOC_PRESENT_U32 => {}
        ALLOC_ABSENT_U32 => return Err(Error::NotNullable),
        _ => return Err(Error::InvalidPresenceIndicator),
    }
    decoder.take_next_handle(object_type, rights)
}

////////////////////////////////////////////////////////////////////////////////
// Optionals
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type `T:optional` where `T` is a vector, string, handle, or client/server end.
pub struct Optional<T: TypeMarker>(PhantomData<T>);

/// The FIDL type `T:optional` where `T` is a union.
pub struct OptionalUnion<T: TypeMarker>(PhantomData<T>);

/// The FIDL type `box<T>`.
pub struct Boxed<T: TypeMarker>(PhantomData<T>);

unsafe impl<T: TypeMarker> TypeMarker for Optional<T> {
    type Owned = Option<T::Owned>;

    #[inline(always)]
    fn inline_align(context: Context) -> usize {
        T::inline_align(context)
    }

    #[inline(always)]
    fn inline_size(context: Context) -> usize {
        T::inline_size(context)
    }
}

unsafe impl<T: TypeMarker> TypeMarker for OptionalUnion<T> {
    type Owned = Option<Box<T::Owned>>;

    #[inline(always)]
    fn inline_align(context: Context) -> usize {
        T::inline_align(context)
    }

    #[inline(always)]
    fn inline_size(context: Context) -> usize {
        T::inline_size(context)
    }
}

unsafe impl<T: TypeMarker> TypeMarker for Boxed<T> {
    type Owned = Option<Box<T::Owned>>;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        8
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        8
    }
}

impl<T: ValueTypeMarker> ValueTypeMarker for Optional<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value.as_ref().map(T::borrow)
    }
}

impl<T: ValueTypeMarker> ValueTypeMarker for OptionalUnion<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value.as_deref().map(T::borrow)
    }
}

impl<T: ValueTypeMarker> ValueTypeMarker for Boxed<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn borrow(value: &Self::Owned) -> Self::Borrowed<'_> {
        value.as_deref().map(T::borrow)
    }
}

impl<T: ResourceTypeMarker> ResourceTypeMarker for Optional<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        value.as_mut().map(T::take_or_borrow)
    }
}

impl<T: ResourceTypeMarker> ResourceTypeMarker for OptionalUnion<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        value.as_deref_mut().map(T::take_or_borrow)
    }
}

impl<T: ResourceTypeMarker> ResourceTypeMarker for Boxed<T> {
    type Borrowed<'a> = Option<T::Borrowed<'a>>;
    #[inline(always)]
    fn take_or_borrow(value: &mut Self::Owned) -> Self::Borrowed<'_> {
        value.as_deref_mut().map(T::take_or_borrow)
    }
}

unsafe impl<T: TypeMarker, E: Encode<T>> Encode<Optional<T>> for Option<E> {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Optional<T>>(offset);
        encode_naturally_optional::<T, E>(self, encoder, offset, depth)
    }
}

unsafe impl<T: TypeMarker, E: Encode<T>> Encode<OptionalUnion<T>> for Option<E> {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<OptionalUnion<T>>(offset);
        encode_naturally_optional::<T, E>(self, encoder, offset, depth)
    }
}

unsafe impl<T: TypeMarker, E: Encode<T>> Encode<Boxed<T>> for Option<E> {
    #[inline]
    unsafe fn encode(
        self,
        encoder: &mut Encoder<'_>,
        offset: usize,
        mut depth: Depth,
    ) -> Result<()> {
        encoder.debug_check_bounds::<Boxed<T>>(offset);
        match self {
            Some(val) => {
                depth.increment()?;
                encoder.write_num(ALLOC_PRESENT_U64, offset);
                let offset = encoder.out_of_line_offset(T::inline_size(encoder.context));
                val.encode(encoder, offset, depth)?;
            }
            None => encoder.write_num(ALLOC_ABSENT_U64, offset),
        }
        Ok(())
    }
}

impl<T: TypeMarker> Decode<Optional<T>> for Option<T::Owned> {
    #[inline(always)]
    fn new_empty() -> Self {
        None
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Optional<T>>(offset);
        let inline_size = T::inline_size(decoder.context);
        if check_for_presence(decoder, offset, inline_size) {
            self.get_or_insert(T::Owned::new_empty()).decode(decoder, offset, depth)
        } else {
            *self = None;
            decoder.check_padding(offset, inline_size)?;
            Ok(())
        }
    }
}

impl<T: TypeMarker> Decode<OptionalUnion<T>> for Option<Box<T::Owned>> {
    #[inline(always)]
    fn new_empty() -> Self {
        None
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<OptionalUnion<T>>(offset);
        let inline_size = T::inline_size(decoder.context);
        if check_for_presence(decoder, offset, inline_size) {
            decode!(
                T,
                self.get_or_insert_with(|| Box::new(T::Owned::new_empty())),
                decoder,
                offset,
                depth
            )
        } else {
            *self = None;
            decoder.check_padding(offset, inline_size)?;
            Ok(())
        }
    }
}

impl<T: TypeMarker> Decode<Boxed<T>> for Option<Box<T::Owned>> {
    #[inline(always)]
    fn new_empty() -> Self {
        None
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        mut depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Boxed<T>>(offset);
        match decoder.read_num::<u64>(offset) {
            ALLOC_PRESENT_U64 => {
                depth.increment()?;
                let offset = decoder.out_of_line_offset(T::inline_size(decoder.context))?;
                decode!(
                    T,
                    self.get_or_insert_with(|| Box::new(T::Owned::new_empty())),
                    decoder,
                    offset,
                    depth
                )?;
                Ok(())
            }
            ALLOC_ABSENT_U64 => {
                *self = None;
                Ok(())
            }
            _ => Err(Error::InvalidPresenceIndicator),
        }
    }
}

/// Encodes a "naturally optional" value, i.e. one where absence is represented
/// by a run of 0x00 bytes matching the type's inline size.
#[inline]
unsafe fn encode_naturally_optional<T: TypeMarker, E: Encode<T>>(
    value: Option<E>,
    encoder: &mut Encoder<'_>,
    offset: usize,
    depth: Depth,
) -> Result<()> {
    match value {
        Some(val) => val.encode(encoder, offset, depth)?,
        None => encoder.padding(offset, T::inline_size(encoder.context)),
    }
    Ok(())
}

/// Presence indicators always include at least one non-zero byte, while absence
/// indicators should always be entirely zeros. Like `Decode::decode`, the
/// caller is responsible for bounds checks.
#[inline]
fn check_for_presence(decoder: &mut Decoder<'_>, offset: usize, inline_size: usize) -> bool {
    debug_assert!(offset + inline_size <= decoder.buf.len());
    let range = unsafe { decoder.buf.get_unchecked(offset..offset + inline_size) };
    range.iter().any(|byte| *byte != 0)
}

////////////////////////////////////////////////////////////////////////////////
// Envelopes
////////////////////////////////////////////////////////////////////////////////

#[doc(hidden)] // only exported for use in macros or generated code
#[inline]
pub unsafe fn encode_in_envelope<T: TypeMarker>(
    val: impl Encode<T>,
    encoder: &mut Encoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    depth.increment()?;
    let bytes_before = encoder.buf.len();
    let handles_before = encoder.handles.len();
    let (inner_offset, finish) = prepare_envelope(T::inline_size(encoder.context), encoder, offset);
    val.encode(encoder, inner_offset, depth)?;
    finish(encoder, offset, bytes_before, handles_before);
    Ok(())
}

#[doc(hidden)] // only exported for use in macros or generated code
#[inline]
pub unsafe fn encode_in_envelope_optional<T: TypeMarker>(
    val: Option<impl Encode<T>>,
    encoder: &mut Encoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    let bytes_before = encoder.buf.len();
    let handles_before = encoder.handles.len();
    let Some((inner_offset, finish)) = prepare_envelope_optional(
        val.is_some(),
        T::inline_size(encoder.context),
        encoder,
        offset,
    ) else {
        return Ok(());
    };
    depth.increment()?;
    unsafe { val.unwrap_unchecked() }.encode(encoder, inner_offset, depth)?;
    finish(encoder, offset, bytes_before, handles_before);
    Ok(())
}

/// Helper for encoding a value in an envelope. Returns the offset where the
/// value should be written, and a function to be called afterwards.
#[inline]
unsafe fn prepare_envelope(
    inline_size: usize,
    encoder: &mut Encoder<'_>,
    offset: usize,
) -> (usize, FinishEnvelopeFn) {
    let v1 = match encoder.context.wire_format_version {
        WireFormatVersion::V1 => true,
        WireFormatVersion::V2 => false,
    };
    if v1 || inline_size > 4 {
        encoder.write_num(0u64, offset);
        if v1 {
            encoder.write_num(ALLOC_PRESENT_U64, offset + 8);
        }
        (encoder.out_of_line_offset(inline_size), finish_out_of_line_envelope)
    } else {
        // This simultaneously zeroes out the first 4 bytes and writes the flag
        // byte indicating the envelope is inlined (1u16 at offset + 6).
        encoder.write_num(1u64 << 48, offset);
        (offset, finish_inlined_envelope)
    }
}

#[inline]
unsafe fn prepare_envelope_optional(
    present: bool,
    inline_size: usize,
    encoder: &mut Encoder<'_>,
    offset: usize,
) -> Option<(usize, FinishEnvelopeFn)> {
    if present {
        Some(prepare_envelope(inline_size, encoder, offset))
    } else {
        encode_absent_envelope(encoder, offset);
        None
    }
}

type FinishEnvelopeFn =
    unsafe fn(encoder: &mut Encoder<'_>, offset: usize, bytes_before: usize, handles_before: usize);

unsafe fn finish_out_of_line_envelope(
    encoder: &mut Encoder<'_>,
    offset: usize,
    bytes_before: usize,
    handles_before: usize,
) {
    let bytes_written = (encoder.buf.len() - bytes_before) as u32;
    let handles_written = (encoder.handles.len() - handles_before) as u32;
    debug_assert!(bytes_written % 8 == 0);
    encoder.write_num(bytes_written, offset);
    encoder.write_num(handles_written, offset + 4);
}

unsafe fn finish_inlined_envelope(
    encoder: &mut Encoder<'_>,
    offset: usize,
    _bytes_before: usize,
    handles_before: usize,
) {
    let handles_written = (encoder.handles.len() - handles_before) as u16;
    encoder.write_num(handles_written, offset + 4);
}

#[inline]
unsafe fn encode_absent_envelope(encoder: &mut Encoder<'_>, offset: usize) {
    encoder.write_num(0u64, offset);
    match encoder.context.wire_format_version {
        WireFormatVersion::V1 => encoder.write_num(ALLOC_ABSENT_U64, offset + 8),
        WireFormatVersion::V2 => {}
    }
}

/// Decodes and validates an envelope header. Returns `None` if absent and
/// `Some((inlined, num_bytes, num_handles))` if present.
#[doc(hidden)] // only exported for use in macros or generated code
#[inline(always)]
pub unsafe fn decode_envelope_header(
    decoder: &mut Decoder<'_>,
    offset: usize,
) -> Result<Option<(bool, u32, u32)>> {
    let mut num_bytes: u32 = decoder.read_num::<u32>(offset);
    let num_handles: u32;
    let inlined: bool;
    let is_present: bool;

    match decoder.context.wire_format_version {
        WireFormatVersion::V1 => {
            inlined = false;
            num_handles = decoder.read_num::<u32>(offset + 4);
            is_present = match decoder.read_num::<u64>(offset + 8) {
                ALLOC_PRESENT_U64 => true,
                ALLOC_ABSENT_U64 => false,
                _ => return Err(Error::InvalidPresenceIndicator),
            };
        }
        WireFormatVersion::V2 => {
            num_handles = decoder.read_num::<u16>(offset + 4) as u32;
            inlined = match decoder.read_num::<u16>(offset + 6) {
                0 => false,
                1 => true,
                _ => return Err(Error::InvalidInlineMarkerInEnvelope),
            };
            if inlined {
                num_bytes = 4;
            }
            is_present = num_bytes != 0 || num_handles != 0;
        }
    }

    if is_present {
        if !inlined && num_bytes % 8 != 0 {
            Err(Error::InvalidNumBytesInEnvelope)
        } else {
            Ok(Some((inlined, num_bytes, num_handles)))
        }
    } else if num_bytes != 0 {
        Err(Error::InvalidNumBytesInEnvelope)
    } else if num_handles != 0 {
        Err(Error::InvalidNumHandlesInEnvelope)
    } else {
        Ok(None)
    }
}

/// Decodes a FIDL envelope and skips over any out-of-line bytes and handles.
#[doc(hidden)] // only exported for use in macros or generated code
#[inline]
pub unsafe fn decode_unknown_envelope(
    decoder: &mut Decoder<'_>,
    offset: usize,
    mut depth: Depth,
) -> Result<()> {
    if let Some((inlined, num_bytes, num_handles)) = decode_envelope_header(decoder, offset)? {
        if !inlined {
            depth.increment()?;
            let _ = decoder.out_of_line_offset(num_bytes as usize)?;
        }
        if num_handles != 0 {
            for _ in 0..num_handles {
                decoder.drop_next_handle()?;
            }
        }
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// Unions
////////////////////////////////////////////////////////////////////////////////

/// Decodes the inline portion of a union.
/// Returns `(ordinal, inlined, num_bytes, num_handles)`.
#[doc(hidden)] // only exported for use in macros or generated code
#[inline]
pub unsafe fn decode_union_inline_portion(
    decoder: &mut Decoder<'_>,
    offset: usize,
) -> Result<(u64, bool, u32, u32)> {
    let ordinal = decoder.read_num::<u64>(offset);
    match decode_envelope_header(decoder, offset + 8)? {
        Some((inlined, num_bytes, num_handles)) => Ok((ordinal, inlined, num_bytes, num_handles)),
        None => Err(Error::NotNullable),
    }
}

////////////////////////////////////////////////////////////////////////////////
// Result unions
////////////////////////////////////////////////////////////////////////////////

/// The FIDL union generated for strict two-way methods with errors.
pub struct ResultType<T: TypeMarker, E: TypeMarker>(PhantomData<(T, E)>);

/// The FIDL union generated for flexible two-way methods without errors.
pub struct FlexibleType<T: TypeMarker>(PhantomData<T>);

/// The FIDL union generated for flexible two-way methods with errors.
pub struct FlexibleResultType<T: TypeMarker, E: TypeMarker>(PhantomData<(T, E)>);

/// Owned type for `FlexibleType`.
#[doc(hidden)] // only exported for use in macros or generated code
#[derive(Debug)]
pub enum Flexible<T> {
    Ok(T),
    FrameworkErr(FrameworkErr),
}

/// Owned type for `FlexibleResultType`.
#[doc(hidden)] // only exported for use in macros or generated code
#[derive(Debug)]
pub enum FlexibleResult<T, E> {
    Ok(T),
    DomainErr(E),
    FrameworkErr(FrameworkErr),
}

/// Internal FIDL framework error type used to identify unknown methods.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(i32)]
pub enum FrameworkErr {
    /// Method was not recognized.
    UnknownMethod = zx_types::ZX_ERR_NOT_SUPPORTED,
}

impl FrameworkErr {
    #[inline]
    fn from_primitive(prim: i32) -> Option<Self> {
        match prim {
            zx_types::ZX_ERR_NOT_SUPPORTED => Some(Self::UnknownMethod),
            _ => None,
        }
    }

    #[inline(always)]
    const fn into_primitive(self) -> i32 {
        self as i32
    }
}

unsafe impl TypeMarker for FrameworkErr {
    type Owned = Self;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        std::mem::align_of::<i32>()
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        std::mem::size_of::<i32>()
    }

    #[inline(always)]
    fn encode_is_copy() -> bool {
        true
    }

    #[inline(always)]
    fn decode_is_copy() -> bool {
        false
    }
}

impl ValueTypeMarker for FrameworkErr {
    type Borrowed<'a> = Self;
    #[inline(always)]
    fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
        *value
    }
}

unsafe impl Encode<Self> for FrameworkErr {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<Self>(offset);
        encoder.write_num(self.into_primitive(), offset);
        Ok(())
    }
}

impl Decode<Self> for FrameworkErr {
    #[inline(always)]
    fn new_empty() -> Self {
        Self::UnknownMethod
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Self>(offset);
        let prim = decoder.read_num::<i32>(offset);
        *self = Self::from_primitive(prim).ok_or(Error::InvalidEnumValue)?;
        Ok(())
    }
}

impl<T> Flexible<T> {
    /// Creates a new instance from the underlying value.
    pub fn new(value: T) -> Self {
        Self::Ok(value)
    }

    /// Converts to a `fidl::Result`, mapping framework errors to `fidl::Error`.
    pub fn into_result<P: ProtocolMarker>(self, method_name: &'static str) -> Result<T> {
        match self {
            Flexible::Ok(ok) => Ok(ok),
            Flexible::FrameworkErr(FrameworkErr::UnknownMethod) => {
                Err(Error::UnsupportedMethod { method_name, protocol_name: P::DEBUG_NAME })
            }
        }
    }
}

impl<T, E> FlexibleResult<T, E> {
    /// Creates a new instance from an `std::result::Result`.
    pub fn new(result: std::result::Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Ok(value),
            Err(err) => Self::DomainErr(err),
        }
    }

    /// Converts to a `fidl::Result`, mapping framework errors to `fidl::Error`.
    pub fn into_result<P: ProtocolMarker>(
        self,
        method_name: &'static str,
    ) -> Result<std::result::Result<T, E>> {
        match self {
            FlexibleResult::Ok(ok) => Ok(Ok(ok)),
            FlexibleResult::DomainErr(err) => Ok(Err(err)),
            FlexibleResult::FrameworkErr(FrameworkErr::UnknownMethod) => {
                Err(Error::UnsupportedMethod { method_name, protocol_name: P::DEBUG_NAME })
            }
        }
    }
}

/// Implements `TypeMarker`, `Encode`, and `Decode` for a result union type.
macro_rules! impl_result_union {
    (
        params: [$($encode_param:ident: Encode<$type_param:ident>),*],
        ty: $ty:ty,
        owned: $owned:ty,
        encode: $encode:ty,
        members: [$(
            {
                ctor: { $($member_ctor:tt)* },
                ty: $member_ty:ty,
                ordinal: $member_ordinal:tt,
            },
        )*]
    ) => {
        unsafe impl<$($type_param: TypeMarker),*> TypeMarker for $ty {
            type Owned = $owned;

            #[inline(always)]
            fn inline_align(_context: Context) -> usize {
                8
            }

            #[inline(always)]
            fn inline_size(context: Context) -> usize {
                match context.wire_format_version {
                    WireFormatVersion::V1 => 24,
                    WireFormatVersion::V2 => 16,
                }
            }
        }

        unsafe impl<$($type_param: TypeMarker, $encode_param: Encode<$type_param>),*> Encode<$ty> for $encode {
            #[inline]
            unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
                encoder.debug_check_bounds::<$ty>(offset);
                match self {
                    $(
                        $($member_ctor)*(val) => {
                            encoder.write_num::<u64>($member_ordinal, offset);
                            encode_in_envelope::<$member_ty>(val, encoder, offset + 8, depth)
                        }
                    )*
                }
            }
        }

        impl<$($type_param: TypeMarker),*> Decode<$ty> for $owned {
            #[inline(always)]
            fn new_empty() -> Self {
                #![allow(unreachable_code)]
                $(
                    return $($member_ctor)*(new_empty!($member_ty));
                )*
            }

            #[inline]
            unsafe fn decode(&mut self, decoder: &mut Decoder<'_>, offset: usize, mut depth: Depth) -> Result<()> {
                decoder.debug_check_bounds::<$ty>(offset);
                let next_out_of_line = decoder.next_out_of_line();
                let handles_before = decoder.remaining_handles();
                let (ordinal, inlined, num_bytes, num_handles) = decode_union_inline_portion(decoder, offset)?;
                let member_inline_size = match ordinal {
                    $(
                        $member_ordinal => <$member_ty as TypeMarker>::inline_size(decoder.context),
                    )*
                    _ => return Err(Error::UnknownUnionTag),
                };
                if let WireFormatVersion::V2 = decoder.context.wire_format_version {
                    if inlined != (member_inline_size <= 4) {
                        return Err(Error::InvalidInlineBitInEnvelope);
                    }
                }
                let inner_offset;
                if inlined {
                    decoder.check_inline_envelope_padding(offset + 8, member_inline_size)?;
                    inner_offset = offset + 8;
                } else {
                    depth.increment()?;
                    inner_offset = decoder.out_of_line_offset(member_inline_size)?;
                }
                match ordinal {
                    $(
                        $member_ordinal => {
                            #[allow(irrefutable_let_patterns)]
                            if let $($member_ctor)*(_) = self {
                                // Do nothing, read the value into the object
                            } else {
                                // Initialize `self` to the right variant
                                *self = $($member_ctor)*(new_empty!($member_ty));
                            }
                            #[allow(irrefutable_let_patterns)]
                            if let $($member_ctor)*(ref mut val) = self {
                                decode!($member_ty, val, decoder, inner_offset, depth)?;
                            } else {
                                unreachable!()
                            }
                        }
                    )*
                    ordinal => panic!("unexpected ordinal {:?}", ordinal)
                }
                if !inlined && decoder.next_out_of_line() != next_out_of_line + (num_bytes as usize) {
                    return Err(Error::InvalidNumBytesInEnvelope);
                }
                if handles_before != decoder.remaining_handles() + (num_handles as usize) {
                    return Err(Error::InvalidNumHandlesInEnvelope);
                }
                Ok(())
            }
        }
    };
}

impl_result_union! {
    params: [X: Encode<T>, Y: Encode<E>],
    ty: ResultType<T, E>,
    owned: std::result::Result<T::Owned, E::Owned>,
    encode: std::result::Result<X, Y>,
    members: [
        { ctor: { Ok }, ty: T, ordinal: 1, },
        { ctor: { Err }, ty: E, ordinal: 2, },
    ]
}

impl_result_union! {
    params: [X: Encode<T>],
    ty: FlexibleType<T>,
    owned: Flexible<T::Owned>,
    encode: Flexible<X>,
    members: [
        { ctor: { Flexible::Ok }, ty: T, ordinal: 1, },
        { ctor: { Flexible::FrameworkErr }, ty: FrameworkErr, ordinal: 3, },
    ]
}

impl_result_union! {
    params: [X: Encode<T>, Y: Encode<E>],
    ty: FlexibleResultType<T, E>,
    owned: FlexibleResult<T::Owned, E::Owned>,
    encode: FlexibleResult<X, Y>,
    members: [
        { ctor: { FlexibleResult::Ok }, ty: T, ordinal: 1, },
        { ctor: { FlexibleResult::DomainErr }, ty: E, ordinal: 2, },
        { ctor: { FlexibleResult::FrameworkErr }, ty: FrameworkErr, ordinal: 3, },
    ]
}

////////////////////////////////////////////////////////////////////////////////
// Epitaphs
////////////////////////////////////////////////////////////////////////////////

/// The body of a FIDL Epitaph
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EpitaphBody {
    /// The error status.
    pub error: zx_status::Status,
}

unsafe impl TypeMarker for EpitaphBody {
    type Owned = Self;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        4
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        4
    }
}

impl ValueTypeMarker for EpitaphBody {
    type Borrowed<'a> = &'a Self;
    fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl Encode<EpitaphBody> for &EpitaphBody {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<EpitaphBody>(offset);
        encoder.write_num::<i32>(self.error.into_raw(), offset);
        Ok(())
    }
}

impl Decode<Self> for EpitaphBody {
    #[inline(always)]
    fn new_empty() -> Self {
        Self { error: zx_status::Status::from_raw(0) }
    }

    #[inline]
    unsafe fn decode(
        &mut self,
        decoder: &mut Decoder<'_>,
        offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        decoder.debug_check_bounds::<Self>(offset);
        self.error = zx_status::Status::from_raw(decoder.read_num::<i32>(offset));
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////
// Messages
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type for a message consisting of a header `H` and body `T`.
pub struct GenericMessageType<H: ValueTypeMarker, T: TypeMarker>(PhantomData<(H, T)>);

/// A struct which encodes as `GenericMessageType<H, T>` where `E: Encode<T>`.
pub struct GenericMessage<H, E> {
    /// Header of the message.
    pub header: H,
    /// Body of the message.
    pub body: E,
}

/// The owned type for `GenericMessageType` is uninhabited because we never
/// decode full messages. We decode the header and body separately, as we
/// usually we don't know the body's type until after we've decoded the header.
pub enum GenericMessageOwned {}

unsafe impl<H: ValueTypeMarker, T: TypeMarker> TypeMarker for GenericMessageType<H, T> {
    type Owned = GenericMessageOwned;

    #[inline(always)]
    fn inline_align(context: Context) -> usize {
        std::cmp::max(H::inline_align(context), T::inline_align(context))
    }

    #[inline(always)]
    fn inline_size(context: Context) -> usize {
        H::inline_size(context) + T::inline_size(context)
    }
}

unsafe impl<H: ValueTypeMarker, T: TypeMarker, E: Encode<T>> Encode<GenericMessageType<H, T>>
    for GenericMessage<H::Owned, E>
{
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<GenericMessageType<H, T>>(offset);
        H::borrow(&self.header).encode(encoder, offset, depth)?;
        self.body.encode(encoder, offset + H::inline_size(encoder.context), depth)
    }
}

impl<H: ValueTypeMarker, T: TypeMarker> Decode<GenericMessageType<H, T>> for GenericMessageOwned {
    fn new_empty() -> Self {
        panic!("cannot create GenericMessageOwned");
    }

    unsafe fn decode(
        &mut self,
        _decoder: &mut Decoder<'_>,
        _offset: usize,
        _depth: Depth,
    ) -> Result<()> {
        match *self {}
    }
}

////////////////////////////////////////////////////////////////////////////////
// Transaction messages
////////////////////////////////////////////////////////////////////////////////

/// The FIDL type for a transaction message with body `T`.
pub type TransactionMessageType<T> = GenericMessageType<TransactionHeader, T>;

/// A struct which encodes as `TransactionMessageType<T>` where `E: Encode<T>`.
pub type TransactionMessage<E> = GenericMessage<TransactionHeader, E>;

/// Header for transactional FIDL messages
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TransactionHeader {
    /// Transaction ID which identifies a request-response pair
    tx_id: u32,
    /// Flags set for this message. MUST NOT be validated by bindings. Usually
    /// temporarily during migrations.
    at_rest_flags: [u8; 2],
    /// Flags used for dynamically interpreting the request if it is unknown to
    /// the receiver.
    dynamic_flags: u8,
    /// Magic number indicating the message's wire format. Two sides with
    /// different magic numbers are incompatible with each other.
    magic_number: u8,
    /// Ordinal which identifies the FIDL method
    ordinal: u64,
}

impl TransactionHeader {
    /// Returns whether the message containing this TransactionHeader is in a
    /// compatible wire format
    #[inline]
    pub fn is_compatible(&self) -> bool {
        self.magic_number == MAGIC_NUMBER_INITIAL
    }
}

bitflags! {
    /// Bitflags type for transaction header at-rest flags.
    pub struct AtRestFlags: u16 {
        /// Empty placeholder since empty bitflags are not allowed. Should be
        /// removed once any new header flags are defined.
        #[deprecated = "Placeholder since empty bitflags are not allowed."]
        const __PLACEHOLDER = 0;

        /// Indicates that the V2 wire format should be used instead of the V1
        /// wire format.
        /// This includes the following RFCs:
        /// - Efficient envelopes
        /// - Inlining small values in FIDL envelopes
        const USE_V2_WIRE_FORMAT = 2;
    }
}

bitflags! {
    /// Bitflags type to flags that aid in dynamically identifying features of
    /// the request.
    pub struct DynamicFlags: u8 {
        /// Indicates that the request is for a flexible method.
        const FLEXIBLE = 1 << 7;
    }
}

impl From<AtRestFlags> for [u8; 2] {
    #[inline]
    fn from(value: AtRestFlags) -> Self {
        value.bits.to_le_bytes()
    }
}

impl TransactionHeader {
    /// Creates a new transaction header with the default encode context and magic number.
    #[inline]
    pub fn new(tx_id: u32, ordinal: u64, dynamic_flags: DynamicFlags) -> Self {
        TransactionHeader::new_full(
            tx_id,
            ordinal,
            default_encode_context(),
            dynamic_flags,
            MAGIC_NUMBER_INITIAL,
        )
    }

    /// Creates a new transaction header with a specific context and magic number.
    #[inline]
    pub fn new_full(
        tx_id: u32,
        ordinal: u64,
        context: Context,
        dynamic_flags: DynamicFlags,
        magic_number: u8,
    ) -> Self {
        TransactionHeader {
            tx_id,
            at_rest_flags: context.at_rest_flags().into(),
            dynamic_flags: dynamic_flags.bits,
            magic_number,
            ordinal,
        }
    }

    /// Returns the header's transaction id.
    #[inline]
    pub fn tx_id(&self) -> u32 {
        self.tx_id
    }

    /// Returns the header's message ordinal.
    #[inline]
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Returns true if the header is for an epitaph message.
    #[inline]
    pub fn is_epitaph(&self) -> bool {
        self.ordinal == EPITAPH_ORDINAL
    }

    /// Returns the magic number.
    #[inline]
    pub fn magic_number(&self) -> u8 {
        self.magic_number
    }

    /// Returns the header's migration flags as a `AtRestFlags` value.
    #[inline]
    pub fn at_rest_flags(&self) -> AtRestFlags {
        AtRestFlags::from_bits_truncate(u16::from_le_bytes(self.at_rest_flags))
    }

    /// Returns the header's dynamic flags as a `DynamicFlags` value.
    #[inline]
    pub fn dynamic_flags(&self) -> DynamicFlags {
        DynamicFlags::from_bits_truncate(self.dynamic_flags)
    }

    /// Returns the context to use for decoding the message body associated with
    /// this header. During migrations, this is dependent on `self.flags()` and
    /// controls dynamic behavior in the read path.
    #[inline]
    pub fn decoding_context(&self) -> Context {
        if self.at_rest_flags().contains(AtRestFlags::USE_V2_WIRE_FORMAT) {
            Context { wire_format_version: WireFormatVersion::V2 }
        } else {
            Context { wire_format_version: WireFormatVersion::V1 }
        }
    }
}

/// Decodes the transaction header from a message.
/// Returns the header and a reference to the tail of the message.
pub fn decode_transaction_header(bytes: &[u8]) -> Result<(TransactionHeader, &[u8])> {
    let mut header = new_empty!(TransactionHeader);
    let context = Context { wire_format_version: WireFormatVersion::V2 };
    let header_len = <TransactionHeader as TypeMarker>::inline_size(context);
    if bytes.len() < header_len {
        return Err(Error::OutOfRange);
    }
    let (header_bytes, body_bytes) = bytes.split_at(header_len);
    let handles = &mut [];
    Decoder::decode_with_context::<TransactionHeader>(context, header_bytes, handles, &mut header)?;
    Ok((header, body_bytes))
}

unsafe impl TypeMarker for TransactionHeader {
    type Owned = Self;

    #[inline(always)]
    fn inline_align(_context: Context) -> usize {
        8
    }

    #[inline(always)]
    fn inline_size(_context: Context) -> usize {
        16
    }
}

impl ValueTypeMarker for TransactionHeader {
    type Borrowed<'a> = &'a Self;
    fn borrow(value: &<Self as TypeMarker>::Owned) -> Self::Borrowed<'_> {
        value
    }
}

unsafe impl Encode<TransactionHeader> for &TransactionHeader {
    #[inline]
    unsafe fn encode(self, encoder: &mut Encoder<'_>, offset: usize, _depth: Depth) -> Result<()> {
        encoder.debug_check_bounds::<TransactionHeader>(offset);
        unsafe {
            let buf_ptr = encoder.buf.as_mut_ptr().add(offset);
            (buf_ptr as *mut TransactionHeader).write_unaligned(*self);
        }
        Ok(())
    }
}

impl Decode<Self> for TransactionHeader {
    #[inline(always)]
    fn new_empty() -> Self {
        Self { tx_id: 0, at_rest_flags: [0; 2], dynamic_flags: 0, magic_number: 0, ordinal: 0 }
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
            let obj_ptr = self as *mut TransactionHeader;
            std::ptr::copy_nonoverlapping(buf_ptr, obj_ptr as *mut u8, 16);
        }
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////
// Persistence
////////////////////////////////////////////////////////////////////////////////

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
    // TODO(fxbug.dev/45252): Only accept the new header format.
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

/// Converts a vector of `HandleDisposition` (handles bundled with their
/// intended object type and rights) to a vector of `HandleInfo` (handles
/// bundled with their actual type and rights, guaranteed by the kernel).
///
/// This makes a `zx_handle_replace` syscall for each handle unless the rights
/// are `Rights::SAME_RIGHTS`.
///
/// # Panics
///
/// Panics if any of the handle dispositions uses `HandleOp::Duplicate`. This is
/// never the case for handle dispositions return by `standalone_encode`.
pub fn convert_handle_dispositions_to_infos(
    handle_dispositions: Vec<HandleDisposition<'_>>,
) -> Result<Vec<HandleInfo>> {
    let mut infos = Vec::new();
    for hd in handle_dispositions.into_iter() {
        infos.push(HandleInfo {
            handle: match hd.handle_op {
                HandleOp::Move(h) => {
                    if hd.rights == Rights::SAME_RIGHTS {
                        h
                    } else {
                        h.replace(hd.rights).map_err(Error::HandleReplace)?
                    }
                }
                HandleOp::Duplicate(_) => panic!("unexpected HandleOp::Duplicate"),
            },
            object_type: hd.object_type,
            rights: hd.rights,
        });
    }
    Ok(infos)
}

/// Decodes the persistently stored header from a message.
/// Returns the header and a reference to the tail of the message.
fn decode_wire_metadata(bytes: &[u8]) -> Result<WireMetadata> {
    let context = Context { wire_format_version: WireFormatVersion::V2 };
    match bytes.len() {
        8 => {
            // New 8-byte format.
            let mut header = new_empty!(WireMetadata);
            Decoder::decode_with_context::<WireMetadata>(context, bytes, &mut [], &mut header)?;
            Ok(header)
        }
        // TODO(fxbug.dev/45252): Remove this.
        16 => {
            // Old 16-byte format that matches TransactionHeader.
            let mut header = new_empty!(TransactionHeader);
            Decoder::decode_with_context::<TransactionHeader>(
                context,
                bytes,
                &mut [],
                &mut header,
            )?;
            Ok(WireMetadata {
                disambiguator: 0,
                magic_number: header.magic_number,
                at_rest_flags: header.at_rest_flags,
                reserved: [0; 4],
            })
        }
        _ => Err(Error::InvalidHeader),
    }
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

////////////////////////////////////////////////////////////////////////////////
// TLS buffer
////////////////////////////////////////////////////////////////////////////////

struct TlsBuf {
    bytes: Vec<u8>,
    encode_handles: Vec<HandleDisposition<'static>>,
    decode_handles: Vec<HandleInfo>,
}

impl TlsBuf {
    fn new() -> TlsBuf {
        TlsBuf { bytes: Vec::new(), encode_handles: Vec::new(), decode_handles: Vec::new() }
    }
}

thread_local!(static TLS_BUF: RefCell<TlsBuf> = RefCell::new(TlsBuf::new()));

const MIN_TLS_BUF_BYTES_SIZE: usize = 512;

/// Acquire a mutable reference to the thread-local buffers used for encoding.
///
/// This function may not be called recursively.
#[inline]
pub fn with_tls_encode_buf<R>(
    f: impl FnOnce(&mut Vec<u8>, &mut Vec<HandleDisposition<'static>>) -> R,
) -> R {
    TLS_BUF.with(|buf| {
        let (mut bytes, mut handles) =
            RefMut::map_split(buf.borrow_mut(), |b| (&mut b.bytes, &mut b.encode_handles));
        if bytes.capacity() == 0 {
            bytes.reserve(MIN_TLS_BUF_BYTES_SIZE);
        }
        let res = f(&mut bytes, &mut handles);
        bytes.clear();
        handles.clear();
        res
    })
}

/// Acquire a mutable reference to the thread-local buffers used for decoding.
///
/// This function may not be called recursively.
#[inline]
pub fn with_tls_decode_buf<R>(f: impl FnOnce(&mut Vec<u8>, &mut Vec<HandleInfo>) -> R) -> R {
    TLS_BUF.with(|buf| {
        let (mut bytes, mut handles) =
            RefMut::map_split(buf.borrow_mut(), |b| (&mut b.bytes, &mut b.decode_handles));
        if bytes.capacity() == 0 {
            bytes.reserve(MIN_TLS_BUF_BYTES_SIZE);
        }
        let res = f(&mut bytes, &mut handles);
        bytes.clear();
        handles.clear();
        res
    })
}

/// Encodes the provided type into the thread-local encoding buffers.
///
/// This function may not be called recursively.
#[inline]
pub fn with_tls_encoded<T: TypeMarker, Out>(
    val: impl Encode<T>,
    f: impl FnOnce(&mut Vec<u8>, &mut Vec<HandleDisposition<'static>>) -> Result<Out>,
) -> Result<Out> {
    with_tls_encode_buf(|bytes, handles| {
        Encoder::encode(bytes, handles, val)?;
        f(bytes, handles)
    })
}

////////////////////////////////////////////////////////////////////////////////
// Tests
////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod test {
    // Silence dead code errors from unused functions produced by macros like
    // `fidl_bits!`, `fidl_union!`, etc. To the compiler, it's as if we defined
    // a pub fn in a private mod and never used it. Unfortunately placing this
    // attribute directly on the macro invocations does not work.
    #![allow(dead_code)]

    use super::*;
    use crate::handle::AsHandleRef;
    use assert_matches::assert_matches;
    use std::{f32, f64, fmt, i64, u64};

    const CONTEXTS: [Context; 2] = [
        Context { wire_format_version: WireFormatVersion::V1 },
        Context { wire_format_version: WireFormatVersion::V2 },
    ];

    const OBJECT_TYPE_NONE: u32 = crate::handle::ObjectType::NONE.into_raw();
    const SAME_RIGHTS: u32 = crate::handle::Rights::SAME_RIGHTS.bits();

    #[track_caller]
    fn to_infos(dispositions: &mut Vec<HandleDisposition<'_>>) -> Vec<HandleInfo> {
        convert_handle_dispositions_to_infos(mem::take(dispositions)).unwrap()
    }

    #[track_caller]
    pub fn encode_decode<T: TypeMarker>(ctx: Context, start: impl Encode<T>) -> T::Owned {
        let buf = &mut Vec::new();
        let handle_buf = &mut Vec::new();
        Encoder::encode_with_context::<T>(ctx, buf, handle_buf, start).expect("Encoding failed");
        let mut out = T::Owned::new_empty();
        Decoder::decode_with_context::<T>(ctx, buf, &mut to_infos(handle_buf), &mut out)
            .expect("Decoding failed");
        out
    }

    #[track_caller]
    fn encode_assert_bytes<T: TypeMarker>(
        ctx: Context,
        data: impl Encode<T>,
        encoded_bytes: &[u8],
    ) {
        let buf = &mut Vec::new();
        let handle_buf = &mut Vec::new();
        Encoder::encode_with_context::<T>(ctx, buf, handle_buf, data).expect("Encoding failed");
        assert_eq!(buf, encoded_bytes);
    }

    #[track_caller]
    fn identity<T>(data: &T::Owned)
    where
        T: ValueTypeMarker,
        T::Owned: fmt::Debug + PartialEq,
    {
        for ctx in CONTEXTS {
            assert_eq!(*data, encode_decode(ctx, T::borrow(data)));
        }
    }

    #[track_caller]
    fn identities<T>(values: &[T::Owned])
    where
        T: ValueTypeMarker,
        T::Owned: fmt::Debug + PartialEq,
    {
        for value in values {
            identity::<T>(value);
        }
    }

    #[test]
    fn encode_decode_byte() {
        identities::<u8>(&[0u8, 57u8, 255u8]);
        identities::<i8>(&[0i8, -57i8, 12i8]);
        identity::<Optional<Vector<i32, 3>>>(&None::<Vec<i32>>);
    }

    #[test]
    fn encode_decode_multibyte() {
        identities::<u64>(&[0u64, 1u64, u64::MAX, u64::MIN]);
        identities::<i64>(&[0i64, 1i64, i64::MAX, i64::MIN]);
        identities::<f32>(&[0f32, 1f32, f32::MAX, f32::MIN]);
        identities::<f64>(&[0f64, 1f64, f64::MAX, f64::MIN]);
    }

    #[test]
    fn encode_decode_nan() {
        for ctx in CONTEXTS {
            assert!(encode_decode::<f32>(ctx, f32::NAN).is_nan());
            assert!(encode_decode::<f64>(ctx, f64::NAN).is_nan());
        }
    }

    #[test]
    fn encode_decode_out_of_line() {
        type V<T> = UnboundedVector<T>;
        type S = UnboundedString;
        type O<T> = Optional<T>;

        identity::<V<i32>>(&Vec::<i32>::new());
        identity::<V<i32>>(&vec![1, 2, 3]);
        identity::<O<V<i32>>>(&None::<Vec<i32>>);
        identity::<O<V<i32>>>(&Some(Vec::<i32>::new()));
        identity::<O<V<i32>>>(&Some(vec![1, 2, 3]));
        identity::<O<V<V<i32>>>>(&Some(vec![vec![1, 2, 3]]));
        identity::<O<V<O<V<i32>>>>>(&Some(vec![Some(vec![1, 2, 3])]));
        identity::<S>(&"".to_string());
        identity::<S>(&"foo".to_string());
        identity::<O<S>>(&None::<String>);
        identity::<O<S>>(&Some("".to_string()));
        identity::<O<S>>(&Some("foo".to_string()));
        identity::<O<V<O<S>>>>(&Some(vec![None, Some("foo".to_string())]));
        identity::<V<S>>(&vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn array_of_arrays() {
        identity::<Array<Array<u32, 5>, 2>>(&[[1, 2, 3, 4, 5], [5, 4, 3, 2, 1]]);
    }

    fn slice_identity<T>(start: &[T::Owned])
    where
        T: ValueTypeMarker,
        T::Owned: fmt::Debug + PartialEq,
    {
        for ctx in CONTEXTS {
            let decoded = encode_decode::<UnboundedVector<T>>(ctx, start);
            assert_eq!(start, UnboundedVector::<T>::borrow(&decoded));
        }
    }

    #[test]
    fn encode_slices_of_primitives() {
        slice_identity::<u8>(&[]);
        slice_identity::<u8>(&[0]);
        slice_identity::<u8>(&[1, 2, 3, 4, 5, 255]);

        slice_identity::<i8>(&[]);
        slice_identity::<i8>(&[0]);
        slice_identity::<i8>(&[1, 2, 3, 4, 5, -128, 127]);

        slice_identity::<u64>(&[]);
        slice_identity::<u64>(&[0]);
        slice_identity::<u64>(&[1, 2, 3, 4, 5, u64::MAX]);

        slice_identity::<f32>(&[]);
        slice_identity::<f32>(&[0.0]);
        slice_identity::<f32>(&[1.0, 2.0, 3.0, 4.0, 5.0, f32::MIN, f32::MAX]);

        slice_identity::<f64>(&[]);
        slice_identity::<f64>(&[0.0]);
        slice_identity::<f64>(&[1.0, 2.0, 3.0, 4.0, 5.0, f64::MIN, f64::MAX]);
    }

    #[test]
    fn result_encode_empty_ok_value() {
        for ctx in CONTEXTS {
            // An empty response is represented by () and has zero size.
            encode_assert_bytes::<EmptyPayload>(ctx, (), &[]);
        }
        // But in the context of an error result type Result<(), ErrorType>, the
        // () in Ok(()) represents an empty struct (with size 1).
        encode_assert_bytes::<ResultType<EmptyStruct, i32>>(
            Context { wire_format_version: WireFormatVersion::V2 },
            Ok::<(), i32>(()),
            &[
                0x01, 0x00, 0x00, 0x00, // success ordinal
                0x00, 0x00, 0x00, 0x00, // success ordinal [cont.]
                0x00, 0x00, 0x00, 0x00, // inline value: empty struct + 3 bytes padding
                0x00, 0x00, 0x01, 0x00, // 0 handles, flags (inlined)
            ],
        );
    }

    #[test]
    fn result_decode_empty_ok_value() {
        let mut result = Err(0);
        Decoder::decode_with_context::<ResultType<EmptyStruct, u32>>(
            Context { wire_format_version: WireFormatVersion::V2 },
            &[
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // success ordinal
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, // empty struct inline
            ],
            &mut [],
            &mut result,
        )
        .expect("Decoding failed");
        assert_matches!(result, Ok(()));
    }

    #[test]
    fn encode_decode_result() {
        type Res = ResultType<UnboundedString, u32>;
        for ctx in CONTEXTS {
            assert_eq!(encode_decode::<Res>(ctx, Ok::<&str, u32>("foo")), Ok("foo".to_string()));
            assert_eq!(encode_decode::<Res>(ctx, Err::<&str, u32>(5)), Err(5));
        }
    }

    #[test]
    fn result_validates_num_bytes() {
        type Res = ResultType<u64, u64>;
        for ctx in CONTEXTS {
            for ordinal in [1, 2] {
                // Envelope should have num_bytes set to 8, not 16.
                let bytes = [
                    ordinal, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ordinal
                    0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 16 bytes, 0 handles
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // present
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // number
                ];
                let mut out = new_empty!(Res);
                assert_matches!(
                    Decoder::decode_with_context::<Res>(ctx, &bytes, &mut [], &mut out),
                    Err(Error::InvalidNumBytesInEnvelope)
                );
            }
        }
    }

    #[test]
    fn result_validates_num_handles() {
        type Res = ResultType<u64, u64>;
        for ctx in CONTEXTS {
            for ordinal in [1, 2] {
                // Envelope should have num_handles set to 0, not 1.
                let bytes = [
                    ordinal, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ordinal
                    0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // 16 bytes, 1 handle
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // present
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // number
                ];
                let mut out = new_empty!(Res);
                assert_matches!(
                    Decoder::decode_with_context::<Res>(ctx, &bytes, &mut [], &mut out),
                    Err(Error::InvalidNumHandlesInEnvelope)
                );
            }
        }
    }

    #[test]
    fn decode_result_unknown_tag() {
        type Res = ResultType<u32, u32>;
        let ctx = Context { wire_format_version: WireFormatVersion::V2 };

        let bytes: &[u8] = &[
            // Ordinal 3 (not known to result) ----------|
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            // inline value -----|  NHandles |  Flags ---|
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
        ];
        let handle_buf = &mut Vec::<HandleInfo>::new();

        let mut out = new_empty!(Res);
        let res = Decoder::decode_with_context::<Res>(ctx, bytes, handle_buf, &mut out);
        assert_matches!(res, Err(Error::UnknownUnionTag));
    }

    #[test]
    fn decode_result_success_invalid_empty_struct() {
        type Res = ResultType<EmptyStruct, u32>;
        let ctx = Context { wire_format_version: WireFormatVersion::V2 };

        let bytes: &[u8] = &[
            // Ordinal 1 (success) ----------------------|
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            // inline value -----|  NHandles |  Flags ---|
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
        ];
        let handle_buf = &mut Vec::<HandleInfo>::new();

        let mut out = new_empty!(Res);
        let res = Decoder::decode_with_context::<Res>(ctx, bytes, handle_buf, &mut out);
        assert_matches!(res, Err(Error::Invalid));
    }

    #[test]
    fn encode_decode_transaction_msg() {
        for ctx in CONTEXTS {
            let header = TransactionHeader {
                tx_id: 4,
                ordinal: 6,
                at_rest_flags: [0; 2],
                dynamic_flags: 0,
                magic_number: 1,
            };
            type Body = UnboundedString;
            let body = "hello";

            let start = TransactionMessage { header, body };

            let buf = &mut Vec::new();
            let handle_buf = &mut Vec::new();
            Encoder::encode_with_context::<TransactionMessageType<Body>>(
                ctx, buf, handle_buf, start,
            )
            .expect("Encoding failed");

            let (out_header, out_buf) =
                decode_transaction_header(buf).expect("Decoding header failed");
            assert_eq!(header, out_header);

            let mut body_out = String::new();
            Decoder::decode_into::<Body>(
                &header,
                out_buf,
                &mut to_infos(handle_buf),
                &mut body_out,
            )
            .expect("Decoding body failed");
            assert_eq!(body, body_out);
        }
    }

    #[test]
    fn direct_encode_transaction_header_strict() {
        let bytes = &[
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, //
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        let header = TransactionHeader {
            tx_id: 4,
            ordinal: 6,
            at_rest_flags: [0; 2],
            dynamic_flags: DynamicFlags::empty().bits,
            magic_number: 1,
        };

        for ctx in CONTEXTS {
            encode_assert_bytes::<TransactionHeader>(ctx, &header, bytes);
        }
    }

    #[test]
    fn direct_decode_transaction_header_strict() {
        let bytes = &[
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, //
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        let header = TransactionHeader {
            tx_id: 4,
            ordinal: 6,
            at_rest_flags: [0; 2],
            dynamic_flags: DynamicFlags::empty().bits,
            magic_number: 1,
        };

        for ctx in CONTEXTS {
            let mut out = new_empty!(TransactionHeader);
            Decoder::decode_with_context::<TransactionHeader>(ctx, bytes, &mut [], &mut out)
                .expect("Decoding failed");
            assert_eq!(out, header);
        }
    }

    #[test]
    fn direct_encode_transaction_header_flexible() {
        let bytes = &[
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01, //
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        let header = TransactionHeader {
            tx_id: 4,
            ordinal: 6,
            at_rest_flags: [0; 2],
            dynamic_flags: DynamicFlags::FLEXIBLE.bits,
            magic_number: 1,
        };

        for ctx in CONTEXTS {
            encode_assert_bytes::<TransactionHeader>(ctx, &header, bytes);
        }
    }

    #[test]
    fn direct_decode_transaction_header_flexible() {
        let bytes = &[
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01, //
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        let header = TransactionHeader {
            tx_id: 4,
            ordinal: 6,
            at_rest_flags: [0; 2],
            dynamic_flags: DynamicFlags::FLEXIBLE.bits,
            magic_number: 1,
        };

        for ctx in CONTEXTS {
            let mut out = new_empty!(TransactionHeader);
            Decoder::decode_with_context::<TransactionHeader>(ctx, bytes, &mut [], &mut out)
                .expect("Decoding failed");
            assert_eq!(out, header);
        }
    }

    #[test]
    fn extra_data_is_disallowed() {
        for ctx in CONTEXTS {
            assert_matches!(
                Decoder::decode_with_context::<EmptyPayload>(ctx, &[0], &mut [], &mut ()),
                Err(Error::ExtraBytes)
            );
            assert_matches!(
                Decoder::decode_with_context::<EmptyPayload>(
                    ctx,
                    &[],
                    &mut [HandleInfo {
                        handle: Handle::invalid(),
                        object_type: ObjectType::NONE,
                        rights: Rights::NONE,
                    }],
                    &mut ()
                ),
                Err(Error::ExtraHandles)
            );
        }
    }

    #[test]
    fn encode_default_context() {
        let buf = &mut Vec::new();
        Encoder::encode::<u8>(buf, &mut Vec::new(), 1u8).expect("Encoding failed");
        assert_eq!(buf, &[1u8, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn encode_handle() {
        type T = HandleType<Handle, OBJECT_TYPE_NONE, SAME_RIGHTS>;
        for ctx in CONTEXTS {
            let handle = crate::handle::Event::create().into_handle();
            let raw_handle = handle.raw_handle();
            let buf = &mut Vec::new();
            let handle_buf = &mut Vec::new();
            Encoder::encode_with_context::<T>(ctx, buf, handle_buf, handle)
                .expect("Encoding failed");

            assert_eq!(handle_buf.len(), 1);
            assert_matches!(handle_buf[0].handle_op, HandleOp::Move(ref h) if h.raw_handle() == raw_handle);

            let mut handle_out = new_empty!(T);
            Decoder::decode_with_context::<T>(ctx, buf, &mut to_infos(handle_buf), &mut handle_out)
                .expect("Decoding failed");
            assert_eq!(handle_out.raw_handle(), raw_handle, "foobar");
        }
    }

    #[test]
    fn decode_too_few_handles() {
        type T = HandleType<Handle, OBJECT_TYPE_NONE, SAME_RIGHTS>;
        for ctx in CONTEXTS {
            let bytes: &[u8] = &[0xff; 8];
            let handle_buf = &mut Vec::new();
            let mut handle_out = Handle::invalid();
            let res = Decoder::decode_with_context::<T>(ctx, bytes, handle_buf, &mut handle_out);
            assert_matches!(res, Err(Error::OutOfRange));
        }
    }

    #[test]
    fn encode_epitaph() {
        for ctx in CONTEXTS {
            let buf = &mut Vec::new();
            let handle_buf = &mut Vec::new();
            Encoder::encode_with_context::<EpitaphBody>(
                ctx,
                buf,
                handle_buf,
                &EpitaphBody { error: zx_status::Status::UNAVAILABLE },
            )
            .expect("encoding failed");
            assert_eq!(buf, &[0xe4, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00]);

            let mut out = new_empty!(EpitaphBody);
            Decoder::decode_with_context::<EpitaphBody>(
                ctx,
                buf,
                &mut to_infos(handle_buf),
                &mut out,
            )
            .expect("Decoding failed");
            assert_eq!(EpitaphBody { error: zx_status::Status::UNAVAILABLE }, out);
        }
    }
}
