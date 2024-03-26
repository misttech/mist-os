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

use std::{collections::BTreeSet, fmt::Debug, ops::Range};

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
    /// Holds free storage ranges for the filesystem ordered by length, then offset.
    ranges: BTreeSet<(u64, u64)>,
}

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
        if let Some(&(size, offset)) = self.ranges.range((bytes, 0)..).next() {
            assert!(size >= bytes);
            self.ranges.remove(&(size, offset));
            if size > bytes {
                self.free(offset + bytes..offset + size);
            }
            return Some(offset..offset + bytes);
        }
        // Insufficient space. Return the biggest space we have.
        if let Some(&(size, _)) = self.ranges.iter().rev().next() {
            // Nb: We reversed the iterator to get the biggest size, but we
            // generally want to serve the smallest offset so we do a second
            // lookup here to find the smallest offset with the largest size.
            let &(size, offset) = self.ranges.range((size, 0)..).next().unwrap();
            self.ranges.remove(&(size, offset));
            Some(offset..offset + size)
        } else {
            None
        }
    }

    pub fn allocate_fixed_offset(&mut self, start: u64, bytes: u64) -> Option<Range<u64>> {
        let end = start + bytes;
        for &(size, offset) in self.ranges.range((bytes, 0)..) {
            assert!(size >= bytes);
            let range = offset..offset + size;
            if range.start > start || range.end < end {
                // Not a sub-range.
                continue;
            }
            self.ranges.remove(&(size, offset));
            if range.start < start {
                self.free(range.start..start);
            }
            if range.end > end {
                self.free(end..range.end);
            }
            return Some(start..end);
        }
        None
    }

    pub fn allocate_next_available(&mut self, start: u64, bytes: u64) -> Option<Range<u64>> {
        let mut best = None;
        for &(size, offset) in &self.ranges {
            if offset + size <= start {
                continue;
            }
            if let Some((best_size, best_offset)) = &mut best {
                if offset < *best_offset {
                    *best_size = size;
                    *best_offset = offset;
                }
            } else {
                best = Some((size, offset));
            }
        }
        if let Some((size, offset)) = best {
            self.ranges.remove(&(size, offset));
            let mut range = offset..offset + size;
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

    /// Adds a range of bytes to the pool of available ranges.
    ///
    /// There are no special requirements on the alignment of `range` but
    /// callers should should generally ensure ranges are block aligned.
    pub fn free(&mut self, mut range: Range<u64>) {
        let mut prior_range = None;
        let mut post_range = None;
        // Look for ranges immediately before and after 'range' for coalescing.
        for (size, offset) in &self.ranges {
            if *offset + *size == range.start {
                prior_range = Some((*size, *offset));
            }
            if *offset == range.end {
                post_range = Some((*size, *offset));
            }
        }
        if let Some(prior_range) = prior_range {
            self.ranges.remove(&prior_range);
            range.start = prior_range.1;
        }
        if let Some(post_range) = post_range {
            self.ranges.remove(&post_range);
            range.end = post_range.0 + post_range.1;
        }
        self.ranges.insert((range.end - range.start, range.start));
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn hint_none() {
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
    fn hint_require() {
        let mut bestfit = BestFit::default();
        bestfit.free(0..100);
        assert_eq!(bestfit.allocate_fixed_offset(25, 50), Some(25..75));
        assert_eq!(bestfit.allocate_fixed_offset(25, 10), None);
    }

    #[test]
    fn hint_next_available() {
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
}
