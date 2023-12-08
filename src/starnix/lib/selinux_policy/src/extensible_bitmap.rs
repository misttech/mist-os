// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{error::ParseError, Array, Counted, Validate};

use std::{cmp::Ordering, fmt::Debug, mem, ops::Deref as _};
use zerocopy::{little_endian as le, AsBytes, ByteSlice, FromBytes, FromZeroes, Ref, Unaligned};

/// Maximum number of [`MapItem`] objects in a single [`ExtensibleBitmap`].
pub(crate) const MAX_BITMAP_ITEMS: u32 = 0x40;

/// Fixed expectation for number of bits per [`MapItem`] in every [`ExtensibleBitmap`].
pub(crate) const MAP_NODE_BITS: u32 = 8 * mem::size_of::<u64>() as u32;

/// An [`ExtensibleBitmap`] is an array of [`Metadata`] describing the bitmap followed by a slice
/// of [`MapItem`] objects.
pub(crate) type ExtensibleBitmap<B> = Array<B, Ref<B, Metadata>, Ref<B, [MapItem]>>;

// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
impl<B: ByteSlice + Debug + PartialEq> ExtensibleBitmap<B> {
    /// Returns the number of bits described by this [`ExtensibleBitmap`].
    pub fn num_elements(&self) -> u32 {
        self.high_bit()
    }

    /// Returns the number of 1-bits in this [`ExtensibleBitmap`].
    pub fn num_one_bits(&self) -> usize {
        self.data.deref().iter().map(|item| item.map.get().count_ones() as usize).sum()
    }

    /// Returns whether the `index`'th bit in this bitmap is a 1-bit.
    pub fn is_set(&self, index: u32) -> bool {
        if index > self.high_bit() {
            return false;
        }

        let map_items = self.data.deref();
        if let Ok(i) = map_items.binary_search_by(|map_item| self.item_ordering(map_item, index)) {
            let map_item = &map_items[i];
            let item_index = index - map_item.start_bit.get();
            return map_item.map.get() & (1 << item_index) != 0;
        }

        false
    }

    /// Returns the next bit after the bits in this [`ExtensibleBitmap`]. That is, the bits in this
    /// [`ExtensibleBitmap`] may be indexed by the range `[0, Self::high_bit())`.
    fn high_bit(&self) -> u32 {
        self.metadata.deref().high_bit.get()
    }

    /// Returns the number of [`MapItem`] objects that would be needed to directly encode all bits
    /// in this [`ExtensibleBitmap`]. Note that, in practice, every [`MapItem`] that would contain
    /// all 0-bits in such an encoding is not stored internally.
    fn count(&self) -> u32 {
        self.metadata.deref().count.get()
    }

    fn item_ordering(&self, map_item: &MapItem, index: u32) -> Ordering {
        let map_item_start_bit = map_item.start_bit.get();
        if map_item_start_bit > index {
            Ordering::Greater
        } else if map_item_start_bit + self.metadata.deref().map_item_size_bits.get() <= index {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }
}

impl Validate for Metadata {
    type Error = ParseError;

    /// Validates that [`ExtensibleBitmap`] metadata is internally consistent with data
    /// representation assumptions.
    fn validate(&self) -> Result<(), Self::Error> {
        // Only one size for `MapItem` instances is supported.
        let found_size = self.map_item_size_bits.get();
        if found_size != MAP_NODE_BITS {
            return Err(ParseError::InvalidExtensibleBitmapItemSize { found_size });
        }

        // High bit must be `MapItem` size-aligned.
        let found_high_bit = self.high_bit.get();
        if found_high_bit % found_size != 0 {
            return Err(ParseError::MisalignedExtensibleBitmapHighBit {
                found_size,
                found_high_bit,
            });
        }

        // Count and high bit must be consistent.
        let found_count = self.count.get();
        if found_count * found_size > found_high_bit {
            return Err(ParseError::InvalidExtensibleBitmapHighBit {
                found_size,
                found_high_bit,
                found_count,
            });
        }
        if found_count > MAX_BITMAP_ITEMS {
            return Err(ParseError::InvalidExtensibleBitmapCount { found_count });
        }
        if found_high_bit != 0 && found_count == 0 {
            return Err(ParseError::ExtensibleBitmapNonZeroHighBitAndZeroCount);
        }

        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Metadata {
    /// How many bits on each `MapItem`.
    map_item_size_bits: le::U32,
    /// Highest bit, non-inclusive.
    high_bit: le::U32,
    /// The number of map items.
    count: le::U32,
}

impl Counted for Metadata {
    /// The number of [`MapItem`] objects that follow a [`Metadata`] is the value stored in the
    /// `metadata.count` field.
    fn count(&self) -> u32 {
        self.count.get()
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct MapItem {
    /// The first bit that this [`MapItem`] stores, relative to its [`ExtensibleBitmap`] range:
    /// `[0, extensible_bitmap.high_bit())`.
    start_bit: le::U32,
    /// The bitmap data for this [`MapItem`].
    map: le::U64,
}

impl Validate for MapItem {
    type Error = ParseError;

    /// All [`MapItem`] validation requires access to [`Metadata`]; validation performed in
    /// `ExtensibleBitmap<B>::validate()`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [MapItem] {
    type Error = ParseError;

    /// All [`MapItem`] validation requires access to [`Metadata`]; validation performed in
    /// `ExtensibleBitmap<B>::validate()`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for ExtensibleBitmap<B> {
    type Error = ParseError;

    /// Validates that `metadata` and `data` are internally consistent. [`MapItem`] objects are
    /// expected to be stored in ascending order (by `start_bit`), and their bit ranges must fall
    /// within the range `[0, self.high_bit())`.
    fn validate(&self) -> Result<(), Self::Error> {
        let found_size = self.metadata.deref().map_item_size_bits.get();
        let found_high_bit = self.metadata.deref().high_bit.get();

        // `MapItem` objects must be in sorted order, each with a `MapItem` size-aligned starting bit.
        //
        // Note: If sorted order assumption is violated `ExtensibleBitmap::binary_search_items()` will
        // misbehave and `ExtensibleBitmap` will need to be refactored accordingly.
        let mut min_start: u32 = 0;
        for map_item in self.data.deref().iter() {
            let found_start_bit = map_item.start_bit.get();
            if found_start_bit % found_size != 0 {
                return Err(ParseError::MisalignedExtensibleBitmapItemStartBit {
                    found_start_bit,
                    found_size,
                });
            }
            if found_start_bit < min_start {
                return Err(ParseError::OutOfOrderExtensibleBitmapItems {
                    found_start_bit,
                    min_start,
                });
            }
            min_start = found_start_bit + found_size;
        }

        // Last `MapItem` object may not include bits beyond (and including) high bit value.
        if min_start > found_high_bit {
            return Err(ParseError::ExtensibleBitmapItemOverflow {
                found_items_end: min_start,
                found_high_bit,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{test::as_parse_error, Parse as _},
        *,
    };

    pub struct ExtensibleBitmapIterator<B: ByteSlice + Debug + PartialEq> {
        extensible_bitmap: ExtensibleBitmap<B>,
        i: u32,
    }

    impl<B: ByteSlice + Debug + PartialEq> Iterator for ExtensibleBitmapIterator<B> {
        type Item = bool;

        fn next(&mut self) -> Option<Self::Item> {
            if self.i >= self.extensible_bitmap.high_bit() {
                return None;
            }
            let value = self.extensible_bitmap.is_set(self.i);
            self.i = self.i + 1;
            Some(value)
        }
    }

    impl<B: ByteSlice + Debug + PartialEq> IntoIterator for ExtensibleBitmap<B> {
        type Item = bool;
        type IntoIter = ExtensibleBitmapIterator<B>;

        fn into_iter(self) -> Self::IntoIter {
            ExtensibleBitmapIterator { extensible_bitmap: self, i: 0 }
        }
    }

    #[test]
    fn extensible_bitmap_simple() {
        let bytes = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            MAP_NODE_BITS.to_le_bytes().as_slice(), // high bit for 1-item bitmap
            (1 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in 1-item bitmap
            (0 as u32).to_le_bytes().as_slice(),    // start bit for `MapItem` 0
            (1 as u64).to_le_bytes().as_slice(),    // bit values for `MapItem` 0
        ]
        .concat();
        let (extensible_bitmap, tail) = ExtensibleBitmap::parse(bytes.as_slice()).expect("parse");
        assert_eq!(0, tail.len());
        let mut count: u32 = 0;
        for (i, bit) in extensible_bitmap.into_iter().enumerate() {
            assert!((i == 0 && bit) || (i > 0 && !bit));
            count = count + 1;
        }
        assert_eq!(MAP_NODE_BITS, count);
    }

    #[test]
    fn extensible_bitmap_sparse_two_item() {
        let bytes = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries  in 2-item bitmap
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        let (extensible_bitmap, tail) = ExtensibleBitmap::parse(bytes.as_slice()).expect("parse");
        assert_eq!(0, tail.len());
        for i in 0..(MAP_NODE_BITS * 10) {
            let expected = i == ((MAP_NODE_BITS * 2) + 2) || i == ((MAP_NODE_BITS * 7) + 7);
            assert_eq!(expected, extensible_bitmap.is_set(i));
        }

        let mut count: u32 = 0;
        for (i, bit) in extensible_bitmap.into_iter().enumerate() {
            let expected = i == (((MAP_NODE_BITS * 2) + 2) as usize)
                || i == (((MAP_NODE_BITS * 7) + 7) as usize);
            assert_eq!(expected, bit);
            count = count + 1;
        }
        assert_eq!(MAP_NODE_BITS * 10, count);
    }

    #[test]
    fn extensible_bitmap_sparse_malformed() {
        let malformed = [
            (MAP_NODE_BITS - 1).to_le_bytes().as_slice(), // invalid bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        assert_eq!(
            Some(ParseError::InvalidExtensibleBitmapItemSize { found_size: MAP_NODE_BITS - 1 }),
            ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error),
        );

        let malformed = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            (((MAP_NODE_BITS * 10) + 1) as u32).to_le_bytes().as_slice(), // invalid high bit for 2-item bitmap
            (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        assert_eq!(
            Some(ParseError::MisalignedExtensibleBitmapHighBit {
                found_size: MAP_NODE_BITS,
                found_high_bit: (MAP_NODE_BITS * 10) + 1
            }),
            ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error),
        );

        let malformed = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (11 as u32).to_le_bytes().as_slice(), // invalid count of `MapItem` entries in 2-item bitmap
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        assert_eq!(
            Some(ParseError::InvalidExtensibleBitmapHighBit {
                found_size: MAP_NODE_BITS,
                found_high_bit: MAP_NODE_BITS * 10,
                found_count: 11
            }),
            ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error),
        );

        let malformed = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in 2-item bitmap
            (((MAP_NODE_BITS * 2) + 1) as u32).to_le_bytes().as_slice(), // invalid start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        match ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error) {
            Some(ParseError::MisalignedExtensibleBitmapItemStartBit {
                found_start_bit, ..
            }) => {
                assert_eq!((MAP_NODE_BITS * 2) + 1, found_start_bit);
            }
            parse_err => {
                assert!(
                    false,
                    "Expected Some(MisalignedExtensibleBitmapItemStartBit...), but got {:?}",
                    parse_err
                );
            }
        }

        let malformed = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in 2-item bitmap
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // out-of-order start bit for `MapItem` 0
            ((1 << 7) as u64).to_le_bytes().as_slice(),            // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // out-of-order start bit for `MapItem` 1
            ((1 << 2) as u64).to_le_bytes().as_slice(),            // bit values for `MapItem` 1
        ]
        .concat();
        assert_eq!(
            Some(ParseError::OutOfOrderExtensibleBitmapItems {
                found_start_bit: MAP_NODE_BITS * 2,
                min_start: (MAP_NODE_BITS * 7) + MAP_NODE_BITS
            }),
            ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error),
        );

        let malformed = [
            MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
            ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
            (3 as u32).to_le_bytes().as_slice(), // invalid count of `MapItem` entries in 2-item bitmap
            ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
            ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
            ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
        ]
        .concat();
        match ExtensibleBitmap::parse(malformed.as_slice()).err().map(as_parse_error) {
            Some(ParseError::MissingSliceData {
                type_name,
                type_size,
                num_items: 3,
                num_bytes,
            }) => {
                assert_eq!(std::mem::size_of::<MapItem>(), type_size);
                assert_eq!(2 * std::mem::size_of::<MapItem>(), num_bytes);
                assert!(type_name.contains("MapItem"));
            }
            parse_err => {
                assert!(false, "Expected Some(MissingSliceData...), but got {:?}", parse_err);
            }
        }
    }
}
