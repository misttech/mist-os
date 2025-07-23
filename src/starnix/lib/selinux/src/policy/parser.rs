// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::io::{Cursor, Seek as _, SeekFrom};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

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

#[derive(Clone, Debug, PartialEq)]
pub struct PolicyCursor(Cursor<Vec<u8>>);

impl PolicyCursor {
    /// Returns a new [`PolicyCursor`] that wraps `data` in a [`Cursor`] for parsing.
    pub fn new(data: Vec<u8>) -> Self {
        Self(Cursor::new(data))
    }

    /// Returns an `P` as the parsed output of the next bytes in the underlying [`Cursor`] data.
    pub fn parse<P: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        mut self,
    ) -> Option<(P, Self)> {
        let (output, _) = P::read_from_prefix(ParseCursor::remaining_slice(&self.0)).ok()?;
        if self.0.seek_forward(std::mem::size_of_val(&output)).is_err() {
            return None;
        }
        Some((output, self))
    }

    /// Returns a `Vec<T>` of `count` items as the parsed output of the next bytes in the underlying
    /// [`Cursor`] data.
    pub fn parse_slice<PS: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        mut self,
        count: usize,
    ) -> Option<(Vec<PS>, Self)> {
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

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn into_inner(self) -> Vec<u8> {
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

    // Ensure that "return parser + parsed output" pattern works on `PolicyCursor`.
    fn do_by_value<
        T: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned,
    >(
        data: Vec<u8>,
    ) -> (T, PolicyCursor) {
        let parser = PolicyCursor::new(data);
        parser.parse::<T>().expect("some numbers")
    }
    fn do_slice_by_value<T: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        data: Vec<u8>,
        count: usize,
    ) -> (Vec<T>, PolicyCursor) {
        let parser = PolicyCursor::new(data);
        parser.parse_slice::<T>(count).expect("some numbers")
    }

    #[test]
    fn by_value_cursor_vec_u8() {
        let bytes: Vec<u8> = (0..8).collect();
        let (some_numbers, parser) = do_by_value::<SomeNumbers>(bytes);
        assert_eq!(0, some_numbers.a);
        assert_eq!(7, some_numbers.d);
        assert_eq!(8, parser.0.position());
        assert_eq!(8, parser.into_inner().len());
    }

    #[test]
    fn by_value_slice_u8_parse_slice() {
        let bytes: Vec<u8> = (0..24).collect();
        let (some_numbers, parser) = do_slice_by_value::<SomeNumbers>(bytes, 3);
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
