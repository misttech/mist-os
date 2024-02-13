// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_store::allocator::strategy::{AllocatorStrategy, LocationHint},
    std::{collections::BTreeSet, ops::Range},
};

/// The maximum exponent used in powers of two buckets.
/// This also caps max allocatable size to `1 << MAX_EXPONENT`.
const MAX_EXPONENT: usize = 28; // 256MiB

/// Tracks powers-of-two buckets of free disk space using a buddy allocator system.
#[derive(Default, Debug)]
pub struct Buddy {
    /// Map of size to a sorted set of offsets where free space can be found.
    /// Bucket index n represents size 1 << (n-1).
    buckets: [BTreeSet<u64>; MAX_EXPONENT],
}

impl Buddy {
    fn aligned_free(&mut self, mut exp: usize, mut offset: u64) {
        while exp < (MAX_EXPONENT - 1) {
            let bytes = 1u64 << exp;
            let buddy_offset = offset ^ bytes;
            let first_offset = offset & !bytes;
            if self.buckets[exp].remove(&buddy_offset) {
                if first_offset == buddy_offset {
                    offset -= bytes;
                }
                exp += 1;
            } else {
                break;
            }
        }
        self.buckets[exp].insert(offset);
    }
}

impl AllocatorStrategy for Buddy {
    fn allocate(&mut self, bytes: u64, hint: LocationHint) -> Option<Range<u64>> {
        assert!(bytes > 0);
        match hint {
            LocationHint::None => {
                // Search for buckets of at least 'bytes' size.
                let mut exp = (64 - (bytes - 1).leading_zeros()) as usize;
                assert!((1u64 << exp) >= bytes);
                while exp < MAX_EXPONENT {
                    if let Some(start) = self.buckets[exp].pop_first() {
                        let range = start..start + (1u64 << exp);
                        if range.start + bytes < range.end {
                            self.free(range.start + bytes..range.end);
                        }
                        return Some(start..start + bytes);
                    }
                    exp += 1;
                }
                // If we get here, we haven't found an extent that is big enough so just return the
                // biggest one we have instead.
                for exp in (0..MAX_EXPONENT).rev() {
                    if let Some(start) = self.buckets[exp].pop_first() {
                        return Some(start..start + (1u64 << exp));
                    }
                }
                None
            }
            LocationHint::Require(start) => {
                let end = start + bytes;
                // Nb: It is possible that a required range is free but made of multiple adjacent
                // buckets of different sizes. Because the use case for this hint is currently only
                // fixed-offset structures at filesystem creation, we just don't support this case.
                //
                // e.g. Assuming 'x' means free:
                //
                //   0 1 2 3 4 5 6 7
                //  |.|x|x x|x x x x|
                //
                // A call to `allocate(4, LocationHint::Require(1))` will fail here because
                // we can't find a single bucket that satisfies 4 bytes at offset 1.
                // The same call would pass if '0' was not allocated because we would then have
                // a single 8 byte bucket from which we could cut the requested 4 bytes.

                // Scan our buckets to see if we can find one that contains 'range'.
                let lower_bound_exp = (64 - (bytes - 1).leading_zeros()) as usize;
                for exp in lower_bound_exp..MAX_EXPONENT {
                    let size = 1u64 << exp;
                    let found = self.buckets[exp]
                        .iter()
                        .filter(|offset| **offset <= start && **offset + size >= end)
                        .next()
                        .map(|i| *i);

                    if let Some(found) = found {
                        let src = found..found + size;
                        self.buckets[exp].remove(&found);
                        // Give back any start/end fragments we don't need.
                        if src.start < start {
                            self.free(src.start..start);
                        }
                        if src.end > end {
                            self.free(end..src.end);
                        }
                        return Some(start..end);
                    }
                }
                None
            }
            LocationHint::NextAvailable(start) => {
                // Holds the exp and offset of the next available allocation after 'start'.
                let mut best = (usize::MAX, u64::MAX);
                for exp in 0..MAX_EXPONENT {
                    let size = 1u64 << exp;
                    if let Some(offset) =
                        self.buckets[exp].range(start.saturating_sub(size - 1)..u64::MAX).next()
                    {
                        if *offset < best.1 {
                            best = (exp, *offset);
                        }
                    }
                }
                let (exp, offset) = best;
                if exp != usize::MAX {
                    let size = 1u64 << exp;
                    let mut range = offset..offset + size;
                    self.buckets[exp].remove(&offset);
                    if range.start < start {
                        // If requested start not start of the range, free start and adjust range.
                        self.free(range.start..start);
                        range.start = start;
                    }
                    let len = range.end - range.start;
                    if len > bytes {
                        self.free(range.start + bytes..range.end);
                        range.end = range.start + bytes;
                    }
                    Some(range)
                } else {
                    None
                }
            }
        }
    }

    fn free(&mut self, mut range: Range<u64>) {
        // We free arbitrary ranges by cutting them into larger and larger aligned powers of two
        // and then freeing those. The aligned ranges then coalesce if possible.
        let mut exp = 0;
        while exp < MAX_EXPONENT && range.end > range.start {
            let bytes = 1u64 << exp;
            if range.start & bytes != 0 {
                self.aligned_free(exp, range.start);
                range.start += bytes;
            }
            if range.end & bytes != 0 {
                self.aligned_free(exp, range.end - bytes);
                range.end -= bytes;
            }
            exp += 1;
        }
        let bytes = 1u64 << (MAX_EXPONENT - 1);
        while range.start < range.end {
            // We can end up here if we are freeing a region larger than the max allocation.
            self.aligned_free(MAX_EXPONENT - 1, range.start);
            range.start += bytes;
        }
        assert_eq!(range.end, range.start);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn insufficient_space() {
        let mut buddy = Buddy::default();
        buddy.free(0..0); // NOOP
        assert_eq!(buddy.allocate(256, LocationHint::None), None);
        buddy.free(0..1024);
        assert_eq!(buddy.allocate(2048, LocationHint::None), Some(0..1024));
        buddy.free(512..1024);
        assert_eq!(buddy.allocate(512, LocationHint::None), Some(512..1024));
        assert_eq!(buddy.allocate(512, LocationHint::None), None);
    }

    #[test]
    fn split_and_merge() {
        let mut buddy = Buddy::default();
        buddy.free(0..1024);
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(0..256));
        assert_eq!(
            format!("{:?}", buddy),
            "Buddy { buckets: [{}, {}, {}, {}, {}, {}, {}, {}, {256}, {512}, {}, {}, {}, \
             {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}] }"
        );
        buddy.free(0..256);
        assert_eq!(
            format!("{:?}", buddy),
            "Buddy { buckets: [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {0}, {}, {}, \
             {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}] }"
        );

        // Try again but hold onto the second allocation.
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(0..256));
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(256..512));
        buddy.free(0..256);
        assert_eq!(
            format!("{:?}", buddy),
            "Buddy { buckets: [{}, {}, {}, {}, {}, {}, {}, {}, {0}, {512}, {}, {}, {}, \
             {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}] }"
        );
        // Free the 'buddy pair' this time.
        buddy.free(256..512);
        assert_eq!(
            format!("{:?}", buddy),
            "Buddy { buckets: [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {0}, {}, {}, \
            {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}] }"
        );
    }

    #[test]
    fn split_repeatedly() {
        let mut buddy = Buddy::default();
        buddy.free(0..1024);
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(0..256));
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(256..512));
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(512..768));
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(768..1024));
        assert_eq!(buddy.allocate(256, LocationHint::None), None);
    }
    #[test]
    fn alignment() {
        let mut buddy = Buddy::default();
        buddy.free(1..384);
        assert_eq!(buddy.allocate(128, LocationHint::None), Some(128..256));
        assert_eq!(buddy.allocate(1, LocationHint::None), Some(1..2));
        // Nb: The following returns less than requested.
        assert_eq!(buddy.allocate(256, LocationHint::None), Some(256..384));
        assert_eq!(buddy.allocate(64, LocationHint::None), Some(64..128));
        assert_eq!(buddy.allocate(32, LocationHint::None), Some(32..64));
        assert_eq!(buddy.allocate(2, LocationHint::None), Some(2..4));
        assert_eq!(buddy.allocate(4, LocationHint::None), Some(4..8));
        assert_eq!(buddy.allocate(8, LocationHint::None), Some(8..16));
        assert_eq!(buddy.allocate(16, LocationHint::None), Some(16..32));
        assert_eq!(buddy.allocate(1, LocationHint::None), None);
    }

    #[test]
    fn hint_require() {
        let mut buddy = Buddy::default();
        buddy.free(0..96);

        // An odd request -- exercise unaligned, fixed offset allocations.
        assert_eq!(buddy.allocate(32, LocationHint::Require(25)), Some(25..57));
        assert_eq!(buddy.allocate(32, LocationHint::Require(25)), None);
        buddy.free(25..57);

        // We currently don't support Require requests that don't fit in one bucket range.
        assert_eq!(buddy.allocate(64, LocationHint::None), Some(0..64));
        buddy.free(1..33);
        assert_eq!(buddy.allocate(32, LocationHint::Require(1)), None);
        buddy.free(0..1);
        buddy.free(33..64);

        // An aligned fixed offset allocation.
        assert_eq!(buddy.allocate(32, LocationHint::Require(32)), Some(32..64));
        assert_eq!(buddy.allocate(32, LocationHint::Require(32)), None);
    }

    #[test]
    fn hint_next_available() {
        // NextAvailable
        let mut buddy = Buddy::default();
        buddy.free(0..96);
        assert_eq!(buddy.allocate(1, LocationHint::NextAvailable(0)), Some(0..1));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(1..2));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(2..4));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(4..8));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(8..16));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(16..31));
        assert_eq!(buddy.allocate(15, LocationHint::NextAvailable(0)), Some(31..32));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), Some(32..64));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), Some(64..96));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), None);
        buddy.free(65..80);
        assert_eq!(buddy.allocate(16, LocationHint::NextAvailable(72)), Some(72..80));
        assert_eq!(buddy.allocate(16, LocationHint::NextAvailable(72)), None);
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(10)), Some(65..66));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), Some(66..68));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), Some(68..72));
        assert_eq!(buddy.allocate(96, LocationHint::NextAvailable(0)), None);
    }

    #[test]
    fn coalescing_free() {
        let mut buddy = Buddy::default();
        // Free some bytes at the start and end.
        buddy.free(0..10);
        buddy.free(20..32);
        // Now free the space in the middle, which should coalesce with ranges on both sides.
        buddy.free(10..20);
        // Confirm that we can allocate one block of 32 bytes. This will fail if coalescing
        // didn't occur.
        assert_eq!(buddy.allocate(32, LocationHint::Require(0)), Some(0..32));
    }

    #[test]
    fn max_allocation() {
        let mut buddy = Buddy::default();
        let max_allocation = 1u64 << (MAX_EXPONENT - 1);
        // Allocations that cross max size should be split into multiple pieces.
        buddy.free(0..max_allocation * 3);
        assert_eq!(buddy.buckets[MAX_EXPONENT - 1].len(), 3);
    }
}
