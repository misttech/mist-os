// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements loss-based congestion control algorithms.
//!
//! The currently implemented algorithms are CUBIC from [RFC 8312] and RENO
//! style fast retransmit and fast recovery from [RFC 5681].
//!
//! [RFC 8312]: https://www.rfc-editor.org/rfc/rfc8312
//! [RFC 5681]: https://www.rfc-editor.org/rfc/rfc5681

mod cubic;

use core::cmp::Ordering;
use core::num::{NonZeroU32, NonZeroU8};
use core::time::Duration;

use netstack3_base::{Instant, Mss, SackBlocks, SeqNum, WindowSize};

use crate::internal::sack_scoreboard::SackScoreboard;

// Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#section-3.2):
///   The fast retransmit algorithm uses the arrival of 3 duplicate ACKs (...)
///   as an indication that a segment has been lost.
pub(crate) const DUP_ACK_THRESHOLD: u8 = 3;

/// Holds the parameters of congestion control that are common to algorithms.
#[derive(Debug)]
struct CongestionControlParams {
    /// Slow start threshold.
    ssthresh: u32,
    /// Congestion control window size, in bytes.
    cwnd: u32,
    /// Sender MSS.
    mss: Mss,
}

impl CongestionControlParams {
    fn with_mss(mss: Mss) -> Self {
        let mss_u32 = u32::from(mss);
        // Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#page-5):
        //   IW, the initial value of cwnd, MUST be set using the following
        //   guidelines as an upper bound.
        //   If SMSS > 2190 bytes:
        //       IW = 2 * SMSS bytes and MUST NOT be more than 2 segments
        //   If (SMSS > 1095 bytes) and (SMSS <= 2190 bytes):
        //       IW = 3 * SMSS bytes and MUST NOT be more than 3 segments
        //   if SMSS <= 1095 bytes:
        //       IW = 4 * SMSS bytes and MUST NOT be more than 4 segments
        let cwnd = if mss_u32 > 2190 {
            mss_u32 * 2
        } else if mss_u32 > 1095 {
            mss_u32 * 3
        } else {
            mss_u32 * 4
        };
        Self { cwnd, ssthresh: u32::MAX, mss }
    }

    fn rounded_cwnd(&self) -> CongestionWindow {
        CongestionWindow::new(self.cwnd, self.mss)
    }
}

mod cwnd {
    use super::*;
    /// A witness type for a congestion window that is rounded to a multiple of
    /// MSS.
    ///
    /// This type carries around the mss that was used to calculate it.
    #[derive(Debug, Copy, Clone)]
    #[cfg_attr(test, derive(Eq, PartialEq))]
    pub(crate) struct CongestionWindow {
        cwnd: u32,
        mss: Mss,
    }

    impl CongestionWindow {
        pub(super) fn new(cwnd: u32, mss: Mss) -> Self {
            let mss_u32 = u32::from(mss);
            Self { cwnd: cwnd / mss_u32 * mss_u32, mss }
        }

        pub(crate) fn cwnd(&self) -> u32 {
            self.cwnd
        }

        pub(crate) fn mss(&self) -> Mss {
            self.mss
        }
    }
}
pub(crate) use cwnd::CongestionWindow;

/// Congestion control with five intertwined algorithms.
///
/// - Slow start
/// - Congestion avoidance from a loss-based algorithm
/// - Fast retransmit
/// - Fast recovery: https://datatracker.ietf.org/doc/html/rfc5681#section-3
/// - SACK recovery: https://datatracker.ietf.org/doc/html/rfc6675
#[derive(Debug)]
pub(crate) struct CongestionControl<I> {
    params: CongestionControlParams,
    sack_scoreboard: SackScoreboard,
    algorithm: LossBasedAlgorithm<I>,
    /// The connection is in loss recovery when this field is a [`Some`].
    loss_recovery: Option<LossRecovery>,
}

/// Available congestion control algorithms.
#[derive(Debug)]
enum LossBasedAlgorithm<I> {
    Cubic(cubic::Cubic<I, true /* FAST_CONVERGENCE */>),
}

impl<I: Instant> LossBasedAlgorithm<I> {
    /// Called when there is a loss detected.
    ///
    /// Specifically, packet loss means
    /// - either when the retransmission timer fired;
    /// - or when we have received a certain amount of duplicate acks.
    fn on_loss_detected(&mut self, params: &mut CongestionControlParams) {
        match self {
            LossBasedAlgorithm::Cubic(cubic) => cubic.on_loss_detected(params),
        }
    }

    fn on_ack(
        &mut self,
        params: &mut CongestionControlParams,
        bytes_acked: NonZeroU32,
        now: I,
        rtt: Duration,
    ) {
        match self {
            LossBasedAlgorithm::Cubic(cubic) => cubic.on_ack(params, bytes_acked, now, rtt),
        }
    }

    fn on_retransmission_timeout(&mut self, params: &mut CongestionControlParams) {
        match self {
            LossBasedAlgorithm::Cubic(cubic) => cubic.on_retransmission_timeout(params),
        }
    }
}

impl<I: Instant> CongestionControl<I> {
    /// Preprocesses an ACK that may contain selective ack blocks.
    ///
    /// Returns `Some(true)` if this should be considered a duplicate ACK
    /// according to the rules in [RFC 6675 section 2]. Returns `Some(false)`
    /// otherwise.
    ///
    /// If the incoming ACK does not have SACK information, `None` is returned
    /// and the caller should use the classic algorithm to determine if this is
    /// a duplicate ACk.
    ///
    /// [RFC 6675 section 2]:
    ///     https://datatracker.ietf.org/doc/html/rfc6675#section-2
    pub(super) fn preprocess_ack(
        &mut self,
        seg_ack: SeqNum,
        snd_nxt: SeqNum,
        seg_sack_blocks: &SackBlocks,
    ) -> Option<bool> {
        let Self { params, algorithm: _, loss_recovery, sack_scoreboard } = self;
        let high_rxt = loss_recovery.as_ref().and_then(|lr| match lr {
            LossRecovery::FastRecovery(_) => None,
        });
        let is_dup_ack =
            sack_scoreboard.process_ack(seg_ack, snd_nxt, high_rxt, seg_sack_blocks, params.mss);
        (!seg_sack_blocks.is_empty()).then_some(is_dup_ack)
    }

    /// Informs the congestion control algorithm that a segment of length
    /// `seg_len` is being sent on the wire.
    ///
    /// This allows congestion control to keep the correct estimate of how many
    /// bytes are in flight.
    pub(super) fn on_will_send_segment(&mut self, seg_len: u32) {
        let Self { params: _, sack_scoreboard, algorithm: _, loss_recovery: _ } = self;
        // From RFC 6675:
        //
        //  (C.4) The estimate of the amount of data outstanding in the
        //  network must be updated by incrementing pipe by the number of
        //  octets transmitted in (C.1).
        sack_scoreboard.increment_pipe(seg_len);
    }

    /// Called when there are previously unacknowledged bytes being acked.
    ///
    /// If a round-trip-time estimation is not available, `rtt` can be `None`,
    /// but the loss-based algorithm is not updated in that case.
    ///
    /// Returns `true` if this ack signals a loss recovery.
    pub(super) fn on_ack(
        &mut self,
        // TODO(https://fxbug.dev/42078221): Pass this to SACK recovery.
        _seg_ack: SeqNum,
        bytes_acked: NonZeroU32,
        now: I,
        rtt: Option<Duration>,
    ) -> bool {
        let Self { params, algorithm, loss_recovery, sack_scoreboard: _ } = self;
        // Exit fast recovery since there is an ACK that acknowledges new data.
        let outcome = match loss_recovery {
            None => LossRecoveryOnAckOutcome::None,
            Some(LossRecovery::FastRecovery(fast_recovery)) => fast_recovery.on_ack(params),
        };

        let recovered = match outcome {
            LossRecoveryOnAckOutcome::None => false,
            LossRecoveryOnAckOutcome::Discard { recovered } => {
                *loss_recovery = None;
                recovered
            }
        };

        // It is possible, however unlikely, that we get here without an RTT
        // estimation - in case the first data segment that we send out gets
        // retransmitted. In that case, simply don't update the congestion
        // parameters with the loss based algorithm which at worst causes slow
        // start to take one extra step.
        if let Some(rtt) = rtt {
            algorithm.on_ack(params, bytes_acked, now, rtt);
        }
        recovered
    }

    /// Called when a duplicate ack is arrived.
    ///
    /// Returns `true` if loss recovery was initiated as a result of this ACK.
    pub(super) fn on_dup_ack(
        &mut self,
        seg_ack: SeqNum,
        // TODO(https://fxbug.dev/42078221): Pass this to SACK recovery.
        _snd_nxt: SeqNum,
    ) -> bool {
        let Self { params, algorithm, loss_recovery, sack_scoreboard: _ } = self;
        match loss_recovery {
            None => {
                *loss_recovery = Some(LossRecovery::FastRecovery(FastRecovery::new()));
                false
            }
            Some(LossRecovery::FastRecovery(fast_recovery)) => {
                fast_recovery.on_dup_ack(params, algorithm, seg_ack)
            }
        }
    }

    /// Called upon a retransmission timeout.
    pub(super) fn on_retransmission_timeout(&mut self) {
        let Self { params, algorithm, loss_recovery, sack_scoreboard } = self;
        sack_scoreboard.on_retransmission_timeout();
        // TODO(https://fxbug.dev/42078221): Handle SACK retransmission
        // timeouts.
        *loss_recovery = None;
        algorithm.on_retransmission_timeout(params);
    }

    pub(super) fn slow_start_threshold(&self) -> u32 {
        self.params.ssthresh
    }

    #[cfg(test)]
    pub(super) fn pipe(&self) -> u32 {
        self.sack_scoreboard.pipe()
    }

    pub(super) fn cubic_with_mss(mss: Mss) -> Self {
        Self {
            params: CongestionControlParams::with_mss(mss),
            algorithm: LossBasedAlgorithm::Cubic(Default::default()),
            loss_recovery: None,
            sack_scoreboard: SackScoreboard::default(),
        }
    }

    pub(super) fn mss(&self) -> Mss {
        self.params.mss
    }

    pub(super) fn update_mss(&mut self, mss: Mss, snd_una: SeqNum, snd_nxt: SeqNum) {
        let Self { params, sack_scoreboard, algorithm: _, loss_recovery } = self;
        // From [RFC 5681 section 3.1]:
        //
        //    When initial congestion windows of more than one segment are
        //    implemented along with Path MTU Discovery [RFC1191], and the MSS
        //    being used is found to be too large, the congestion window cwnd
        //    SHOULD be reduced to prevent large bursts of smaller segments.
        //    Specifically, cwnd SHOULD be reduced by the ratio of the old segment
        //    size to the new segment size.
        //
        // [RFC 5681 section 3.1]: https://datatracker.ietf.org/doc/html/rfc5681#section-3.1
        if params.ssthresh == u32::MAX {
            params.cwnd =
                params.cwnd.saturating_div(u32::from(params.mss)).saturating_mul(u32::from(mss));
        }
        params.mss = mss;

        // Given we'll retransmit after receiving this, we need to update the
        // SACK scoreboard so pipe is recalculated based on this value of
        // snd_nxt and mss.
        let high_rxt = loss_recovery.as_ref().and_then(|lr| match lr {
            LossRecovery::FastRecovery(_) => None,
        });
        sack_scoreboard.on_mss_update(snd_una, snd_nxt, high_rxt, mss);
    }

    /// Returns the rounded unmodified by loss recovery window size.
    ///
    /// This is meant to be used for inspection only. Congestion calculation for
    /// sending should use [`CongestionControl::poll_send`].
    pub(super) fn inspect_cwnd(&self) -> CongestionWindow {
        self.params.rounded_cwnd()
    }

    /// Returns true if this [`CongestionControl`] is in fast recovery.
    pub(super) fn in_fast_recovery(&self) -> bool {
        self.loss_recovery.is_some()
    }

    /// Returns true if this [`CongestionControl`] is in slow start.
    pub(super) fn in_slow_start(&self) -> bool {
        self.params.cwnd < self.params.ssthresh
    }

    /// Polls congestion control for the next segment to be sent out.
    ///
    /// Receives pertinent parameters from the sender state machine to allow for
    /// this decision:
    ///
    /// - `snd_una` is SND.UNA the highest unacknowledged sequence number.
    /// - `snd_nxt` is SND.NXT, the next sequence number the sender would send
    ///   without loss recovery.
    /// - `snd_wnd` is the total send window, i.e. the allowable receiver window
    ///   after `snd_una`.
    /// - `available_bytes` is the total number of bytes in the send buffer,
    ///   starting at `snd_una`.
    ///
    /// Returns `None` if no segment should be sent right now.
    pub(super) fn poll_send(
        &mut self,
        snd_una: SeqNum,
        snd_nxt: SeqNum,
        snd_wnd: WindowSize,
        available_bytes: usize,
    ) -> Option<CongestionControlSendOutcome> {
        let Self { params, algorithm: _, loss_recovery, sack_scoreboard } = self;
        let cwnd = params.rounded_cwnd();

        // TODO(https://fxbug.dev/42078221): Pass these to SACK recovery.
        let _ = (snd_una, snd_wnd, available_bytes);

        match loss_recovery {
            None => {
                let pipe = sack_scoreboard.pipe();
                let congestion_window = cwnd.cwnd();
                let available_window = congestion_window.saturating_sub(pipe);
                let congestion_limit = available_window.min(cwnd.mss().into());
                Some(CongestionControlSendOutcome {
                    next_seg: snd_nxt,
                    congestion_limit,
                    congestion_window,
                    loss_recovery: false,
                })
            }
            Some(LossRecovery::FastRecovery(fast_recovery)) => {
                Some(fast_recovery.poll_send(cwnd, sack_scoreboard.pipe(), snd_nxt))
            }
        }
    }
}

/// The outcome of [`CongestionControl::poll_send`].
pub(super) struct CongestionControlSendOutcome {
    /// The next segment to be sent out.
    pub next_seg: SeqNum,
    /// The maximum number of bytes post next_seg that can be sent.
    ///
    /// This limit does not account for the unused/open window on the receiver.
    ///
    /// This is limited by the current congestion limit and the sender MSS.
    pub congestion_limit: u32,
    /// The congestion window used to calculate `congestion limit`.
    ///
    /// This is the estimated total congestion window, including loss
    /// recovery-based inflation.
    pub congestion_window: u32,
    /// True if this segment is the outcome of a loss recovery attempt.
    pub loss_recovery: bool,
}

/// The current loss recovery mode.
#[derive(Debug)]
pub enum LossRecovery {
    FastRecovery(FastRecovery),
    // TODO(https://fxbug.dev/42078221): Add SACK based recovery variant.
}

enum LossRecoveryOnAckOutcome {
    None,
    Discard { recovered: bool },
}

/// Reno style Fast Recovery algorithm as described in
/// [RFC 5681](https://tools.ietf.org/html/rfc5681).
#[derive(Debug)]
pub struct FastRecovery {
    /// Holds the sequence number of the segment to fast retransmit, if any.
    fast_retransmit: Option<SeqNum>,
    /// The running count of consecutive duplicate ACKs we have received so far.
    ///
    /// Here we limit the maximum number of duplicate ACKS we track to 255, as
    /// per a note in the RFC:
    ///
    /// Note: [SCWA99] discusses a receiver-based attack whereby many
    /// bogus duplicate ACKs are sent to the data sender in order to
    /// artificially inflate cwnd and cause a higher than appropriate
    /// sending rate to be used.  A TCP MAY therefore limit the number of
    /// times cwnd is artificially inflated during loss recovery to the
    /// number of outstanding segments (or, an approximation thereof).
    ///
    /// [SCWA99]: https://homes.cs.washington.edu/~tom/pubs/CCR99.pdf
    dup_acks: NonZeroU8,
}

impl FastRecovery {
    fn new() -> Self {
        Self { dup_acks: NonZeroU8::new(1).unwrap(), fast_retransmit: None }
    }

    fn poll_send(
        &mut self,
        cwnd: CongestionWindow,
        used_congestion_window: u32,
        snd_nxt: SeqNum,
    ) -> CongestionControlSendOutcome {
        let Self { fast_retransmit, dup_acks } = self;
        // Per RFC 3042 (https://www.rfc-editor.org/rfc/rfc3042#section-2): ...
        // the Limited Transmit algorithm, which calls for a TCP sender to
        //   transmit new data upon the arrival of the first two consecutive
        //   duplicate ACKs ... The amount of outstanding data would remain less
        //   than or equal to the congestion window plus 2 segments.  In other
        //   words, the sender can only send two segments beyond the congestion
        //   window (cwnd).
        //
        // Note: We don't directly change cwnd in the loss-based algorithm
        // because the RFC says one MUST NOT do that. We follow the requirement
        // here by not changing the cwnd of the algorithm - if a new ACK is
        // received after the two dup acks, the loss-based algorithm will
        // continue to operate the same way as if the 2 SMSS is never added to
        // cwnd.
        let congestion_window = if dup_acks.get() < DUP_ACK_THRESHOLD {
            cwnd.cwnd().saturating_add(u32::from(dup_acks.get()) * u32::from(cwnd.mss()))
        } else {
            cwnd.cwnd()
        };

        // Elect fast retransmit sequence number or snd_nxt if we don't have
        // one.
        let (next_seg, loss_recovery, congestion_limit) = match fast_retransmit.take() {
            // From RFC 5861:
            //
            //  3. The lost segment starting at SND.UNA MUST be retransmitted
            //     [...].
            //
            // So we always set the congestion limit to be just the mss.
            Some(f) => (f, true, cwnd.mss().into()),
            // There's no fast retransmit pending, use snd_nxt applying the used
            // congestion window.
            None => (
                snd_nxt,
                false,
                congestion_window.saturating_sub(used_congestion_window).min(cwnd.mss().into()),
            ),
        };
        CongestionControlSendOutcome {
            next_seg,
            congestion_limit,
            congestion_window,
            loss_recovery,
        }
    }

    fn on_ack(&mut self, params: &mut CongestionControlParams) -> LossRecoveryOnAckOutcome {
        let recovered = self.dup_acks.get() >= DUP_ACK_THRESHOLD;
        if recovered {
            // Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#section-3.2):
            //   When the next ACK arrives that acknowledges previously
            //   unacknowledged data, a TCP MUST set cwnd to ssthresh (the value
            //   set in step 2).  This is termed "deflating" the window.
            params.cwnd = params.ssthresh;
        }
        LossRecoveryOnAckOutcome::Discard { recovered }
    }

    /// Processes a duplicate ack with sequence number `seg_ack`.
    ///
    /// Returns `true` if loss recovery is triggered.
    fn on_dup_ack<I: Instant>(
        &mut self,
        params: &mut CongestionControlParams,
        loss_based: &mut LossBasedAlgorithm<I>,
        seg_ack: SeqNum,
    ) -> bool {
        self.dup_acks = self.dup_acks.saturating_add(1);

        match self.dup_acks.get().cmp(&DUP_ACK_THRESHOLD) {
            Ordering::Less => false,
            Ordering::Equal => {
                loss_based.on_loss_detected(params);
                // Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#section-3.2):
                //   The lost segment starting at SND.UNA MUST be retransmitted
                //   and cwnd set to ssthresh plus 3*SMSS.  This artificially
                //   "inflates" the congestion window by the number of segments
                //   (three) that have left the network and which the receiver
                //   has buffered.
                self.fast_retransmit = Some(seg_ack);
                params.cwnd =
                    params.ssthresh + u32::from(DUP_ACK_THRESHOLD) * u32::from(params.mss);
                true
            }
            Ordering::Greater => {
                // Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#section-3.2):
                //   For each additional duplicate ACK received (after the third),
                //   cwnd MUST be incremented by SMSS. This artificially inflates
                //   the congestion window in order to reflect the additional
                //   segment that has left the network.
                params.cwnd = params.cwnd.saturating_add(u32::from(params.mss));
                false
            }
        }
    }
}

#[cfg(test)]
mod test {
    use netstack3_base::testutil::FakeInstant;

    use super::*;
    use crate::internal::testutil::{self, DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE};

    #[test]
    fn no_recovery_before_reaching_threshold() {
        let mut congestion_control =
            CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE);
        let old_cwnd = congestion_control.params.cwnd;
        assert_eq!(congestion_control.params.ssthresh, u32::MAX);
        assert!(!congestion_control.on_dup_ack(SeqNum::new(0), SeqNum::new(1)));
        assert!(!congestion_control.on_ack(
            SeqNum::new(1),
            NonZeroU32::new(1).unwrap(),
            FakeInstant::from(Duration::from_secs(0)),
            Some(Duration::from_secs(1)),
        ));
        // We have only received one duplicate ack, receiving a new ACK should
        // not mean "loss recovery" - we should not bump our cwnd to initial
        // ssthresh (u32::MAX) and then overflow.
        assert_eq!(old_cwnd + 1, congestion_control.params.cwnd);
    }

    #[test]
    fn preprocess_ack_result() {
        let ack = SeqNum::new(1);
        let snd_nxt = SeqNum::new(100);
        let mut congestion_control =
            CongestionControl::<FakeInstant>::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE);
        assert_eq!(congestion_control.preprocess_ack(ack, snd_nxt, &SackBlocks::EMPTY), None);
        assert_eq!(
            congestion_control.preprocess_ack(ack, snd_nxt, &testutil::sack_blocks([10..20])),
            Some(true)
        );
        assert_eq!(congestion_control.preprocess_ack(ack, snd_nxt, &SackBlocks::EMPTY), None);
        assert_eq!(
            congestion_control.preprocess_ack(ack, snd_nxt, &testutil::sack_blocks([10..20])),
            Some(false)
        );
        assert_eq!(
            congestion_control.preprocess_ack(
                ack,
                snd_nxt,
                &testutil::sack_blocks([10..20, 20..30])
            ),
            Some(true)
        );
    }
}
