// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::io::{Cursor, Seek as _, SeekFrom};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

/// Trait for a cursor that can emit a slice of "remaining" data, and advance forward.
trait ParseCursor: Sized {
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

    /// Consumes self and returns the inner representation of the complete parse input.
    #[allow(dead_code)]
    fn into_inner(self) -> Self::Inner;
}

impl<T: AsRef<[u8]>> ParseCursor for Cursor<T> {
    type Inner = T;
    type Error = std::io::Error;

    fn remaining_slice(&self) -> &[u8] {
        let s: &[u8] = self.get_ref().as_ref();
        let p = self.position() as usize;
        &s[p..]
    }

    fn len(&self) -> usize {
        let position = self.position() as usize;
        self.get_ref().as_ref().len() - position
    }

    fn seek_forward(&mut self, num_bytes: usize) -> Result<(), Self::Error> {
        self.seek(SeekFrom::Current(num_bytes as i64)).map(|_| ())
    }

    #[allow(dead_code)]
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
    /// Type of input supported by this [`ParseStrategy`].
    type Input;

    /// Type of successfully parsed output from `Self::parse()`.
    type Output<T: Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>: Debug
        + PartialEq;

    /// Type of successfully parsed output from `Self::parse_slice()`.
    type Slice<T: Debug + FromBytes + Immutable + PartialEq + Unaligned>: Debug + PartialEq;

    /// Parses a `Self::Output<T>` from the next bytes underlying `self`. If the parse succeeds,
    /// then return `(Some(output), self)` after advancing past the parsed bytes. Otherwise, return
    /// `None` without advancing past the parsed bytes.
    fn parse<T: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        self,
    ) -> Option<(Self::Output<T>, Self)>;

    /// Parses a `Self::Slice<T>` of `count` elements from the next bytes underlying `self`. If the
    /// parse succeeds, then return `(Some(slice), self)` after advancing past the parsed bytes.
    /// Otherwise, return `None` without advancing past the parsed bytes.
    fn parse_slice<T: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        self,
        count: usize,
    ) -> Option<(Self::Slice<T>, Self)>;

    /// Dereferences borrow of `Self::Output<T>` as borrow of `T`.
    fn deref<'a, T: Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        output: &'a Self::Output<T>,
    ) -> &'a T;

    /// Dereferences borrow of `Self::Slice<T>` as borrow of `[T]`.
    fn deref_slice<'a, T: Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        slice: &'a Self::Slice<T>,
    ) -> &'a [T];

    /// Returns the number of bytes remaining to be parsed by this [`ParseStrategy`].
    fn len(&self) -> usize;

    /// Returns the complete parse input being consumed by this strategy.
    fn into_inner(self) -> Self::Input;
}

/// A [`ParseStrategy`] that produces (copied/cloned) `T`.
///
/// This strategy makes up to one copy of the parser input (in addition to parser output data
/// structures). It is intended to support use cases where the parser input and parser output must
/// be retained outside the lexical from which parsing is invoked. For example:
///
/// ```rust,ignore
/// fn do_by_value<
///     D: AsRef<[u8]> + Debug + PartialEq,
///     T: zerocopy::FromBytes + zerocopy::Unaligned,
/// >(
///     data: D,
/// ) -> (T, ByValue<D>) {
///     let parser = ByValue::new(data);
///     parser.parse::<T>().unwrap()
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ByValue<T: AsRef<[u8]>>(Cursor<T>);

impl<T: AsRef<[u8]>> ByValue<T> {
    /// Returns a new [`ByValue`] that wraps `data` in a [`Cursor`] for parsing.
    pub fn new(data: T) -> Self {
        Self(Cursor::new(data))
    }
}

impl<T: AsRef<[u8]> + Debug + PartialEq> ParseStrategy for ByValue<T>
where
    Cursor<T>: Debug + ParseCursor + PartialEq,
{
    type Input = T;
    type Output<O: Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned> = O;
    type Slice<S: Debug + FromBytes + Immutable + PartialEq + Unaligned> = Vec<S>;

    /// Returns an `P` as the parsed output of the next bytes in the underlying [`Cursor`] data.
    fn parse<P: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        mut self,
    ) -> Option<(Self::Output<P>, Self)> {
        let (output, _) = P::read_from_prefix(ParseCursor::remaining_slice(&self.0)).ok()?;
        if self.0.seek_forward(std::mem::size_of_val(&output)).is_err() {
            return None;
        }
        Some((output, self))
    }

    /// Returns a `Vec<T>` of `count` items as the parsed output of the next bytes in the underlying
    /// [`Cursor`] data.
    fn parse_slice<PS: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        mut self,
        count: usize,
    ) -> Option<(Self::Slice<PS>, Self)> {
        let (slice, _) =
            <[PS]>::ref_from_prefix_with_elems(ParseCursor::remaining_slice(&self.0), count)
                .ok()?;
        let size = std::mem::size_of_val(&slice);
        let slice = slice.to_owned();
        if self.0.seek_forward(size).is_err() {
            return None;
        }
        Some((slice, self))
    }

    fn deref<'a, D: Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        output: &'a Self::Output<D>,
    ) -> &'a D {
        output
    }

    fn deref_slice<'a, DS: Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        slice: &'a Self::Slice<DS>,
    ) -> &'a [DS] {
        slice
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn into_inner(self) -> Self::Input {
        self.0.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::little_endian as le;

    #[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
    #[repr(C, packed)]
    struct SomeNumbers {
        a: u8,
        b: le::U32,
        c: le::U16,
        d: u8,
    }

    // Ensure that "return parser + parsed output" pattern works on `ByValue`.
    fn do_by_value<
        D: AsRef<[u8]> + Debug + PartialEq,
        T: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned,
    >(
        data: D,
    ) -> (T, ByValue<D>) {
        let parser = ByValue::new(data);
        parser.parse::<T>().expect("some numbers")
    }
    fn do_slice_by_value<
        D: AsRef<[u8]> + Debug + PartialEq,
        T: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned,
    >(
        data: D,
        count: usize,
    ) -> (Vec<T>, ByValue<D>) {
        let parser = ByValue::new(data);
        parser.parse_slice::<T>(count).expect("some numbers")
    }

    #[test]
    fn by_value_cursor_vec_u8() {
        let bytes: Vec<u8> = (0..8).collect();
        let (some_numbers, parser) = do_by_value::<_, SomeNumbers>(bytes);
        assert_eq!(0, some_numbers.a);
        assert_eq!(7, some_numbers.d);
        assert_eq!(8, parser.0.position());
        assert_eq!(8, parser.into_inner().len());
    }

    #[test]
    fn by_value_slice_u8_parse_slice() {
        let bytes: Vec<u8> = (0..24).collect();
        let (some_numbers, parser) = do_slice_by_value::<_, SomeNumbers>(bytes.as_slice(), 3);
        assert_eq!(3, some_numbers.len());
        assert_eq!(0, some_numbers[0].a);
        assert_eq!(7, some_numbers[0].d);
        assert_eq!(8, some_numbers[1].a);
        assert_eq!(15, some_numbers[1].d);
        assert_eq!(16, some_numbers[2].a);
        assert_eq!(23, some_numbers[2].d);
        assert_eq!(24, parser.into_inner().len());
    }
}
