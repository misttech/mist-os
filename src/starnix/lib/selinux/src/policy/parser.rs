// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

pub type PolicyData = Arc<Vec<u8>>;
pub type PolicyOffset = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct PolicyCursor {
    data: PolicyData,
    offset: PolicyOffset,
}

impl PolicyCursor {
    /// Returns a new [`PolicyCursor`] that wraps `data` in a [`Cursor`] for parsing.
    pub fn new(data: PolicyData) -> Self {
        Self { data, offset: 0 }
    }

    pub fn new_at(data: PolicyData, offset: PolicyOffset) -> Self {
        Self { data, offset }
    }

    /// Returns an `P` as the parsed output of the next bytes in the underlying [`Cursor`] data.
    pub fn parse<P: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned>(
        mut self,
    ) -> Option<(P, Self)> {
        let (output, _) = P::read_from_prefix(self.remaining_slice()).ok()?;
        self.seek_forward(std::mem::size_of_val(&output)).ok()?;
        Some((output, self))
    }

    /// Returns a `Vec<T>` of `count` items as the parsed output of the next bytes in the underlying
    /// [`Cursor`] data.
    pub fn parse_slice<PS: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        mut self,
        count: usize,
    ) -> Option<(Vec<PS>, Self)> {
        let (slice, _) = <[PS]>::ref_from_prefix_with_elems(self.remaining_slice(), count).ok()?;
        let size = std::mem::size_of_val(&slice);
        let slice = slice.to_owned();
        self.seek_forward(size).ok()?;
        Some((slice, self))
    }

    pub fn offset(&self) -> PolicyOffset {
        self.offset
    }

    pub fn len(&self) -> usize {
        self.data.len() - self.offset as usize
    }

    /// Returns a slice of remaining data.
    fn remaining_slice(&self) -> &[u8] {
        let s: &[u8] = self.data.as_ref();
        let p = self.offset as usize;
        &s[p..]
    }

    /// Seeks forward by `num_bytes`, returning a `std::io::Error` if seeking fails.
    pub fn seek_forward(&mut self, num_bytes: usize) -> Result<(), std::io::Error> {
        if num_bytes > self.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        self.offset += num_bytes as PolicyOffset;
        Ok(())
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
        let parser = PolicyCursor::new(Arc::new(data));
        parser.parse::<T>().expect("some numbers")
    }
    fn do_slice_by_value<T: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned>(
        data: Vec<u8>,
        count: usize,
    ) -> (Vec<T>, PolicyCursor) {
        let parser = PolicyCursor::new(Arc::new(data));
        parser.parse_slice::<T>(count).expect("some numbers")
    }

    #[test]
    fn by_value_cursor_vec_u8() {
        let bytes: Vec<u8> = (0..8).collect();
        let (some_numbers, parser) = do_by_value::<SomeNumbers>(bytes);
        assert_eq!(0, some_numbers.a);
        assert_eq!(7, some_numbers.d);
        assert_eq!(8, parser.offset);
        assert_eq!(8, parser.data.len());
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
        assert_eq!(24, parser.data.len());
    }
}
