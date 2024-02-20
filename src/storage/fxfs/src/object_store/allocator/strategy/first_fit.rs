// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_store::allocator::strategy::{AllocatorStrategy, LocationHint},
    async_trait::async_trait,
    std::{collections::BTreeSet, ops::Range},
};

/// An allocation strategy that returns the first extent that is large enough to hold the
/// requested allocation, or the next best if no extent is big enough.
/// When the disk is empty, this minimises fragmentation AND is cheaper on CPU
/// than the best-fit strategy, which requires us to scan all possible extents every time.
/// When disk fills, this is likely to fragment any large remaining extents early, causing the
/// very last part of the disk to be filled in a highly-fragmented way.
///
/// Because we are currently storing ALL allocations in RAM, the difference between this and BestFit
/// is expected to be minimal with perhaps the exception of a very full filesystem.
#[derive(Debug, Default)]
pub struct FirstFit {
    /// Holds free storage ranges for the filesystem.
    /// (Ranges are stored as tuples to allow for sorting. They should never overlap.)
    ranges: BTreeSet<(u64, u64)>,
}

#[async_trait]
impl AllocatorStrategy for FirstFit {
    fn allocate(&mut self, bytes: u64, hint: LocationHint) -> Option<Range<u64>> {
        match hint {
            LocationHint::None => {
                for range in &self.ranges {
                    let range = range.0..range.1;
                    let len = range.end - range.start;
                    if len < bytes {
                        continue;
                    }
                    self.ranges.remove(&(range.start, range.end));
                    if len > bytes {
                        self.free(range.start + bytes..range.end);
                    }
                    return Some(range.start..range.start + bytes);
                }
                // Insufficient space. Return the first space we find.
                if let Some(range) = self.ranges.iter().next() {
                    let range = range.0..range.1;
                    self.ranges.remove(&(range.start, range.end));
                    Some(range)
                } else {
                    None
                }
            }
            LocationHint::Require(start) => {
                let end = start + bytes;
                for range in &self.ranges {
                    let range = range.0..range.1;
                    if range.start > start || range.end < end {
                        // Not a sub-range.
                        continue;
                    }
                    self.ranges.remove(&(range.start, range.end));
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
            LocationHint::NextAvailable(start) => {
                for range in &self.ranges {
                    let mut range = range.0..range.1;
                    if range.end > start {
                        self.ranges.remove(&(range.start, range.end));
                        if range.start < start {
                            self.free(range.start..start);
                            range.start = start;
                        }
                        if range.start + bytes < range.end {
                            self.free(range.start + bytes..range.end);
                            range.end = range.start + bytes;
                        }
                        return Some(range);
                    }
                }
                None
            }
        }
    }
    fn free(&mut self, mut range: Range<u64>) {
        if range.end <= range.start {
            return;
        }
        let mut prior_range = None;
        let mut post_range = None;
        // Look for ranges immediately before and after 'range' for coalescing.
        for free_range in &self.ranges {
            let free_range = free_range.0..free_range.1;
            if free_range.end == range.start {
                prior_range = Some(free_range.clone());
            }
            if free_range.start == range.end {
                post_range = Some(free_range.clone());
            }
        }
        if let Some(prior_range) = prior_range {
            self.ranges.remove(&(prior_range.start, prior_range.end));
            range.start = prior_range.start;
        }
        if let Some(post_range) = post_range {
            self.ranges.remove(&(post_range.start, post_range.end));
            range.end = post_range.end;
        }
        self.ranges.insert((range.start, range.end));
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn hint_none() {
        let mut firstfit = FirstFit::default();
        firstfit.free(0..0); // NOOP
        firstfit.free(0..90);
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(0..10));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(10..20));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(20..30));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(30..40));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(40..50));
        // Make some holes.
        firstfit.free(30..40);
        firstfit.free(10..20);
        // Holes get filled first.
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(10..20));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(30..40));
        assert_eq!(firstfit.allocate(10, LocationHint::None), Some(50..60));
        // Free a contiguous bunch of allocations at once.
        firstfit.free(0..50);
        // Return less than requested.
        assert_eq!(firstfit.allocate(100, LocationHint::None), Some(0..50));
        // Return all remaining space.
        assert_eq!(firstfit.allocate(100, LocationHint::None), Some(60..90));
        // No space left. Return None.
        assert_eq!(firstfit.allocate(100, LocationHint::None), None);
        // Now we have some more back.
        firstfit.free(50..100);
        assert_eq!(firstfit.allocate(100, LocationHint::None), Some(50..100));
    }

    #[test]
    fn hint_require() {
        let mut firstfit = FirstFit::default();
        firstfit.free(0..100);
        assert_eq!(firstfit.allocate(50, LocationHint::Require(25)), Some(25..75));
        assert_eq!(firstfit.allocate(10, LocationHint::Require(25)), None);
    }

    #[test]
    fn hint_next_available() {
        // NextAvailable
        let mut firstfit = FirstFit::default();
        firstfit.free(0..96);
        assert_eq!(firstfit.allocate(1, LocationHint::None), Some(0..1));
        assert_eq!(firstfit.allocate(15, LocationHint::NextAvailable(15)), Some(15..30));
        assert_eq!(firstfit.allocate(15, LocationHint::NextAvailable(15)), Some(30..45));
        assert_eq!(firstfit.allocate(15, LocationHint::NextAvailable(15)), Some(45..60));
        assert_eq!(firstfit.allocate(96, LocationHint::NextAvailable(15)), Some(60..96));
        assert_eq!(firstfit.allocate(96, LocationHint::NextAvailable(15)), None);
        assert_eq!(firstfit.allocate(5, LocationHint::NextAvailable(0)), Some(1..6));
        assert_eq!(firstfit.allocate(9, LocationHint::NextAvailable(0)), Some(6..15));
        assert_eq!(firstfit.allocate(96, LocationHint::NextAvailable(0)), None);
    }

    #[test]
    fn coalescing_free() {
        let mut firstfit = FirstFit::default();
        // Free some bytes at the start and end.
        firstfit.free(0..10);
        firstfit.free(20..32);
        // Now free the space in the middle, which should coalesce with ranges on both sides.
        firstfit.free(10..20);
        // Confirm that we can allocate one block of 32 bytes. This will fail if coalescing
        // didn't occur.
        assert_eq!(firstfit.allocate(32, LocationHint::Require(0)), Some(0..32));
    }
}
