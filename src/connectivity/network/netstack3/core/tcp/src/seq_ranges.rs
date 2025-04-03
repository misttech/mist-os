// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::collections::VecDeque;
use core::ops::Range;

use derivative::Derivative;
use netstack3_base::SeqNum;

/// A generic data structure that keeps track of ordered sequence number ranges.
///
/// Each kept range has associated metadata of type `M`.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) struct SeqRanges<M> {
    blocks: VecDeque<SeqRange<M>>,
}

impl<M> SeqRanges<M> {
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub(crate) fn pop_front_if<F: FnOnce(&SeqRange<M>) -> bool>(
        &mut self,
        f: F,
    ) -> Option<SeqRange<M>> {
        let front = self.blocks.front()?;
        if f(front) {
            self.blocks.pop_front()
        } else {
            None
        }
    }

    fn find_first_after(blocks: &mut VecDeque<SeqRange<M>>, start: SeqNum) -> usize {
        match blocks.binary_search_by(|block| {
            if block.start() == start {
                return core::cmp::Ordering::Equal;
            }
            if block.start().before(start) {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Greater
            }
        }) {
            Ok(r) => {
                // We found the exact same start point, so the first segment
                // whose start is greater must be the next one.
                r + 1
            }
            Err(e) => {
                // When binary search doesn't find the exact place it returns
                // the index where this block should be in, which should be the
                // next greater range.
                e
            }
        }
    }

    /// Inserts `range` into this tracking structure.
    ///
    /// No-op if the range is empty.
    ///
    /// `meta` is attached to the newly created range and all the ranges it
    /// touches, including if `range` is a subset of a currently tracked range.
    ///
    /// Returns `true` iff `range` insertion increases the total number of
    /// tracked bytes contained in `SeqRanges`.
    pub(crate) fn insert(&mut self, range: Range<SeqNum>, meta: M) -> bool
    where
        M: Clone,
    {
        let Some(range) = SeqRange::new(range, meta) else {
            return false;
        };
        self.insert_seq_range(range)
    }

    fn insert_seq_range(&mut self, mut range: SeqRange<M>) -> bool
    where
        M: Clone,
    {
        let Self { blocks } = self;

        if blocks.is_empty() {
            blocks.push_back(range);
            return true;
        }

        // Search for the first segment whose `start` is greater.
        let first_after = Self::find_first_after(blocks, range.start());

        let mut merge_right = 0;
        for block in blocks.range(first_after..blocks.len()) {
            match range.merge_right(block) {
                MergeRightResult::Before => break,
                MergeRightResult::After { merged } => {
                    merge_right += 1;
                    if merged {
                        break;
                    }
                }
            }
        }

        // Given we're always sorted and we know the first range strictly after
        // the inserting one, we only need to check to the left once.
        let merge_left = match first_after
            .checked_sub(1)
            .and_then(|first_before| blocks.get_mut(first_before))
        {
            Some(block) => {
                match block.merge_right(&range) {
                    MergeRightResult::Before => 0,
                    MergeRightResult::After { merged } => {
                        if merged {
                            range.clone_range_from(&block);
                            1
                        } else {
                            // The new range fits entirely within an existing
                            // block. Update the metadata and return early.
                            block.set_meta(range.into_meta());
                            return false;
                        }
                    }
                }
            }
            None => 0,
        };

        if merge_left == 0 && merge_right == 0 {
            // If the new segment cannot merge with any of its neighbors, we
            // add a new entry for it.
            blocks.insert(first_after, range);
        } else {
            // Otherwise, we put the new segment at the left edge of the merge
            // window and remove all other existing segments.
            let left_edge = first_after - merge_left;
            let right_edge = first_after + merge_right;
            blocks[left_edge] = range;
            for i in right_edge..blocks.len() {
                blocks[i - merge_left - merge_right + 1] = blocks[i].clone();
            }
            blocks.truncate(blocks.len() - merge_left - merge_right + 1);
        }

        true
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &SeqRange<M>> + '_ {
        self.blocks.iter()
    }
}

impl<M: Clone> FromIterator<SeqRange<M>> for SeqRanges<M> {
    fn from_iter<T: IntoIterator<Item = SeqRange<M>>>(iter: T) -> Self {
        let mut ranges = SeqRanges::default();
        for range in iter {
            let _: bool = ranges.insert_seq_range(range);
        }
        ranges
    }
}

mod range {
    use netstack3_base::SackBlock;

    use super::*;

    /// A range kept in [`SeqRanges`].
    #[derive(Debug, Clone)]
    #[cfg_attr(test, derive(PartialEq, Eq))]
    pub(crate) struct SeqRange<M> {
        range: Range<SeqNum>,
        meta: M,
    }

    impl<M> SeqRange<M> {
        pub(crate) fn new(range: Range<SeqNum>, meta: M) -> Option<Self> {
            range.end.after(range.start).then(|| Self { range, meta })
        }

        pub(crate) fn start(&self) -> SeqNum {
            self.range.start
        }

        pub(crate) fn end(&self) -> SeqNum {
            self.range.end
        }

        pub(crate) fn set_meta(&mut self, meta: M) {
            self.meta = meta;
        }

        pub(crate) fn meta(&self) -> &M {
            &self.meta
        }

        pub(crate) fn into_meta(self) -> M {
            self.meta
        }

        pub(super) fn clone_range_from(&mut self, other: &Self) {
            let Self { range, meta: _ } = self;
            *range = other.range.clone();
        }

        pub(crate) fn len(&self) -> u32 {
            let Self { range: Range { start, end }, meta: _ } = self;
            let len = *end - *start;
            // Assert on SeqRange invariant in debug only.
            debug_assert!(len >= 0);
            len as u32
        }

        pub(crate) fn to_sack_block(&self) -> SackBlock {
            let Self { range: Range { start, end }, meta: _ } = self;
            // SAFETY: SackBlock requires that end is after start, which is the
            // same invariant held by SeqRange.
            unsafe { SackBlock::new_unchecked(*start, *end) }
        }

        pub(super) fn merge_right(&mut self, other: &Self) -> MergeRightResult {
            if self.range.end.before(other.range.start) {
                return MergeRightResult::Before;
            }

            let merged = self.range.end.before(other.range.end);
            if merged {
                self.range.end = other.range.end;
            }

            MergeRightResult::After { merged }
        }
    }

    pub(super) enum MergeRightResult {
        Before,
        After { merged: bool },
    }
}
use range::MergeRightResult;
pub(crate) use range::SeqRange;

#[cfg(test)]
mod test {
    use super::*;

    use alloc::format;

    use netstack3_base::{SackBlock, WindowSize};
    use proptest::strategy::{Just, Strategy};
    use proptest::test_runner::Config;
    use proptest::{prop_assert, prop_assert_eq, proptest};
    use proptest_support::failed_seeds_no_std;

    impl SeqRanges<()> {
        fn insert_u32(&mut self, range: Range<u32>) -> bool {
            let Range { start, end } = range;
            self.insert(SeqNum::new(start)..SeqNum::new(end), ())
        }
    }

    proptest! {
        #![proptest_config(Config {
            // Add all failed seeds here.
            failure_persistence: failed_seeds_no_std!(
                "cc f621ca7d3a2b108e0dc41f7169ad028f4329b79e90e73d5f68042519a9f63999",
                "cc c449aebed201b4ec4f137f3c224f20325f4cfee0b7fd596d9285176b6d811aa9"
            ),
            ..Config::default()
        })]

        #[test]
        fn seq_ranges_insert(insertions in proptest::collection::vec(insertions(), 200)) {
            let mut seq_ranges = SeqRanges::<()>::default();
            let mut num_insertions_performed = 0;
            let mut min_seq = SeqNum::new(WindowSize::MAX.into());
            let mut max_seq = SeqNum::new(0);
            for Range { start, end } in insertions {
                if min_seq.after(start) {
                    min_seq = start;
                }
                if max_seq.before(end) {
                    max_seq = end;
                }
                // assert that it's impossible to have more entries than the
                // number of insertions performed.
                prop_assert!(seq_ranges.blocks.len() <= num_insertions_performed);
                let _: bool = seq_ranges.insert(start..end, ());
                num_insertions_performed += 1;

                // assert that the ranges are sorted and don't overlap with
                // each other.
                for i in 1..seq_ranges.blocks.len() {
                    prop_assert!(
                        seq_ranges.blocks[i-1].end().before(seq_ranges.blocks[i].start())
                    );
                }
            }
            prop_assert_eq!(seq_ranges.blocks.front().unwrap().start(), min_seq);
            prop_assert_eq!(seq_ranges.blocks.back().unwrap().end(), max_seq);
        }

        // Test that the invariants between SackBlock and SeqRange creation
        // match. Supports unsafe block in SeqRange::to_sack_block.
        #[test]
        fn seq_range_to_sack_block((start, end) in sequence_numbers()) {
            prop_assert_eq!(
                SeqRange::new(start..end, ()).map(|sr| sr.to_sack_block()),
                SackBlock::try_new(start, end).ok()
            );
        }
    }

    fn insertions() -> impl Strategy<Value = Range<SeqNum>> {
        (0..u32::from(WindowSize::MAX)).prop_flat_map(|start| {
            (start + 1..=u32::from(WindowSize::MAX)).prop_flat_map(move |end| {
                Just(Range { start: SeqNum::new(start), end: SeqNum::new(end) })
            })
        })
    }

    fn sequence_numbers() -> impl Strategy<Value = (SeqNum, SeqNum)> {
        (0u32..5).prop_flat_map(|start| {
            (0u32..5).prop_flat_map(move |end| Just((SeqNum::new(start), SeqNum::new(end))))
        })
    }

    #[test]
    fn insert_return() {
        let mut sr = SeqRanges::default();
        assert!(sr.insert_u32(10..20));

        assert!(!sr.insert_u32(10..20));
        assert!(!sr.insert_u32(11..20));
        assert!(!sr.insert_u32(11..12));
        assert!(!sr.insert_u32(19..20));

        assert!(sr.insert_u32(0..5));
        assert!(sr.insert_u32(25..35));
        assert!(sr.insert_u32(5..7));
        assert!(sr.insert_u32(22..25));

        assert!(sr.insert_u32(7..22));
        assert!(!sr.insert_u32(0..35));
    }
}
