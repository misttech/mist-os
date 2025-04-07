// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the selective acknowledgement scoreboard data structure as
//! described in [RFC 6675].
//!
//! [RFC 6675]: https://datatracker.ietf.org/doc/html/rfc6675

use netstack3_base::{Mss, SackBlocks, SeqNum};

use crate::internal::congestion::DUP_ACK_THRESHOLD;
use crate::internal::seq_ranges::{FirstHoleResult, SeqRange, SeqRanges};

#[derive(Debug, Default)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) struct SackScoreboard {
    /// The ranges for which we've received selective acknowledgements.
    ///
    /// Each range is tagged with a boolean that caches the result of [IsLost] for
    /// the un-sacked range before it.
    ///
    /// [IsLost]: https://datatracker.ietf.org/doc/html/rfc6675#section-4
    acked_ranges: SeqRanges<bool>,
    /// Stores the number of bytes assumed to be in transit according to the definition of Pipe
    /// defined in [RFC 6675 section 4].
    ///
    /// [RFC 6675 section 4]: https://datatracker.ietf.org/doc/html/rfc6675#section-4
    pipe: u32,
}

impl SackScoreboard {
    /// Processes an incoming ACK and updates the scoreboard.
    ///
    /// - `ack` is the cumulative acknowledgement in the received segment.
    /// - `snd_nxt` is the value of SND.NXT or the highest sequence number that
    ///   has been sent out.
    /// - `high_rxt` is the equivalent to the HighRxt value defined in [RFC 6675
    ///   section 2], but we define it as the next available sequence number for
    ///   retransmission (off by one from the RFC definition). It is expected to
    ///   only be available if loss recovery is initiated.
    /// - `sack_blocks` are the selective ack blocks informed by the peer in the
    ///   segment.
    /// - `smss` is the send size mss.
    ///
    /// Any blocks referencing data before `ack` or after `snd_nxt` are
    /// *ignored* as bad data. We chose to ignore any blocks after `snd_nxt`
    /// here so the SACK recovery algorithm works as described in [RFC 6675].
    /// Note that [RFC 6675 section 5.1] calls out that the algorithm described
    /// in the RFC is not suited to deal with a retransmit timeout, so to avoid
    /// an improper pipe calculation we ignore any blocks past SND.NXT.
    ///
    /// Returns `true` if this segment should be considered a duplicate as per
    /// the definition in [RFC 6675 section 2].
    ///
    /// [RFC 6675]: https://datatracker.ietf.org/doc/html/rfc6675
    /// [RFC 6675 section 5.1]:
    ///     https://datatracker.ietf.org/doc/html/rfc6675#section-5.1
    /// [RFC 6675 Section 2]:
    ///     https://datatracker.ietf.org/doc/html/rfc6675#section-2
    pub(crate) fn process_ack(
        &mut self,
        ack: SeqNum,
        snd_nxt: SeqNum,
        high_rxt: Option<SeqNum>,
        sack_blocks: &SackBlocks,
        smss: Mss,
    ) -> bool {
        let Self { acked_ranges, pipe } = self;

        // If we receive an ACK that is after SND.NXT, this must be a very
        // delayed acknowledgement post a retransmission event. The state
        // machine will eventually move SND.NXT to account for this, but this
        // violates the scoreboard's expectations.
        //
        // Because we need to ensure the pipe is updated accordingly and any
        // previous SACK ranges are cleared, process this as if it was a full
        // cumulative ack.
        let snd_nxt = snd_nxt.latest(ack);

        // Fast exit if there's nothing interesting to do.
        if acked_ranges.is_empty() && sack_blocks.is_empty() {
            // Ack must not be after snd_nxt.
            *pipe = u32::try_from(snd_nxt - ack).unwrap();
            return false;
        }

        // Update the scoreboard with the cumulative acknowledgement.
        //
        // A note here: we discard all the sacked ranges that start at or after
        // the acknowledged number. If there is intersection we must assume that
        // the peer reneged the block.
        acked_ranges.discard_starting_at_or_before(ack);

        // Insert each valid block in acked ranges.
        let new = sack_blocks.iter_skip_invalid().fold(false, |new, sack_block| {
            // NB: SackBlock type here ensures this is a valid non-empty range.
            let (start, end) = sack_block.into_parts();
            if start.before_or_eq(ack) || end.after(snd_nxt) {
                // Ignore block that is not in the expected range [ack, snd_nxt].
                return new;
            }
            // Insert with an arbitrary metadata value, we'll update it with the
            // IsLost value afterwards.
            let is_lost = false;
            let changed = acked_ranges.insert(start..end, is_lost);

            new || changed
        });

        let sacked_byte_threshold = sacked_bytes_threshold(smss);
        let high_rxt = high_rxt.unwrap_or(ack);
        let get_pipe_increment = |hole: SeqRange<bool>| {
            // From RFC 6675, where S1 is a single
            // sequence number:
            //
            // (a) If IsLost (S1) returns false: Pipe is incremented by 1
            // octet.
            // (b) If S1 <= HighRxt: Pipe is incremented by 1 octet.
            let mut pipe = 0u32;
            let is_lost = *(hole.meta());
            if !is_lost {
                pipe = pipe.saturating_add(hole.len());
            }

            if let Some(hole) = hole.cap_right(high_rxt) {
                pipe = pipe.saturating_add(hole.len());
            }
            pipe
        };

        // Recalculate pipe and update IsLost in the collection.
        //
        // We iterate acked_ranges in reverse order so we can fold over the
        // total number of ranges and SACKed bytes that come *after* the range we
        // operate on at each point.
        let (new_pipe, _sacked, _count, later_start, later_is_lost) =
            acked_ranges.iter_mut().rev().fold(
                (0u32, 0, 0, snd_nxt, false),
                |(pipe, sacked, count, later_start, later_is_lost), acked_range| {
                    // IsLost is kept for the hole to the left of this block. So the
                    // block we're currently iterating on counts as part of is_lost,
                    // as well as the the total number of sacked bytes.
                    let count = count + 1;
                    let sacked = sacked + acked_range.len();
                    // From RFC 6675, IsLost is defined as:
                    //
                    //  The routine returns true when either DupThresh discontiguous
                    //  SACKed sequences have arrived above 'SeqNum' or more than
                    //  (DupThresh - 1) * SMSS bytes with sequence numbers greater
                    //  than 'SeqNum' have been SACKed
                    let is_lost = count >= DUP_ACK_THRESHOLD || sacked > sacked_byte_threshold;
                    acked_range.set_meta(is_lost);

                    // Increment pipe. From RFC 6675:
                    //
                    //  After initializing pipe to zero, the following steps are
                    //  taken for each octet 'S1' in the sequence space between
                    //  HighACK and HighData that has not been SACKed[...]
                    //
                    // So pipe is only calculated for the gaps between the acked
                    // ranges, i.e., from the current end to the start of the
                    // later block.
                    let pipe = if let Some(hole) =
                        SeqRange::new(acked_range.end()..later_start, later_is_lost)
                    {
                        pipe.saturating_add(get_pipe_increment(hole))
                    } else {
                        // An empty hole can only happen in the first iteration,
                        // when the right edge is SND.NXT.
                        assert_eq!(later_start, snd_nxt);
                        pipe
                    };

                    (pipe, sacked, count, acked_range.start(), is_lost)
                },
            );
        // Add the final hole between cumulative ack and first sack block
        // and finalize the pipe value.
        *pipe = match SeqRange::new(ack..later_start, later_is_lost) {
            Some(first_hole) => new_pipe.saturating_add(get_pipe_increment(first_hole)),
            None => {
                // An empty first hole can only happen if we don't have any
                // sack blocks, and ACK is equal to SND.NXT.
                assert_eq!(ack, snd_nxt);
                new_pipe
            }
        };

        new
    }

    pub(crate) fn has_sack_info(&self) -> bool {
        !self.acked_ranges.is_empty()
    }

    /// Helper to check rule (2) from [RFC 6675 section 5]:
    ///
    /// > (2) If DupAcks < DupThresh but IsLost (HighACK + 1) returns true --
    /// > indicating at least three segments have arrived above the current
    /// > cumulative acknowledgment point, which is taken to indicate loss -- go
    /// > to step (4).
    ///
    /// [RFC 6675 section 5]: https://datatracker.ietf.org/doc/html/rfc6675#section-4
    pub(crate) fn is_first_hole_lost(&self) -> bool {
        self.acked_ranges.iter().next().is_some_and(|range| *range.meta())
    }

    pub(crate) fn pipe(&self) -> u32 {
        self.pipe
    }

    /// Increments the pipe value kept by the scoreboard by `value`.
    ///
    /// Note that [`SackScoreboard::process_ack`] always updates the pipe value
    /// based on the scoreboard. Whenever a segment is sent, we must increment
    /// the pipe value so the estimate of total bytes in transit is always up to
    /// date until the next ACK arrives.
    pub(crate) fn increment_pipe(&mut self, value: u32) {
        self.pipe = self.pipe.saturating_add(value);
    }

    /// Returns the right-side-bounded unsacked range starting at or later than
    /// `marker`.
    ///
    /// Returns `None` if `mark` is not a hole bounded to the right side by a
    /// received SACK.
    ///
    /// Returns a [`SeqRange`] whose metadata is a boolean indicating if this
    /// range is considered lost.
    pub(crate) fn first_unsacked_range_from(&self, mark: SeqNum) -> Option<SeqRange<bool>> {
        let Self { acked_ranges, pipe: _ } = self;
        match acked_ranges.first_hole_on_or_after(mark) {
            FirstHoleResult::None => None,
            FirstHoleResult::Right(right) => SeqRange::new(mark..right.start(), *right.meta()),
            FirstHoleResult::Both(left, right) => {
                SeqRange::new(left.end().latest(mark)..right.start(), *right.meta())
            }
        }
    }

    /// Returns the end of the sequence number range in the scoreboard, if there
    /// are any ranges tracked.
    pub(crate) fn right_edge(&self) -> Option<SeqNum> {
        let Self { acked_ranges, pipe: _ } = self;
        acked_ranges.last().map(|seq_range| seq_range.end())
    }

    pub(crate) fn on_retransmission_timeout(&mut self) {
        let Self { acked_ranges, pipe } = self;
        // RFC 2018 says that we MUST clear all SACK information on a
        // retransmission timeout.
        //
        // RFC 6675 changes that to a SHOULD keep SACK information on a
        // retransmission timeout, but doesn't quite specify how to deal with
        // the SACKed ranges post the timeout. Notably, the pipe estimate is
        // very clearly off post an RTO.
        //
        // Given that, the conservative thing to do here is to clear the
        // scoreboard and reset the pipe so estimates can be based again on the
        // rewound value of SND.NXT and the eventually retransmitted SACK blocks
        // that we may get post the RTO event. Note that `process_ack` ignores
        // any SACK blocks post SND.NXT in order to maintain the pipe variable
        // sensible as well.
        //
        // See:
        // - https://datatracker.ietf.org/doc/html/rfc2018
        // - https://datatracker.ietf.org/doc/html/rfc6675
        *pipe = 0;
        acked_ranges.clear();
    }

    pub(crate) fn on_mss_update(
        &mut self,
        snd_una: SeqNum,
        snd_nxt: SeqNum,
        high_rxt: Option<SeqNum>,
        mss: Mss,
    ) {
        // When MSS updates, we must recalculate so we know what frames are
        // considered lost or not.
        //
        // Notably, this will also update the pipe variable so we have a new
        // estimate of bytes in flight with a new value for snd_nxt.
        //
        // Given we don't detect renegging, this is equivalent to processing an
        // ACK at the given parameters and without any SACK blocks.
        let _: bool = self.process_ack(snd_una, snd_nxt, high_rxt, &SackBlocks::EMPTY, mss);
    }
}

/// Returns the threshold over which a sequence number is considered lost per
/// the definition of `IsLost` in [RFC 6675 section 4].
///
/// [RFC 6675 section 4]:
///     https://datatracker.ietf.org/doc/html/rfc6675#section-4
fn sacked_bytes_threshold(mss: Mss) -> u32 {
    u32::from(DUP_ACK_THRESHOLD - 1) * u32::from(mss)
}

#[cfg(test)]
mod test {
    use core::num::NonZeroU16;
    use core::ops::Range;

    use super::*;
    use crate::internal::seq_ranges::SeqRange;
    use crate::internal::testutil;

    const TEST_MSS: Mss = Mss(NonZeroU16::new(50).unwrap());

    fn seq_ranges(iter: impl IntoIterator<Item = (Range<u32>, bool)>) -> SeqRanges<bool> {
        iter.into_iter()
            .map(|(Range { start, end }, is_lost)| {
                SeqRange::new(SeqNum::new(start)..SeqNum::new(end), is_lost).unwrap()
            })
            .collect()
    }

    impl SackScoreboard {
        fn sacked_bytes(&self) -> u32 {
            self.acked_ranges.iter().map(|seq_range| seq_range.len()).sum()
        }
    }

    #[test]
    fn process_ack_noop_if_empty() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(1);
        let snd_nxt = SeqNum::new(100);
        let high_rxt = None;
        assert!(!sb.process_ack(ack, snd_nxt, high_rxt, &SackBlocks::default(), TEST_MSS));
        assert!(sb.acked_ranges.is_empty());
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap());
    }

    #[test]
    fn process_ack_ignores_bad_blocks() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(100);
        let high_rxt = None;
        // Ignores everything that doesn't match the cumulative ack.
        assert!(!sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([0..1, 4..6, 5..10]),
            TEST_MSS
        ));
        assert!(sb.acked_ranges.is_empty());

        // Ignores everything past snd_nxt.
        assert!(!sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([100..200, 50..150]),
            TEST_MSS
        ));
        assert!(sb.acked_ranges.is_empty());
    }

    #[test]
    fn process_ack_cumulative_ack() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(100);
        let high_rxt = None;
        let blocks = testutil::sack_blocks([20..30]);
        assert!(sb.process_ack(ack, snd_nxt, high_rxt, &blocks, TEST_MSS));
        let expect_ranges = seq_ranges([(20..30, false)]);
        assert_eq!(sb.acked_ranges, expect_ranges);
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap() - sb.sacked_bytes());

        let ack = SeqNum::new(10);
        assert!(!sb.process_ack(ack, snd_nxt, high_rxt, &blocks, TEST_MSS));
        assert_eq!(sb.acked_ranges, expect_ranges);
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap() - sb.sacked_bytes());
    }

    #[test]
    fn process_ack_is_lost_dup_thresh() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(100);
        let high_rxt = None;

        let block1 = 20..30;
        let block2 = 35..40;
        let block3 = 45..50;

        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([block1.clone(), block2.clone(), block3.clone()]),
            TEST_MSS
        ));
        assert_eq!(
            sb.acked_ranges,
            seq_ranges([(block1.clone(), true), (block2, false), (block3, false)])
        );
        assert_eq!(
            sb.pipe,
            u32::try_from(snd_nxt - ack).unwrap()
                - sb.sacked_bytes()
                - (block1.start - u32::from(ack))
        );
    }

    #[test]
    fn process_ack_pipe_rule_a() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(500);
        let high_rxt = None;
        let small_block = 20..30;
        let large_block_start = 35;
        let large_block = large_block_start..(large_block_start + sacked_bytes_threshold(TEST_MSS));

        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([small_block.clone(), large_block.clone()]),
            TEST_MSS
        ));
        // Large block is exactly at the limit of the hole to its left being
        // considered lost as well.
        assert_eq!(
            sb.acked_ranges,
            seq_ranges([(small_block.clone(), true), (large_block.clone(), false)])
        );
        assert_eq!(
            sb.pipe,
            u32::try_from(snd_nxt - ack).unwrap()
                - sb.sacked_bytes()
                - (small_block.start - u32::from(ack))
        );

        // Now increase the large block by one.
        let large_block = large_block.start..(large_block.end + 1);
        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([small_block.clone(), large_block.clone()]),
            TEST_MSS
        ));
        // Now the hole to the left of large block is also considered lost.
        assert_eq!(
            sb.acked_ranges,
            seq_ranges([(small_block.clone(), true), (large_block.clone(), true)])
        );
        assert_eq!(
            sb.pipe,
            u32::try_from(snd_nxt - ack).unwrap()
                - sb.sacked_bytes()
                - (small_block.start - u32::from(ack))
                - (large_block.start - small_block.end)
        );
    }

    #[test]
    fn process_ack_pipe_rule_b() {
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(500);
        let first_block = 20..30;
        let second_block = 40..50;

        let blocks = testutil::sack_blocks([first_block.clone(), second_block.clone()]);

        // Extract the baseline pipe that if we receive an ACK with the
        // parameters above but without a HighRxt value.
        let baseline = {
            let mut sb = SackScoreboard::default();
            assert!(sb.process_ack(ack, snd_nxt, None, &blocks, TEST_MSS));
            sb.pipe
        };

        // Drive HighRxt across the entire possible sequence number range that
        // we expect to see it and check the pipe value is changing accordingly.
        let hole1 = (u32::from(ack)..first_block.start).map(|seq| (seq, true));
        let block1 = first_block.clone().map(|seq| (seq, false));
        let hole2 = (first_block.end..second_block.start).map(|seq| (seq, true));
        let block2 = second_block.map(|seq| (seq, false));
        // Shift expecting an increment one over because HighRxt starting at
        // HighAck is expected to be a zero contribution. This aligns the
        // off-by-one in the expectations.
        let iter =
            hole1.chain(block1).chain(hole2).chain(block2).scan(false, |prev, (seq, sacked)| {
                let expect_increment = core::mem::replace(prev, sacked);
                Some((seq, expect_increment))
            });

        let _: u32 = iter.fold(0u32, |total, (seq, expect_increment)| {
            let total = total + u32::from(expect_increment);
            let mut sb = SackScoreboard::default();
            assert!(sb.process_ack(ack, snd_nxt, Some(SeqNum::new(seq)), &blocks, TEST_MSS));
            assert_eq!(sb.pipe - baseline, total, "at {seq}");
            total
        });
    }

    #[test]
    fn process_ack_simple() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(500);
        let high_rxt = None;

        // Receive a single cumulative ack up to ack.
        assert!(!sb.process_ack(ack, snd_nxt, high_rxt, &SackBlocks::default(), TEST_MSS));
        assert_eq!(sb.acked_ranges, SeqRanges::default(),);
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap());

        // Cumulative ack doesn't move, 1 SACK range signaling loss is received.
        let sack1 = 10..(10 + sacked_bytes_threshold(TEST_MSS) + 1);
        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([sack1.clone()]),
            TEST_MSS
        ));
        assert_eq!(sb.acked_ranges, seq_ranges([(sack1.clone(), true)]),);
        assert_eq!(
            sb.pipe,
            u32::try_from(snd_nxt - ack).unwrap()
                - sb.sacked_bytes()
                - (sack1.start - u32::from(ack))
        );

        // Another SACK range comes in, at the end of this transmission block.
        let sack2 = (u32::from(snd_nxt) - u32::from(TEST_MSS))..u32::from(snd_nxt);
        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([sack1.clone(), sack2.clone()]),
            TEST_MSS
        ));
        assert_eq!(sb.acked_ranges, seq_ranges([(sack1.clone(), true), (sack2.clone(), false)]));
        assert_eq!(
            sb.pipe,
            u32::try_from(snd_nxt - ack).unwrap()
                - sb.sacked_bytes()
                - (sack1.start - u32::from(ack))
        );

        // Cumulative acknowledge the first SACK range.
        let ack = SeqNum::new(sack1.end);
        assert!(!sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([sack2.clone()]),
            TEST_MSS
        ));
        assert_eq!(sb.acked_ranges, seq_ranges([(sack2, false)]));
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap() - sb.sacked_bytes());

        // Cumulative acknowledge all the transmission.
        assert!(!sb.process_ack(snd_nxt, snd_nxt, high_rxt, &SackBlocks::default(), TEST_MSS));
        assert_eq!(sb.acked_ranges, SeqRanges::default());
        assert_eq!(sb.pipe, 0);
    }

    #[test]
    fn ack_after_snd_nxt() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(500);
        let high_rxt = None;
        let block = 10..20;
        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([block.clone()]),
            TEST_MSS
        ));
        assert_eq!(sb.acked_ranges, seq_ranges([(block.clone(), false)]));
        assert_eq!(sb.pipe, u32::try_from(snd_nxt - ack).unwrap() - sb.sacked_bytes());

        // SND.NXT rewinds after RTO.
        let snd_nxt = ack;
        // But we receive an ACK post the kept block.
        let ack = SeqNum::new(block.end);
        assert!(ack.after(snd_nxt));
        assert!(!sb.process_ack(ack, snd_nxt, high_rxt, &SackBlocks::default(), TEST_MSS));
        assert_eq!(sb.acked_ranges, SeqRanges::default());
        assert_eq!(sb.pipe, 0);
    }

    #[test]
    fn first_unsacked_range_from() {
        let mut sb = SackScoreboard::default();
        let ack = SeqNum::new(5);
        let snd_nxt = SeqNum::new(60);
        let high_rxt = None;
        let block1 = 10..20;
        let block2 = 30..40;
        let block3 = 50..60;
        assert!(sb.process_ack(
            ack,
            snd_nxt,
            high_rxt,
            &testutil::sack_blocks([block1.clone(), block2.clone(), block3.clone()]),
            TEST_MSS
        ));
        assert_eq!(
            sb.acked_ranges,
            seq_ranges([(block1.clone(), true), (block2.clone(), false), (block3.clone(), false)])
        );
        for high_rxt in u32::from(ack)..u32::from(snd_nxt) {
            let expect = if high_rxt < block3.start {
                let lost = high_rxt < block1.start;
                let (start, end) = if high_rxt < block1.start {
                    (high_rxt, block1.start)
                } else if high_rxt < block2.start {
                    (block1.end.max(high_rxt), block2.start)
                } else {
                    (block2.end.max(high_rxt), block3.start)
                };
                Some(SeqRange::new(SeqNum::new(start)..SeqNum::new(end), lost).unwrap())
            } else {
                None
            };
            assert_eq!(
                sb.first_unsacked_range_from(SeqNum::new(high_rxt)),
                expect,
                "high_rxt={high_rxt}"
            );
        }
    }
}
