// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A strategy tracks free space and decides where allocations are to be made.
//!
//! Note that strategy *excludes*:
//!
//!   * get/set byte limits
//!   * reservation tracking of future allocations.
//!   * deallocated but not yet usable regions (awaiting flush).
//!   * Any TRIM-specific logic
//!   * Volume Deletion
//!
//! These sorts of higher-level concepts should be implemented in `Allocator`.
//!
//! Strategies should be concerned only with selecting which free regions of disk to hand out.

use std::{
    collections::{btree_map, BTreeMap, BTreeSet},
    fmt::Debug,
    ops::Range,
};

/// An allocation strategy that returns the smallest extent that is large enough to hold the
/// requested allocation, or the next best if no extent is big enough.
///
/// This strategy either leads to a perfect fit of a free extent to an allocation or a small
/// amount of free space being left over, creating even smaller fragments of free space.
///
/// This tendency to create smaller fragments of free space starts to affect file fragmentation
/// when filesystem use approaches capacity and there are nothing but small fragments of free
/// space remaining.
#[derive(Debug, Default)]
pub struct BestFit {
    /// Holds free storage ranges for the filesystem ordered by length, then start.
    ranges: BTreeMap<u64, BTreeSet<u64>>,
    /// A secondary index used to speed up coalescing of 'free' calls. Keyed by end -> start
    by_end: BTreeMap<u64, u64>,
}

/// Tests (in allocator.rs) may set max extent size to low values for other reasons.
/// We want to make sure that we never go below a value that is big enough for superblocks
/// so the value we use here is chosen to be larger than the value allocator.rs calculates and
/// a convenient power of two.
const DEFAULT_MAX_EXTENT_SIZE: u64 = 2 << 20;

impl BestFit {
    /// Tries to assign a set of contiguous `bytes` and returns the range, removing it
    /// from the pool of available bytes and returning it.
    ///
    /// If insufficient contiguous space is available, the largest available range will
    /// be returned. If no bytes are available, None will be returned.
    ///
    /// There are no special requirements on alignment of `bytes` but the caller is generally
    /// encouraged to align to device block size.
    pub fn allocate(&mut self, bytes: u64) -> Option<Range<u64>> {
        let mut result = self.ranges.range_mut(bytes..).next();
        if result.is_none() {
            // Insufficient space. Return the biggest range we have.
            result = self.ranges.iter_mut().rev().next();
        }
        if let Some((&size, offsets)) = result {
            debug_assert!(!offsets.is_empty());
            let offset = offsets.pop_first().unwrap();
            if offsets.is_empty() {
                self.ranges.remove(&size);
            }
            self.by_end.remove(&(offset + size));
            if size > bytes {
                self.free(offset + bytes..offset + size);
                Some(offset..offset + bytes)
            } else {
                Some(offset..offset + size)
            }
        } else {
            None
        }
    }

    /// Allocates a specific range of the device.
    ///
    /// Used only in allocating space for the first extent of each Fxfs superblock.
    /// (This is the only data only in the filesystem that is stored at a fixed device offset.)
    pub fn allocate_fixed_offset(&mut self, start: u64, bytes: u64) -> Option<Range<u64>> {
        let mut to_remove = vec![];
        let mut cur_end = start;
        let mut first_offset = None;
        let mut prev_end = None;
        for (&end, &offset) in self.by_end.range(start + 1..) {
            if first_offset.is_none() {
                // Check that range covers start.
                if offset > start {
                    return None;
                }
                first_offset = Some(offset);
            }
            if offset > start + bytes {
                break;
            }
            if let Some(prev) = prev_end {
                if prev != offset {
                    // Non-contiguous ranges. Can't allocate.
                    return None;
                }
            }
            prev_end = Some(end);
            cur_end = end;
            to_remove.push((end, offset));
        }
        if cur_end < start + bytes {
            return None;
        }

        for (end, offset) in to_remove.into_iter() {
            self.remove_range(offset..end);
        }

        if let Some(first_offset) = first_offset {
            if first_offset < start {
                self.free(first_offset..start);
            }
            if cur_end > start + bytes {
                self.free(start + bytes..cur_end);
            }
            Some(start..start + bytes)
        } else {
            None
        }
    }

    pub fn allocate_next_available(&mut self, start: u64, bytes: u64) -> Option<Range<u64>> {
        // by_offset is keyed by end, so this will give the first available range that ends after
        // 'start', which is exactly what we need. ;)
        if let Some((&end, &offset)) = self.by_end.range(start + 1..).next() {
            self.remove_range(offset..end);
            let mut range = offset..end;
            if range.start < start {
                self.free(range.start..start);
                range.start = start;
            }
            if range.start + bytes < range.end {
                self.free(range.start + bytes..range.end);
                range.end = range.start + bytes;
            }
            Some(range)
        } else {
            None
        }
    }

    /// Internal helper function. Assumes range exists.
    fn remove_range(&mut self, range: Range<u64>) {
        let btree_map::Entry::Occupied(mut offsets) = self.ranges.entry(range.end - range.start)
        else {
            unreachable!()
        };
        offsets.get_mut().remove(&range.start);
        if offsets.get().is_empty() {
            offsets.remove_entry();
        }
        self.by_end.remove(&range.end);
    }

    /// Adds an arbitrary range of bytes to the pool of available ranges.
    ///
    /// Note that we keep these ranges in a map keyed by their length. To bound the size of this
    /// map we only track ranges up to N blocks long (up to 2MB). Longer ranges
    /// are broken up into ranges of this size.
    pub fn free(&mut self, mut range: Range<u64>) {
        // If there is a free range immediately before this one, merge with it.
        let mut iter = self.by_end.range(range.start..);
        let mut next_item = iter.next();
        if let Some((&end, &start)) = next_item {
            if end == range.start {
                self.remove_range(start..end);
                range.start = start;
                iter = self.by_end.range(range.start + 1..);
                next_item = iter.next();
            }
        }
        // If there is a free range immediately after this one, merge with it.
        if let Some((&end, &start)) = next_item {
            if start == range.end {
                self.remove_range(start..end);
                range.end = end;
            }
        }

        // We don't allow ranges longer than maximum extent size, but if we need to split such
        // a range, we want the smaller fragment to come first (pushing small fragments together at
        // the start of the device).
        while (range.end - range.start) > DEFAULT_MAX_EXTENT_SIZE {
            self.ranges
                .entry(DEFAULT_MAX_EXTENT_SIZE)
                .or_default()
                .insert(range.end - DEFAULT_MAX_EXTENT_SIZE);
            self.by_end.insert(range.end, range.end - DEFAULT_MAX_EXTENT_SIZE);
            range.end -= DEFAULT_MAX_EXTENT_SIZE;
        }
        self.ranges.entry(range.end - range.start).or_default().insert(range.start);
        self.by_end.insert(range.end, range.start);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn allocate() {
        let mut bestfit = BestFit::default();
        bestfit.free(0..0); // NOOP
        bestfit.free(0..100);
        assert_eq!(bestfit.allocate(10), Some(0..10));
        assert_eq!(bestfit.allocate(10), Some(10..20));
        assert_eq!(bestfit.allocate(10), Some(20..30));
        assert_eq!(bestfit.allocate(10), Some(30..40));
        assert_eq!(bestfit.allocate(10), Some(40..50));
        // Make some holes.
        bestfit.free(30..40);
        bestfit.free(10..20);
        // Holes get filled first.
        assert_eq!(bestfit.allocate(10), Some(10..20));
        assert_eq!(bestfit.allocate(10), Some(30..40));
        assert_eq!(bestfit.allocate(10), Some(50..60));
        // Free a contiguous bunch of allocations at once.
        bestfit.free(0..50);
        // Return less than requested.
        assert_eq!(bestfit.allocate(100), Some(0..50));
        // Return all remaining space.
        assert_eq!(bestfit.allocate(100), Some(60..100));
        // No space left. Return None.
        assert_eq!(bestfit.allocate(100), None);
        // Now we have some more back.
        bestfit.free(50..100);
        assert_eq!(bestfit.allocate(100), Some(50..100));
    }

    #[test]
    fn fixed_offset() {
        let mut bestfit = BestFit::default();
        bestfit.free(0..100);
        assert_eq!(bestfit.allocate_fixed_offset(25, 50), Some(25..75));
        assert_eq!(bestfit.allocate_fixed_offset(25, 10), None);

        // Test coalescing of adjacent available ranges if the request is large.
        let mut bestfit = BestFit::default();
        bestfit.free(0..DEFAULT_MAX_EXTENT_SIZE * 3 + 10000);
        assert_eq!(
            bestfit.allocate_fixed_offset(0, DEFAULT_MAX_EXTENT_SIZE * 2),
            Some(0..DEFAULT_MAX_EXTENT_SIZE * 2)
        );

        // Try to allocate a range with a hole in it.
        let mut bestfit = BestFit::default();
        bestfit.free(0..100);
        bestfit.free(200..400);
        assert_eq!(bestfit.allocate_fixed_offset(50, 300), None);
        //
        // Try to allocate a range missing a tail.
        let mut bestfit = BestFit::default();
        bestfit.free(0..250);
        assert_eq!(bestfit.allocate_fixed_offset(50, 300), None);
    }

    #[test]
    fn next_available() {
        let mut bestfit = BestFit::default();
        bestfit.free(0..96);
        assert_eq!(bestfit.allocate(1), Some(0..1));
        assert_eq!(bestfit.allocate_next_available(15, 15), Some(15..30));
        assert_eq!(bestfit.allocate_next_available(15, 15), Some(30..45));
        assert_eq!(bestfit.allocate_next_available(15, 15), Some(45..60));
        assert_eq!(bestfit.allocate_next_available(15, 96), Some(60..96));
        assert_eq!(bestfit.allocate_next_available(15, 96), None);
        assert_eq!(bestfit.allocate_next_available(0, 5), Some(1..6));
        assert_eq!(bestfit.allocate_next_available(0, 9), Some(6..15));
        assert_eq!(bestfit.allocate_next_available(0, 96), None);
    }

    #[test]
    fn coalescing_free() {
        let mut bestfit = BestFit::default();
        // Free some bytes at the start and end.
        bestfit.free(0..10);
        bestfit.free(20..32);
        // Now free the space in the middle, which should coalesce with ranges on both sides.
        bestfit.free(10..20);
        // Confirm that we can allocate one block of 32 bytes. This will fail if coalescing
        // didn't occur.
        assert_eq!(bestfit.allocate_fixed_offset(0, 32), Some(0..32));
    }

    #[test]
    fn max_range() {
        let mut bestfit = BestFit::default();
        bestfit.free(10..10 + 10 * DEFAULT_MAX_EXTENT_SIZE);

        // We can't allocate bigger than DEFAULT_MAX_EXTENT_SIZE.
        assert_eq!(
            bestfit.allocate(2 * DEFAULT_MAX_EXTENT_SIZE),
            Some(10..10 + DEFAULT_MAX_EXTENT_SIZE)
        );

        // Make sure that coalescing still works properly
        bestfit.free(10..10 + DEFAULT_MAX_EXTENT_SIZE);
        assert_eq!(bestfit.allocate(10), Some(10..20));
        assert_eq!(bestfit.allocate(10), Some(20..30));
        assert_eq!(
            bestfit.allocate(DEFAULT_MAX_EXTENT_SIZE - 20),
            Some(30..10 + DEFAULT_MAX_EXTENT_SIZE)
        );
    }
}
