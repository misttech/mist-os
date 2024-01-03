// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    fmt::Debug,
    io::{Cursor, Seek as _, SeekFrom},
    ops::Deref,
};
use zerocopy::{ByteSlice, FromBytes, NoCell, Ref, Unaligned};

/// Trait for a cursor that can emit a slice of "remaining" data, and advance forward.
pub trait ParseCursor: Sized {
    /// The inner representation that owns the underlying data.
    type Inner;

    /// The error returned when seeking forward fails on this cursor.
    type Error;

    /// Returns a slice of remaining data.
    fn remaining_slice(&self) -> &[u8];

    /// Returns the number of bytes remaining to be parsed by this [`ParseCursor`].
    fn len(&self) -> usize;

    /// Seeks forward by `num_bytes`, returning a `Self::Error` if seeking fails.
    fn seek_forward(&mut self, num_bytes: usize) -> Result<(), Self::Error>;

    /// Consumes self and returns the inner reprepresentation that owns the underlying data.
    fn into_inner(self) -> Self::Inner;
}

impl ParseCursor for Cursor<Vec<u8>> {
    type Inner = Vec<u8>;
    type Error = std::io::Error;

    fn remaining_slice(&self) -> &[u8] {
        let s: &[u8] = self.get_ref().as_ref();
        let p = self.position() as usize;
        &s[p..]
    }

    fn len(&self) -> usize {
        let position = self.position() as usize;
        self.get_ref().len() - position
    }

    fn seek_forward(&mut self, num_bytes: usize) -> Result<(), Self::Error> {
        self.seek(SeekFrom::Current(num_bytes as i64)).map(|_| ())
    }

    fn into_inner(self) -> Self::Inner {
        self.into_inner()
    }
}

/// A strategy for parsing data. Parsed structures that may contain references to parsed data are
/// generally of the form:
///
/// ```rust,ignore
/// type ParserOutput<PS: ParseStrategy> {
///     ref_or_value_t: PS::Output<T>,
///     // ...
/// }
/// ```
///
/// The above pattern allows [`ParseStrategy`] implementations to dictate how values are stored (by
/// copied value, or reference to parser input data).
pub trait ParseStrategy: Debug + PartialEq + Sized {
    /// Type of successfully parsed output from `Self::parse()`.
    type Output<T: Debug + FromBytes + NoCell + PartialEq + Unaligned>: Debug + PartialEq;

    /// Type of successfully parsed output from `Self::parse_slice()`.
    type Slice<T: Debug + FromBytes + NoCell + PartialEq + Unaligned>: Debug + PartialEq;

    /// Parses a `Self::Output<T>` from the next bytes underlying `self`. If the parse succeeds,
    /// then return `(Some(output), self)` after advancing past the parsed bytes. Otherwise, return
    /// `None` without advancing past the parsed bytes.
    fn parse<T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        self,
    ) -> Option<(Self::Output<T>, Self)>;

    /// Parses a `Self::Slice<T>` of `count` elements from the next bytes underlying `self`. If the
    /// parse succeeds, then return `(Some(slice), self)` after advancing past the parsed bytes.
    /// Otherwise, return `None` without advancing past the parsed bytes.
    fn parse_slice<T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        self,
        count: usize,
    ) -> Option<(Self::Slice<T>, Self)>;

    /// Dereferences borrow of `Self::Output<T>` as borrow of `T`.
    fn deref<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        output: &'a Self::Output<T>,
    ) -> &'a T;

    /// Dereferences borrow of `Self::Slice<T>` as borrow of `[T]`.
    fn deref_slice<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        slice: &'a Self::Slice<T>,
    ) -> &'a [T];

    /// Returns the number of bytes remaining to be parsed by this [`ParseStrategy`].
    fn len(&self) -> usize;
}

/// A [`ParseStrategy`] that produces [`Ref<B, T>`].
///
/// This strategy is zero-copy, but one consequence is that the parser input and output cannot be
/// retained outside the lexical scope from which the parser input was borrowed. For example, the
/// following will not compile:
///
/// ```rust,ignore
/// fn do_by_ref<'a, T: zerocopy::FromBytes + zerocopy::Unaligned>() -> (
///     zerocopy::Ref<&'a [u8], T>, ByRef<&'a [u8]>,
/// ) {
///     let bytes: Vec<u8> = // ...
///     let parser = ByRef::new(bytes.as_slice());
///     parser.parse::<T>().unwrap()
/// }
/// ```
///
/// The above code induces the following error:
///
/// ```rust,ignore
/// error[E0515]: cannot return value referencing local variable `bytes`
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ByRef<B: ByteSlice>(B);

impl<B: ByteSlice> ByRef<B> {
    /// Returns a new [`ByRef`] that wraps `bytes_slice`.
    pub fn new(byte_slice: B) -> Self {
        Self(byte_slice)
    }
}

impl<B: Debug + ByteSlice + PartialEq> ParseStrategy for ByRef<B> {
    type Output<T: Debug + FromBytes + NoCell + PartialEq + Unaligned> = Ref<B, T>;
    type Slice<T: Debug + FromBytes + NoCell + PartialEq + Unaligned> = Ref<B, [T]>;

    /// Returns a [`Ref<B, T>`] as the parsed output of the next bytes in the underlying
    /// [`ByteSlice`].
    fn parse<T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        self,
    ) -> Option<(Self::Output<T>, Self)> {
        let (output, tail) = Ref::new_unaligned_from_prefix(self.0)?;
        Some((output, Self(tail)))
    }

    /// Returns a `Ref<B, [T]>` as the parsed output of the next bytes in the underlying
    /// [`ByteSlice`].
    fn parse_slice<T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        self,
        num: usize,
    ) -> Option<(Self::Slice<T>, Self)> {
        let (slice, tail) = Ref::new_slice_unaligned_from_prefix(self.0, num)?;
        Some((slice, Self(tail)))
    }

    fn deref<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        output: &'a Self::Output<T>,
    ) -> &'a T {
        output.deref() as &T
    }

    fn deref_slice<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        slice: &'a Self::Slice<T>,
    ) -> &'a [T] {
        slice.deref() as &[T]
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

/// A [`ParseStrategy`] that produces (copied/cloned) `T`.
///
/// This strategy makes up to one copy of the parser input (in addition to parser output data
/// structures). It is intended to support use cases where the parser input and parser output must
/// be retained outside the lexical from which parsing is invoked. For example:
///
/// ```rust,ignore
/// fn do_by_value<PC: ParseCursor, T: zerocopy::FromBytes + zerocopy::Unaligned>(
///     parse_cursor: PC,
/// ) -> (T, ByValue<PC>) {
///     let parser = ByValue::new(parse_cursor);
///     parser.parse::<T>().unwrap()
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ByValue<PC: ParseCursor>(PC);

impl<PC: ParseCursor> ByValue<PC> {
    /// Returns a new [`ByValue`] that wraps `parse_cursor`.
    pub fn new(parse_cursor: PC) -> Self {
        Self(parse_cursor)
    }

    /// Consumes this [`ByValue`] and returns the [`ParseCursor`] that it wraps.
    pub fn into_inner(self) -> PC {
        self.0
    }
}

impl<P: Debug + ParseCursor + PartialEq> ParseStrategy for ByValue<P> {
    type Output<T: Debug + FromBytes + NoCell + PartialEq + Unaligned> = T;
    type Slice<T: Debug + FromBytes + NoCell + PartialEq + Unaligned> = Vec<T>;

    /// Returns a `T` as the parsed output of the next bytes in the underlying [`ParseCursor`].
    fn parse<T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        mut self,
    ) -> Option<(Self::Output<T>, Self)> {
        let output = T::read_from_prefix(self.0.remaining_slice())?;
        if self.0.seek_forward(std::mem::size_of_val(&output)).is_err() {
            return None;
        }
        Some((output, self))
    }

    /// Returns a `Vec<T>` of `count` items as the parsed output of the next bytes in the underlying
    /// [`ParseCursor`].
    fn parse_slice<T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        mut self,
        count: usize,
    ) -> Option<(Self::Slice<T>, Self)> {
        let (slice, _) = T::slice_from_prefix(self.0.remaining_slice(), count)?;
        let size = std::mem::size_of_val(&slice);
        let slice = slice.to_owned();
        if self.0.seek_forward(size).is_err() {
            return None;
        }
        Some((slice, self))
    }

    fn deref<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        output: &'a Self::Output<T>,
    ) -> &'a T {
        output
    }

    fn deref_slice<'a, T: Debug + FromBytes + NoCell + PartialEq + Unaligned>(
        slice: &'a Self::Slice<T>,
    ) -> &'a [T] {
        slice
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::{little_endian as le, FromZeroes};

    #[derive(Clone, Debug, FromBytes, FromZeroes, NoCell, PartialEq, Unaligned)]
    #[repr(C, packed)]
    struct SomeNumbers {
        a: u8,
        b: le::U32,
        c: le::U16,
        d: u8,
    }

    // Ensure that "return parser + parsed output" pattern works on `ByValue`.
    fn do_by_value<
        PC: Debug + ParseCursor + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    >(
        parse_cursor: PC,
    ) -> (T, ByValue<PC>) {
        let parser = ByValue::new(parse_cursor);
        parser.parse::<T>().expect("some numbers")
    }
    fn do_slice_by_value<
        PC: Debug + ParseCursor + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    >(
        parse_cursor: PC,
        count: usize,
    ) -> (Vec<T>, ByValue<PC>) {
        let parser = ByValue::new(parse_cursor);
        parser.parse_slice::<T>(count).expect("some numbers")
    }

    #[test]
    fn by_ref_slice_u8_parse() {
        let bytes: Vec<u8> = (0..8).collect();
        let parser = ByRef::new(bytes.as_slice());
        let (some_numbers, parser) = parser.parse::<SomeNumbers>().expect("some numbers");
        assert_eq!(0, some_numbers.a);
        assert_eq!(7, some_numbers.d);
        assert_eq!(0, parser.0.len());
    }

    #[test]
    fn by_ref_slice_u8_parse_slice() {
        let bytes: Vec<u8> = (0..24).collect();
        let parser = ByRef::new(bytes.as_slice());
        let (some_numbers, parser) = parser.parse_slice::<SomeNumbers>(3).expect("some numbers");
        assert_eq!(3, some_numbers.len());
        assert_eq!(0, some_numbers[0].a);
        assert_eq!(7, some_numbers[0].d);
        assert_eq!(8, some_numbers[1].a);
        assert_eq!(15, some_numbers[1].d);
        assert_eq!(16, some_numbers[2].a);
        assert_eq!(23, some_numbers[2].d);
        assert_eq!(0, parser.0.len());
    }

    #[test]
    fn by_value_cursor_vec_u8() {
        let bytes: Cursor<Vec<u8>> = Cursor::new((0..8).collect());
        let (some_numbers, parser) = do_by_value::<_, SomeNumbers>(bytes);
        assert_eq!(0, some_numbers.a);
        assert_eq!(7, some_numbers.d);
        assert_eq!(8, parser.0.position());
        assert_eq!(8, parser.into_inner().into_inner().len());
    }

    #[test]
    fn by_value_slice_u8_parse_slice() {
        let bytes: Cursor<Vec<u8>> = Cursor::new((0..24).collect());
        let (some_numbers, parser) = do_slice_by_value::<_, SomeNumbers>(bytes, 3);
        assert_eq!(3, some_numbers.len());
        assert_eq!(0, some_numbers[0].a);
        assert_eq!(7, some_numbers[0].d);
        assert_eq!(8, some_numbers[1].a);
        assert_eq!(15, some_numbers[1].d);
        assert_eq!(16, some_numbers[2].a);
        assert_eq!(23, some_numbers[2].d);
        assert_eq!(24, parser.into_inner().into_inner().len());
    }
}
