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

    /// Inserts `range` into this tracking structure.
    ///
    /// No-op if the range is empty.
    ///
    /// `meta` is attached to the newly created range and all the ranges it
    /// touches, including if `range` is a subset of a currently tracked range.
    pub(crate) fn insert(&mut self, range: Range<SeqNum>, meta: M)
    where
        M: Clone,
    {
        let Range { mut start, mut end } = range;
        let Self { blocks } = self;

        if start == end {
            return;
        }

        if blocks.is_empty() {
            blocks.push_back(SeqRange { range: Range { start, end }, meta });
            return;
        }

        // Search for the first segment whose `start` is greater.
        let first_after = match blocks.binary_search_by(|block| {
            if block.range.start == start {
                return core::cmp::Ordering::Equal;
            }
            if block.range.start.before(start) {
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
        };

        let mut merge_right = 0;
        for block in blocks.range(first_after..blocks.len()) {
            if end.before(block.range.start) {
                break;
            }
            merge_right += 1;
            if end.before(block.range.end) {
                end = block.range.end;
                break;
            }
        }

        let mut merge_left = 0;
        for block in blocks.range(0..first_after).rev() {
            if start.after(block.range.end) {
                break;
            }
            // There is no guarantee that `end.after(range.end)`, not doing
            // the following may shrink existing coverage. For example:
            // range.start = 0, range.end = 10, start = 0, end = 1, will result
            // in only 0..1 being tracked in the resulting assembler. We didn't
            // do the symmetrical thing above when merging to the right because
            // the search guarantees that `start.before(range.start)`, thus the
            // problem doesn't exist there. The asymmetry rose from the fact
            // that we used `start` to perform the search.
            if end.before(block.range.end) {
                end = block.range.end;
            }
            merge_left += 1;
            if start.after(block.range.start) {
                start = block.range.start;
                break;
            }
        }

        if merge_left == 0 && merge_right == 0 {
            // If the new segment cannot merge with any of its neighbors, we
            // add a new entry for it.
            blocks.insert(first_after, SeqRange { range: Range { start, end }, meta });
        } else {
            // Otherwise, we put the new segment at the left edge of the merge
            // window and remove all other existing segments.
            let left_edge = first_after - merge_left;
            let right_edge = first_after + merge_right;
            blocks[left_edge] = SeqRange { range: Range { start, end }, meta };
            for i in right_edge..blocks.len() {
                blocks[i - merge_left - merge_right + 1] = blocks[i].clone();
            }
            blocks.truncate(blocks.len() - merge_left - merge_right + 1);
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &SeqRange<M>> + '_ {
        self.blocks.iter()
    }
}

impl<M: Clone> FromIterator<SeqRange<M>> for SeqRanges<M> {
    fn from_iter<T: IntoIterator<Item = SeqRange<M>>>(iter: T) -> Self {
        let mut ranges = SeqRanges::default();
        for SeqRange { range, meta } in iter {
            ranges.insert(range, meta)
        }
        ranges
    }
}

/// A range kept in [`SeqRanges`].
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) struct SeqRange<M> {
    pub(crate) range: Range<SeqNum>,
    pub(crate) meta: M,
}

#[cfg(test)]
mod test {
    use super::*;

    use alloc::format;

    use netstack3_base::WindowSize;
    use proptest::strategy::{Just, Strategy};
    use proptest::test_runner::Config;
    use proptest::{prop_assert, prop_assert_eq, proptest};
    use proptest_support::failed_seeds_no_std;

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
                seq_ranges.insert(start..end, ());
                num_insertions_performed += 1;

                // assert that the ranges are sorted and don't overlap with
                // each other.
                for i in 1..seq_ranges.blocks.len() {
                    prop_assert!(
                        seq_ranges.blocks[i-1].range.end.before(seq_ranges.blocks[i].range.start)
                    );
                }
            }
            prop_assert_eq!(seq_ranges.blocks.front().unwrap().range.start, min_seq);
            prop_assert_eq!(seq_ranges.blocks.back().unwrap().range.end, max_seq);
        }
    }

    fn insertions() -> impl Strategy<Value = Range<SeqNum>> {
        (0..u32::from(WindowSize::MAX)).prop_flat_map(|start| {
            (start + 1..=u32::from(WindowSize::MAX)).prop_flat_map(move |end| {
                Just(Range { start: SeqNum::new(start), end: SeqNum::new(end) })
            })
        })
    }
}
