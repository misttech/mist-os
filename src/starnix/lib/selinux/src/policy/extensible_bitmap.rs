// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::error::ValidateError;
use super::parser::ParseStrategy;
use super::{array_type, array_type_validate_deref_both, Array, Counted, Validate, ValidateArray};

use std::cmp::Ordering;
use std::fmt::Debug;
use std::mem;
use zerocopy::{little_endian as le, FromBytes, Immutable, KnownLayout, Unaligned};

/// Maximum number of [`MapItem`] objects in a single [`ExtensibleBitmap`].
pub(super) const MAX_BITMAP_ITEMS: u32 = 0x40;

/// Fixed expectation for number of bits per [`MapItem`] in every [`ExtensibleBitmap`].
pub(super) const MAP_NODE_BITS: u32 = 8 * mem::size_of::<u64>() as u32;

array_type!(ExtensibleBitmap, PS, PS::Output<Metadata>, PS::Slice<MapItem>);

array_type_validate_deref_both!(ExtensibleBitmap);

// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
impl<PS: ParseStrategy> ExtensibleBitmap<PS> {
    /// Returns the number of bits described by this [`ExtensibleBitmap`].
    pub fn num_elements(&self) -> u32 {
        self.high_bit()
    }

    /// Returns the number of 1-bits in this [`ExtensibleBitmap`].
    pub fn num_one_bits(&self) -> usize {
        PS::deref_slice(&self.data).iter().map(|item| item.map.get().count_ones() as usize).sum()
    }

    /// Returns whether the `index`'th bit in this bitmap is a 1-bit.
    pub fn is_set(&self, index: u32) -> bool {
        if index > self.high_bit() {
            return false;
        }

        let map_items = PS::deref_slice(&self.data);
        if let Ok(i) = map_items.binary_search_by(|map_item| self.item_ordering(map_item, index)) {
            let map_item = &map_items[i];
            let item_index = index - map_item.start_bit.get();
            return map_item.map.get() & (1 << item_index) != 0;
        }

        false
    }

    /// Returns an iterator that returns a set of spans of continuous set bits.
    /// Each span consists of inclusive low and high bit indexes (i.e. zero-based).
    pub fn spans<'a>(&'a self) -> ExtensibleBitmapSpansIterator<'a, PS> {
        ExtensibleBitmapSpansIterator::<'a, PS> { bitmap: self, map_item: 0, next_bit: 0 }
    }

    /// Returns the next bit after the bits in this [`ExtensibleBitmap`]. That is, the bits in this
    /// [`ExtensibleBitmap`] may be indexed by the range `[0, Self::high_bit())`.
    fn high_bit(&self) -> u32 {
        PS::deref(&self.metadata).high_bit.get()
    }

    /// Returns the number of [`MapItem`] objects that would be needed to directly encode all bits
    /// in this [`ExtensibleBitmap`]. Note that, in practice, every [`MapItem`] that would contain
    /// all 0-bits in such an encoding is not stored internally.
    fn count(&self) -> u32 {
        PS::deref(&self.metadata).count.get()
    }

    fn item_ordering(&self, map_item: &MapItem, index: u32) -> Ordering {
        let map_item_start_bit = map_item.start_bit.get();
        if map_item_start_bit > index {
            Ordering::Greater
        } else if map_item_start_bit + PS::deref(&self.metadata).map_item_size_bits.get() <= index {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }
}

/// Describes the indexes of a span of "true" bits in an `ExtensibleBitmap`.
/// Low and high values are inclusive, such that when `low==high`, the span consists
/// of a single bit.
#[derive(Debug, PartialEq)]
pub(super) struct ExtensibleBitmapSpan {
    pub low: u32,
    pub high: u32,
}

/// Iterator returned by `ExtensibleBitmap::spans()`.
pub(super) struct ExtensibleBitmapSpansIterator<'a, PS: ParseStrategy> {
    bitmap: &'a ExtensibleBitmap<PS>,
    map_item: usize, // Zero-based `Vec<MapItem>` index.
    next_bit: u32,   // Zero-based bit index within the bitmap.
}

impl<PS: ParseStrategy> ExtensibleBitmapSpansIterator<'_, PS> {
    /// Returns the zero-based index of the next bit with the specified value, if any.
    fn next_bit_with_value(&mut self, is_set: bool) -> Option<u32> {
        let map_item_size_bits = PS::deref(&self.bitmap.metadata).map_item_size_bits.get();
        let num_elements = self.bitmap.num_elements();

        while self.next_bit < num_elements {
            let (start_bit, map) = PS::deref_slice(&self.bitmap.data)
                .get(self.map_item)
                .map_or((num_elements, 0), |item| (item.start_bit.get(), item.map.get()));

            if start_bit > self.next_bit {
                if is_set {
                    // Skip the implicit "false" bits, to the next `MapItem`.
                    self.next_bit = start_bit
                } else {
                    return Some(self.next_bit);
                }
            } else {
                // Scan the `MapItem` for the next matching bit.
                let next_map_item_bit = self.next_bit - start_bit;
                for map_bit in next_map_item_bit..map_item_size_bits {
                    if ((map & (1 << map_bit)) != 0) == is_set {
                        self.next_bit = start_bit + map_bit;
                        return Some(self.next_bit);
                    }
                }

                // Move on to the next `MapItem`, which may not be contiguous with
                // this one.
                self.next_bit = start_bit + map_item_size_bits;
                self.map_item += 1;
            }
        }

        None
    }
}

impl<PS: ParseStrategy> Iterator for ExtensibleBitmapSpansIterator<'_, PS> {
    type Item = ExtensibleBitmapSpan;

    /// Returns the next span of at least one bit set in the bitmap.
    fn next(&mut self) -> Option<Self::Item> {
        let low = self.next_bit_with_value(true)?;
        // End the span at the bit preceding either the next false bit, or the end of the bitmap.
        let high =
            self.next_bit_with_value(false).unwrap_or_else(|| self.bitmap.num_elements()) - 1;
        Some(Self::Item { low, high })
    }
}

impl<PS: ParseStrategy> Validate for Vec<ExtensibleBitmap<PS>> {
    type Error = <ExtensibleBitmap<PS> as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        for extensible_bitmap in self.iter() {
            extensible_bitmap.validate()?;
        }

        Ok(())
    }
}

impl Validate for Metadata {
    type Error = ValidateError;

    /// Validates that [`ExtensibleBitmap`] metadata is internally consistent with data
    /// representation assumptions.
    fn validate(&self) -> Result<(), Self::Error> {
        // Only one size for `MapItem` instances is supported.
        let found_size = self.map_item_size_bits.get();
        if found_size != MAP_NODE_BITS {
            return Err(ValidateError::InvalidExtensibleBitmapItemSize { found_size });
        }

        // High bit must be `MapItem` size-aligned.
        let found_high_bit = self.high_bit.get();
        if found_high_bit % found_size != 0 {
            return Err(ValidateError::MisalignedExtensibleBitmapHighBit {
                found_size,
                found_high_bit,
            });
        }

        // Count and high bit must be consistent.
        let found_count = self.count.get();
        if found_count * found_size > found_high_bit {
            return Err(ValidateError::InvalidExtensibleBitmapHighBit {
                found_size,
                found_high_bit,
                found_count,
            });
        }
        if found_count > MAX_BITMAP_ITEMS {
            return Err(ValidateError::InvalidExtensibleBitmapCount { found_count });
        }
        if found_high_bit != 0 && found_count == 0 {
            return Err(ValidateError::ExtensibleBitmapNonZeroHighBitAndZeroCount);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct Metadata {
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

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct MapItem {
    /// The first bit that this [`MapItem`] stores, relative to its [`ExtensibleBitmap`] range:
    /// `[0, extensible_bitmap.high_bit())`.
    start_bit: le::U32,
    /// The bitmap data for this [`MapItem`].
    map: le::U64,
}

impl Validate for [MapItem] {
    type Error = anyhow::Error;

    /// All [`MapItem`] validation requires access to [`Metadata`]; validation performed in
    /// `ExtensibleBitmap<PS>::validate()`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> ValidateArray<Metadata, MapItem> for ExtensibleBitmap<PS> {
    type Error = anyhow::Error;

    /// Validates that `metadata` and `data` are internally consistent. [`MapItem`] objects are
    /// expected to be stored in ascending order (by `start_bit`), and their bit ranges must fall
    /// within the range `[0, metadata.high_bit())`.
    fn validate_array<'a>(metadata: &'a Metadata, data: &'a [MapItem]) -> Result<(), Self::Error> {
        let found_size = metadata.map_item_size_bits.get();
        let found_high_bit = metadata.high_bit.get();

        // `MapItem` objects must be in sorted order, each with a `MapItem` size-aligned starting bit.
        //
        // Note: If sorted order assumption is violated `ExtensibleBitmap::binary_search_items()` will
        // misbehave and `ExtensibleBitmap` will need to be refactored accordingly.
        let mut min_start: u32 = 0;
        for map_item in data.iter() {
            let found_start_bit = map_item.start_bit.get();
            if found_start_bit % found_size != 0 {
                return Err(ValidateError::MisalignedExtensibleBitmapItemStartBit {
                    found_start_bit,
                    found_size,
                }
                .into());
            }
            if found_start_bit < min_start {
                return Err(ValidateError::OutOfOrderExtensibleBitmapItems {
                    found_start_bit,
                    min_start,
                }
                .into());
            }
            min_start = found_start_bit + found_size;
        }

        // Last `MapItem` object may not include bits beyond (and including) high bit value.
        if min_start > found_high_bit {
            return Err(ValidateError::ExtensibleBitmapItemOverflow {
                found_items_end: min_start,
                found_high_bit,
            }
            .into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::error::ParseError;
    use crate::policy::parser::{ByRef, ByValue};
    use crate::policy::testing::{as_parse_error, as_validate_error};
    use crate::policy::Parse;

    use std::borrow::Borrow;
    use std::marker::PhantomData;

    macro_rules! parse_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = $data;
            fn check_by_ref<'a>(
                $result: Result<
                    ($parse_output<ByRef<&'a [u8]>>, ByRef<&'a [u8]>),
                    <$parse_output<ByRef<&'a [u8]>> as crate::policy::Parse<ByRef<&'a [u8]>>>::Error,
                >,
            ) {
                $check_impl;
            }

            fn check_by_value(
                $result: Result<
                    ($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>),
                    <$parse_output<ByValue<Vec<u8>>> as crate::policy::Parse<ByValue<Vec<u8>>>>::Error,
                >,
            ) -> Option<($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>)> {
                $check_impl
            }

            let by_ref = ByRef::new(data.as_slice());
            let by_ref_result = $parse_output::parse(by_ref);
            check_by_ref(by_ref_result);
            let by_value_result = $parse_output::<ByValue<Vec<u8>>>::parse(ByValue::new(data));
            let _ = check_by_value(by_value_result);
        }};
    }

    pub(in super::super) struct ExtensibleBitmapIterator<
        PS: ParseStrategy,
        B: Borrow<ExtensibleBitmap<PS>>,
    > {
        extensible_bitmap: B,
        i: u32,
        _marker: PhantomData<PS>,
    }

    impl<PS: ParseStrategy, B: Borrow<ExtensibleBitmap<PS>>> Iterator
        for ExtensibleBitmapIterator<PS, B>
    {
        type Item = bool;

        fn next(&mut self) -> Option<Self::Item> {
            if self.i >= self.extensible_bitmap.borrow().high_bit() {
                return None;
            }
            let value = self.extensible_bitmap.borrow().is_set(self.i);
            self.i = self.i + 1;
            Some(value)
        }
    }

    impl<PS: ParseStrategy> IntoIterator for ExtensibleBitmap<PS> {
        type Item = bool;
        type IntoIter = ExtensibleBitmapIterator<PS, ExtensibleBitmap<PS>>;

        fn into_iter(self) -> Self::IntoIter {
            ExtensibleBitmapIterator { extensible_bitmap: self, i: 0, _marker: PhantomData }
        }
    }

    impl<PS: ParseStrategy> ExtensibleBitmap<PS> {
        fn iter(&self) -> ExtensibleBitmapIterator<PS, &ExtensibleBitmap<PS>> {
            ExtensibleBitmapIterator { extensible_bitmap: self, i: 0, _marker: PhantomData }
        }
    }

    #[test]
    fn extensible_bitmap_simple() {
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                MAP_NODE_BITS.to_le_bytes().as_slice(), // high bit for 1-item bitmap
                (1 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 1-item bitmap
                (0 as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                (1 as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());
                let mut count: u32 = 0;
                for (i, bit) in extensible_bitmap.iter().enumerate() {
                    assert!((i == 0 && bit) || (i > 0 && !bit));
                    count = count + 1;
                }
                assert_eq!(MAP_NODE_BITS, count);
                Some((extensible_bitmap, tail))
            }
        );
    }

    #[test]
    fn extensible_bitmap_sparse_two_item() {
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries  in 2-item bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());
                for i in 0..(MAP_NODE_BITS * 10) {
                    let expected = i == ((MAP_NODE_BITS * 2) + 2) || i == ((MAP_NODE_BITS * 7) + 7);
                    assert_eq!(expected, extensible_bitmap.is_set(i));
                }

                let mut count: u32 = 0;
                for (i, bit) in extensible_bitmap.iter().enumerate() {
                    let expected = i == (((MAP_NODE_BITS * 2) + 2) as usize)
                        || i == (((MAP_NODE_BITS * 7) + 7) as usize);
                    assert_eq!(expected, bit);
                    count = count + 1;
                }
                assert_eq!(MAP_NODE_BITS * 10, count);
                Some((extensible_bitmap, tail))
            }
        );
    }

    #[test]
    fn extensible_bitmap_sparse_malformed() {
        parse_test!(
            ExtensibleBitmap,
            [
                (MAP_NODE_BITS - 1).to_le_bytes().as_slice(), // invalid bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (parsed, tail) = result.expect("parsed");
                assert_eq!(0, tail.len());
                assert_eq!(
                    Err(ValidateError::InvalidExtensibleBitmapItemSize {
                        found_size: MAP_NODE_BITS - 1
                    }),
                    parsed.validate().map_err(as_validate_error)
                );
                Some((parsed, tail))
            }
        );

        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                (((MAP_NODE_BITS * 10) + 1) as u32).to_le_bytes().as_slice(), // invalid high bit for 2-item bitmap
                (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (parsed, tail) = result.expect("parsed");
                assert_eq!(0, tail.len());
                assert_eq!(
                    Err(ValidateError::MisalignedExtensibleBitmapHighBit {
                        found_size: MAP_NODE_BITS,
                        found_high_bit: (MAP_NODE_BITS * 10) + 1
                    }),
                    parsed.validate().map_err(as_validate_error),
                );
                Some((parsed, tail))
            }
        );

        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (11 as u32).to_le_bytes().as_slice(), // invalid count of `MapItem` entries in 2-item bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                match result.err().map(Into::<anyhow::Error>::into).map(as_parse_error) {
                    // `ByRef` attempts to read large slice.
                    Some(ParseError::MissingSliceData {
                        type_name,
                        type_size,
                        num_items: 11,
                        num_bytes: 24,
                    }) => {
                        assert_eq!(std::any::type_name::<MapItem>(), type_name);
                        assert_eq!(std::mem::size_of::<MapItem>(), type_size);
                    }
                    // `ByValue` attempts to read `Vec` one item at a time.
                    Some(ParseError::MissingData { type_name, type_size, num_bytes: 0 }) => {
                        assert_eq!(std::any::type_name::<MapItem>(), type_name);
                        assert_eq!(std::mem::size_of::<MapItem>(), type_size);
                    }
                    v => {
                        panic!(
                            "Expected Some({:?}) or Some({:?}), but got {:?}",
                            ParseError::MissingSliceData {
                                type_name: std::any::type_name::<MapItem>(),
                                type_size: std::mem::size_of::<MapItem>(),
                                num_items: 11,
                                num_bytes: 24,
                            },
                            ParseError::MissingData {
                                type_name: std::any::type_name::<MapItem>(),
                                type_size: std::mem::size_of::<MapItem>(),
                                num_bytes: 0,
                            },
                            v
                        );
                    }
                };
                None::<(ExtensibleBitmap<ByValue<Vec<u8>>>, ByValue<Vec<u8>>)>
            }
        );

        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
                (((MAP_NODE_BITS * 2) + 1) as u32).to_le_bytes().as_slice(), // invalid start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (parsed, tail) = result.expect("parsed");
                assert_eq!(0, tail.len());
                match parsed.validate().map_err(as_validate_error) {
                    Err(ValidateError::MisalignedExtensibleBitmapItemStartBit {
                        found_start_bit,
                        ..
                    }) => {
                        assert_eq!((MAP_NODE_BITS * 2) + 1, found_start_bit);
                    }
                    parse_err => {
                        assert!(
                            false,
                            "Expected Err(MisalignedExtensibleBitmapItemStartBit...), but got {:?}",
                            parse_err
                        );
                    }
                }
                Some((parsed, tail))
            }
        );

        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (2 as u32).to_le_bytes().as_slice(), // count of `MapItem` entries in 2-item bitmap
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // out-of-order start bit for `MapItem` 0
                ((1 << 7) as u64).to_le_bytes().as_slice(),            // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // out-of-order start bit for `MapItem` 1
                ((1 << 2) as u64).to_le_bytes().as_slice(),            // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (parsed, tail) = result.expect("parsed");
                assert_eq!(0, tail.len());
                assert_eq!(
                    parsed.validate().map_err(as_validate_error),
                    Err(ValidateError::OutOfOrderExtensibleBitmapItems {
                        found_start_bit: MAP_NODE_BITS * 2,
                        min_start: (MAP_NODE_BITS * 7) + MAP_NODE_BITS,
                    })
                );
                Some((parsed, tail))
            }
        );

        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for 2-item bitmap
                (3 as u32).to_le_bytes().as_slice(), // invalid count of `MapItem` entries in 2-item bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                match result.err().map(Into::<anyhow::Error>::into).map(as_parse_error) {
                    // `ByRef` attempts to read large slice.
                    Some(ParseError::MissingSliceData {
                        type_name,
                        type_size,
                        num_items: 3,
                        num_bytes,
                    }) => {
                        assert_eq!(std::any::type_name::<MapItem>(), type_name);
                        assert_eq!(std::mem::size_of::<MapItem>(), type_size);
                        assert_eq!(2 * std::mem::size_of::<MapItem>(), num_bytes);
                    }
                    // `ByValue` attempts to read `Vec` one item at a time.
                    Some(ParseError::MissingData { type_name, type_size, num_bytes: 0 }) => {
                        assert_eq!(std::any::type_name::<MapItem>(), type_name);
                        assert_eq!(std::mem::size_of::<MapItem>(), type_size);
                    }
                    parse_err => {
                        assert!(
                            false,
                            "Expected Some({:?}) or Some({:?}), but got {:?}",
                            ParseError::MissingSliceData {
                                type_name: std::any::type_name::<MapItem>(),
                                type_size: std::mem::size_of::<MapItem>(),
                                num_items: 3,
                                num_bytes: 2 * std::mem::size_of::<MapItem>(),
                            },
                            ParseError::MissingData {
                                type_name: std::any::type_name::<MapItem>(),
                                type_size: std::mem::size_of::<MapItem>(),
                                num_bytes: 0
                            },
                            parse_err
                        );
                    }
                };
                None::<(ExtensibleBitmap<ByValue<Vec<u8>>>, ByValue<Vec<u8>>)>
            }
        );
    }

    #[test]
    fn extensible_bitmap_spans_iterator() {
        type Span = ExtensibleBitmapSpan;

        // Single- and multi-bit spans.
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for bitmap
                (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in bitmap
                ((MAP_NODE_BITS * 2) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 << 2) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 7) | (1 << 8) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());

                let mut iterator = extensible_bitmap.spans();
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 2) + 2, high: (MAP_NODE_BITS * 2) + 2 })
                );
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 7) + 7, high: (MAP_NODE_BITS * 7) + 8 })
                );
                assert_eq!(iterator.next(), None);

                Some((extensible_bitmap, tail))
            }
        );

        // Multi-bit span that straddles two `MapItem`s.
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for bitmap
                (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in bitmap
                ((MAP_NODE_BITS * 6) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                ((1 as u64) << 63).to_le_bytes().as_slice(), // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                ((1 << 0) | (1 << 1) as u64).to_le_bytes().as_slice(), // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());

                let mut iterator = extensible_bitmap.spans();
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 6) + 63, high: (MAP_NODE_BITS * 7) + 1 })
                );
                assert_eq!(iterator.next(), None);

                Some((extensible_bitmap, tail))
            }
        );

        // Multi-bit spans of full `MapItem`s, separated by an implicit span of false bits,
        // and with further implicit spans of false bits at the end.
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for bitmap
                (2 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries in bitmap
                ((MAP_NODE_BITS * 5) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                (u64::MAX).to_le_bytes().as_slice(),    // bit values for `MapItem` 0
                ((MAP_NODE_BITS * 7) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 1
                (u64::MAX).to_le_bytes().as_slice(),    // bit values for `MapItem` 1
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());

                let mut iterator = extensible_bitmap.spans();
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 5), high: (MAP_NODE_BITS * 6) - 1 })
                );
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 7), high: (MAP_NODE_BITS * 8) - 1 })
                );
                assert_eq!(iterator.next(), None);

                Some((extensible_bitmap, tail))
            }
        );

        // Span reaching the end of the bitmap is handled correctly.
        parse_test!(
            ExtensibleBitmap,
            [
                MAP_NODE_BITS.to_le_bytes().as_slice(), // bits per node
                ((MAP_NODE_BITS * 10) as u32).to_le_bytes().as_slice(), // high bit for bitmap
                (1 as u32).to_le_bytes().as_slice(),    // count of `MapItem` entries  in bitmap
                ((MAP_NODE_BITS * 9) as u32).to_le_bytes().as_slice(), // start bit for `MapItem` 0
                (u64::MAX).to_le_bytes().as_slice(),    // bit values for `MapItem` 0
            ]
            .concat(),
            result,
            {
                let (extensible_bitmap, tail) = result.expect("parse");
                assert_eq!(0, tail.len());

                let mut iterator = extensible_bitmap.spans();
                assert_eq!(
                    iterator.next(),
                    Some(Span { low: (MAP_NODE_BITS * 9), high: (MAP_NODE_BITS * 10) - 1 })
                );
                assert_eq!(iterator.next(), None);

                Some((extensible_bitmap, tail))
            }
        );
    }
}
