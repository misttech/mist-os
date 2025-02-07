// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TCP state machine per [RFC 793](https://tools.ietf.org/html/rfc793).
// Note: All RFC quotes (with two extra spaces at the beginning of each line) in
// this file are from https://tools.ietf.org/html/rfc793#section-3.9 if not
// specified otherwise.

use core::convert::{Infallible, TryFrom as _};
use core::fmt::Debug;
use core::num::{NonZeroU32, NonZeroU8, NonZeroUsize, TryFromIntError};
use core::ops::{Deref, DerefMut};
use core::time::Duration;

use assert_matches::assert_matches;
use derivative::Derivative;
use explicit::ResultExt as _;
use log::error;
use netstack3_base::{
    Control, IcmpErrorCode, Instant, Mss, Options, Payload, PayloadLen as _, Segment,
    SegmentHeader, SeqNum, UnscaledWindowSize, WindowScale, WindowSize,
};
use packet_formats::utils::NonZeroDuration;
use replace_with::{replace_with, replace_with_and};

use crate::internal::base::{
    BufferSizes, BuffersRefMut, ConnectionError, KeepAlive, SocketOptions, TcpCountersInner,
};
use crate::internal::buffer::{Assembler, BufferLimits, IntoBuffers, ReceiveBuffer, SendBuffer};
use crate::internal::congestion::CongestionControl;
use crate::internal::rtt::Estimator;

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-81):
/// MSL
///       Maximum Segment Lifetime, the time a TCP segment can exist in
///       the internetwork system.  Arbitrarily defined to be 2 minutes.
pub(super) const MSL: Duration = Duration::from_secs(2 * 60);

const DEFAULT_MAX_RETRIES: NonZeroU8 = NonZeroU8::new(12).unwrap();
/// Assuming a 200ms RTT, with 12 retries, the timeout should be at least 0.2 * (2 ^ 12 - 1) = 819s.
/// Rounding it up to have some extra buffer.
const DEFAULT_USER_TIMEOUT: Duration = Duration::from_secs(900);

/// Default maximum SYN's to send before giving up an attempt to connect.
// TODO(https://fxbug.dev/42077087): Make these constants configurable.
pub(super) const DEFAULT_MAX_SYN_RETRIES: NonZeroU8 = NonZeroU8::new(6).unwrap();
const DEFAULT_MAX_SYNACK_RETRIES: NonZeroU8 = NonZeroU8::new(5).unwrap();

/// Per RFC 9293 (https://tools.ietf.org/html/rfc9293#section-3.8.6.3):
///  ... in particular, the delay MUST be less than 0.5 seconds.
const ACK_DELAY_THRESHOLD: Duration = Duration::from_millis(500);
/// Per RFC 9293 Section 3.8.6.2.1:
///  ... The override timeout should be in the range 0.1 - 1.0 seconds.
/// Note that we pick the lower end of the range because this case should be
/// rare and the installing a timer itself represents a high probability of
/// receiver having reduced its window so that our MAX(SND.WND) is an
/// overestimation, so we choose the value to avoid unnecessary delay.
const SWS_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
/// Per RFC 9293 Section 3.8.6.2.2 and 3.8.6.2.1:
///   where Fr is a fraction whose recommended value is 1/2,
/// Note that we use the inverse since we want to avoid floating point.
const SWS_BUFFER_FACTOR: u32 = 2;

/// A helper trait for duration socket options that use 0 to indicate default.
trait NonZeroDurationOptionExt {
    fn get_or_default(&self, default: Duration) -> Duration;
}

impl NonZeroDurationOptionExt for Option<NonZeroDuration> {
    fn get_or_default(&self, default: Duration) -> Duration {
        self.map(NonZeroDuration::get).unwrap_or(default)
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-22):
///
///   CLOSED - represents no connection state at all.
///
/// Allowed operations:
///   - listen
///   - connect
/// Disallowed operations:
///   - send
///   - recv
///   - shutdown
///   - accept
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Closed<Error> {
    /// Describes a reason why the connection was closed.
    pub(crate) reason: Error,
}

/// An uninhabited type used together with [`Closed`] to sugest that it is in
/// initial condition and no errors have occurred yet.
pub(crate) enum Initial {}

impl Closed<Initial> {
    /// Corresponds to the [OPEN](https://tools.ietf.org/html/rfc793#page-54)
    /// user call.
    ///
    /// `iss`is The initial send sequence number. Which is effectively the
    /// sequence number of SYN.
    pub(crate) fn connect<I: Instant, ActiveOpen>(
        iss: SeqNum,
        now: I,
        active_open: ActiveOpen,
        buffer_sizes: BufferSizes,
        device_mss: Mss,
        default_mss: Mss,
        SocketOptions {
            keep_alive: _,
            nagle_enabled: _,
            user_timeout,
            delayed_ack: _,
            fin_wait2_timeout: _,
            max_syn_retries,
            ip_options: _,
        }: &SocketOptions,
    ) -> (SynSent<I, ActiveOpen>, Segment<()>) {
        let user_timeout = user_timeout.get_or_default(DEFAULT_USER_TIMEOUT);
        let rcv_wnd_scale = buffer_sizes.rwnd().scale();
        // RFC 7323 Section 2.2:
        //  The window field in a segment where the SYN bit is set (i.e., a
        //  <SYN> or <SYN,ACK>) MUST NOT be scaled.
        let rwnd = buffer_sizes.rwnd_unscaled();
        (
            SynSent {
                iss,
                timestamp: Some(now),
                retrans_timer: RetransTimer::new(
                    now,
                    Estimator::RTO_INIT,
                    user_timeout,
                    *max_syn_retries,
                ),
                active_open,
                buffer_sizes,
                device_mss,
                default_mss,
                rcv_wnd_scale,
            },
            Segment::syn(
                iss,
                rwnd,
                Options { mss: Some(device_mss), window_scale: Some(rcv_wnd_scale) },
            ),
        )
    }

    pub(crate) fn listen(
        iss: SeqNum,
        buffer_sizes: BufferSizes,
        device_mss: Mss,
        default_mss: Mss,
        user_timeout: Option<NonZeroDuration>,
    ) -> Listen {
        Listen { iss, buffer_sizes, device_mss, default_mss, user_timeout }
    }
}

impl<Error> Closed<Error> {
    /// Processes an incoming segment in the CLOSED state.
    ///
    /// TCP will either drop the incoming segment or generate a RST.
    pub(crate) fn on_segment(&self, segment: &Segment<impl Payload>) -> Option<Segment<()>> {
        let segment_len = segment.len();
        let Segment {
            header: SegmentHeader { seq: seg_seq, ack: seg_ack, wnd: _, control, options: _ },
            data: _,
        } = segment;

        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
        //   If the state is CLOSED (i.e., TCB does not exist) then
        //   all data in the incoming segment is discarded.  An incoming
        //   segment containing a RST is discarded.  An incoming segment
        //   not containing a RST causes a RST to be sent in response.
        //   The acknowledgment and sequence field values are selected to
        //   make the reset sequence acceptable to the TCP that sent the
        //   offending segment.
        //   If the ACK bit is off, sequence number zero is used,
        //    <SEQ=0><ACK=SEG.SEQ+SEG.LEN><CTL=RST,ACK>
        //   If the ACK bit is on,
        //    <SEQ=SEG.ACK><CTL=RST>
        //   Return.
        if *control == Some(Control::RST) {
            return None;
        }
        Some(match seg_ack {
            Some(seg_ack) => Segment::rst(*seg_ack),
            None => Segment::rst_ack(SeqNum::from(0), *seg_seq + segment_len),
        })
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
///   LISTEN - represents waiting for a connection request from any remote
///   TCP and port.
///
/// Allowed operations:
///   - send (queued until connection is established)
///   - recv (queued until connection is established)
///   - connect
///   - shutdown
///   - accept
/// Disallowed operations:
///   - listen
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Listen {
    iss: SeqNum,
    buffer_sizes: BufferSizes,
    device_mss: Mss,
    default_mss: Mss,
    user_timeout: Option<NonZeroDuration>,
}

/// Dispositions of [`Listen::on_segment`].
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum ListenOnSegmentDisposition<I: Instant> {
    SendSynAckAndEnterSynRcvd(Segment<()>, SynRcvd<I, Infallible>),
    SendRst(Segment<()>),
    Ignore,
}

impl Listen {
    fn on_segment<I: Instant>(
        &self,
        Segment { header: SegmentHeader { seq, ack, wnd: _, control, options }, data: _ }: Segment<
            impl Payload,
        >,
        now: I,
    ) -> ListenOnSegmentDisposition<I> {
        let Listen { iss, buffer_sizes, device_mss, default_mss, user_timeout } = *self;
        let smss = options.mss.unwrap_or(default_mss).min(device_mss);
        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
        //   first check for an RST
        //   An incoming RST should be ignored.  Return.
        if control == Some(Control::RST) {
            return ListenOnSegmentDisposition::Ignore;
        }
        if let Some(ack) = ack {
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
            //   second check for an ACK
            //   Any acknowledgment is bad if it arrives on a connection still in
            //   the LISTEN state.  An acceptable reset segment should be formed
            //   for any arriving ACK-bearing segment.  The RST should be
            //   formatted as follows:
            //     <SEQ=SEG.ACK><CTL=RST>
            //   Return.
            return ListenOnSegmentDisposition::SendRst(Segment::rst(ack));
        }
        if control == Some(Control::SYN) {
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
            //   third check for a SYN
            //   Set RCV.NXT to SEG.SEQ+1, IRS is set to SEG.SEQ and any other
            //   control or text should be queued for processing later.  ISS
            //   should be selected and a SYN segment sent of the form:
            //     <SEQ=ISS><ACK=RCV.NXT><CTL=SYN,ACK>
            //   SND.NXT is set to ISS+1 and SND.UNA to ISS.  The connection
            //   state should be changed to SYN-RECEIVED.  Note that any other
            //   incoming control or data (combined with SYN) will be processed
            //   in the SYN-RECEIVED state, but processing of SYN and ACK should
            //   not be repeated.
            // Note: We don't support data being tranmistted in this state, so
            // there is no need to store these the RCV and SND variables.
            let user_timeout = user_timeout.get_or_default(DEFAULT_USER_TIMEOUT);
            let rcv_wnd_scale = buffer_sizes.rwnd().scale();
            // RFC 7323 Section 2.2:
            //  The window field in a segment where the SYN bit is set (i.e., a
            //  <SYN> or <SYN,ACK>) MUST NOT be scaled.
            let rwnd = buffer_sizes.rwnd_unscaled();
            return ListenOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                Segment::syn_ack(
                    iss,
                    seq + 1,
                    rwnd,
                    Options {
                        mss: Some(smss),
                        // Per RFC 7323 Section 2.3:
                        //   If a TCP receives a <SYN> segment containing a
                        //   Window Scale option, it SHOULD send its own Window
                        //   Scale option in the <SYN,ACK> segment.
                        window_scale: options.window_scale.map(|_| rcv_wnd_scale),
                    },
                ),
                SynRcvd {
                    iss,
                    irs: seq,
                    timestamp: Some(now),
                    retrans_timer: RetransTimer::new(
                        now,
                        Estimator::RTO_INIT,
                        user_timeout,
                        DEFAULT_MAX_SYNACK_RETRIES,
                    ),
                    simultaneous_open: None,
                    buffer_sizes,
                    smss,
                    rcv_wnd_scale,
                    snd_wnd_scale: options.window_scale,
                },
            );
        }
        ListenOnSegmentDisposition::Ignore
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
///   SYN-SENT - represents waiting for a matching connection request
///   after having sent a connection request.
///
/// Allowed operations:
///   - send (queued until connection is established)
///   - recv (queued until connection is established)
///   - shutdown
/// Disallowed operations:
///   - listen
///   - accept
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct SynSent<I, ActiveOpen> {
    iss: SeqNum,
    // The timestamp when the SYN segment was sent. A `None` here means that
    // the SYN segment was retransmitted so that it can't be used to estimate
    // RTT.
    timestamp: Option<I>,
    retrans_timer: RetransTimer<I>,
    active_open: ActiveOpen,
    buffer_sizes: BufferSizes,
    device_mss: Mss,
    default_mss: Mss,
    rcv_wnd_scale: WindowScale,
}

/// Dispositions of [`SynSent::on_segment`].
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum SynSentOnSegmentDisposition<I: Instant, ActiveOpen> {
    SendAckAndEnterEstablished(Established<I, (), ()>),
    SendSynAckAndEnterSynRcvd(Segment<()>, SynRcvd<I, ActiveOpen>),
    SendRst(Segment<()>),
    EnterClosed(Closed<Option<ConnectionError>>),
    Ignore,
}

impl<I: Instant + 'static, ActiveOpen> SynSent<I, ActiveOpen> {
    /// Processes an incoming segment in the SYN-SENT state.
    ///
    /// Transitions to ESTABLSHED if the incoming segment is a proper SYN-ACK.
    /// Transitions to SYN-RCVD if the incoming segment is a SYN. Otherwise,
    /// the segment is dropped or an RST is generated.
    fn on_segment(
        &self,
        Segment {
            header: SegmentHeader { seq: seg_seq, ack: seg_ack, wnd: seg_wnd, control, options },
            data: _,
        }: Segment<impl Payload>,
        now: I,
    ) -> SynSentOnSegmentDisposition<I, ActiveOpen> {
        let SynSent {
            iss,
            timestamp: syn_sent_ts,
            retrans_timer: RetransTimer { user_timeout_until, remaining_retries: _, at: _, rto: _ },
            active_open: _,
            buffer_sizes,
            device_mss,
            default_mss,
            rcv_wnd_scale,
        } = *self;
        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
        //   first check the ACK bit
        //   If the ACK bit is set
        //     If SEG.ACK =< ISS, or SEG.ACK > SND.NXT, send a reset (unless
        //     the RST bit is set, if so drop the segment and return)
        //       <SEQ=SEG.ACK><CTL=RST>
        //     and discard the segment.  Return.
        //     If SND.UNA =< SEG.ACK =< SND.NXT then the ACK is acceptable.
        let has_ack = match seg_ack {
            Some(ack) => {
                // In our implementation, because we don't carry data in our
                // initial SYN segment, SND.UNA == ISS, SND.NXT == ISS+1.
                if ack.before(iss) || ack.after(iss + 1) {
                    return if control == Some(Control::RST) {
                        SynSentOnSegmentDisposition::Ignore
                    } else {
                        SynSentOnSegmentDisposition::SendRst(Segment::rst(ack))
                    };
                }
                true
            }
            None => false,
        };

        match control {
            Some(Control::RST) => {
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-67):
                //   second check the RST bit
                //   If the RST bit is set
                //     If the ACK was acceptable then signal the user "error:
                //     connection reset", drop the segment, enter CLOSED state,
                //     delete TCB, and return.  Otherwise (no ACK) drop the
                //     segment and return.
                if has_ack {
                    SynSentOnSegmentDisposition::EnterClosed(Closed {
                        reason: Some(ConnectionError::ConnectionRefused),
                    })
                } else {
                    SynSentOnSegmentDisposition::Ignore
                }
            }
            Some(Control::SYN) => {
                let smss = options.mss.unwrap_or(default_mss).min(device_mss);
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-67):
                //   fourth check the SYN bit
                //   This step should be reached only if the ACK is ok, or there
                //   is no ACK, and it [sic] the segment did not contain a RST.
                match seg_ack {
                    Some(seg_ack) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-67):
                        //   If the SYN bit is on and the security/compartment
                        //   and precedence are acceptable then, RCV.NXT is set
                        //   to SEG.SEQ+1, IRS is set to SEG.SEQ.  SND.UNA
                        //   should be advanced to equal SEG.ACK (if there is an
                        //   ACK), and any segments on the retransmission queue
                        //   which are thereby acknowledged should be removed.

                        //   If SND.UNA > ISS (our SYN has been ACKed), change
                        //   the connection state to ESTABLISHED, form an ACK
                        //   segment
                        //     <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK>
                        //   and send it.  Data or controls which were queued
                        //   for transmission may be included.  If there are
                        //   other controls or text in the segment then
                        //   continue processing at the sixth step below where
                        //   the URG bit is checked, otherwise return.
                        if seg_ack.after(iss) {
                            let irs = seg_seq;
                            let mut rtt_estimator = Estimator::default();
                            if let Some(syn_sent_ts) = syn_sent_ts {
                                rtt_estimator.sample(now.saturating_duration_since(syn_sent_ts));
                            }
                            let (rcv_wnd_scale, snd_wnd_scale) = options
                                .window_scale
                                .map(|snd_wnd_scale| (rcv_wnd_scale, snd_wnd_scale))
                                .unwrap_or_default();
                            let established = Established {
                                snd: Send {
                                    nxt: iss + 1,
                                    max: iss + 1,
                                    una: seg_ack,
                                    // This segment has a SYN, do not scale.
                                    wnd: seg_wnd << WindowScale::default(),
                                    wl1: seg_seq,
                                    wl2: seg_ack,
                                    buffer: (),
                                    last_seq_ts: None,
                                    rtt_estimator,
                                    timer: None,
                                    congestion_control: CongestionControl::cubic_with_mss(smss),
                                    wnd_scale: snd_wnd_scale,
                                    wnd_max: seg_wnd << WindowScale::default(),
                                }
                                .into(),
                                rcv: Recv {
                                    buffer: RecvBufferState::Open {
                                        buffer: (),
                                        assembler: Assembler::new(irs + 1),
                                    },
                                    timer: None,
                                    mss: smss,
                                    wnd_scale: rcv_wnd_scale,
                                    last_window_update: (irs + 1, buffer_sizes.rwnd()),
                                }
                                .into(),
                            };
                            SynSentOnSegmentDisposition::SendAckAndEnterEstablished(established)
                        } else {
                            SynSentOnSegmentDisposition::Ignore
                        }
                    }
                    None => {
                        if now < user_timeout_until {
                            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-68):
                            //   Otherwise enter SYN-RECEIVED, form a SYN,ACK
                            //   segment
                            //     <SEQ=ISS><ACK=RCV.NXT><CTL=SYN,ACK>
                            //   and send it.  If there are other controls or text
                            //   in the segment, queue them for processing after the
                            //   ESTABLISHED state has been reached, return.
                            let rcv_wnd_scale = buffer_sizes.rwnd().scale();
                            // RFC 7323 Section 2.2:
                            //  The window field in a segment where the SYN bit
                            //  is set (i.e., a <SYN> or <SYN,ACK>) MUST NOT be
                            //  scaled.
                            let rwnd = buffer_sizes.rwnd_unscaled();
                            SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                                Segment::syn_ack(
                                    iss,
                                    seg_seq + 1,
                                    rwnd,
                                    Options {
                                        mss: Some(smss),
                                        window_scale: options.window_scale.map(|_| rcv_wnd_scale),
                                    },
                                ),
                                SynRcvd {
                                    iss,
                                    irs: seg_seq,
                                    timestamp: Some(now),
                                    retrans_timer: RetransTimer::new(
                                        now,
                                        Estimator::RTO_INIT,
                                        user_timeout_until.saturating_duration_since(now),
                                        DEFAULT_MAX_SYNACK_RETRIES,
                                    ),
                                    // This should be set to active_open by the caller:
                                    simultaneous_open: None,
                                    buffer_sizes,
                                    smss,
                                    rcv_wnd_scale,
                                    snd_wnd_scale: options.window_scale,
                                },
                            )
                        } else {
                            SynSentOnSegmentDisposition::EnterClosed(Closed { reason: None })
                        }
                    }
                }
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-68):
            //   fifth, if neither of the SYN or RST bits is set then drop the
            //   segment and return.
            Some(Control::FIN) | None => SynSentOnSegmentDisposition::Ignore,
        }
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
///   SYN-RECEIVED - represents waiting for a confirming connection
///   request acknowledgment after having both received and sent a
///   connection request.
///
/// Allowed operations:
///   - send (queued until connection is established)
///   - recv (queued until connection is established)
///   - shutdown
/// Disallowed operations:
///   - listen
///   - accept
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct SynRcvd<I, ActiveOpen> {
    iss: SeqNum,
    irs: SeqNum,
    /// The timestamp when the SYN segment was received, and consequently, our
    /// SYN-ACK segment was sent. A `None` here means that the SYN-ACK segment
    /// was retransmitted so that it can't be used to estimate RTT.
    timestamp: Option<I>,
    retrans_timer: RetransTimer<I>,
    /// Indicates that we arrive this state from [`SynSent`], i.e., this was an
    /// active open connection. Store this information so that we don't use the
    /// wrong routines to construct buffers.
    simultaneous_open: Option<ActiveOpen>,
    buffer_sizes: BufferSizes,
    /// The sender MSS negotiated as described in [RFC 9293 section 3.7.1].
    ///
    /// [RFC 9293 section 3.7.1]: https://datatracker.ietf.org/doc/html/rfc9293#name-maximum-segment-size-option
    smss: Mss,
    rcv_wnd_scale: WindowScale,
    snd_wnd_scale: Option<WindowScale>,
}

impl<I: Instant, R: ReceiveBuffer, S: SendBuffer, ActiveOpen> From<SynRcvd<I, Infallible>>
    for State<I, R, S, ActiveOpen>
{
    fn from(
        SynRcvd {
            iss,
            irs,
            timestamp,
            retrans_timer,
            simultaneous_open,
            buffer_sizes,
            smss,
            rcv_wnd_scale,
            snd_wnd_scale,
        }: SynRcvd<I, Infallible>,
    ) -> Self {
        match simultaneous_open {
            None => State::SynRcvd(SynRcvd {
                iss,
                irs,
                timestamp,
                retrans_timer,
                simultaneous_open: None,
                buffer_sizes,
                smss,
                rcv_wnd_scale,
                snd_wnd_scale,
            }),
        }
    }
}
enum FinQueued {}

impl FinQueued {
    // TODO(https://github.com/rust-lang/rust/issues/95174): Before we can use
    // enum for const generics, we define the following constants to give
    // meaning to the bools when used.
    const YES: bool = true;
    const NO: bool = false;
}

/// TCP control block variables that are responsible for sending.
#[derive(Derivative)]
#[derivative(Debug)]
#[cfg_attr(test, derivative(PartialEq, Eq))]
pub(crate) struct Send<I, S, const FIN_QUEUED: bool> {
    nxt: SeqNum,
    pub(crate) max: SeqNum,
    una: SeqNum,
    wnd: WindowSize,
    wnd_scale: WindowScale,
    wnd_max: WindowSize,
    wl1: SeqNum,
    wl2: SeqNum,
    // The last sequence number sent out and its timestamp when sent.
    last_seq_ts: Option<(SeqNum, I)>,
    rtt_estimator: Estimator,
    timer: Option<SendTimer<I>>,
    #[derivative(PartialEq = "ignore")]
    congestion_control: CongestionControl<I>,
    buffer: S,
}

impl<I> Send<I, (), false> {
    fn with_buffer<S>(self, buffer: S) -> Send<I, S, false> {
        let Self {
            nxt,
            max,
            una,
            wnd,
            wnd_scale,
            wnd_max,
            wl1,
            wl2,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            buffer: _,
        } = self;
        Send {
            nxt,
            max,
            una,
            wnd,
            wnd_scale,
            wnd_max,
            wl1,
            wl2,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            buffer,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct RetransTimer<I> {
    user_timeout_until: I,
    remaining_retries: Option<NonZeroU8>,
    at: I,
    rto: Duration,
}

impl<I: Instant> RetransTimer<I> {
    fn new(now: I, rto: Duration, user_timeout: Duration, max_retries: NonZeroU8) -> Self {
        let wakeup_after = rto.min(user_timeout);
        let at = now.panicking_add(wakeup_after);
        let user_timeout_until = now.saturating_add(user_timeout);
        Self { at, rto, user_timeout_until, remaining_retries: Some(max_retries) }
    }

    fn backoff(&mut self, now: I) {
        let Self { at, rto, user_timeout_until, remaining_retries } = self;
        *remaining_retries = remaining_retries.and_then(|r| NonZeroU8::new(r.get() - 1));
        *rto = rto.saturating_mul(2);
        let remaining = if now < *user_timeout_until {
            user_timeout_until.saturating_duration_since(now)
        } else {
            // `now` has already passed  `user_timeout_until`, just update the
            // timer to expire as soon as possible.
            Duration::ZERO
        };
        *at = now.panicking_add(core::cmp::min(*rto, remaining));
    }

    fn rearm(&mut self, now: I) {
        let Self { at, rto, user_timeout_until: _, remaining_retries: _ } = self;
        *at = now.panicking_add(*rto);
    }

    fn timed_out(&self, now: I) -> bool {
        let RetransTimer { user_timeout_until, remaining_retries, at, rto: _ } = self;
        (remaining_retries.is_none() && now >= *at) || now >= *user_timeout_until
    }
}

/// Possible timers for a sender.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum SendTimer<I> {
    /// A retransmission timer can only be installed when there is outstanding
    /// data.
    Retrans(RetransTimer<I>),
    /// A keep-alive timer can only be installed when the connection is idle,
    /// i.e., the connection must not have any outstanding data.
    KeepAlive(KeepAliveTimer<I>),
    /// A zero window probe timer is installed when the receiver advertises a
    /// zero window but we have data to send. RFC 9293 Section 3.8.6.1 suggests
    /// that:
    ///   The transmitting host SHOULD send the first zero-window probe when a
    ///   zero window has existed for the retransmission timeout period, and
    ///   SHOULD increase exponentially the interval between successive probes.
    /// So we choose a retransmission timer as its implementation.
    ZeroWindowProbe(RetransTimer<I>),
    /// A timer installed to override silly window avoidance, when the receiver
    /// reduces its buffer size to be below 1 MSS (should happen very rarely),
    /// it's possible for the connection to make no progress if there is no such
    /// timer. Per RFC 9293 Section 3.8.6.2.1:
    ///   To avoid a resulting deadlock, it is necessary to have a timeout to
    ///   force transmission of data, overriding the SWS avoidance algorithm.
    ///   In practice, this timeout should seldom occur.
    SWSProbe { at: I },
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum ReceiveTimer<I> {
    DelayedAck { at: I, received_bytes: NonZeroU32 },
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct KeepAliveTimer<I> {
    at: I,
    already_sent: u8,
}

impl<I: Instant> KeepAliveTimer<I> {
    fn idle(now: I, keep_alive: &KeepAlive) -> Self {
        let at = now.saturating_add(keep_alive.idle.into());
        Self { at, already_sent: 0 }
    }
}

impl<I: Instant> SendTimer<I> {
    fn expiry(&self) -> I {
        match self {
            SendTimer::Retrans(RetransTimer {
                at,
                rto: _,
                user_timeout_until: _,
                remaining_retries: _,
            })
            | SendTimer::KeepAlive(KeepAliveTimer { at, already_sent: _ })
            | SendTimer::ZeroWindowProbe(RetransTimer {
                at,
                rto: _,
                user_timeout_until: _,
                remaining_retries: _,
            }) => *at,
            SendTimer::SWSProbe { at } => *at,
        }
    }
}

impl<I: Instant> ReceiveTimer<I> {
    fn expiry(&self) -> I {
        match self {
            ReceiveTimer::DelayedAck { at, received_bytes: _ } => *at,
        }
    }
}

/// Pair of receive buffer and `Assembler` Both dropped when receive is shut down.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum RecvBufferState<R> {
    Open { buffer: R, assembler: Assembler },
    Closed { buffer_size: usize, nxt: SeqNum },
}

impl<R: ReceiveBuffer> RecvBufferState<R> {
    fn is_closed(&self) -> bool {
        matches!(self, Self::Closed { .. })
    }

    fn has_out_of_order(&self) -> bool {
        match self {
            Self::Open { assembler, .. } => assembler.has_out_of_order(),
            Self::Closed { .. } => false,
        }
    }

    fn close(&mut self) {
        let new_state = match self {
            Self::Open { buffer, assembler } => {
                Self::Closed { nxt: assembler.nxt(), buffer_size: buffer.limits().capacity }
            }
            Self::Closed { .. } => return,
        };
        *self = new_state;
    }
}

/// TCP control block variables that are responsible for receiving.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) struct Recv<I, R> {
    timer: Option<ReceiveTimer<I>>,
    mss: Mss,
    wnd_scale: WindowScale,
    last_window_update: (SeqNum, WindowSize),

    // Buffer may be closed once receive is shutdown (e.g. with `shutdown(SHUT_RD)`).
    buffer: RecvBufferState<R>,
}

impl<I> Recv<I, ()> {
    fn with_buffer<R>(self, buffer: R) -> Recv<I, R> {
        let Self { timer, mss, wnd_scale, last_window_update, buffer: old_buffer } = self;
        let nxt = match old_buffer {
            RecvBufferState::Open { assembler, .. } => assembler.nxt(),
            RecvBufferState::Closed { .. } => unreachable!(),
        };
        Recv {
            timer,
            mss,
            wnd_scale,
            last_window_update,
            buffer: RecvBufferState::Open { buffer, assembler: Assembler::new(nxt) },
        }
    }
}

impl<I: Instant, R: ReceiveBuffer> Recv<I, R> {
    /// Returns the next window size and updates [`Recv::last_window_update`].
    ///
    /// NOTE: Since it updates [`Recv::last_window_update`], this method should
    /// only be called when a segment is definitely going to be sent. If not,
    /// prefer to call [`Recv::calculate_window_size`] instead.
    fn select_window(&mut self) -> WindowSize {
        let (rcv_nxt, last_wnd) = self.calculate_window_size();
        self.last_window_update = (rcv_nxt, last_wnd);

        last_wnd
    }

    /// Calculates the next window size to advertise to the peer.
    ///
    /// Returns the next window size and the sequence number of the next octet
    /// that we expect to receive from the peer.
    fn calculate_window_size(&self) -> (SeqNum, WindowSize) {
        let rcv_nxt = self.nxt();
        let Self { buffer, timer: _, mss, wnd_scale: _, last_window_update: (rcv_wup, last_wnd) } =
            self;

        // Per RFC 9293 Section 3.8.6.2.2:
        //   The suggested SWS avoidance algorithm for the receiver is to keep
        //   RCV.NXT+RCV.WND fixed until the reduction satisfies:
        //     RCV.BUFF - RCV.USER - RCV.WND  >=
        //            min( Fr * RCV.BUFF, Eff.snd.MSS )
        //   where Fr is a fraction whose recommended value is 1/2, and
        //   Eff.snd.MSS is the effective send MSS for the connection.
        //   When the inequality is satisfied, RCV.WND is set to RCV.BUFF-RCV.USER.

        // `len` and `capacity` are RCV.USER and RCV.BUFF respectively.
        // Note that the window is still kept open even after receiver was shut down.
        let BufferLimits { capacity, len } = match buffer {
            RecvBufferState::Open { buffer, .. } => buffer.limits(),
            RecvBufferState::Closed { buffer_size, .. } => {
                BufferLimits { capacity: *buffer_size, len: 0 }
            }
        };

        // `unused_window` is RCV.WND as described above.
        let unused_window = WindowSize::from_u32(u32::try_from(*rcv_wup + *last_wnd - rcv_nxt).unwrap_or_else(|_: TryFromIntError| {
            error!(
                "we received more bytes than we advertised, rcv_nxt: {:?}, rcv_wup: {:?}, last_wnd: {:?}",
                rcv_nxt, rcv_wup, last_wnd
            );
            0
        })).unwrap_or(WindowSize::MAX);
        // Note: between the last window update and now, it's possible that we
        // have reduced our receive buffer's capacity, so we need to use
        // saturating arithmetic below.
        let reduction = capacity.saturating_sub(len.saturating_add(usize::from(unused_window)));
        let last_wnd = if reduction >= usize::min(capacity / 2, usize::from(mss.get().get())) {
            // We have enough reduction in the buffer space, advertise more.
            WindowSize::new(capacity - len).unwrap_or(WindowSize::MAX)
        } else {
            // Keep the right edge fixed by only advertise whatever is unused in
            // the last advertisement.
            unused_window
        };

        (rcv_nxt, last_wnd)
    }

    /// Processes data being removed from the receive buffer. Returns a window
    /// update segment to be sent immediately if necessary.
    fn poll_receive_data_dequeued(&mut self, snd_max: SeqNum) -> Option<Segment<()>> {
        let (rcv_nxt, next_window_size): (_, WindowSize) = self.calculate_window_size();
        let (_, last_window_size): (_, WindowSize) = self.last_window_update;

        // Make sure next_window_size is a multiple of wnd_scale, since that's what the peer will
        // receive. Otherwise, our later MSS comparison might pass, but we'll advertise a window
        // that's less than MSS, which we don't want.
        let next_window_size = (next_window_size >> self.wnd_scale) << self.wnd_scale;

        // NOTE: We lose type information here, but we already know these are
        // both WindowSize from the type annotations above.
        let next_window_size_u32: u32 = next_window_size.into();
        let last_window_size_u32: u32 = last_window_size.into();

        // NOTE: The MSS always is a number of octets, which is the same for
        // WindowSize (though not UnscaledWindowSize), so any comparisons are
        // safe.
        let mss: u32 = self.mss.into();

        // If the previously-advertised window was less than advertised MSS, we
        // assume the sender is in SWS avoidance.  If we now have enough space
        // to accept at least one MSS worth of data, tell the caller to
        // immediately send a window update to get the sender back into normal
        // operation.
        if last_window_size_u32 < mss && next_window_size_u32 >= mss {
            self.last_window_update = (rcv_nxt, next_window_size);
            Some(Segment::ack(snd_max, self.nxt(), next_window_size >> self.wnd_scale))
        } else {
            None
        }
    }

    pub(crate) fn nxt(&self) -> SeqNum {
        match &self.buffer {
            RecvBufferState::Open { assembler, .. } => assembler.nxt(),
            RecvBufferState::Closed { nxt, .. } => *nxt,
        }
    }

    fn poll_send(&mut self, snd_max: SeqNum, now: I) -> Option<Segment<()>> {
        match self.timer {
            Some(ReceiveTimer::DelayedAck { at, received_bytes: _ }) => (at <= now).then(|| {
                self.timer = None;
                Segment::ack(snd_max, self.nxt(), self.select_window() >> self.wnd_scale)
            }),
            None => None,
        }
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-22):
///
///   ESTABLISHED - represents an open connection, data received can be
///   delivered to the user.  The normal state for the data transfer phase
///   of the connection.
///
/// Allowed operations:
///   - send
///   - recv
///   - shutdown
/// Disallowed operations:
///   - listen
///   - accept
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Established<I, R, S> {
    pub(crate) snd: Takeable<Send<I, S, { FinQueued::NO }>>,
    pub(crate) rcv: Takeable<Recv<I, R>>,
}

/// Indicates whether at least one byte of data was acknowledged by the remote
/// in an incoming segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DataAcked {
    Yes,
    No,
}

impl<I: Instant, S: SendBuffer, const FIN_QUEUED: bool> Send<I, S, FIN_QUEUED> {
    /// Returns true if the connection should still be alive per the send state.
    fn timed_out(&self, now: I, keep_alive: &KeepAlive) -> bool {
        match self.timer {
            Some(SendTimer::KeepAlive(keep_alive_timer)) => {
                keep_alive.enabled && keep_alive_timer.already_sent >= keep_alive.count.get()
            }
            Some(SendTimer::Retrans(timer)) | Some(SendTimer::ZeroWindowProbe(timer)) => {
                timer.timed_out(now)
            }
            Some(SendTimer::SWSProbe { at: _ }) | None => false,
        }
    }

    /// Polls for new segments with enabled options.
    ///
    /// `limit` is the maximum bytes wanted in the TCP segment (if any). The
    /// returned segment will have payload size up to the smaller of the given
    /// limit or the calculated MSS for the connection.
    fn poll_send(
        &mut self,
        counters: &TcpCountersInner,
        rcv_nxt: SeqNum,
        rcv_wnd: UnscaledWindowSize,
        limit: u32,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled,
            user_timeout,
            delayed_ack: _,
            fin_wait2_timeout: _,
            max_syn_retries: _,
            ip_options: _,
        }: &SocketOptions,
    ) -> Option<Segment<S::Payload<'_>>> {
        let Self {
            nxt: snd_nxt,
            max: snd_max,
            una: snd_una,
            wnd: snd_wnd,
            buffer,
            wl1: _,
            wl2: _,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            wnd_scale: _,
            wnd_max: snd_wnd_max,
        } = self;
        let BufferLimits { capacity: _, len: readable_bytes } = buffer.limits();
        let mss = u32::from(congestion_control.mss());
        let mut zero_window_probe = false;
        let mut override_sws = false;

        let increment_retransmit_counters = |congestion_control: &CongestionControl<I>| {
            counters.retransmits.increment();
            if congestion_control.in_fast_recovery() {
                counters.fast_retransmits.increment();
            }
            if congestion_control.in_slow_start() {
                counters.slow_start_retransmits.increment();
            }
        };

        match timer {
            Some(SendTimer::Retrans(retrans_timer)) => {
                if retrans_timer.at <= now {
                    // Per https://tools.ietf.org/html/rfc6298#section-5:
                    //   (5.4) Retransmit the earliest segment that has not
                    //         been acknowledged by the TCP receiver.
                    //   (5.5) The host MUST set RTO <- RTO * 2 ("back off
                    //         the timer").  The maximum value discussed in
                    //         (2.5) above may be used to provide an upper
                    //         bound to this doubling operation.
                    //   (5.6) Start the retransmission timer, such that it
                    //         expires after RTO seconds (for the value of
                    //         RTO after the doubling operation outlined in
                    //         5.5).
                    *snd_nxt = *snd_una;
                    retrans_timer.backoff(now);
                    congestion_control.on_retransmission_timeout();
                    counters.timeouts.increment();
                    increment_retransmit_counters(congestion_control);
                }
            }
            Some(SendTimer::ZeroWindowProbe(retrans_timer)) => {
                debug_assert!(readable_bytes > 0 || FIN_QUEUED);
                if retrans_timer.at <= now {
                    zero_window_probe = true;
                    *snd_nxt = *snd_una;
                    // Per RFC 9293 Section 3.8.6.1:
                    //   [...] SHOULD increase exponentially the interval
                    //   between successive probes.
                    retrans_timer.backoff(now);
                }
            }
            Some(SendTimer::KeepAlive(KeepAliveTimer { at, already_sent })) => {
                // Per RFC 9293 Section 3.8.4:
                //   Keep-alive packets MUST only be sent when no sent data is
                //   outstanding, and no data or acknowledgment packets have
                //   been received for the connection within an interval.
                if keep_alive.enabled && !FIN_QUEUED && readable_bytes == 0 {
                    if *at <= now {
                        *at = now.saturating_add(keep_alive.interval.into());
                        *already_sent = already_sent.saturating_add(1);
                        // Per RFC 9293 Section 3.8.4:
                        //   Such a segment generally contains SEG.SEQ = SND.NXT-1
                        return Some(Segment::ack(*snd_max - 1, rcv_nxt, rcv_wnd));
                    }
                } else {
                    *timer = None;
                }
            }
            Some(SendTimer::SWSProbe { at }) => {
                if *at <= now {
                    override_sws = true;
                    *timer = None;
                }
            }
            None => {}
        };

        // If there's an empty advertised window but we want to send data, we
        // need to start Zero Window Probing, overwriting any previous timer.
        if *snd_wnd == WindowSize::ZERO && readable_bytes > 0 {
            match timer {
                Some(SendTimer::ZeroWindowProbe(_)) => {}
                _ => {
                    let user_timeout = user_timeout.get_or_default(DEFAULT_USER_TIMEOUT);
                    *timer = Some(SendTimer::ZeroWindowProbe(RetransTimer::new(
                        now,
                        rtt_estimator.rto(),
                        user_timeout,
                        DEFAULT_MAX_RETRIES,
                    )));

                    // RFC 9293 3.8.6.1:
                    //   The transmitting host SHOULD send the first zero-window
                    //   probe when a zero window has existed for the
                    //   retransmission timeout period (SHLD-29)
                    //
                    // We'll only end up here if the peer sent a zero window
                    // advertisement. In that case, we have not yet waited, and
                    // so should immedaitely return.
                    return None;
                }
            }
        }

        // Find the sequence number for the next segment, we start with snd_nxt
        // unless a fast retransmit is needed.
        let next_seg = match congestion_control.fast_retransmit() {
            None => *snd_nxt,
            Some(seg) => {
                increment_retransmit_counters(congestion_control);
                seg
            }
        };
        // First calculate the open window, note that if our peer has shrank
        // their window (it is strongly discouraged), the following conversion
        // will fail and we return early.
        let cwnd = congestion_control.cwnd();
        let swnd = WindowSize::min(*snd_wnd, cwnd);
        let open_window =
            u32::try_from(*snd_una + swnd - next_seg).ok_checked::<TryFromIntError>()?;
        let offset =
            usize::try_from(next_seg - *snd_una).unwrap_or_else(|TryFromIntError { .. }| {
                panic!("next_seg({:?}) should never fall behind snd.una({:?})", *snd_nxt, *snd_una);
            });
        let available = u32::try_from(readable_bytes + usize::from(FIN_QUEUED) - offset)
            .unwrap_or_else(|_| WindowSize::MAX.into());
        // We can only send the minimum of the open window and the bytes that
        // are available, additionally, if in zero window probe mode, allow at
        // least one byte past the limit to be sent.
        let can_send =
            open_window.min(available).min(mss).min(limit).max(u32::from(zero_window_probe));
        if can_send == 0 {
            if available == 0 && offset == 0 && timer.is_none() && keep_alive.enabled {
                *timer = Some(SendTimer::KeepAlive(KeepAliveTimer::idle(now, keep_alive)));
            }
            return None;
        }
        let has_fin = FIN_QUEUED && can_send == available;
        let seg = buffer.peek_with(offset, |readable| {
            let bytes_to_send = u32::min(
                can_send - u32::from(has_fin),
                u32::try_from(readable.len()).unwrap_or(u32::MAX),
            );
            let has_fin = has_fin && bytes_to_send == can_send - u32::from(has_fin);
            // If we have more bytes to send than a MSS (enough to send) or the
            // segment is a FIN (no need to wait for more), don't hold off.
            if bytes_to_send < mss && !has_fin {
                if bytes_to_send == 0 {
                    return None;
                }
                // First check if disallowed by nagle.
                // Per RFC 9293 Section 3.7.4:
                //   If there is unacknowledged data (i.e., SND.NXT > SND.UNA),
                //   then the sending TCP endpoint buffers all user data
                //   (regardless of the PSH bit) until the outstanding data has
                //   been acknowledged or until the TCP endpoint can send a
                //   full-sized segment (Eff.snd.MSS bytes).
                if *nagle_enabled && snd_nxt.after(*snd_una) {
                    return None;
                }
                // Otherwise check if disallowed by SWS avoidance.
                // Per RFC 9293 Section 3.8.6.2.1:
                //   Send data:
                //   (1) if a maximum-sized segment can be sent, i.e., if:
                //       min(D,U) >= Eff.snd.MSS;
                //   (2) or if the data is pushed and all queued data can be
                //       sent now, i.e., if:
                //       [SND.NXT = SND.UNA and] PUSHed and D <= U
                //       (the bracketed condition is imposed by the Nagle algorithm);
                //   (3) or if at least a fraction Fs of the maximum window can
                //       be sent, i.e., if:
                //       [SND.NXT = SND.UNA and] min(D,U) >= Fs * Max(SND.WND);
                //   (4) or if the override timeout occurs.
                //   ... Here Fs is a fraction whose recommended value is 1/2
                // Explanation:
                // To simplify the conditions, we can ignore the brackets since
                // those are controlled by the nagle algorithm and is handled by
                // the block above. Also we consider all data as PUSHed so for
                // example (2) is now simply `D <= U`. Mapping into the code
                // context, `D` is `available` and `U` is `open_window`.
                //
                // The RFC says when to send data, negating it, we will get the
                // condition for when to hold off sending segments, that is:
                // - negate (2) we get D > U,
                // - negate (1) and combine with D > U, we get U < Eff.snd.MSS,
                // - negate (3) and combine with D > U, we get U < Fs * Max(SND.WND).
                // If the overriding timer fired or we are in zero window
                // probing phase, we override it to send data anyways.
                if available > open_window
                    && open_window < u32::min(mss, u32::from(*snd_wnd_max) / SWS_BUFFER_FACTOR)
                    && !override_sws
                    && !zero_window_probe
                {
                    if timer.is_none() {
                        *timer =
                            Some(SendTimer::SWSProbe { at: now.panicking_add(SWS_PROBE_TIMEOUT) })
                    }
                    return None;
                }
            }
            let (seg, discarded) = Segment::with_data(
                next_seg,
                Some(rcv_nxt),
                has_fin.then_some(Control::FIN),
                rcv_wnd,
                readable.slice(0..bytes_to_send),
            );
            debug_assert_eq!(discarded, 0);
            Some(seg)
        })?;
        let seq_max = next_seg + seg.len();
        match *last_seq_ts {
            Some((seq, _ts)) => {
                if seq_max.after(seq) {
                    *last_seq_ts = Some((seq_max, now));
                } else {
                    // If the recorded sequence number is ahead of us, we are
                    // in retransmission, we should discard the timestamp and
                    // abort the estimation.
                    *last_seq_ts = None;
                }
            }
            None => *last_seq_ts = Some((seq_max, now)),
        }
        if seq_max.after(*snd_nxt) {
            *snd_nxt = seq_max;
        }
        if seq_max.after(*snd_max) {
            *snd_max = seq_max;
        }
        // Per https://tools.ietf.org/html/rfc6298#section-5:
        //   (5.1) Every time a packet containing data is sent (including a
        //         retransmission), if the timer is not running, start it
        //         running so that it will expire after RTO seconds (for the
        //         current value of RTO).
        match timer {
            Some(SendTimer::Retrans(_)) | Some(SendTimer::ZeroWindowProbe(_)) => {}
            Some(SendTimer::KeepAlive(_)) | Some(SendTimer::SWSProbe { at: _ }) | None => {
                let user_timeout = user_timeout.get_or_default(DEFAULT_USER_TIMEOUT);
                *timer = Some(SendTimer::Retrans(RetransTimer::new(
                    now,
                    rtt_estimator.rto(),
                    user_timeout,
                    DEFAULT_MAX_RETRIES,
                )))
            }
        }
        Some(seg)
    }

    /// Processes an incoming ACK and returns a segment if one needs to be sent,
    /// along with whether at least one byte of data was ACKed.
    fn process_ack(
        &mut self,
        counters: &TcpCountersInner,
        seg_seq: SeqNum,
        seg_ack: SeqNum,
        seg_wnd: UnscaledWindowSize,
        pure_ack: bool,
        rcv_nxt: SeqNum,
        rcv_wnd: WindowSize,
        now: I,
        keep_alive: &KeepAlive,
    ) -> (Option<Segment<()>>, DataAcked) {
        let Self {
            nxt: snd_nxt,
            max: snd_max,
            una: snd_una,
            wnd: snd_wnd,
            wl1: snd_wl1,
            wl2: snd_wl2,
            wnd_max,
            buffer,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            wnd_scale,
        } = self;
        let seg_wnd = seg_wnd << *wnd_scale;
        match timer {
            Some(SendTimer::KeepAlive(_)) | None => {
                if keep_alive.enabled {
                    *timer = Some(SendTimer::KeepAlive(KeepAliveTimer::idle(now, keep_alive)));
                }
            }
            Some(SendTimer::Retrans(retrans_timer)) => {
                // Per https://tools.ietf.org/html/rfc6298#section-5:
                //   (5.2) When all outstanding data has been acknowledged,
                //         turn off the retransmission timer.
                //   (5.3) When an ACK is received that acknowledges new
                //         data, restart the retransmission timer so that
                //         it will expire after RTO seconds (for the current
                //         value of RTO).
                if seg_ack == *snd_max {
                    *timer = None;
                } else if seg_ack.before(*snd_max) && seg_ack.after(*snd_una) {
                    retrans_timer.rearm(now);
                }
            }
            Some(SendTimer::ZeroWindowProbe(_)) | Some(SendTimer::SWSProbe { at: _ }) => {}
        }
        // Note: we rewind SND.NXT to SND.UNA on retransmission; if
        // `seg_ack` is after `snd.max`, it means the segment acks
        // something we never sent.
        if seg_ack.after(*snd_max) {
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   If the ACK acks something not yet sent (SEG.ACK >
            //   SND.NXT) then send an ACK, drop the segment, and
            //   return.
            (Some(Segment::ack(*snd_max, rcv_nxt, rcv_wnd >> *wnd_scale)), DataAcked::No)
        } else {
            let res = if seg_ack.after(*snd_una) {
                // The unwrap is safe because the result must be positive.
                let acked = u32::try_from(seg_ack - *snd_una)
                    .ok_checked::<TryFromIntError>()
                    .and_then(NonZeroU32::new)
                    .unwrap_or_else(|| {
                        panic!("seg_ack({:?}) - snd_una({:?}) must be positive", seg_ack, snd_una);
                    });
                let BufferLimits { len, capacity: _ } = buffer.limits();
                let fin_acked = FIN_QUEUED && seg_ack == *snd_una + len + 1;
                // Remove the acked bytes from the send buffer. The following
                // operation should not panic because we are in this branch
                // means seg_ack is before snd.max, thus seg_ack - snd.una
                // cannot exceed the buffer length.
                buffer.mark_read(
                    NonZeroUsize::try_from(acked)
                        .unwrap_or_else(|TryFromIntError { .. }| {
                            // we've checked that acked must be smaller than the outstanding
                            // bytes we have in the buffer; plus in Rust, any allocation can
                            // only have a size up to isize::MAX bytes.
                            panic!(
                                "acked({:?}) must be smaller than isize::MAX({:?})",
                                acked,
                                isize::MAX
                            )
                        })
                        .get()
                        - usize::from(fin_acked),
                );
                *snd_una = seg_ack;
                // If the incoming segment acks something that has been sent
                // but not yet retransmitted (`snd.nxt < seg_ack <= snd.max`),
                // bump `snd.nxt` as well.
                if seg_ack.after(*snd_nxt) {
                    *snd_nxt = seg_ack;
                }
                // If the incoming segment acks the sequence number that we used
                // for RTT estimate, feed the sample to the estimator.
                if let Some((seq_max, timestamp)) = *last_seq_ts {
                    if !seg_ack.before(seq_max) {
                        rtt_estimator.sample(now.saturating_duration_since(timestamp));
                    }
                }
                congestion_control.on_ack(acked, now, rtt_estimator.rto());
                // At least one byte of data was ACKed by the peer.
                (None, DataAcked::Yes)
            } else {
                // Per RFC 5681 (https://www.rfc-editor.org/rfc/rfc5681#section-2):
                //   DUPLICATE ACKNOWLEDGMENT: An acknowledgment is considered a
                //   "duplicate" in the following algorithms when (a) the receiver of
                //   the ACK has outstanding data, (b) the incoming acknowledgment
                //   carries no data, (c) the SYN and FIN bits are both off, (d) the
                //   acknowledgment number is equal to the greatest acknowledgment
                //   received on the given connection (TCP.UNA from [RFC793]) and (e)
                //   the advertised window in the incoming acknowledgment equals the
                //   advertised window in the last incoming acknowledgment.
                let is_dup_ack = {
                    snd_nxt.after(*snd_una) // (a)
                && pure_ack // (b) & (c)
                && seg_ack == *snd_una // (d)
                && seg_wnd == *snd_wnd // (e)
                };
                if is_dup_ack {
                    let fast_recovery_initiated = congestion_control.on_dup_ack(seg_ack);
                    if fast_recovery_initiated {
                        counters.fast_recovery.increment();
                    }
                }
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
                //   If the ACK is a duplicate (SEG.ACK < SND.UNA), it can be
                //   ignored.
                (None, DataAcked::No)
            };

            // Per RFC 9293
            // (https://datatracker.ietf.org/doc/html/rfc9293#section-3.10.7.4-2.5.2.2.2.3.2.2):
            //   If SND.UNA =< SEG.ACK =< SND.NXT, the send window should be
            //   updated.  If (SND.WL1 < SEG.SEQ or (SND.WL1 = SEG.SEQ and
            //   SND.WL2 =< SEG.ACK)), set SND.WND <- SEG.WND, set
            //   SND.WL1 <- SEG.SEQ, and set SND.WL2 <- SEG.ACK.
            if !snd_una.after(seg_ack)
                && (snd_wl1.before(seg_seq) || (seg_seq == *snd_wl1 && !snd_wl2.after(seg_ack)))
            {
                *snd_wnd = seg_wnd;
                *snd_wl1 = seg_seq;
                *snd_wl2 = seg_ack;
                *wnd_max = seg_wnd.max(*wnd_max);
                if seg_wnd != WindowSize::ZERO
                    && matches!(timer, Some(SendTimer::ZeroWindowProbe(_)))
                {
                    *timer = None;
                    // We need to ensure that we're reset when exiting ZWP as if
                    // we're going to retransmit. The actual retransmit handling
                    // will be performed by poll_send.
                    *snd_nxt = *snd_una;
                }
            }

            res
        }
    }
}

impl<I: Instant, S: SendBuffer> Send<I, S, { FinQueued::NO }> {
    fn queue_fin(self) -> Send<I, S, { FinQueued::YES }> {
        let Self {
            nxt,
            max,
            una,
            wnd,
            wl1,
            wl2,
            buffer,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            wnd_scale,
            wnd_max,
        } = self;
        Send {
            nxt,
            max,
            una,
            wnd,
            wl1,
            wl2,
            buffer,
            last_seq_ts,
            rtt_estimator,
            timer,
            congestion_control,
            wnd_scale,
            wnd_max,
        }
    }
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
///   CLOSE-WAIT - represents waiting for a connection termination request
///   from the local user.
///
/// Allowed operations:
///   - send
///   - recv (only leftovers and no new data will be accepted from the peer)
///   - shutdown
/// Disallowed operations:
///   - listen
///   - accept
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct CloseWait<I, S> {
    snd: Takeable<Send<I, S, { FinQueued::NO }>>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
    last_wnd_scale: WindowScale,
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
/// LAST-ACK - represents waiting for an acknowledgment of the
/// connection termination request previously sent to the remote TCP
/// (which includes an acknowledgment of its connection termination
/// request).
///
/// Allowed operations:
///   - recv (only leftovers and no new data will be accepted from the peer)
/// Disallowed operations:
///   - send
///   - shutdown
///   - accept
///   - listen
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct LastAck<I, S> {
    snd: Send<I, S, { FinQueued::YES }>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
    last_wnd_scale: WindowScale,
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
/// FIN-WAIT-1 - represents waiting for a connection termination request
/// from the remote TCP, or an acknowledgment of the connection
/// termination request previously sent.
///
/// Allowed operations:
///   - recv
/// Disallowed operations:
///   - send
///   - shutdown
///   - accept
///   - listen
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct FinWait1<I, R, S> {
    snd: Takeable<Send<I, S, { FinQueued::YES }>>,
    rcv: Takeable<Recv<I, R>>,
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
/// FIN-WAIT-2 - represents waiting for a connection termination request
/// from the remote TCP.
///
/// Allowed operations:
///   - recv
/// Disallowed operations:
///   - send
///   - shutdown
///   - accept
///   - listen
///   - connect
pub struct FinWait2<I, R> {
    last_seq: SeqNum,
    rcv: Recv<I, R>,
    timeout_at: Option<I>,
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
///
/// CLOSING - represents waiting for a connection termination request
/// acknowledgment from the remote TCP
///
/// Allowed operations:
///   - recv
/// Disallowed operations:
///   - send
///   - shutdown
///   - accept
///   - listen
///   - connect
pub struct Closing<I, S> {
    snd: Send<I, S, { FinQueued::YES }>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
    last_wnd_scale: WindowScale,
}

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-22):
///
/// TIME-WAIT - represents waiting for enough time to pass to be sure
/// the remote TCP received the acknowledgment of its connection
/// termination request.
///
/// Allowed operations:
///   - recv (only leftovers and no new data will be accepted from the peer)
/// Disallowed operations:
///   - send
///   - shutdown
///   - accept
///   - listen
///   - connect
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct TimeWait<I> {
    pub(super) last_seq: SeqNum,
    pub(super) last_ack: SeqNum,
    pub(super) last_wnd: WindowSize,
    pub(super) last_wnd_scale: WindowScale,
    pub(super) expiry: I,
}

fn new_time_wait_expiry<I: Instant>(now: I) -> I {
    now.panicking_add(MSL * 2)
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum State<I, R, S, ActiveOpen> {
    Closed(Closed<Option<ConnectionError>>),
    Listen(Listen),
    SynRcvd(SynRcvd<I, ActiveOpen>),
    SynSent(SynSent<I, ActiveOpen>),
    Established(Established<I, R, S>),
    CloseWait(CloseWait<I, S>),
    LastAck(LastAck<I, S>),
    FinWait1(FinWait1<I, R, S>),
    FinWait2(FinWait2<I, R>),
    Closing(Closing<I, S>),
    TimeWait(TimeWait<I>),
}

impl<I, R, S, ActiveOpen> core::fmt::Display for State<I, R, S, ActiveOpen> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        let name = match self {
            State::Closed(_) => "Closed",
            State::Listen(_) => "Listen",
            State::SynRcvd(_) => "SynRcvd",
            State::SynSent(_) => "SynSent",
            State::Established(_) => "Established",
            State::CloseWait(_) => "CloseWait",
            State::LastAck(_) => "LastAck",
            State::FinWait1(_) => "FinWait1",
            State::FinWait2(_) => "FinWait2",
            State::Closing(_) => "Closing",
            State::TimeWait(_) => "TimeWait",
        };
        write!(f, "{name}")
    }
}

#[derive(Debug, PartialEq, Eq)]
/// Possible errors for closing a connection
pub(super) enum CloseError {
    /// The connection is already being closed.
    Closing,
    /// There is no connection to be closed.
    NoConnection,
}

/// A provider that creates receive and send buffers when the connection
/// becomes established.
pub(crate) trait BufferProvider<R: ReceiveBuffer, S: SendBuffer> {
    /// An object that is returned when a passive open connection is
    /// established.
    type PassiveOpen;

    /// An object that is needed to initiate a connection which will be
    /// later used to create buffers once the connection is established.
    type ActiveOpen: IntoBuffers<R, S>;

    /// Creates new send and receive buffers and an object that needs to be
    /// vended to the application.
    fn new_passive_open_buffers(buffer_sizes: BufferSizes) -> (R, S, Self::PassiveOpen);
}

/// Provides a helper for data that needs to be moved out of a reference.
///
/// `Takeable` helps implement the TCP protocol and socket state machines by
/// providing a wrapper around a type that can be moved out of its placement.
///
/// Once moved, `Takeable` will always panic when the value is accessed.
///
/// Moving a `Takeable` always happens through a [`TakeableRef`] which can be
/// transformed either into the inner value or a new `Takeable`. This is useful
/// because [`TakeableRef::take`] moves out of `self``, which makes it a bit
/// more difficult to accidentally call `take`` twice on the same `Takeable`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Takeable<T>(Option<T>);

impl<T> From<T> for Takeable<T> {
    fn from(value: T) -> Self {
        Self(Some(value))
    }
}

impl<T> Takeable<T> {
    pub(crate) fn get(&self) -> &T {
        let Self(i) = self;
        i.as_ref().expect("accessed taken takeable")
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        let Self(i) = self;
        i.as_mut().expect("accessed taken takeable")
    }

    pub(crate) fn new(v: T) -> Self {
        Self(Some(v))
    }

    pub(crate) fn to_ref(&mut self) -> TakeableRef<'_, T> {
        TakeableRef(self)
    }

    pub(crate) fn from_ref(t: TakeableRef<'_, T>) -> Self {
        let TakeableRef(Self(t)) = t;
        Self(Some(t.take().expect("accessed taken takeable")))
    }

    pub(crate) fn into_inner(self) -> T {
        let Self(i) = self;
        i.expect("accessed taken takeable")
    }

    pub(crate) fn map<R, F: FnOnce(T) -> R>(self, f: F) -> Takeable<R> {
        Takeable(Some(f(self.into_inner())))
    }
}

impl<T> Deref for Takeable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T> DerefMut for Takeable<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_mut()
    }
}

/// A mutable reference to a [`Takeable`].
pub(crate) struct TakeableRef<'a, T>(&'a mut Takeable<T>);

impl<'a, T> TakeableRef<'a, T> {
    pub(crate) fn take(self) -> T {
        let Self(Takeable(t)) = self;
        t.take().expect("accessed taken takeable")
    }

    pub(crate) fn to_takeable(self) -> Takeable<T> {
        Takeable::new(self.take())
    }
}

#[must_use = "must check to determine if the socket needs to be removed from the demux state"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewlyClosed {
    No,
    Yes,
}

impl<I: Instant + 'static, R: ReceiveBuffer, S: SendBuffer, ActiveOpen: Debug>
    State<I, R, S, ActiveOpen>
{
    /// Updates this state to the provided new state.
    fn transition_to_state(
        &mut self,
        counters: &TcpCountersInner,
        new_state: State<I, R, S, ActiveOpen>,
    ) -> NewlyClosed {
        let newly_closed = if let State::Closed(Closed { reason }) = &new_state {
            let (was_established, was_closed) = match self {
                State::Closed(_) => (false, true),
                State::Listen(_) | State::SynRcvd(_) | State::SynSent(_) => (false, false),
                State::Established(_)
                | State::CloseWait(_)
                | State::LastAck(_)
                | State::FinWait1(_)
                | State::FinWait2(_)
                | State::Closing(_)
                | State::TimeWait(_) => (true, false),
            };
            if was_established {
                counters.established_closed.increment();
                match reason {
                    Some(ConnectionError::ConnectionRefused)
                    | Some(ConnectionError::ConnectionReset) => {
                        counters.established_resets.increment();
                    }
                    Some(ConnectionError::TimedOut) => {
                        counters.established_timedout.increment();
                    }
                    _ => {}
                }
            }
            (!was_closed).then_some(NewlyClosed::Yes).unwrap_or(NewlyClosed::No)
        } else {
            NewlyClosed::No
        };
        *self = new_state;
        newly_closed
    }
    /// Processes an incoming segment and advances the state machine.
    ///
    /// Returns a segment if one needs to be sent; if a passive open connection
    /// is newly established, the corresponding object that our client needs
    /// will be returned. Also returns whether an at least one byte of data was
    /// ACKed by the incoming segment.
    pub(crate) fn on_segment<P: Payload, BP: BufferProvider<R, S, ActiveOpen = ActiveOpen>>(
        &mut self,
        counters: &TcpCountersInner,
        incoming: Segment<P>,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled: _,
            user_timeout: _,
            delayed_ack,
            fin_wait2_timeout,
            max_syn_retries: _,
            ip_options: _,
        }: &SocketOptions,
        defunct: bool,
    ) -> (Option<Segment<()>>, Option<BP::PassiveOpen>, DataAcked, NewlyClosed)
    where
        BP::PassiveOpen: Debug,
        ActiveOpen: IntoBuffers<R, S>,
    {
        let mut passive_open = None;
        let mut data_acked = DataAcked::No;
        let (seg, newly_closed) = (|| {
            let (mut rcv_nxt, rcv_wnd, rcv_wnd_scale, snd_max, rst_on_new_data) = match self {
                State::Closed(closed) => return (closed.on_segment(&incoming), NewlyClosed::No),
                State::Listen(listen) => {
                    return (
                        match listen.on_segment(incoming, now) {
                            ListenOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                                syn_ack,
                                SynRcvd {
                                    iss,
                                    irs,
                                    timestamp,
                                    retrans_timer,
                                    simultaneous_open,
                                    buffer_sizes,
                                    smss,
                                    rcv_wnd_scale,
                                    snd_wnd_scale,
                                },
                            ) => {
                                match simultaneous_open {
                                    None => {
                                        assert_eq!(
                                            self.transition_to_state(
                                                counters,
                                                State::SynRcvd(SynRcvd {
                                                    iss,
                                                    irs,
                                                    timestamp,
                                                    retrans_timer,
                                                    simultaneous_open: None,
                                                    buffer_sizes,
                                                    smss,
                                                    rcv_wnd_scale,
                                                    snd_wnd_scale,
                                                }),
                                            ),
                                            NewlyClosed::No
                                        )
                                    }
                                }
                                Some(syn_ack)
                            }
                            ListenOnSegmentDisposition::SendRst(rst) => Some(rst),
                            ListenOnSegmentDisposition::Ignore => None,
                        },
                        NewlyClosed::No,
                    );
                }
                State::SynSent(synsent) => {
                    return match synsent.on_segment(incoming, now) {
                        SynSentOnSegmentDisposition::SendAckAndEnterEstablished(established) => (
                            replace_with_and(self, |this| {
                                assert_matches!(this, State::SynSent(SynSent {
                                    active_open,
                                    buffer_sizes,
                                    rcv_wnd_scale,
                                    ..
                                }) => {
                                    let Established {snd, rcv} = established;
                                    let (rcv_buffer, snd_buffer) =
                                        active_open.into_buffers(buffer_sizes);
                                    let mut established = Established {
                                        snd: snd.map(|s| s.with_buffer(snd_buffer)),
                                        rcv: rcv.map(|s| s.with_buffer(rcv_buffer)),
                                    };
                                    let ack = Some(Segment::ack(
                                        established.snd.max,
                                        established.rcv.nxt(),
                                        established.rcv.select_window() >> rcv_wnd_scale,
                                    ));
                                    (State::Established(established), ack)
                                })
                            }),
                            NewlyClosed::No,
                        ),
                        SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                            syn_ack,
                            mut syn_rcvd,
                        ) => {
                            replace_with(self, |this| {
                                assert_matches!(this, State::SynSent(SynSent {
                                    active_open,
                                    ..
                                }) => {
                                    assert_matches!(syn_rcvd.simultaneous_open.replace(active_open), None);
                                    State::SynRcvd(syn_rcvd)
                                })
                            });
                            (Some(syn_ack), NewlyClosed::No)
                        }
                        SynSentOnSegmentDisposition::SendRst(rst) => (Some(rst), NewlyClosed::No),
                        SynSentOnSegmentDisposition::EnterClosed(closed) => {
                            assert_eq!(
                                self.transition_to_state(counters, State::Closed(closed)),
                                NewlyClosed::Yes,
                            );
                            (None, NewlyClosed::Yes)
                        }
                        SynSentOnSegmentDisposition::Ignore => (None, NewlyClosed::No),
                    }
                }
                State::SynRcvd(SynRcvd {
                    iss,
                    irs,
                    timestamp: _,
                    retrans_timer: _,
                    simultaneous_open: _,
                    buffer_sizes,
                    smss: _,
                    rcv_wnd_scale: _,
                    snd_wnd_scale: _,
                }) => {
                    // RFC 7323 Section 2.2:
                    //  The window field in a segment where the SYN bit is set
                    //  (i.e., a <SYN> or <SYN,ACK>) MUST NOT be scaled.
                    let advertised = buffer_sizes.rwnd_unscaled();
                    (
                        *irs + 1,
                        advertised << WindowScale::default(),
                        WindowScale::default(),
                        *iss + 1,
                        false,
                    )
                }
                State::Established(Established { rcv, snd }) => {
                    (rcv.nxt(), rcv.select_window(), rcv.wnd_scale, snd.max, false)
                }
                State::CloseWait(CloseWait { snd, last_ack, last_wnd, last_wnd_scale }) => {
                    (*last_ack, *last_wnd, *last_wnd_scale, snd.max, true)
                }
                State::LastAck(LastAck { snd, last_ack, last_wnd, last_wnd_scale })
                | State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale }) => {
                    (*last_ack, *last_wnd, *last_wnd_scale, snd.max, true)
                }
                State::FinWait1(FinWait1 { rcv, snd }) => {
                    (rcv.nxt(), rcv.select_window(), rcv.wnd_scale, snd.max, rcv.buffer.is_closed())
                }
                State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => (
                    rcv.nxt(),
                    rcv.select_window(),
                    rcv.wnd_scale,
                    *last_seq,
                    rcv.buffer.is_closed(),
                ),
                State::TimeWait(TimeWait {
                    last_seq,
                    last_ack,
                    last_wnd,
                    expiry: _,
                    last_wnd_scale: _,
                }) => (*last_ack, *last_wnd, WindowScale::default(), *last_seq, true),
            };

            // Reset the connection if we receive new data while the socket is being closed
            // and the receiver has been shut down.
            if rst_on_new_data && (incoming.header.seq + incoming.data.len()).after(rcv_nxt) {
                return (
                    Some(Segment::rst(snd_max)),
                    self.transition_to_state(
                        counters,
                        State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }),
                    ),
                );
            }

            // Unreachable note(1): The above match returns early for states CLOSED,
            // SYN_SENT and LISTEN, so it is impossible to have the above states
            // past this line.
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-69):
            //   first check sequence number
            let is_rst = incoming.header.control == Some(Control::RST);
            // pure ACKs (empty segments) don't need to be ack'ed.
            let pure_ack = incoming.len() == 0;
            let needs_ack = !pure_ack;
            let Segment {
                header:
                    SegmentHeader { seq: seg_seq, ack: seg_ack, wnd: seg_wnd, control, options: _ },
                data,
            } = match incoming.overlap(rcv_nxt, rcv_wnd) {
                Some(incoming) => incoming,
                None => {
                    // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-69):
                    //   If an incoming segment is not acceptable, an acknowledgment
                    //   should be sent in reply (unless the RST bit is set, if so drop
                    //   the segment and return):
                    //     <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK>
                    //   After sending the acknowledgment, drop the unacceptable segment
                    //   and return.
                    return (
                        if is_rst {
                            None
                        } else {
                            Some(Segment::ack(snd_max, rcv_nxt, rcv_wnd >> rcv_wnd_scale))
                        },
                        NewlyClosed::No,
                    );
                }
            };
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-70):
            //   second check the RST bit
            //   If the RST bit is set then, any outstanding RECEIVEs and SEND
            //   should receive "reset" responses.  All segment queues should be
            //   flushed.  Users should also receive an unsolicited general
            //   "connection reset" signal.  Enter the CLOSED state, delete the
            //   TCB, and return.
            if control == Some(Control::RST) {
                return (
                    None,
                    self.transition_to_state(
                        counters,
                        State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }),
                    ),
                );
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-70):
            //   fourth, check the SYN bit
            //   If the SYN is in the window it is an error, send a reset, any
            //   outstanding RECEIVEs and SEND should receive "reset" responses,
            //   all segment queues should be flushed, the user should also
            //   receive an unsolicited general "connection reset" signal, enter
            //   the CLOSED state, delete the TCB, and return.
            //   If the SYN is not in the window this step would not be reached
            //   and an ack would have been sent in the first step (sequence
            //   number check).
            if control == Some(Control::SYN) {
                return (
                    Some(Segment::rst(snd_max)),
                    self.transition_to_state(
                        counters,
                        State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }),
                    ),
                );
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   fifth check the ACK field
            match seg_ack {
                Some(seg_ack) => match self {
                    State::Closed(_) | State::Listen(_) | State::SynSent(_) => {
                        // This unreachable assert is justified by note (1).
                        unreachable!("encountered an already-handled state: {:?}", self)
                    }
                    State::SynRcvd(SynRcvd {
                        iss,
                        irs,
                        timestamp: syn_rcvd_ts,
                        retrans_timer: _,
                        simultaneous_open,
                        buffer_sizes,
                        smss,
                        rcv_wnd_scale,
                        snd_wnd_scale,
                    }) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
                        //    if the ACK bit is on
                        //    SYN-RECEIVED STATE
                        //    If SND.UNA =< SEG.ACK =< SND.NXT then enter ESTABLISHED state
                        //    and continue processing.
                        //    If the segment acknowledgment is not acceptable, form a
                        //    reset segment,
                        //      <SEQ=SEG.ACK><CTL=RST>
                        //    and send it.
                        // Note: We don't support sending data with SYN, so we don't
                        // store the `SND` variables because they can be easily derived
                        // from ISS: SND.UNA=ISS and SND.NXT=ISS+1.
                        if seg_ack != *iss + 1 {
                            return (Some(Segment::rst(seg_ack)), NewlyClosed::No);
                        } else {
                            let mut rtt_estimator = Estimator::default();
                            if let Some(syn_rcvd_ts) = syn_rcvd_ts {
                                rtt_estimator.sample(now.saturating_duration_since(*syn_rcvd_ts));
                            }
                            let (rcv_buffer, snd_buffer) = match simultaneous_open.take() {
                                None => {
                                    let (rcv_buffer, snd_buffer, client) =
                                        BP::new_passive_open_buffers(*buffer_sizes);
                                    assert_matches!(passive_open.replace(client), None);
                                    (rcv_buffer, snd_buffer)
                                }
                                Some(active_open) => active_open.into_buffers(*buffer_sizes),
                            };
                            let (snd_wnd_scale, rcv_wnd_scale) = snd_wnd_scale
                                .map(|snd_wnd_scale| (snd_wnd_scale, *rcv_wnd_scale))
                                .unwrap_or_default();
                            let established = Established {
                                snd: Send {
                                    nxt: *iss + 1,
                                    max: *iss + 1,
                                    una: seg_ack,
                                    wnd: seg_wnd << snd_wnd_scale,
                                    wl1: seg_seq,
                                    wl2: seg_ack,
                                    buffer: snd_buffer,
                                    last_seq_ts: None,
                                    rtt_estimator,
                                    timer: None,
                                    congestion_control: CongestionControl::cubic_with_mss(*smss),
                                    wnd_scale: snd_wnd_scale,
                                    wnd_max: seg_wnd << snd_wnd_scale,
                                }
                                .into(),
                                rcv: Recv {
                                    buffer: RecvBufferState::Open {
                                        buffer: rcv_buffer,
                                        assembler: Assembler::new(*irs + 1),
                                    },
                                    timer: None,
                                    mss: *smss,
                                    wnd_scale: rcv_wnd_scale,
                                    last_window_update: (*irs + 1, buffer_sizes.rwnd()),
                                }
                                .into(),
                            };
                            assert_eq!(
                                self.transition_to_state(counters, State::Established(established)),
                                NewlyClosed::No
                            );
                        }
                        // Unreachable note(2): Because we either return early or
                        // transition to Established for the ack processing, it is
                        // impossible for SYN_RCVD to appear past this line.
                    }
                    State::Established(Established { snd, rcv: _ })
                    | State::CloseWait(CloseWait {
                        snd,
                        last_ack: _,
                        last_wnd: _,
                        last_wnd_scale: _,
                    }) => {
                        let (ack, segment_acked_data) = snd.process_ack(
                            counters, seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now,
                            keep_alive,
                        );
                        data_acked = segment_acked_data;
                        if let Some(ack) = ack {
                            return (Some(ack), NewlyClosed::No);
                        }
                    }
                    State::LastAck(LastAck {
                        snd,
                        last_ack: _,
                        last_wnd: _,
                        last_wnd_scale: _,
                    }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        let (ack, segment_acked_data) = snd.process_ack(
                            counters, seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now,
                            keep_alive,
                        );
                        data_acked = segment_acked_data;
                        if let Some(ack) = ack {
                            return (Some(ack), NewlyClosed::No);
                        } else if seg_ack == fin_seq {
                            return (
                                None,
                                self.transition_to_state(
                                    counters,
                                    State::Closed(Closed { reason: None }),
                                ),
                            );
                        }
                    }
                    State::FinWait1(FinWait1 { snd, rcv }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        let (ack, segment_acked_data) = snd.process_ack(
                            counters, seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now,
                            keep_alive,
                        );
                        data_acked = segment_acked_data;
                        if let Some(ack) = ack {
                            return (Some(ack), NewlyClosed::No);
                        } else if seg_ack == fin_seq {
                            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-73):
                            //   In addition to the processing for the ESTABLISHED
                            //   state, if the FIN segment is now acknowledged then
                            //   enter FIN-WAIT-2 and continue processing in that
                            //   state
                            let last_seq = snd.nxt;
                            let finwait2 = FinWait2 {
                                last_seq,
                                rcv: rcv.to_ref().take(),
                                // If the connection is already defunct, we set
                                // a timeout to reclaim, but otherwise, a later
                                // `close` call should set the timer.
                                timeout_at: fin_wait2_timeout.and_then(|timeout| {
                                    defunct.then_some(now.saturating_add(timeout))
                                }),
                            };
                            assert_eq!(
                                self.transition_to_state(counters, State::FinWait2(finwait2)),
                                NewlyClosed::No
                            );
                        }
                    }
                    State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        let (ack, segment_acked_data) = snd.process_ack(
                            counters, seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now,
                            keep_alive,
                        );
                        data_acked = segment_acked_data;
                        if let Some(ack) = ack {
                            data_acked = segment_acked_data;
                            return (Some(ack), NewlyClosed::No);
                        } else if seg_ack == fin_seq {
                            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-73):
                            //   In addition to the processing for the ESTABLISHED state, if
                            //   the ACK acknowledges our FIN then enter the TIME-WAIT state,
                            //   otherwise ignore the segment.
                            let timewait = TimeWait {
                                last_seq: snd.nxt,
                                last_ack: *last_ack,
                                last_wnd: *last_wnd,
                                expiry: new_time_wait_expiry(now),
                                last_wnd_scale: *last_wnd_scale,
                            };
                            assert_eq!(
                                self.transition_to_state(counters, State::TimeWait(timewait)),
                                NewlyClosed::No
                            );
                        }
                    }
                    State::FinWait2(_) | State::TimeWait(_) => {}
                },
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
                //   if the ACK bit is off drop the segment and return
                None => return (None, NewlyClosed::No),
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-74):
            //   seventh, process the segment text
            //   Once in the ESTABLISHED state, it is possible to deliver segment
            //   text to user RECEIVE buffers.  Text from segments can be moved
            //   into buffers until either the buffer is full or the segment is
            //   empty.  If the segment empties and carries an PUSH flag, then
            //   the user is informed, when the buffer is returned, that a PUSH
            //   has been received.
            //
            //   When the TCP takes responsibility for delivering the data to the
            //   user it must also acknowledge the receipt of the data.
            //   Once the TCP takes responsibility for the data it advances
            //   RCV.NXT over the data accepted, and adjusts RCV.WND as
            //   apporopriate to the current buffer availability.  The total of
            //   RCV.NXT and RCV.WND should not be reduced.
            //
            //   Please note the window management suggestions in section 3.7.
            //   Send an acknowledgment of the form:
            //     <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK>
            //   This acknowledgment should be piggybacked on a segment being
            //   transmitted if possible without incurring undue delay.
            let ack_to_text = if needs_ack {
                let mut maybe_ack_to_text = |rcv: &mut Recv<I, R>| {
                    // Write the segment data in the buffer and keep track if it fills
                    // any hole in the assembler.
                    let had_out_of_order = rcv.buffer.has_out_of_order();
                    if data.len() > 0 {
                        let offset = usize::try_from(seg_seq - rcv.nxt()).unwrap_or_else(|TryFromIntError {..}| {
                                panic!("The segment was trimmed to fit the window, thus seg.seq({:?}) must not come before rcv.nxt({:?})", seg_seq, rcv.nxt());
                            });
                        match &mut rcv.buffer {
                            RecvBufferState::Open { buffer, assembler } => {
                                let nwritten = buffer.write_at(offset, &data);
                                let readable = assembler.insert(seg_seq..seg_seq + nwritten);
                                buffer.make_readable(readable, assembler.has_outstanding());
                            }
                            RecvBufferState::Closed { nxt, .. } => *nxt = seg_seq + data.len(),
                        }
                        rcv_nxt = rcv.nxt();
                    }
                    // Per RFC 5681 Section 4.2:
                    //  Out-of-order data segments SHOULD be acknowledged
                    //  immediately, ...  the receiver SHOULD send an
                    //  immediate ACK when it receives a data segment that
                    //  fills in all or part of a gap in the sequence space.
                    let immediate_ack =
                        !*delayed_ack || had_out_of_order || rcv.buffer.has_out_of_order();
                    if immediate_ack {
                        rcv.timer = None;
                    } else {
                        match &mut rcv.timer {
                            Some(ReceiveTimer::DelayedAck { at: _, received_bytes }) => {
                                *received_bytes = received_bytes
                                    .saturating_add(u32::try_from(data.len()).unwrap_or(u32::MAX));
                                // Per RFC 5681 Section 4.2:
                                //  An implementation is deemed to comply
                                //  with this requirement if it sends at
                                //  least one acknowledgment every time it
                                //  receives 2*RMSS bytes of new data from
                                //  the sender
                                if received_bytes.get() >= 2 * u32::from(rcv.mss) {
                                    rcv.timer = None;
                                }
                            }
                            None => {
                                if let Some(received_bytes) =
                                    NonZeroU32::new(u32::try_from(data.len()).unwrap_or(u32::MAX))
                                {
                                    rcv.timer = Some(ReceiveTimer::DelayedAck {
                                        at: now.panicking_add(ACK_DELAY_THRESHOLD),
                                        received_bytes,
                                    })
                                }
                            }
                        }
                    }
                    (!matches!(rcv.timer, Some(ReceiveTimer::DelayedAck { .. }))).then_some(
                        Segment::ack(snd_max, rcv.nxt(), rcv.select_window() >> rcv.wnd_scale),
                    )
                };

                match self {
                    State::Closed(_) | State::Listen(_) | State::SynRcvd(_) | State::SynSent(_) => {
                        // This unreachable assert is justified by note (1) and (2).
                        unreachable!("encountered an already-handled state: {:?}", self)
                    }
                    State::Established(Established { snd: _, rcv })
                    | State::FinWait1(FinWait1 { snd: _, rcv }) => maybe_ack_to_text(rcv.get_mut()),
                    State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                        maybe_ack_to_text(rcv)
                    }
                    State::CloseWait(_)
                    | State::LastAck(_)
                    | State::Closing(_)
                    | State::TimeWait(_) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
                        //   This should not occur, since a FIN has been received from the
                        //   remote side.  Ignore the segment text.
                        None
                    }
                }
            } else {
                None
            };
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
            //   eighth, check the FIN bit
            let ack_to_fin = if control == Some(Control::FIN) && rcv_nxt == seg_seq + data.len() {
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
                //   If the FIN bit is set, signal the user "connection closing" and
                //   return any pending RECEIVEs with same message, advance RCV.NXT
                //   over the FIN, and send an acknowledgment for the FIN.
                match self {
                    State::Closed(_) | State::Listen(_) | State::SynRcvd(_) | State::SynSent(_) => {
                        // This unreachable assert is justified by note (1) and (2).
                        unreachable!("encountered an already-handled state: {:?}", self)
                    }
                    State::Established(Established { snd, rcv }) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
                        //   Enter the CLOSE-WAIT state.
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd =
                            rcv.select_window().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_wnd = last_wnd >> rcv.wnd_scale;
                        let closewait = CloseWait {
                            snd: snd.to_ref().to_takeable(),
                            last_ack,
                            last_wnd,
                            last_wnd_scale: rcv.wnd_scale,
                        };
                        assert_eq!(
                            self.transition_to_state(counters, State::CloseWait(closewait)),
                            NewlyClosed::No
                        );
                        Some(Segment::ack(snd_max, last_ack, scaled_wnd))
                    }
                    State::CloseWait(_) | State::LastAck(_) | State::Closing(_) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
                        //   CLOSE-WAIT STATE
                        //     Remain in the CLOSE-WAIT state.
                        //   CLOSING STATE
                        //     Remain in the CLOSING state.
                        //   LAST-ACK STATE
                        //     Remain in the LAST-ACK state.
                        None
                    }
                    State::FinWait1(FinWait1 { snd, rcv }) => {
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd =
                            rcv.select_window().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_wnd = last_wnd >> rcv.wnd_scale;
                        let closing = Closing {
                            snd: snd.to_ref().take(),
                            last_ack,
                            last_wnd,
                            last_wnd_scale: rcv.wnd_scale,
                        };
                        assert_eq!(
                            self.transition_to_state(counters, State::Closing(closing)),
                            NewlyClosed::No
                        );
                        Some(Segment::ack(snd_max, last_ack, scaled_wnd))
                    }
                    State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => {
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd =
                            rcv.select_window().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_window = last_wnd >> rcv.wnd_scale;
                        let timewait = TimeWait {
                            last_seq: *last_seq,
                            last_ack,
                            last_wnd,
                            expiry: new_time_wait_expiry(now),
                            last_wnd_scale: rcv.wnd_scale,
                        };
                        assert_eq!(
                            self.transition_to_state(counters, State::TimeWait(timewait)),
                            NewlyClosed::No,
                        );
                        Some(Segment::ack(snd_max, last_ack, scaled_window))
                    }
                    State::TimeWait(TimeWait {
                        last_seq,
                        last_ack,
                        last_wnd,
                        expiry,
                        last_wnd_scale,
                    }) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-76):
                        //   TIME-WAIT STATE
                        //     Remain in the TIME-WAIT state.  Restart the 2 MSL time-wait
                        //     timeout.
                        *expiry = new_time_wait_expiry(now);
                        Some(Segment::ack(*last_seq, *last_ack, *last_wnd >> *last_wnd_scale))
                    }
                }
            } else {
                None
            };
            // If we generated an ACK to FIN, then because of the cumulative nature
            // of ACKs, the ACK generated to text (if any) can be safely overridden.
            (ack_to_fin.or(ack_to_text), NewlyClosed::No)
        })();
        (seg, passive_open, data_acked, newly_closed)
    }

    /// To be called when bytes have been dequeued from the receive buffer.
    ///
    /// Returns a segment to send.
    pub(crate) fn poll_receive_data_dequeued(&mut self) -> Option<Segment<()>> {
        let (rcv, snd_max) = match self {
            State::Closed(_)
            | State::Listen(_)
            | State::SynRcvd(_)
            | State::SynSent(_)
            | State::CloseWait(_)
            | State::LastAck(_)
            | State::Closing(_)
            | State::TimeWait(_) => return None,
            State::Established(Established { snd, rcv }) => (rcv.get_mut(), snd.max),
            State::FinWait1(FinWait1 { snd, rcv }) => (rcv.get_mut(), snd.max),
            State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => (rcv, *last_seq),
        };

        rcv.poll_receive_data_dequeued(snd_max)
    }

    /// Polls if there are any bytes available to send in the buffer.
    ///
    /// Forms one segment of at most `limit` available bytes, as long as the
    /// receiver window allows.
    ///
    /// Returns `Ok` if a segment is available, otherwise whether the state has
    /// become closed will be returned in `Err`.
    pub(crate) fn poll_send(
        &mut self,
        counters: &TcpCountersInner,
        limit: u32,
        now: I,
        socket_options: &SocketOptions,
    ) -> Result<Segment<S::Payload<'_>>, NewlyClosed> {
        let newly_closed = self.poll_close(counters, now, socket_options);
        if matches!(self, State::Closed(_)) {
            return Err(newly_closed);
        }
        fn poll_rcv_then_snd<
            'a,
            I: Instant,
            R: ReceiveBuffer,
            S: SendBuffer,
            const FIN_QUEUED: bool,
        >(
            counters: &TcpCountersInner,
            snd: &'a mut Send<I, S, FIN_QUEUED>,
            rcv: &'a mut Recv<I, R>,
            limit: u32,
            now: I,
            socket_options: &SocketOptions,
        ) -> Option<Segment<S::Payload<'a>>> {
            // We favor `rcv` over `snd` if both are present. The alternative
            // will also work (sending whatever is ready at the same time of
            // sending the delayed ACK) but we need special treatment for
            // FIN_WAIT_1 if we choose the alternative, because otherwise we
            // will have a spurious retransmitted FIN.
            let rcv_nxt = rcv.nxt();
            let rcv_wnd = rcv.select_window();
            let seg = rcv.poll_send(snd.max, now).map(|seg| seg.into_empty()).or_else(|| {
                snd.poll_send(
                    counters,
                    rcv_nxt,
                    rcv_wnd >> rcv.wnd_scale,
                    limit,
                    now,
                    socket_options,
                )
            });
            // We must have piggybacked an ACK so we can cancel the timer now.
            if seg.is_some() && matches!(rcv.timer, Some(ReceiveTimer::DelayedAck { .. })) {
                rcv.timer = None;
            }
            seg
        }
        let seg = match self {
            State::SynSent(SynSent {
                iss,
                timestamp,
                retrans_timer,
                active_open: _,
                buffer_sizes: _,
                device_mss,
                default_mss: _,
                rcv_wnd_scale,
            }) => (retrans_timer.at <= now).then(|| {
                *timestamp = None;
                retrans_timer.backoff(now);
                Segment::syn(
                    *iss,
                    UnscaledWindowSize::from(u16::MAX),
                    Options { mss: Some(*device_mss), window_scale: Some(*rcv_wnd_scale) },
                )
            }),
            State::SynRcvd(SynRcvd {
                iss,
                irs,
                timestamp,
                retrans_timer,
                simultaneous_open: _,
                buffer_sizes: _,
                smss,
                rcv_wnd_scale,
                snd_wnd_scale,
            }) => (retrans_timer.at <= now).then(|| {
                *timestamp = None;
                retrans_timer.backoff(now);
                Segment::syn_ack(
                    *iss,
                    *irs + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    Options {
                        mss: Some(*smss),
                        window_scale: snd_wnd_scale.map(|_| *rcv_wnd_scale),
                    },
                )
            }),
            State::Established(Established { snd, rcv }) => {
                poll_rcv_then_snd(counters, snd, rcv, limit, now, socket_options)
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd, last_wnd_scale }) => snd
                .poll_send(
                    counters,
                    *last_ack,
                    *last_wnd >> *last_wnd_scale,
                    limit,
                    now,
                    socket_options,
                ),
            State::LastAck(LastAck { snd, last_ack, last_wnd, last_wnd_scale })
            | State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale }) => snd.poll_send(
                counters,
                *last_ack,
                *last_wnd >> *last_wnd_scale,
                limit,
                now,
                socket_options,
            ),
            State::FinWait1(FinWait1 { snd, rcv }) => {
                poll_rcv_then_snd(counters, snd, rcv, limit, now, socket_options)
            }
            State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => {
                rcv.poll_send(*last_seq, now).map(|seg| seg.into_empty())
            }
            State::Closed(_) | State::Listen(_) | State::TimeWait(_) => None,
        };
        seg.ok_or(NewlyClosed::No)
    }

    /// Polls the state machine to check if the connection should be closed.
    ///
    /// Returns whether the connection has been closed.
    fn poll_close(
        &mut self,
        counters: &TcpCountersInner,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled: _,
            user_timeout: _,
            delayed_ack: _,
            fin_wait2_timeout: _,
            max_syn_retries: _,
            ip_options: _,
        }: &SocketOptions,
    ) -> NewlyClosed {
        let timed_out = match self {
            State::Established(Established { snd, rcv: _ }) => snd.timed_out(now, keep_alive),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                snd.timed_out(now, keep_alive)
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                snd.timed_out(now, keep_alive)
            }
            State::FinWait1(FinWait1 { snd, rcv: _ }) => snd.timed_out(now, keep_alive),
            State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer,
                active_open: _,
                buffer_sizes: _,
                device_mss: _,
                default_mss: _,
                rcv_wnd_scale: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer,
                simultaneous_open: _,
                buffer_sizes: _,
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            }) => retrans_timer.timed_out(now),

            State::Closed(_) | State::Listen(_) | State::TimeWait(_) => false,
            State::FinWait2(FinWait2 { last_seq: _, rcv: _, timeout_at }) => {
                timeout_at.map(|at| now >= at).unwrap_or(false)
            }
        };
        if timed_out {
            return self.transition_to_state(
                counters,
                State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }),
            );
        } else if let State::TimeWait(tw) = self {
            if tw.expiry <= now {
                return self.transition_to_state(counters, State::Closed(Closed { reason: None }));
            }
        }
        NewlyClosed::No
    }

    /// Returns an instant at which the caller SHOULD make their best effort to
    /// call [`poll_send`].
    ///
    /// An example synchronous protocol loop would look like:
    ///
    /// ```ignore
    /// loop {
    ///     let now = Instant::now();
    ///     output(state.poll_send(now));
    ///     let incoming = wait_until(state.poll_send_at())
    ///     output(state.on_segment(incoming, Instant::now()));
    /// }
    /// ```
    ///
    /// Note: When integrating asynchronously, the caller needs to install
    /// timers (for example, by using `TimerContext`), then calls to
    /// `poll_send_at` and to `install_timer`/`cancel_timer` should not
    /// interleave, otherwise timers may be lost.
    pub(crate) fn poll_send_at(&self) -> Option<I> {
        let combine_expiry = |e1: Option<I>, e2: Option<I>| match (e1, e2) {
            (None, None) => None,
            (None, Some(e2)) => Some(e2),
            (Some(e1), None) => Some(e1),
            (Some(e1), Some(e2)) => Some(e1.min(e2)),
        };
        match self {
            State::Established(Established { snd, rcv }) => combine_expiry(
                snd.timer.as_ref().map(SendTimer::expiry),
                rcv.timer.as_ref().map(ReceiveTimer::expiry),
            ),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                Some(snd.timer?.expiry())
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                Some(snd.timer?.expiry())
            }
            State::FinWait1(FinWait1 { snd, rcv }) => combine_expiry(
                snd.timer.as_ref().map(SendTimer::expiry),
                rcv.timer.as_ref().map(ReceiveTimer::expiry),
            ),
            State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at }) => {
                combine_expiry(*timeout_at, rcv.timer.as_ref().map(ReceiveTimer::expiry))
            }
            State::SynRcvd(syn_rcvd) => Some(syn_rcvd.retrans_timer.at),
            State::SynSent(syn_sent) => Some(syn_sent.retrans_timer.at),
            State::Closed(_) | State::Listen(_) => None,
            State::TimeWait(TimeWait {
                last_seq: _,
                last_ack: _,
                last_wnd: _,
                expiry,
                last_wnd_scale: _,
            }) => Some(*expiry),
        }
    }

    /// Corresponds to the [CLOSE](https://tools.ietf.org/html/rfc793#page-60)
    /// user call.
    ///
    /// The caller should provide the current time if this close call would make
    /// the connection defunct, so that we can reclaim defunct connections based
    /// on timeouts.
    pub(super) fn close(
        &mut self,
        counters: &TcpCountersInner,
        close_reason: CloseReason<I>,
        socket_options: &SocketOptions,
    ) -> Result<NewlyClosed, CloseError>
    where
        ActiveOpen: IntoBuffers<R, S>,
    {
        match self {
            State::Closed(_) => Err(CloseError::NoConnection),
            State::Listen(_) | State::SynSent(_) => {
                Ok(self.transition_to_state(counters, State::Closed(Closed { reason: None })))
            }
            State::SynRcvd(SynRcvd {
                iss,
                irs,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open,
                buffer_sizes,
                smss,
                rcv_wnd_scale,
                snd_wnd_scale,
            }) => {
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-60):
                //   SYN-RECEIVED STATE
                //     If no SENDs have been issued and there is no pending data
                //     to send, then form a FIN segment and send it, and enter
                //     FIN-WAIT-1 state; otherwise queue for processing after
                //     entering ESTABLISHED state.
                // Note: Per RFC, we should transition into FIN-WAIT-1, however
                // popular implementations deviate from it - Freebsd resets the
                // connection instead of a normal shutdown:
                // https://github.com/freebsd/freebsd-src/blob/8fc80638496e620519b2585d9fab409494ea4b43/sys/netinet/tcp_subr.c#L2344-L2346
                // while Linux simply does not send anything:
                // https://github.com/torvalds/linux/blob/68e77ffbfd06ae3ef8f2abf1c3b971383c866983/net/ipv4/inet_connection_sock.c#L1180-L1187
                // Here we choose the Linux's behavior, because it is more
                // popular and it is still correct from the protocol's point of
                // view: the peer will find out eventually when it retransmits
                // its SYN - it will get a RST back because now the listener no
                // longer exists - it is as if the initial SYN is lost. The
                // following check makes sure we only proceed if we were
                // actively opened, i.e., initiated by `connect`.
                let (rcv_buffer, snd_buffer) = simultaneous_open
                    .take()
                    .expect(
                        "a SYN-RCVD state that is in the pending queue \
                        should call abort instead of close",
                    )
                    .into_buffers(*buffer_sizes);
                // Note: `Send` in `FinWait1` always has a FIN queued.
                // Since we don't support sending data when connection
                // isn't established, so enter FIN-WAIT-1 immediately.
                let (snd_wnd_scale, rcv_wnd_scale) = snd_wnd_scale
                    .map(|snd_wnd_scale| (snd_wnd_scale, *rcv_wnd_scale))
                    .unwrap_or_default();
                let finwait1 = FinWait1 {
                    snd: Send {
                        nxt: *iss + 1,
                        max: *iss + 1,
                        una: *iss + 1,
                        wnd: WindowSize::DEFAULT,
                        wl1: *iss,
                        wl2: *irs,
                        buffer: snd_buffer,
                        last_seq_ts: None,
                        rtt_estimator: Estimator::NoSample,
                        timer: None,
                        congestion_control: CongestionControl::cubic_with_mss(*smss),
                        wnd_scale: snd_wnd_scale,
                        wnd_max: WindowSize::DEFAULT,
                    }
                    .into(),
                    rcv: Recv {
                        buffer: RecvBufferState::Open {
                            buffer: rcv_buffer,
                            assembler: Assembler::new(*irs + 1),
                        },
                        timer: None,
                        mss: *smss,
                        wnd_scale: rcv_wnd_scale,
                        last_window_update: (*irs + 1, buffer_sizes.rwnd()),
                    }
                    .into(),
                };
                Ok(self.transition_to_state(counters, State::FinWait1(finwait1)))
            }
            State::Established(Established { snd, rcv }) => {
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-60):
                //   ESTABLISHED STATE
                //     Queue this until all preceding SENDs have been segmentized,
                //     then form a FIN segment and send it.  In any case, enter
                //     FIN-WAIT-1 state.
                let finwait1 = FinWait1 {
                    snd: snd.to_ref().take().queue_fin().into(),
                    rcv: rcv.to_ref().to_takeable(),
                };
                Ok(self.transition_to_state(counters, State::FinWait1(finwait1)))
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd, last_wnd_scale }) => {
                let lastack = LastAck {
                    snd: snd.to_ref().take().queue_fin(),
                    last_ack: *last_ack,
                    last_wnd: *last_wnd,
                    last_wnd_scale: *last_wnd_scale,
                };
                Ok(self.transition_to_state(counters, State::LastAck(lastack)))
            }
            State::LastAck(_) | State::FinWait1(_) | State::Closing(_) | State::TimeWait(_) => {
                Err(CloseError::Closing)
            }
            State::FinWait2(FinWait2 { last_seq: _, rcv: _, timeout_at }) => {
                if let (CloseReason::Close { now }, Some(fin_wait2_timeout)) =
                    (close_reason, socket_options.fin_wait2_timeout)
                {
                    assert_eq!(timeout_at.replace(now.saturating_add(fin_wait2_timeout)), None);
                }
                Err(CloseError::Closing)
            }
        }
    }

    pub(super) fn shutdown_recv(&mut self) -> Result<(), CloseError> {
        match self {
            State::Closed(_) => Err(CloseError::NoConnection),

            State::Listen(_)
            | State::SynSent(_)
            | State::SynRcvd(_)
            | State::CloseWait(_)
            | State::LastAck(_)
            | State::Closing(_)
            | State::TimeWait(_) => Ok(()),

            // Shutdown receive by closing the receive buffer.
            State::Established(Established { rcv, .. }) | State::FinWait1(FinWait1 { rcv, .. }) => {
                rcv.buffer.close();
                Ok(())
            }
            State::FinWait2(FinWait2 { rcv, .. }) => {
                rcv.buffer.close();
                Ok(())
            }
        }
    }

    /// Corresponds to [ABORT](https://tools.ietf.org/html/rfc9293#section-3.10.5)
    /// user call.
    pub(crate) fn abort(
        &mut self,
        counters: &TcpCountersInner,
    ) -> (Option<Segment<()>>, NewlyClosed) {
        let reply = match self {
            //   LISTEN STATE
            //      *  Any outstanding RECEIVEs should be returned with "error:
            //      connection reset" responses.  Delete TCB, enter CLOSED state, and
            //      return.
            //   SYN-SENT STATE
            //   *  All queued SENDs and RECEIVEs should be given "connection reset"
            //      notification.  Delete the TCB, enter CLOSED state, and return.
            //   CLOSING STATE
            //   LAST-ACK STATE
            //   TIME-WAIT STATE
            //   *  Respond with "ok" and delete the TCB, enter CLOSED state, and
            //      return.
            State::Closed(_)
            | State::Listen(_)
            | State::SynSent(_)
            | State::Closing(_)
            | State::LastAck(_)
            | State::TimeWait(_) => None,
            //   SYN-RECEIVED STATE
            //   ESTABLISHED STATE
            //   FIN-WAIT-1 STATE
            //   FIN-WAIT-2 STATE
            //   CLOSE-WAIT STATE
            //   *  Send a reset segment:
            //      <SEQ=SND.NXT><CTL=RST>
            //   *  All queued SENDs and RECEIVEs should be given "connection reset"
            //      notification; all segments queued for transmission (except for the
            //      RST formed above) or retransmission should be flushed.  Delete the
            //      TCB, enter CLOSED state, and return.
            State::SynRcvd(SynRcvd {
                iss,
                irs,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: _,
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            }) => {
                // When we're in SynRcvd we already sent out SYN-ACK with iss,
                // so a RST must have iss+1.
                Some(Segment::rst_ack(*iss + 1, *irs + 1))
            }
            State::Established(Established { snd, rcv }) => {
                Some(Segment::rst_ack(snd.nxt, rcv.nxt()))
            }
            State::FinWait1(FinWait1 { snd, rcv }) => Some(Segment::rst_ack(snd.nxt, rcv.nxt())),
            State::FinWait2(FinWait2 { rcv, last_seq, timeout_at: _ }) => {
                Some(Segment::rst_ack(*last_seq, rcv.nxt()))
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd: _, last_wnd_scale: _ }) => {
                Some(Segment::rst_ack(snd.nxt, *last_ack))
            }
        };
        (
            reply,
            self.transition_to_state(
                counters,
                State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }),
            ),
        )
    }

    pub(crate) fn buffers_mut(&mut self) -> BuffersRefMut<'_, R, S> {
        match self {
            State::TimeWait(_) | State::Closed(_) => BuffersRefMut::NoBuffers,
            State::Listen(Listen { buffer_sizes, .. })
            | State::SynRcvd(SynRcvd { buffer_sizes, .. })
            | State::SynSent(SynSent { buffer_sizes, .. }) => BuffersRefMut::Sizes(buffer_sizes),
            State::Established(Established { snd, rcv }) => match &mut rcv.buffer {
                RecvBufferState::Open { buffer: ref mut recv_buf, .. } => {
                    BuffersRefMut::Both { send: &mut snd.buffer, recv: recv_buf }
                }
                RecvBufferState::Closed { .. } => BuffersRefMut::SendOnly(&mut snd.buffer),
            },
            State::FinWait1(FinWait1 { snd, rcv }) => match &mut rcv.buffer {
                RecvBufferState::Open { buffer: ref mut recv_buf, .. } => {
                    BuffersRefMut::Both { send: &mut snd.buffer, recv: recv_buf }
                }
                RecvBufferState::Closed { .. } => BuffersRefMut::SendOnly(&mut snd.buffer),
            },
            State::FinWait2(FinWait2::<I, R> { rcv, .. }) => match &mut rcv.buffer {
                RecvBufferState::Open { buffer: ref mut recv_buf, .. } => {
                    BuffersRefMut::RecvOnly(recv_buf)
                }
                RecvBufferState::Closed { .. } => BuffersRefMut::NoBuffers,
            },
            State::Closing(Closing::<I, S> { snd, .. })
            | State::LastAck(LastAck::<I, S> { snd, .. }) => {
                BuffersRefMut::SendOnly(&mut snd.buffer)
            }
            State::CloseWait(CloseWait::<I, S> { snd, .. }) => {
                BuffersRefMut::SendOnly(&mut snd.buffer)
            }
        }
    }

    /// Processes an incoming ICMP error, returns an soft error that needs to be
    /// recorded in the containing socket.
    pub(super) fn on_icmp_error(
        &mut self,
        counters: &TcpCountersInner,
        err: IcmpErrorCode,
        seq: SeqNum,
    ) -> (Option<ConnectionError>, NewlyClosed) {
        let Some(err) = ConnectionError::try_from_icmp_error(err) else {
            return (None, NewlyClosed::No);
        };
        // We consider the following RFC quotes when implementing this function.
        // Per RFC 5927 Section 4.1:
        //  Many TCP implementations have incorporated a validation check such
        //  that they react only to those ICMP error messages that appear to
        //  relate to segments currently "in flight" to the destination system.
        //  These implementations check that the TCP sequence number contained
        //  in the payload of the ICMP error message is within the range
        //  SND.UNA =< SEG.SEQ < SND.NXT.
        // Per RFC 5927 Section 5.2:
        //  Based on this analysis, most popular TCP implementations treat all
        //  ICMP "hard errors" received for connections in any of the
        //  synchronized states (ESTABLISHED, FIN-WAIT-1, FIN-WAIT-2, CLOSE-WAIT,
        //  CLOSING, LAST-ACK, or TIME-WAIT) as "soft errors".  That is, they do
        //  not abort the corresponding connection upon receipt of them.
        // Per RFC 5461 Section 4.1:
        //  A number of TCP implementations have modified their reaction to all
        //  ICMP soft errors and treat them as hard errors when they are received
        //  for connections in the SYN-SENT or SYN-RECEIVED states. For example,
        //  this workaround has been implemented in the Linux kernel since
        //  version 2.0.0 (released in 1996) [Linux]
        let connect_error = match self {
            State::Closed(_) => None,
            State::Listen(listen) => unreachable!(
                "ICMP errors should not be delivered on a listener, received code {:?} on {:?}",
                err, listen
            ),
            State::SynRcvd(SynRcvd {
                iss,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: _,
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            })
            | State::SynSent(SynSent {
                iss,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes: _,
                device_mss: _,
                default_mss: _,
                rcv_wnd_scale: _,
            }) => {
                if *iss == seq {
                    return (
                        None,
                        self.transition_to_state(
                            counters,
                            State::Closed(Closed { reason: Some(err) }),
                        ),
                    );
                }
                None
            }
            State::Established(Established { snd, rcv: _ })
            | State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                (!snd.una.after(seq) && seq.before(snd.nxt)).then_some(err)
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ }) => {
                (!snd.una.after(seq) && seq.before(snd.nxt)).then_some(err)
            }
            State::FinWait1(FinWait1 { snd, rcv: _ }) => {
                (!snd.una.after(seq) && seq.before(snd.nxt)).then_some(err)
            }
            // The following states does not have any outstanding segments, so
            // they don't expect any incoming ICMP error.
            State::FinWait2(_) | State::TimeWait(_) => None,
        };
        (connect_error, NewlyClosed::No)
    }
}

/// From the socket layer, both `close` and `shutdown` will result in a state
/// machine level `close` call. We need to differentiate between the two
/// because we may need to do extra work if it is a socket `close`.
pub(super) enum CloseReason<I: Instant> {
    Shutdown,
    Close { now: I },
}

#[cfg(test)]
mod test {
    use alloc::vec;
    use core::fmt::Debug;
    use core::num::NonZeroU16;
    use core::time::Duration;

    use assert_matches::assert_matches;
    use net_types::ip::Ipv4;
    use netstack3_base::testutil::{FakeInstant, FakeInstantCtx};
    use netstack3_base::{FragmentedPayload, InstantContext as _};
    use test_case::test_case;

    use super::*;
    use crate::internal::base::testutil::{
        DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE, DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE,
    };
    use crate::internal::base::DEFAULT_FIN_WAIT2_TIMEOUT;
    use crate::internal::buffer::testutil::RingBuffer;
    use crate::internal::buffer::Buffer;

    const ISS_1: SeqNum = SeqNum::new(100);
    const ISS_2: SeqNum = SeqNum::new(300);

    const RTT: Duration = Duration::from_millis(500);

    const DEVICE_MAXIMUM_SEGMENT_SIZE: Mss = Mss(NonZeroU16::new(1400 as u16).unwrap());

    /// A buffer provider that doesn't need extra information to construct
    /// buffers as this is only used in unit tests for the state machine only.
    enum ClientlessBufferProvider {}

    impl<R: ReceiveBuffer + Default, S: SendBuffer + Default> BufferProvider<R, S>
        for ClientlessBufferProvider
    {
        type PassiveOpen = ();
        type ActiveOpen = ();

        fn new_passive_open_buffers(_buffer_sizes: BufferSizes) -> (R, S, Self::PassiveOpen) {
            (R::default(), S::default(), ())
        }
    }

    impl RingBuffer {
        fn with_data<'a>(cap: usize, data: &'a [u8]) -> Self {
            let mut buffer = RingBuffer::new(cap);
            let nwritten = buffer.write_at(0, &data);
            assert_eq!(nwritten, data.len());
            buffer.make_readable(nwritten, false);
            buffer
        }
    }

    /// A buffer that can't read or write for test purpose.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct NullBuffer;

    impl Buffer for NullBuffer {
        fn capacity_range() -> (usize, usize) {
            (usize::MIN, usize::MAX)
        }

        fn limits(&self) -> BufferLimits {
            BufferLimits { len: 0, capacity: 0 }
        }

        fn target_capacity(&self) -> usize {
            0
        }

        fn request_capacity(&mut self, _size: usize) {}
    }

    impl ReceiveBuffer for NullBuffer {
        fn write_at<P: Payload>(&mut self, _offset: usize, _data: &P) -> usize {
            0
        }

        fn make_readable(&mut self, count: usize, has_outstanding: bool) {
            assert_eq!(count, 0);
            assert_eq!(has_outstanding, false);
        }
    }

    impl SendBuffer for NullBuffer {
        type Payload<'a> = &'a [u8];

        fn mark_read(&mut self, count: usize) {
            assert_eq!(count, 0);
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(Self::Payload<'a>) -> R,
        {
            assert_eq!(offset, 0);
            f(&[])
        }
    }

    impl<R: ReceiveBuffer, S: SendBuffer> State<FakeInstant, R, S, ()> {
        fn poll_send_with_default_options(
            &mut self,
            mss: u32,
            now: FakeInstant,
            counters: &TcpCountersInner,
        ) -> Option<Segment<S::Payload<'_>>> {
            self.poll_send(counters, mss, now, &SocketOptions::default()).ok()
        }

        fn on_segment_with_default_options<P: Payload, BP: BufferProvider<R, S, ActiveOpen = ()>>(
            &mut self,
            incoming: Segment<P>,
            now: FakeInstant,
            counters: &TcpCountersInner,
        ) -> (Option<Segment<()>>, Option<BP::PassiveOpen>)
        where
            BP::PassiveOpen: Debug,
            R: Default,
            S: Default,
        {
            // In testing, it is convenient to disable delayed ack by default.
            let (segment, passive_open, _data_acked, _newly_closed) = self.on_segment::<P, BP>(
                counters,
                incoming,
                now,
                &SocketOptions::default(),
                false, /* defunct */
            );
            (segment, passive_open)
        }
    }

    impl<S: SendBuffer + Debug> State<FakeInstant, RingBuffer, S, ()> {
        fn read_with(&mut self, f: impl for<'b> FnOnce(&'b [&'_ [u8]]) -> usize) -> usize {
            match self {
                State::Closed(_)
                | State::Listen(_)
                | State::SynRcvd(_)
                | State::SynSent(_)
                | State::CloseWait(_)
                | State::LastAck(_)
                | State::Closing(_)
                | State::TimeWait(_) => {
                    panic!("No receive state in {:?}", self);
                }
                State::Established(Established { snd: _, rcv })
                | State::FinWait1(FinWait1 { snd: _, rcv }) => {
                    assert_matches!(&mut rcv.buffer, RecvBufferState::Open{ buffer, .. } => buffer.read_with(f))
                }
                State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                    assert_matches!(&mut rcv.buffer, RecvBufferState::Open{ buffer, .. } => buffer.read_with(f))
                }
            }
        }
    }

    impl State<FakeInstant, RingBuffer, NullBuffer, ()> {
        fn new_syn_rcvd(instant: FakeInstant) -> Self {
            State::SynRcvd(SynRcvd {
                iss: ISS_2,
                irs: ISS_1,
                timestamp: Some(instant),
                retrans_timer: RetransTimer::new(
                    instant,
                    Estimator::RTO_INIT,
                    DEFAULT_USER_TIMEOUT,
                    DEFAULT_MAX_RETRIES,
                ),
                simultaneous_open: Some(()),
                buffer_sizes: Default::default(),
                smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                rcv_wnd_scale: WindowScale::default(),
                snd_wnd_scale: Some(WindowScale::default()),
            })
        }
    }

    #[test_case(Segment::rst(ISS_1) => None; "drop RST")]
    #[test_case(Segment::rst_ack(ISS_1, ISS_2) => None; "drop RST|ACK")]
    #[test_case(Segment::syn(ISS_1, UnscaledWindowSize::from(0), Options { mss: None, window_scale: None }) => Some(Segment::rst_ack(SeqNum::new(0), ISS_1 + 1)); "reset SYN")]
    #[test_case(Segment::syn_ack(ISS_1, ISS_2, UnscaledWindowSize::from(0), Options { mss: None, window_scale: None }) => Some(Segment::rst(ISS_2)); "reset SYN|ACK")]
    #[test_case(Segment::data(ISS_1, ISS_2, UnscaledWindowSize::from(0), &[0, 1, 2][..]) => Some(Segment::rst(ISS_2)); "reset data segment")]
    fn segment_arrives_when_closed(
        incoming: impl Into<Segment<&'static [u8]>>,
    ) -> Option<Segment<()>> {
        let closed = Closed { reason: () };
        closed.on_segment(&incoming.into())
    }

    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 - 1), RTT
    => SynSentOnSegmentDisposition::Ignore; "unacceptable ACK with RST")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1 - 1, UnscaledWindowSize::from(u16::MAX)), RTT
    => SynSentOnSegmentDisposition::SendRst(
        Segment::rst(ISS_1-1),
    ); "unacceptable ACK without RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1), RTT
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: Some(ConnectionError::ConnectionRefused) },
    ); "acceptable ACK(ISS) with RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 + 1), RTT
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: Some(ConnectionError::ConnectionRefused) },
    ); "acceptable ACK(ISS+1) with RST")]
    #[test_case(
        Segment::rst(ISS_2), RTT
    => SynSentOnSegmentDisposition::Ignore; "RST without ack")]
    #[test_case(
        Segment::syn(ISS_2, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: Some(WindowScale::default()) }), RTT
    => SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
        Segment::syn_ack(ISS_1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: Some(Mss::default::<Ipv4>()), window_scale: Some(WindowScale::default()) }),
        SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: Some(FakeInstant::from(RTT)),
            retrans_timer: RetransTimer::new(
                FakeInstant::from(RTT),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT - RTT,
                DEFAULT_MAX_SYNACK_RETRIES
            ),
            simultaneous_open: None,
            buffer_sizes: BufferSizes::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        }
    ); "SYN only")]
    #[test_case(
        Segment::fin(ISS_2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)), RTT
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK with FIN")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)), RTT
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK(ISS+1) with nothing")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)), RTT
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK(ISS) without RST")]
    #[test_case(
        Segment::syn(ISS_2, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: None }),
        DEFAULT_USER_TIMEOUT
    => SynSentOnSegmentDisposition::EnterClosed(Closed {
        reason: None
    }); "syn but timed out")]
    fn segment_arrives_when_syn_sent(
        incoming: Segment<()>,
        delay: Duration,
    ) -> SynSentOnSegmentDisposition<FakeInstant, ()> {
        let syn_sent = SynSent {
            iss: ISS_1,
            timestamp: Some(FakeInstant::default()),
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT,
                DEFAULT_MAX_RETRIES,
            ),
            active_open: (),
            buffer_sizes: BufferSizes::default(),
            default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
        };
        syn_sent.on_segment(incoming, FakeInstant::from(delay))
    }

    #[test_case(Segment::rst(ISS_2) => ListenOnSegmentDisposition::Ignore; "ignore RST")]
    #[test_case(Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)) =>
        ListenOnSegmentDisposition::SendRst(Segment::rst(ISS_1)); "reject ACK")]
    #[test_case(Segment::syn(ISS_2, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: Some(WindowScale::default()) }) =>
        ListenOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
            Segment::syn_ack(ISS_1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: Some(Mss::default::<Ipv4>()), window_scale: Some(WindowScale::default()) }),
            SynRcvd {
                iss: ISS_1,
                irs: ISS_2,
                timestamp: Some(FakeInstant::default()),
                retrans_timer: RetransTimer::new(
                    FakeInstant::default(),
                    Estimator::RTO_INIT,
                    DEFAULT_USER_TIMEOUT,
                    DEFAULT_MAX_SYNACK_RETRIES,
                ),
                simultaneous_open: None,
                buffer_sizes: BufferSizes::default(),
                smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
            }); "accept syn")]
    fn segment_arrives_when_listen(
        incoming: Segment<()>,
    ) -> ListenOnSegmentDisposition<FakeInstant> {
        let listen = Closed::<Initial>::listen(
            ISS_1,
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            None,
        );
        listen.on_segment(incoming, FakeInstant::default())
    }

    #[test_case(
        Segment::ack(ISS_1, ISS_2, UnscaledWindowSize::from(u16::MAX)),
        None
    => Some(
        Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX))
    ); "OTW segment")]
    #[test_case(
        Segment::rst_ack(ISS_1, ISS_2),
        None
    => None; "OTW RST")]
    #[test_case(
        Segment::rst_ack(ISS_1 + 1, ISS_2),
        Some(State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }))
    => None; "acceptable RST")]
    #[test_case(
        Segment::syn(ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: Some(WindowScale::default()) }),
        Some(State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) }))
    => Some(
        Segment::rst(ISS_2 + 1)
    ); "duplicate syn")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2, UnscaledWindowSize::from(u16::MAX)),
        None
    => Some(
        Segment::rst(ISS_2)
    ); "unacceptable ack (ISS)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX)),
        Some(State::Established(
            Established {
                snd: Send {
                    nxt: ISS_2 + 1,
                    max: ISS_2 + 1,
                    una: ISS_2 + 1,
                    wnd: WindowSize::DEFAULT,
                    wnd_max: WindowSize::DEFAULT,
                    buffer: NullBuffer,
                    wl1: ISS_1 + 1,
                    wl2: ISS_2 + 1,
                    rtt_estimator: Estimator::Measured {
                        srtt: RTT,
                        rtt_var: RTT / 2,
                    },
                    last_seq_ts: None,
                    timer: None,
                    congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
                }.into(),
                rcv: Recv {
                    buffer: RecvBufferState::Open {
                        buffer: RingBuffer::default(),
                        assembler: Assembler::new(ISS_1 + 1),
                    },
                    timer: None,
                    mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                    wnd_scale: WindowScale::default(),
                    last_window_update: (ISS_1 + 1, WindowSize::DEFAULT),
                }.into(),
            }
        ))
    => None; "acceptable ack (ISS + 1)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 + 2, UnscaledWindowSize::from(u16::MAX)),
        None
    => Some(
        Segment::rst(ISS_2 + 2)
    ); "unacceptable ack (ISS + 2)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 - 1, UnscaledWindowSize::from(u16::MAX)),
        None
    => Some(
        Segment::rst(ISS_2 - 1)
    ); "unacceptable ack (ISS - 1)")]
    #[test_case(
        Segment::new(ISS_1 + 1, None, None, UnscaledWindowSize::from(u16::MAX)),
        None
    => None; "no ack")]
    #[test_case(
        Segment::fin(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX)),
        Some(State::CloseWait(CloseWait {
            snd: Send {
                nxt: ISS_2 + 1,
                max: ISS_2 + 1,
                una: ISS_2 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_1 + 1,
                wl2: ISS_2 + 1,
                rtt_estimator: Estimator::Measured{
                    srtt: RTT,
                    rtt_var: RTT / 2,
                },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
            }.into(),
            last_ack: ISS_1 + 2,
            last_wnd: WindowSize::from_u32(u32::from(u16::MAX - 1)).unwrap(),
            last_wnd_scale: WindowScale::ZERO,
        }))
    => Some(
        Segment::ack(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX - 1))
    ); "fin")]
    fn segment_arrives_when_syn_rcvd(
        incoming: Segment<()>,
        expected: Option<State<FakeInstant, RingBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut state = State::new_syn_rcvd(clock.now());
        clock.sleep(RTT);
        let (seg, _passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                incoming,
                clock.now(),
                &counters,
            );
        match expected {
            Some(new_state) => assert_eq!(new_state, state),
            None => assert_matches!(state, State::SynRcvd(_)),
        };
        seg
    }

    #[test]
    fn abort_when_syn_rcvd() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut state = State::new_syn_rcvd(clock.now());
        let segment = assert_matches!(
            state.abort(&counters),
            (Some(seg), NewlyClosed::Yes) => seg
        );
        assert_eq!(segment.header.control, Some(Control::RST));
        assert_eq!(segment.header.seq, ISS_2 + 1);
        assert_eq!(segment.header.ack, Some(ISS_1 + 1));
    }

    #[test_case(
        Segment::syn(ISS_2 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: None }),
        Some(State::Closed (
            Closed { reason: Some(ConnectionError::ConnectionReset) },
        ))
    => Some(Segment::rst(ISS_1 + 1)); "duplicate syn")]
    #[test_case(
        Segment::rst(ISS_2 + 1),
        Some(State::Closed (
            Closed { reason: Some(ConnectionError::ConnectionReset) },
        ))
    => None; "accepatable rst")]
    #[test_case(
        Segment::ack(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
        None
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(2))
    ); "unacceptable ack")]
    #[test_case(
        Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)),
        None
    => None; "pure ack")]
    #[test_case(
        Segment::fin(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)),
        Some(State::CloseWait(CloseWait {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
            }.into(),
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::new(1).unwrap(),
            last_wnd_scale: WindowScale::ZERO,
        }))
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 2, UnscaledWindowSize::from(1))
    ); "pure fin")]
    #[test_case(
        Segment::piggybacked_fin(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), "A".as_bytes()),
        Some(State::CloseWait(CloseWait {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
            }.into(),
            last_ack: ISS_2 + 3,
            last_wnd: WindowSize::ZERO,
            last_wnd_scale: WindowScale::ZERO,
        }))
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 3, UnscaledWindowSize::from(0))
    ); "fin with 1 byte")]
    #[test_case(
        Segment::piggybacked_fin(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), "AB".as_bytes()),
        None
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 3, UnscaledWindowSize::from(0))
    ); "fin with 2 bytes")]
    fn segment_arrives_when_established(
        incoming: Segment<&[u8]>,
        expected: Option<State<FakeInstant, RingBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let counters = TcpCountersInner::default();
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(2),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(2).unwrap()),
            }
            .into(),
        });
        let (seg, passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                incoming,
                FakeInstant::default(),
                &counters,
            );
        assert_eq!(passive_open, None);
        match expected {
            Some(new_state) => assert_eq!(new_state, state),
            None => assert_matches!(state, State::Established(_)),
        };
        seg
    }

    #[test]
    fn common_rcv_data_segment_arrives() {
        let counters = TcpCountersInner::default();
        // Tests the common behavior when data segment arrives in states that
        // have a receive state.
        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::DEFAULT,
            wnd_max: WindowSize::DEFAULT,
            buffer: NullBuffer,
            wl1: ISS_2 + 1,
            wl2: ISS_1 + 1,
            rtt_estimator: Estimator::default(),
            last_seq_ts: None,
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            ),
            wnd_scale: WindowScale::default(),
        };
        let new_rcv = || Recv {
            buffer: RecvBufferState::Open {
                buffer: RingBuffer::new(TEST_BYTES.len()),
                assembler: Assembler::new(ISS_2 + 1),
            },
            timer: None,
            mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            wnd_scale: WindowScale::default(),
            last_window_update: (ISS_2 + 1, WindowSize::new(TEST_BYTES.len()).unwrap()),
        };
        for mut state in [
            State::Established(Established { snd: new_snd().into(), rcv: new_rcv().into() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin().into(), rcv: new_rcv().into() }),
            State::FinWait2(FinWait2 { last_seq: ISS_1 + 1, rcv: new_rcv(), timeout_at: None }),
        ] {
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::data(
                        ISS_2 + 1,
                        ISS_1 + 1,
                        UnscaledWindowSize::from(u16::MAX),
                        TEST_BYTES
                    ),
                    FakeInstant::default(),
                    &counters,
                ),
                (
                    Some(Segment::ack(
                        ISS_1 + 1,
                        ISS_2 + 1 + TEST_BYTES.len(),
                        UnscaledWindowSize::from(0)
                    )),
                    None
                )
            );
            assert_eq!(
                state.read_with(|bytes| {
                    assert_eq!(bytes.concat(), TEST_BYTES);
                    TEST_BYTES.len()
                }),
                TEST_BYTES.len()
            );
        }
    }

    #[test]
    fn common_snd_ack_segment_arrives() {
        let counters = TcpCountersInner::default();
        // Tests the common behavior when ack segment arrives in states that
        // have a send state.
        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::DEFAULT,
            wnd_max: WindowSize::DEFAULT,
            buffer: RingBuffer::with_data(TEST_BYTES.len(), TEST_BYTES),
            wl1: ISS_2 + 1,
            wl2: ISS_1 + 1,
            rtt_estimator: Estimator::default(),
            last_seq_ts: None,
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            ),
            wnd_scale: WindowScale::default(),
        };
        let new_rcv = || Recv {
            buffer: RecvBufferState::Open {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_2 + 1),
            },
            timer: None,
            mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            wnd_scale: WindowScale::default(),
            last_window_update: (ISS_2 + 1, WindowSize::ZERO),
        };
        for mut state in [
            State::Established(Established { snd: new_snd().into(), rcv: new_rcv().into() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin().into(), rcv: new_rcv().into() }),
            State::Closing(Closing {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
                last_wnd_scale: WindowScale::default(),
            }),
            State::CloseWait(CloseWait {
                snd: new_snd().into(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
                last_wnd_scale: WindowScale::default(),
            }),
            State::LastAck(LastAck {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
                last_wnd_scale: WindowScale::default(),
            }),
        ] {
            assert_eq!(
                state.poll_send_with_default_options(
                    u32::try_from(TEST_BYTES.len()).unwrap(),
                    FakeInstant::default(),
                    &counters,
                ),
                Some(Segment::data(
                    ISS_1 + 1,
                    ISS_2 + 1,
                    UnscaledWindowSize::from(0),
                    FragmentedPayload::new_contiguous(TEST_BYTES)
                ))
            );
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::<()>::ack(
                        ISS_2 + 1,
                        ISS_1 + 1 + TEST_BYTES.len(),
                        UnscaledWindowSize::from(u16::MAX)
                    ),
                    FakeInstant::default(),
                    &counters,
                ),
                (None, None),
            );
            assert_eq!(state.poll_send_at(), None);
            let snd = match state {
                State::Closed(_)
                | State::Listen(_)
                | State::SynRcvd(_)
                | State::SynSent(_)
                | State::FinWait2(_)
                | State::TimeWait(_) => unreachable!("Unexpected state {:?}", state),
                State::Established(e) => e.snd.into_inner().queue_fin(),
                State::CloseWait(c) => c.snd.into_inner().queue_fin(),
                State::LastAck(l) => l.snd,
                State::FinWait1(f) => f.snd.into_inner(),
                State::Closing(c) => c.snd,
            };
            assert_eq!(snd.nxt, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.max, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.una, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.buffer.limits().len, 0);
        }
    }

    #[test_case(
        Segment::syn(ISS_2 + 2, UnscaledWindowSize::from(u16::MAX),
                     Options { mss: None, window_scale: None }),
        Some(State::Closed (
            Closed { reason: Some(ConnectionError::ConnectionReset) },
        ))
    => Some(Segment::rst(ISS_1 + 1)); "syn")]
    #[test_case(
        Segment::rst(ISS_2 + 2),
        Some(State::Closed (
            Closed { reason: Some(ConnectionError::ConnectionReset) },
        ))
    => None; "rst")]
    #[test_case(
        Segment::fin(ISS_2 + 2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)),
        None
    => None; "ignore fin")]
    #[test_case(
        Segment::data(ISS_2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), "a".as_bytes()),
        None => Some(Segment::ack(ISS_1 + 1, ISS_2 + 2, UnscaledWindowSize::from(u16::MAX)));
        "ack old data")]
    #[test_case(
        Segment::data(ISS_2 + 2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), "Hello".as_bytes()),
        Some(State::Closed (
            Closed { reason: Some(ConnectionError::ConnectionReset) },
        ))
    => Some(Segment::rst(ISS_1 + 1)); "reset on new data")]
    fn segment_arrives_when_close_wait(
        incoming: Segment<&[u8]>,
        expected: Option<State<FakeInstant, RingBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let counters = TcpCountersInner::default();
        let mut state = State::CloseWait(CloseWait {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::DEFAULT,
            last_wnd_scale: WindowScale::ZERO,
        });
        let (seg, _passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                incoming,
                FakeInstant::default(),
                &counters,
            );
        match expected {
            Some(new_state) => assert_eq!(new_state, state),
            None => assert_matches!(state, State::CloseWait(_)),
        };
        seg
    }

    #[test]
    fn active_passive_open() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let (syn_sent, syn_seg) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        assert_eq!(
            syn_seg,
            Segment::syn(
                ISS_1,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );
        assert_eq!(
            syn_sent,
            SynSent {
                iss: ISS_1,
                timestamp: Some(clock.now()),
                retrans_timer: RetransTimer::new(
                    clock.now(),
                    Estimator::RTO_INIT,
                    DEFAULT_USER_TIMEOUT,
                    DEFAULT_MAX_SYN_RETRIES,
                ),
                active_open: (),
                buffer_sizes: BufferSizes::default(),
                default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                rcv_wnd_scale: WindowScale::default(),
            }
        );
        let mut active = State::SynSent(syn_sent);
        let mut passive = State::Listen(Closed::<Initial>::listen(
            ISS_2,
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            None,
        ));
        clock.sleep(RTT / 2);
        let (seg, passive_open) = passive
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_seg,
                clock.now(),
                &counters,
            );
        let syn_ack = seg.expect("failed to generate a syn-ack segment");
        assert_eq!(passive_open, None);
        assert_eq!(
            syn_ack,
            Segment::syn_ack(
                ISS_2,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );
        assert_matches!(passive, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_2,
            irs: ISS_1,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(
                clock.now(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT,
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: None,
            buffer_sizes: Default::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        });
        clock.sleep(RTT / 2);
        let (seg, passive_open) = active
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack,
                clock.now(),
                &counters,
            );
        let ack_seg = seg.expect("failed to generate a ack segment");
        assert_eq!(passive_open, None);
        assert_eq!(ack_seg, Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX)));
        assert_matches!(active, State::Established(ref established) if established == &Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::default(),
                wl1: ISS_2,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::Measured {
                    srtt: RTT,
                    rtt_var: RTT / 2,
                },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,

                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::DEFAULT),
            }.into()
        });
        clock.sleep(RTT / 2);
        assert_eq!(
            passive.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                ack_seg,
                clock.now(),
                &counters,
            ),
            (None, Some(())),
        );
        assert_matches!(passive, State::Established(ref established) if established == &Established {
            snd: Send {
                nxt: ISS_2 + 1,
                max: ISS_2 + 1,
                una: ISS_2 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::default(),
                wl1: ISS_1 + 1,
                wl2: ISS_2 + 1,
                rtt_estimator: Estimator::Measured {
                    srtt: RTT,
                    rtt_var: RTT / 2,
                },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_1 + 1),
                },
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_1 + 1, WindowSize::DEFAULT),
            }.into()
        })
    }

    #[test]
    fn simultaneous_open() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let start = clock.now();
        let (syn_sent1, syn1) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        let (syn_sent2, syn2) = Closed::<Initial>::connect(
            ISS_2,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );

        assert_eq!(
            syn1,
            Segment::syn(
                ISS_1,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );
        assert_eq!(
            syn2,
            Segment::syn(
                ISS_2,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );

        let mut state1 = State::SynSent(syn_sent1);
        let mut state2 = State::SynSent(syn_sent2);

        clock.sleep(RTT);
        let (seg, passive_open) = state1
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn2,
                clock.now(),
                &counters,
            );
        let syn_ack1 = seg.expect("failed to generate syn ack");
        assert_eq!(passive_open, None);
        let (seg, passive_open) = state2
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn1,
                clock.now(),
                &counters,
            );
        let syn_ack2 = seg.expect("failed to generate syn ack");
        assert_eq!(passive_open, None);

        assert_eq!(
            syn_ack1,
            Segment::syn_ack(
                ISS_1,
                ISS_2 + 1,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );
        assert_eq!(
            syn_ack2,
            Segment::syn_ack(
                ISS_2,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(WindowScale::default())
                }
            )
        );

        let elapsed = clock.now() - start;
        assert_matches!(state1, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(
                clock.now(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT - elapsed,
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: Some(()),
            buffer_sizes: BufferSizes::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        });
        assert_matches!(state2, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_2,
            irs: ISS_1,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(
                clock.now(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT - elapsed,
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: Some(()),
            buffer_sizes: BufferSizes::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        });

        clock.sleep(RTT);
        assert_eq!(
            state1.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack2,
                clock.now(),
                &counters,
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX))), None)
        );
        assert_eq!(
            state2.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack1,
                clock.now(),
                &counters,
            ),
            (Some(Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX))), None)
        );

        assert_matches!(state1, State::Established(established) if established == Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::default(),
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::Measured {
                    srtt: RTT,
                    rtt_var: RTT / 2,
                },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::DEFAULT)
            }.into()
        });

        assert_matches!(state2, State::Established(established) if established == Established {
            snd: Send {
                nxt: ISS_2 + 1,
                max: ISS_2 + 1,
                una: ISS_2 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::default(),
                wl1: ISS_1 + 1,
                wl2: ISS_2 + 1,
                rtt_estimator: Estimator::Measured {
                    srtt: RTT,
                    rtt_var: RTT / 2,
                },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_1 + 1),
                },
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_1 + 1, WindowSize::DEFAULT)
            }.into()
        });
    }

    const BUFFER_SIZE: usize = 16;
    const TEST_BYTES: &[u8] = "Hello".as_bytes();

    #[test]
    fn established_receive() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut established = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::ZERO,
                wnd_max: WindowSize::ZERO,
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(Mss(
                    NonZeroU16::new(5).unwrap()
                )),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: Mss(NonZeroU16::new(5).unwrap()),
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });

        // Received an expected segment at rcv.nxt.
        assert_eq!(
            established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0), TEST_BYTES,),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from((BUFFER_SIZE - TEST_BYTES.len()) as u16),
                )),
                None
            ),
        );
        assert_eq!(
            established.read_with(|available| {
                assert_eq!(available, &[TEST_BYTES]);
                available[0].len()
            }),
            TEST_BYTES.len()
        );

        // Receive an out-of-order segment.
        assert_eq!(
            established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len() * 2,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(0),
                    TEST_BYTES,
                ),
                clock.now(),
                &counters
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(u16::try_from(BUFFER_SIZE).unwrap()),
                )),
                None
            ),
        );
        assert_eq!(
            established.read_with(|available| {
                let empty: &[u8] = &[];
                assert_eq!(available, &[empty]);
                0
            }),
            0
        );

        // Receive the next segment that fills the hole.
        assert_eq!(
            established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(0),
                    TEST_BYTES,
                ),
                clock.now(),
                &counters
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + 3 * TEST_BYTES.len(),
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - 2 * TEST_BYTES.len()),
                )),
                None
            ),
        );
        assert_eq!(
            established.read_with(|available| {
                assert_eq!(available, &[[TEST_BYTES, TEST_BYTES].concat()]);
                available[0].len()
            }),
            10
        );
    }

    #[test]
    fn established_send() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        let mut established = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1,
                wnd: WindowSize::ZERO,
                wnd_max: WindowSize::ZERO,
                buffer: send_buffer,
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        // Data queued but the window is not opened, nothing to send.
        assert_eq!(
            established.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            None
        );
        let open_window = |established: &mut State<FakeInstant, RingBuffer, RingBuffer, ()>,
                           ack: SeqNum,
                           win: usize,
                           now: FakeInstant,
                           counters: &TcpCountersInner| {
            assert_eq!(
                established.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                    Segment::ack(ISS_2 + 1, ack, UnscaledWindowSize::from_usize(win)),
                    now,
                    counters
                ),
                (None, None),
            );
        };
        // Open up the window by 1 byte.
        open_window(&mut established, ISS_1 + 1, 1, clock.now(), &counters);
        assert_eq!(
            established.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[1..2]),
            ))
        );

        // Open up the window by 10 bytes, but the MSS is limited to 2 bytes.
        open_window(&mut established, ISS_1 + 2, 10, clock.now(), &counters);
        assert_eq!(
            established.poll_send_with_default_options(2, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 2,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[2..4]),
            ))
        );

        assert_eq!(
            established.poll_send(
                &counters,
                1,
                clock.now(),
                &SocketOptions { nagle_enabled: false, ..SocketOptions::default() }
            ),
            Ok(Segment::data(
                ISS_1 + 4,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[4..5]),
            ))
        );

        // We've exhausted our send buffer.
        assert_eq!(established.poll_send_with_default_options(1, clock.now(), &counters), None);
    }

    #[test]
    fn self_connect_retransmission() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let (syn_sent, syn) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        let mut state = State::<_, RingBuffer, RingBuffer, ()>::SynSent(syn_sent);
        // Retransmission timer should be installed.
        assert_eq!(state.poll_send_at(), Some(FakeInstant::from(Estimator::RTO_INIT)));
        clock.sleep(Estimator::RTO_INIT);
        // The SYN segment should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(syn.into_empty())
        );

        // Bring the state to SYNRCVD.
        let (seg, passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn,
                clock.now(),
                &counters,
            );
        let syn_ack = seg.expect("expected SYN-ACK");
        assert_eq!(passive_open, None);
        // Retransmission timer should be installed.
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));
        clock.sleep(Estimator::RTO_INIT);
        // The SYN-ACK segment should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(syn_ack.into_empty())
        );

        // Bring the state to ESTABLISHED and write some data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack,
                clock.now(),
                &counters,
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX))), None)
        );
        match state {
            State::Closed(_)
            | State::Listen(_)
            | State::SynRcvd(_)
            | State::SynSent(_)
            | State::LastAck(_)
            | State::FinWait1(_)
            | State::FinWait2(_)
            | State::Closing(_)
            | State::TimeWait(_) => {
                panic!("expected that we have entered established state, but got {:?}", state)
            }
            State::Established(Established { ref mut snd, rcv: _ })
            | State::CloseWait(CloseWait {
                ref mut snd,
                last_ack: _,
                last_wnd: _,
                last_wnd_scale: _,
            }) => {
                assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
            }
        }
        // We have no outstanding segments, so there is no retransmission timer.
        assert_eq!(state.poll_send_at(), None);
        // The retransmission timer should backoff exponentially.
        for i in 0..3 {
            assert_eq!(
                state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
                Some(Segment::data(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    FragmentedPayload::new_contiguous(TEST_BYTES),
                ))
            );
            assert_eq!(state.poll_send_at(), Some(clock.now() + (1 << i) * Estimator::RTO_INIT));
            clock.sleep((1 << i) * Estimator::RTO_INIT);
            assert_eq!(counters.retransmits.get(), i);
            assert_eq!(counters.timeouts.get(), i);
        }
        // The receiver acks the first byte of the payload.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + 1,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (None, None),
        );
        // The timer is rearmed, and the current RTO after 2 retransmissions
        // should be 4s (1s, 2s, 4s).
        assert_eq!(state.poll_send_at(), Some(clock.now() + 4 * Estimator::RTO_INIT));
        clock.sleep(4 * Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(1, clock.now(), &counters,),
            Some(Segment::data(
                ISS_1 + 1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&TEST_BYTES[1..2]),
            ))
        );
        // Currently, snd.nxt = ISS_1 + 2, snd.max = ISS_1 + 5, a segment
        // with ack number ISS_1 + 4 should bump snd.nxt immediately.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + 3,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        // Since we retransmitted once more, the RTO is now 8s.
        assert_eq!(counters.retransmits.get(), 3);
        assert_eq!(counters.timeouts.get(), 3);
        assert_eq!(state.poll_send_at(), Some(clock.now() + 8 * Estimator::RTO_INIT));
        assert_eq!(
            state.poll_send_with_default_options(1, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1 + 3,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&TEST_BYTES[3..4]),
            ))
        );
        // Finally the receiver ACKs all the outstanding data.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters
            ),
            (None, None)
        );
        // The retransmission timer should be removed.
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn passive_close() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        let last_wnd = WindowSize::new(BUFFER_SIZE - 1).unwrap();
        let last_wnd_scale = WindowScale::default();
        // Transition the state machine to CloseWait by sending a FIN.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - 1)
                )),
                None
            )
        );
        // Then call CLOSE to transition the state machine to LastAck.
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Ok(NewlyClosed::No)
        );
        assert_eq!(
            state,
            State::LastAck(LastAck {
                snd: Send {
                    nxt: ISS_1 + 1,
                    max: ISS_1 + 1,
                    una: ISS_1 + 1,
                    wnd: WindowSize::DEFAULT,
                    wnd_max: WindowSize::DEFAULT,
                    buffer: send_buffer,
                    wl1: ISS_2 + 1,
                    wl2: ISS_1 + 1,
                    last_seq_ts: None,
                    rtt_estimator: Estimator::default(),
                    timer: None,
                    congestion_control: CongestionControl::cubic_with_mss(
                        DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE
                    ),
                    wnd_scale: WindowScale::default(),
                },
                last_ack: ISS_2 + 2,
                last_wnd,
                last_wnd_scale,
            })
        );
        // When the send window is not big enough, there should be no FIN.
        assert_eq!(
            state.poll_send_with_default_options(2, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 2,
                last_wnd >> WindowScale::default(),
                FragmentedPayload::new_contiguous(&TEST_BYTES[..2]),
            ))
        );
        // We should be able to send out all remaining bytes together with a FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 2,
                last_wnd >> WindowScale::default(),
                FragmentedPayload::new_contiguous(&TEST_BYTES[2..]),
            ))
        );
        // Now let's test we retransmit correctly by only acking the data.
        clock.sleep(RTT);
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + 2,
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));
        clock.sleep(Estimator::RTO_INIT);
        // The FIN should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::fin(
                ISS_1 + 1 + TEST_BYTES.len(),
                ISS_2 + 2,
                last_wnd >> WindowScale::default()
            ))
        );

        // Finally, our FIN is acked.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + 2,
                    ISS_1 + 1 + TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from(u16::MAX),
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        // The connection is closed.
        assert_eq!(state, State::Closed(Closed { reason: None }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 0);
        assert_eq!(counters.established_resets.get(), 0);
    }

    #[test]
    fn syn_rcvd_active_close() {
        let counters = TcpCountersInner::default();
        let mut state: State<_, RingBuffer, NullBuffer, ()> = State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer {
                at: FakeInstant::default(),
                rto: Duration::new(0, 0),
                user_timeout_until: FakeInstant::from(DEFAULT_USER_TIMEOUT),
                remaining_retries: Some(DEFAULT_MAX_RETRIES),
            },
            simultaneous_open: Some(()),
            buffer_sizes: Default::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        });
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Ok(NewlyClosed::No)
        );
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, FakeInstant::default(), &counters),
            Some(Segment::fin(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX)))
        );
    }

    #[test]
    fn established_active_close() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(Mss(
                    NonZeroU16::new(5).unwrap()
                )),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: Mss(NonZeroU16::new(5).unwrap()),
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Ok(NewlyClosed::No)
        );
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Err(CloseError::Closing)
        );

        // Poll for 2 bytes.
        assert_eq!(
            state.poll_send_with_default_options(2, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[..2])
            ))
        );

        // And we should send the rest of the buffer together with the FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[2..])
            ))
        );

        // Test that the recv state works in FIN_WAIT_1.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1,
                    ISS_1 + 1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    TEST_BYTES
                ),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - TEST_BYTES.len()),
                )),
                None
            )
        );

        assert_eq!(
            state.read_with(|avail| {
                let got = avail.concat();
                assert_eq!(got, TEST_BYTES);
                got.len()
            }),
            TEST_BYTES.len()
        );

        // The retrans timer should be installed correctly.
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));

        // Because only the first byte was acked, we need to retransmit.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::piggybacked_fin(
                ISS_1 + 2,
                ISS_2 + TEST_BYTES.len() + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[1..]),
            ))
        );

        // Now our FIN is acked, we should transition to FinWait2.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + TEST_BYTES.len() + 1,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        assert_matches!(state, State::FinWait2(_));

        // Test that the recv state works in FIN_WAIT_2.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len(),
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from(u16::MAX),
                    TEST_BYTES
                ),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + 2 * TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - TEST_BYTES.len()),
                )),
                None
            )
        );

        assert_eq!(
            state.read_with(|avail| {
                let got = avail.concat();
                assert_eq!(got, TEST_BYTES);
                got.len()
            }),
            TEST_BYTES.len()
        );

        // Should ack the FIN and transition to TIME_WAIT.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::fin(
                    ISS_2 + 2 * TEST_BYTES.len() + 1,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + 2 * TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - 1),
                )),
                None
            )
        );

        assert_matches!(state, State::TimeWait(_));

        const SMALLEST_DURATION: Duration = Duration::from_secs(1);
        assert_eq!(state.poll_send_at(), Some(clock.now() + MSL * 2));
        clock.sleep(MSL * 2 - SMALLEST_DURATION);
        // The state should still be in time wait before the time out.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_matches!(state, State::TimeWait(_));
        clock.sleep(SMALLEST_DURATION);
        // The state should become closed.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_eq!(state, State::Closed(Closed { reason: None }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 0);
        assert_eq!(counters.established_resets.get(), 0);
    }

    #[test]
    fn fin_wait_1_fin_ack_to_time_wait() {
        let counters = TcpCountersInner::default();
        // Test that we can transition from FIN-WAIT-2 to TIME-WAIT directly
        // with one FIN-ACK segment.
        let mut state = State::FinWait1(FinWait1 {
            snd: Send {
                nxt: ISS_1 + 2,
                max: ISS_1 + 2,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                FakeInstant::default(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 2,
                    ISS_2 + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - 1)
                )),
                None
            ),
        );
        assert_matches!(state, State::TimeWait(_));
    }

    #[test]
    fn simultaneous_close() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_1 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_1 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Ok(NewlyClosed::No)
        );
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.close(&counters, CloseReason::Shutdown, &SocketOptions::default()),
            Err(CloseError::Closing)
        );

        let fin = state.poll_send_with_default_options(u32::MAX, clock.now(), &counters);
        assert_eq!(
            fin,
            Some(Segment::piggybacked_fin(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            ))
        );
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::piggybacked_fin(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE),
                    TEST_BYTES,
                ),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - TEST_BYTES.len() - 1),
                )),
                None
            )
        );

        // We have a self connection, feeding the FIN packet we generated should
        // make us transition to CLOSING.
        assert_matches!(state, State::Closing(_));
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - TEST_BYTES.len() - 1),
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );

        // And feeding the ACK we produced for FIN should make us transition to
        // TIME-WAIT.
        assert_matches!(state, State::TimeWait(_));

        const SMALLEST_DURATION: Duration = Duration::from_secs(1);
        assert_eq!(state.poll_send_at(), Some(clock.now() + MSL * 2));
        clock.sleep(MSL * 2 - SMALLEST_DURATION);
        // The state should still be in time wait before the time out.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_matches!(state, State::TimeWait(_));
        clock.sleep(SMALLEST_DURATION);
        // The state should become closed.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_eq!(state, State::Closed(Closed { reason: None }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 0);
        assert_eq!(counters.established_resets.get(), 0);
    }

    #[test]
    fn time_wait_restarts_timer() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut time_wait = State::<_, NullBuffer, NullBuffer, ()>::TimeWait(TimeWait {
            last_seq: ISS_1 + 2,
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::DEFAULT,
            last_wnd_scale: WindowScale::default(),
            expiry: new_time_wait_expiry(clock.now()),
        });

        assert_eq!(time_wait.poll_send_at(), Some(clock.now() + MSL * 2));
        clock.sleep(Duration::from_secs(1));
        assert_eq!(
            time_wait.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 2, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
                &counters,
            ),
            (Some(Segment::ack(ISS_1 + 2, ISS_2 + 2, UnscaledWindowSize::from(u16::MAX))), None),
        );
        assert_eq!(time_wait.poll_send_at(), Some(clock.now() + MSL * 2));
    }

    #[test_case(
        State::Established(Established {
            snd: Send {
                nxt: ISS_1,
                max: ISS_1,
                una: ISS_1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: NullBuffer,
                wl1: ISS_2,
                wl2: ISS_1,
                rtt_estimator: Estimator::Measured { srtt: RTT, rtt_var: RTT / 2 },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_2 + 5),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 5, WindowSize::DEFAULT),
            }.into(),
        }),
        Segment::data(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX), TEST_BYTES) =>
        Some(Segment::ack(ISS_1, ISS_2 + 5, UnscaledWindowSize::from(u16::MAX))); "retransmit data"
    )]
    #[test_case(
        State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer {
                at: FakeInstant::default(),
                rto: Duration::new(0, 0),
                user_timeout_until: FakeInstant::from(DEFAULT_USER_TIMEOUT),
                remaining_retries: Some(DEFAULT_MAX_RETRIES),
            },
            simultaneous_open: None,
            buffer_sizes: BufferSizes::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        }),
        Segment::syn_ack(ISS_2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: Some(WindowScale::default()) }) =>
        Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX))); "retransmit syn_ack"
    )]
    // Regression test for https://fxbug.dev/42058963
    fn ack_to_retransmitted_segment(
        mut state: State<FakeInstant, RingBuffer, NullBuffer, ()>,
        seg: Segment<&[u8]>,
    ) -> Option<Segment<()>> {
        let counters = TcpCountersInner::default();
        let (reply, _): (_, Option<()>) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                seg,
                FakeInstant::default(),
                &counters,
            );
        reply
    }

    #[test]
    fn fast_retransmit() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::default();
        for b in b'A'..=b'D' {
            assert_eq!(
                send_buffer.enqueue_data(&[b; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE]),
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE
            );
        }
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1,
                max: ISS_1,
                una: ISS_1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer,
                wl1: ISS_2,
                wl2: ISS_1,
                rtt_estimator: Estimator::Measured { srtt: RTT, rtt_var: RTT / 2 },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_2),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2, WindowSize::DEFAULT),
            }
            .into(),
        });

        assert_eq!(
            state.poll_send_with_default_options(
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now(),
                &counters,
            ),
            Some(Segment::data(
                ISS_1,
                ISS_2,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&[b'A'; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE])
            ))
        );

        let mut dup_ack = |expected_byte: u8, counters: &TcpCountersInner| {
            clock.sleep(Duration::from_millis(10));
            assert_eq!(
                state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                    Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)),
                    clock.now(),
                    counters,
                ),
                (None, None)
            );

            assert_eq!(
                state.poll_send_with_default_options(
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    clock.now(),
                    counters,
                ),
                Some(Segment::data(
                    ISS_1
                        + u32::from(expected_byte - b'A')
                            * u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    ISS_2,
                    UnscaledWindowSize::from(u16::MAX),
                    FragmentedPayload::new_contiguous(
                        &[expected_byte; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE]
                    )
                ))
            );
        };

        // The first two dup acks should allow two previously unsent segments
        // into the network.
        assert_eq!(counters.fast_retransmits.get(), 0);
        assert_eq!(counters.fast_recovery.get(), 0);
        dup_ack(b'B', &counters);
        assert_eq!(counters.fast_retransmits.get(), 0);
        assert_eq!(counters.fast_recovery.get(), 1);
        dup_ack(b'C', &counters);
        assert_eq!(counters.fast_retransmits.get(), 0);
        assert_eq!(counters.fast_recovery.get(), 1);
        // The third dup ack will cause a fast retransmit of the first segment
        // at snd.una.
        dup_ack(b'A', &counters);
        assert_eq!(counters.fast_retransmits.get(), 1);
        assert_eq!(counters.fast_recovery.get(), 1);
        // Afterwards, we continue to send previously unsent data if allowed.
        dup_ack(b'D', &counters);
        assert_eq!(counters.fast_retransmits.get(), 1);
        assert_eq!(counters.fast_recovery.get(), 1);

        // Make sure the window size is deflated after loss is recovered.
        clock.sleep(Duration::from_millis(10));
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2,
                    ISS_1 + u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        let established = assert_matches!(state, State::Established(established) => established);
        assert_eq!(
            u32::from(established.snd.congestion_control.cwnd()),
            2 * u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE)
        );
    }

    #[test]
    fn keep_alive() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1,
                max: ISS_1,
                una: ISS_1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::default(),
                wl1: ISS_2,
                wl2: ISS_1,
                rtt_estimator: Estimator::Measured { srtt: RTT, rtt_var: RTT / 2 },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::default(),
                    assembler: Assembler::new(ISS_2),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2, WindowSize::DEFAULT),
            }
            .into(),
        });

        let socket_options = {
            let mut socket_options = SocketOptions::default();
            socket_options.keep_alive.enabled = true;
            socket_options
        };
        let socket_options = &socket_options;
        let keep_alive = &socket_options.keep_alive;

        // Currently we have nothing to send,
        assert_eq!(
            state.poll_send(
                &counters,
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now(),
                socket_options,
            ),
            Err(NewlyClosed::No),
        );
        // so the above poll_send call will install a timer, which will fire
        // after `keep_alive.idle`.
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(keep_alive.idle.into())));

        // Now we receive an ACK after an hour.
        clock.sleep(Duration::from_secs(60 * 60));
        assert_eq!(
            state.on_segment::<&[u8], ClientlessBufferProvider>(
                &counters,
                Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
                socket_options,
                false, /* defunct */
            ),
            (None, None, DataAcked::No, NewlyClosed::No)
        );
        // the timer is reset to fire in 2 hours.
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(keep_alive.idle.into())),);
        clock.sleep(keep_alive.idle.into());

        // Then there should be `count` probes being sent out after `count`
        // `interval` seconds.
        for _ in 0..keep_alive.count.get() {
            assert_eq!(
                state.poll_send(
                    &counters,
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    clock.now(),
                    socket_options,
                ),
                Ok(Segment::ack(ISS_1 - 1, ISS_2, UnscaledWindowSize::from(u16::MAX)))
            );
            clock.sleep(keep_alive.interval.into());
            assert_matches!(state, State::Established(_));
        }

        // At this time the connection is closed and we don't have anything to
        // send.
        assert_eq!(
            state.poll_send(
                &counters,
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now(),
                socket_options,
            ),
            Err(NewlyClosed::Yes),
        );
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 1);
        assert_eq!(counters.established_resets.get(), 0);
    }

    /// A `SendBuffer` that doesn't allow peeking some number of bytes.
    #[derive(Debug)]
    struct ReservingBuffer<B> {
        buffer: B,
        reserved_bytes: usize,
    }

    impl<B: Buffer> Buffer for ReservingBuffer<B> {
        fn capacity_range() -> (usize, usize) {
            B::capacity_range()
        }

        fn limits(&self) -> BufferLimits {
            self.buffer.limits()
        }

        fn target_capacity(&self) -> usize {
            self.buffer.target_capacity()
        }

        fn request_capacity(&mut self, size: usize) {
            self.buffer.request_capacity(size)
        }
    }

    impl<B: SendBuffer> SendBuffer for ReservingBuffer<B> {
        type Payload<'a> = B::Payload<'a>;

        fn mark_read(&mut self, count: usize) {
            self.buffer.mark_read(count)
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(B::Payload<'a>) -> R,
        {
            let Self { buffer, reserved_bytes } = self;
            buffer.peek_with(offset, |payload| {
                let len = payload.len();
                let new_len = len.saturating_sub(*reserved_bytes);
                f(payload.slice(0..new_len.try_into().unwrap_or(u32::MAX)))
            })
        }
    }

    #[test_case(true, 0)]
    #[test_case(false, 0)]
    #[test_case(true, 1)]
    #[test_case(false, 1)]
    fn poll_send_len(has_fin: bool, reserved_bytes: usize) {
        const VALUE: u8 = 0xaa;

        fn with_poll_send_result<const HAS_FIN: bool>(
            f: impl FnOnce(Segment<FragmentedPayload<'_, 2>>),
            reserved_bytes: usize,
        ) {
            const DATA_LEN: usize = 40;
            let buffer = ReservingBuffer {
                buffer: RingBuffer::with_data(DATA_LEN, &vec![VALUE; DATA_LEN]),
                reserved_bytes,
            };
            assert_eq!(buffer.limits().len, DATA_LEN);

            let mut snd = Send::<FakeInstant, _, HAS_FIN> {
                nxt: ISS_1,
                max: ISS_1,
                una: ISS_1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer,
                wl1: ISS_2,
                wl2: ISS_1,
                rtt_estimator: Estimator::Measured { srtt: RTT, rtt_var: RTT / 2 },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            };

            let counters = TcpCountersInner::default();

            f(snd
                .poll_send(
                    &counters,
                    ISS_1,
                    WindowSize::DEFAULT >> WindowScale::ZERO,
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    FakeInstant::default(),
                    &SocketOptions::default(),
                )
                .expect("has data"))
        }

        let f = |segment: Segment<FragmentedPayload<'_, 2>>| {
            let segment_len = segment.len();
            let Segment { header: _, data } = segment;
            let data_len = data.len();

            if has_fin && reserved_bytes == 0 {
                assert_eq!(
                    segment_len,
                    u32::try_from(data_len + 1).unwrap(),
                    "FIN not accounted for"
                );
            } else {
                assert_eq!(segment_len, u32::try_from(data_len).unwrap());
            }

            let mut target = vec![0; data_len];
            data.partial_copy(0, target.as_mut_slice());
            assert_eq!(target, vec![VALUE; data_len]);
        };
        match has_fin {
            true => with_poll_send_result::<true>(f, reserved_bytes),
            false => with_poll_send_result::<false>(f, reserved_bytes),
        }
    }

    #[test]
    fn zero_window_probe() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::ZERO,
                wnd_max: WindowSize::ZERO,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(Estimator::RTO_INIT)));

        // Send the first probe after first RTO.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver still has a zero window.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0)),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        // The timer should backoff exponentially.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(Estimator::RTO_INIT * 2)));

        // No probe should be sent before the timeout.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);

        // Probe sent after the timeout.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver now opens its receive window.
        assert_eq!(
            state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
                &counters,
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), None);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 2,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[1..])
            ))
        );
    }

    #[test]
    fn nagle() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::default(),
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });
        let mut socket_options = SocketOptions::default();
        assert_eq!(
            state.poll_send(&counters, 3, clock.now(), &socket_options),
            Ok(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[0..3])
            ))
        );
        assert_eq!(
            state.poll_send(&counters, 3, clock.now(), &socket_options),
            Err(NewlyClosed::No)
        );
        socket_options.nagle_enabled = false;
        assert_eq!(
            state.poll_send(&counters, 3, clock.now(), &socket_options),
            Ok(Segment::data(
                ISS_1 + 4,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                FragmentedPayload::new_contiguous(&TEST_BYTES[3..5])
            ))
        );
    }

    #[test]
    fn mss_option() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let (syn_sent, syn) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            Mss(NonZeroU16::new(1).unwrap()),
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        let mut state = State::<_, RingBuffer, RingBuffer, ()>::SynSent(syn_sent);

        // Bring the state to SYNRCVD.
        let (seg, passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn,
                clock.now(),
                &counters,
            );
        let syn_ack = seg.expect("expected SYN-ACK");
        assert_eq!(passive_open, None);

        // Bring the state to ESTABLISHED and write some data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack,
                clock.now(),
                &counters,
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX))), None)
        );
        match state {
            State::Closed(_)
            | State::Listen(_)
            | State::SynRcvd(_)
            | State::SynSent(_)
            | State::LastAck(_)
            | State::FinWait1(_)
            | State::FinWait2(_)
            | State::Closing(_)
            | State::TimeWait(_) => {
                panic!("expected that we have entered established state, but got {:?}", state)
            }
            State::Established(Established { ref mut snd, rcv: _ })
            | State::CloseWait(CloseWait {
                ref mut snd,
                last_ack: _,
                last_wnd: _,
                last_wnd_scale: _,
            }) => {
                assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
            }
        }
        // Since the MSS of the connection is 1, we can only get the first byte.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&TEST_BYTES[..1]),
            ))
        );
    }

    const TEST_USER_TIMEOUT: NonZeroDuration = NonZeroDuration::from_secs(2 * 60).unwrap();

    // We can use a smaller and a larger RTT so that when using the smaller one,
    // we can reach maximum retransmit retires and when using the larger one, we
    // timeout before reaching maximum retries.
    #[test_case(Duration::from_millis(1), false, true; "retrans_max_retries")]
    #[test_case(Duration::from_secs(1), false, false; "retrans_no_max_retries")]
    #[test_case(Duration::from_millis(1), true, true; "zwp_max_retries")]
    #[test_case(Duration::from_secs(1), true, false; "zwp_no_max_retires")]
    fn user_timeout(rtt: Duration, zero_window_probe: bool, max_retries: bool) {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                last_seq_ts: None,
                rtt_estimator: Estimator::Measured { srtt: rtt, rtt_var: Duration::ZERO },
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::DEFAULT),
            }
            .into(),
        });
        let mut times = 1;
        let start = clock.now();
        while let Ok(seg) = state.poll_send(
            &counters,
            u32::MAX,
            clock.now(),
            &SocketOptions { user_timeout: Some(TEST_USER_TIMEOUT), ..SocketOptions::default() },
        ) {
            if zero_window_probe {
                let zero_window_ack = Segment::ack(
                    seg.header.ack.unwrap(),
                    seg.header.seq,
                    UnscaledWindowSize::from(0),
                );
                assert_matches!(
                    state.on_segment::<(), ClientlessBufferProvider>(
                        &counters,
                        zero_window_ack,
                        clock.now(),
                        &SocketOptions {
                            user_timeout: Some(TEST_USER_TIMEOUT),
                            ..SocketOptions::default()
                        },
                        false,
                    ),
                    (None, None, DataAcked::No, _newly_closed)
                );

                // In non-test code, calling poll_send is done automatically in
                // try_handle_incoming_for_connection.
                //
                // This is when the ZWP timer gets set.
                assert_matches!(
                    state.poll_send(
                        &counters,
                        u32::MAX,
                        clock.now(),
                        &SocketOptions {
                            user_timeout: Some(TEST_USER_TIMEOUT),
                            ..SocketOptions::default()
                        },
                    ),
                    Err(NewlyClosed::No)
                );
                let inner_state = assert_matches!(state, State::Established(ref e) => e);
                assert_matches!(inner_state.snd.timer, Some(SendTimer::ZeroWindowProbe(_)));
            }

            let deadline = state.poll_send_at().expect("must have a retransmission timer");
            clock.sleep(deadline.checked_duration_since(clock.now()).unwrap());
            times += 1;
        }
        let elapsed = clock.now().checked_duration_since(start).unwrap();
        assert_eq!(elapsed, TEST_USER_TIMEOUT.get());
        if max_retries {
            assert_eq!(times, DEFAULT_MAX_RETRIES.get());
        } else {
            assert!(times < DEFAULT_MAX_RETRIES.get());
        }
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 1);
        assert_eq!(counters.established_resets.get(), 0);
    }

    #[test]
    fn retrans_timer_backoff() {
        let mut clock = FakeInstantCtx::default();
        let mut timer = RetransTimer::new(
            clock.now(),
            Duration::from_secs(1),
            DEFAULT_USER_TIMEOUT,
            DEFAULT_MAX_RETRIES,
        );
        clock.sleep(DEFAULT_USER_TIMEOUT);
        timer.backoff(clock.now());
        assert_eq!(timer.at, FakeInstant::from(DEFAULT_USER_TIMEOUT));
        clock.sleep(Duration::from_secs(1));
        // The current time is now later than the timeout deadline,
        timer.backoff(clock.now());
        // The timer should adjust its expiration time to be the current time
        // instead of panicking.
        assert_eq!(timer.at, clock.now());
    }

    #[test_case(
        State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::new(BUFFER_SIZE),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::Measured {
                    srtt: Estimator::RTO_INIT,
                    rtt_var: Duration::ZERO,
                },
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(
                        TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                    ),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (
                    ISS_2 + 1,
                    WindowSize::new(TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize).unwrap()
                ),
            }.into(),
        }); "established")]
    #[test_case(
        State::FinWait1(FinWait1 {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::new(BUFFER_SIZE),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::Measured {
                    srtt: Estimator::RTO_INIT,
                    rtt_var: Duration::ZERO,
                },
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }.into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(
                        TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                    ),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (
                    ISS_2 + 1,
                    WindowSize::new(TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize).unwrap()
                ),
            }.into(),
        }); "fin_wait_1")]
    #[test_case(
        State::FinWait2(FinWait2 {
            last_seq: ISS_1 + 1,
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(
                        TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                    ),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (
                    ISS_2 + 1,
                    WindowSize::new(TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize).unwrap()
                ),
            },
            timeout_at: None,
        }); "fin_wait_2")]
    fn delayed_ack(mut state: State<FakeInstant, RingBuffer, RingBuffer, ()>) {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let socket_options = SocketOptions { delayed_ack: true, ..SocketOptions::default() };
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                &counters,
                Segment::data(
                    ISS_2 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    TEST_BYTES,
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (None, None, DataAcked::No, NewlyClosed::No)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(ACK_DELAY_THRESHOLD)));
        clock.sleep(ACK_DELAY_THRESHOLD);
        assert_eq!(
            state.poll_send(&counters, u32::MAX, clock.now(), &socket_options),
            Ok(Segment::ack(
                ISS_1 + 1,
                ISS_2 + 1 + TEST_BYTES.len(),
                UnscaledWindowSize::from_u32(2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE)),
            ))
        );
        let full_segment_sized_payload =
            vec![b'0'; u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize];
        // The first full sized segment should not trigger an immediate ACK,
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                &counters,
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    &full_segment_sized_payload[..],
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (None, None, DataAcked::No, NewlyClosed::No)
        );
        // ... but just a timer.
        assert_eq!(state.poll_send_at(), Some(clock.now().panicking_add(ACK_DELAY_THRESHOLD)));
        // Now the second full sized segment arrives, an ACK should be sent
        // immediately.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                &counters,
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len() + full_segment_sized_payload.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    &full_segment_sized_payload[..],
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    UnscaledWindowSize::from(0),
                )),
                None,
                DataAcked::No,
                NewlyClosed::No,
            )
        );
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn immediate_ack_if_out_of_order_or_fin() {
        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let socket_options = SocketOptions { delayed_ack: true, ..SocketOptions::default() };
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer: RingBuffer::new(BUFFER_SIZE),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::Measured {
                    srtt: Estimator::RTO_INIT,
                    rtt_var: Duration::ZERO,
                },
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(TEST_BYTES.len() + 1),
                    assembler: Assembler::new(ISS_2 + 1),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2 + 1, WindowSize::new(TEST_BYTES.len() + 1).unwrap()),
            }
            .into(),
        });
        // Upon receiving an out-of-order segment, we should send an ACK
        // immediately.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                &counters,
                Segment::data(
                    ISS_2 + 2,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    &TEST_BYTES[1..]
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1,
                    UnscaledWindowSize::from(u16::try_from(TEST_BYTES.len() + 1).unwrap())
                )),
                None,
                DataAcked::No,
                NewlyClosed::No,
            )
        );
        assert_eq!(state.poll_send_at(), None);
        // The next segment fills a gap, so it should trigger an immediate
        // ACK.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                &counters,
                Segment::data(
                    ISS_2 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    &TEST_BYTES[..1]
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(1),
                )),
                None,
                DataAcked::No,
                NewlyClosed::No
            )
        );
        assert_eq!(state.poll_send_at(), None);
        // We should also respond immediately with an ACK to a FIN.
        assert_eq!(
            state.on_segment::<(), ClientlessBufferProvider>(
                &counters,
                Segment::fin(
                    ISS_2 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from(0),
                )),
                None,
                DataAcked::No,
                NewlyClosed::No,
            )
        );
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn fin_wait2_timeout() {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut state: State<_, _, NullBuffer, ()> = State::FinWait2(FinWait2 {
            last_seq: ISS_1,
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: NullBuffer,
                    assembler: Assembler::new(ISS_2),
                },
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_2, WindowSize::DEFAULT),
            },
            timeout_at: None,
        });
        assert_eq!(
            state.close(
                &counters,
                CloseReason::Close { now: clock.now() },
                &SocketOptions::default()
            ),
            Err(CloseError::Closing)
        );
        assert_eq!(
            state.poll_send_at(),
            Some(clock.now().panicking_add(DEFAULT_FIN_WAIT2_TIMEOUT))
        );
        clock.sleep(DEFAULT_FIN_WAIT2_TIMEOUT);
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now(), &counters), None);
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
        assert_eq!(counters.established_closed.get(), 1);
        assert_eq!(counters.established_timedout.get(), 1);
        assert_eq!(counters.established_resets.get(), 0);
    }

    #[test_case(RetransTimer {
        user_timeout_until: FakeInstant::from(Duration::from_secs(100)),
        remaining_retries: None,
        at: FakeInstant::from(Duration::from_secs(1)),
        rto: Duration::from_secs(1),
    }, FakeInstant::from(Duration::from_secs(1)) => true)]
    #[test_case(RetransTimer {
        user_timeout_until: FakeInstant::from(Duration::from_secs(100)),
        remaining_retries: None,
        at: FakeInstant::from(Duration::from_secs(2)),
        rto: Duration::from_secs(1),
    }, FakeInstant::from(Duration::from_secs(1)) => false)]
    #[test_case(RetransTimer {
        user_timeout_until: FakeInstant::from(Duration::from_secs(100)),
        remaining_retries: Some(NonZeroU8::new(1).unwrap()),
        at: FakeInstant::from(Duration::from_secs(2)),
        rto: Duration::from_secs(1),
    }, FakeInstant::from(Duration::from_secs(1)) => false)]
    #[test_case(RetransTimer {
        user_timeout_until: FakeInstant::from(Duration::from_secs(1)),
        remaining_retries: Some(NonZeroU8::new(1).unwrap()),
        at: FakeInstant::from(Duration::from_secs(1)),
        rto: Duration::from_secs(1),
    }, FakeInstant::from(Duration::from_secs(1)) => true)]
    fn send_timed_out(timer: RetransTimer<FakeInstant>, now: FakeInstant) -> bool {
        timer.timed_out(now)
    }

    #[test_case(
        State::SynSent(SynSent{
            iss: ISS_1,
            timestamp: Some(FakeInstant::default()),
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Duration::from_millis(1),
                Duration::from_secs(60),
                DEFAULT_MAX_SYN_RETRIES,
            ),
            active_open: (),
            buffer_sizes: Default::default(),
            device_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
        })
    => DEFAULT_MAX_SYN_RETRIES.get(); "syn_sent")]
    #[test_case(
        State::SynRcvd(SynRcvd{
            iss: ISS_1,
            irs: ISS_2,
            timestamp: Some(FakeInstant::default()),
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Duration::from_millis(1),
                Duration::from_secs(60),
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: None,
            buffer_sizes: BufferSizes::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        })
    => DEFAULT_MAX_SYNACK_RETRIES.get(); "syn_rcvd")]
    fn handshake_timeout(mut state: State<FakeInstant, RingBuffer, RingBuffer, ()>) -> u8 {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let mut retransmissions = 0;
        clock.sleep(Duration::from_millis(1));
        while let Some(_seg) =
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters)
        {
            let deadline = state.poll_send_at().expect("must have a retransmission timer");
            clock.sleep(deadline.checked_duration_since(clock.now()).unwrap());
            retransmissions += 1;
        }
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
        assert_eq!(counters.established_closed.get(), 0);
        assert_eq!(counters.established_timedout.get(), 0);
        assert_eq!(counters.established_resets.get(), 0);
        retransmissions
    }

    #[test_case(
        u16::MAX as usize, WindowScale::default(), Some(WindowScale::default())
    => (WindowScale::default(), WindowScale::default()))]
    #[test_case(
        u16::MAX as usize + 1, WindowScale::new(1).unwrap(), Some(WindowScale::default())
    => (WindowScale::new(1).unwrap(), WindowScale::default()))]
    #[test_case(
        u16::MAX as usize + 1, WindowScale::new(1).unwrap(), None
    => (WindowScale::default(), WindowScale::default()))]
    #[test_case(
        u16::MAX as usize, WindowScale::default(), Some(WindowScale::new(1).unwrap())
    => (WindowScale::default(), WindowScale::new(1).unwrap()))]
    fn window_scale(
        buffer_size: usize,
        syn_window_scale: WindowScale,
        syn_ack_window_scale: Option<WindowScale>,
    ) -> (WindowScale, WindowScale) {
        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        let (syn_sent, syn_seg) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            BufferSizes { receive: buffer_size, ..Default::default() },
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        assert_eq!(
            syn_seg,
            Segment::syn(
                ISS_1,
                UnscaledWindowSize::from(u16::try_from(buffer_size).unwrap_or(u16::MAX)),
                Options {
                    mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE),
                    window_scale: Some(syn_window_scale)
                },
            )
        );
        let mut active = State::SynSent(syn_sent);
        clock.sleep(RTT / 2);
        let (seg, passive_open) = active
            .on_segment_with_default_options::<(), ClientlessBufferProvider>(
                Segment::syn_ack(
                    ISS_2,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    Options {
                        mss: Some(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                        window_scale: syn_ack_window_scale,
                    },
                ),
                clock.now(),
                &counters,
            );
        assert_eq!(passive_open, None);
        assert_matches!(seg, Some(_));

        let established: Established<FakeInstant, RingBuffer, NullBuffer> =
            assert_matches!(active, State::Established(established) => established);

        assert_eq!(established.snd.wnd, WindowSize::DEFAULT);

        (established.rcv.wnd_scale, established.snd.wnd_scale)
    }

    #[test_case(
        u16::MAX as usize,
        Segment::syn_ack(
            ISS_2 + 1 + u16::MAX as usize,
            ISS_1 + 1,
            UnscaledWindowSize::from(u16::MAX),
            Options::default(),
        )
    )]
    #[test_case(
        u16::MAX as usize + 1,
        Segment::syn_ack(
            ISS_2 + 1 + u16::MAX as usize,
            ISS_1 + 1,
            UnscaledWindowSize::from(u16::MAX),
            Options::default(),
        )
    )]
    #[test_case(
        u16::MAX as usize,
        Segment::data(
            ISS_2 + 1 + u16::MAX as usize,
            ISS_1 + 1,
            UnscaledWindowSize::from(u16::MAX),
            &TEST_BYTES[..],
        )
    )]
    #[test_case(
        u16::MAX as usize + 1,
        Segment::data(
            ISS_2 + 1 + u16::MAX as usize,
            ISS_1 + 1,
            UnscaledWindowSize::from(u16::MAX),
            &TEST_BYTES[..],
        )
    )]
    fn window_scale_otw_seq(receive_buf_size: usize, otw_seg: impl Into<Segment<&'static [u8]>>) {
        let counters = TcpCountersInner::default();
        let buffer_sizes = BufferSizes { send: 0, receive: receive_buf_size };
        let rcv_wnd_scale = buffer_sizes.rwnd().scale();
        let mut syn_rcvd: State<_, RingBuffer, RingBuffer, ()> = State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Estimator::RTO_INIT,
                Duration::from_secs(10),
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: None,
            buffer_sizes: BufferSizes { send: 0, receive: receive_buf_size },
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale,
            snd_wnd_scale: WindowScale::new(1),
        });

        assert_eq!(
            syn_rcvd.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                otw_seg.into(),
                FakeInstant::default(),
                &counters
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, buffer_sizes.rwnd_unscaled())), None),
        )
    }

    #[test]
    fn poll_send_reserving_buffer() {
        const RESERVED_BYTES: usize = 3;
        let mut snd: Send<FakeInstant, _, false> = Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::DEFAULT,
            wnd_max: WindowSize::DEFAULT,
            wnd_scale: WindowScale::default(),
            wl1: ISS_1,
            wl2: ISS_2,
            buffer: ReservingBuffer {
                buffer: RingBuffer::with_data(TEST_BYTES.len(), TEST_BYTES),
                reserved_bytes: RESERVED_BYTES,
            },
            last_seq_ts: None,
            rtt_estimator: Estimator::default(),
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            ),
        };

        let counters = TcpCountersInner::default();

        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                FakeInstant::default(),
                &SocketOptions::default(),
            ),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::default(),
                FragmentedPayload::new_contiguous(&TEST_BYTES[..TEST_BYTES.len() - RESERVED_BYTES])
            ))
        );

        assert_eq!(snd.nxt, ISS_1 + 1 + (TEST_BYTES.len() - RESERVED_BYTES));
    }

    #[test]
    fn rcv_silly_window_avoidance() {
        const MULTIPLE: usize = 3;
        const CAP: usize = TEST_BYTES.len() * MULTIPLE;
        let mut rcv: Recv<FakeInstant, RingBuffer> = Recv {
            buffer: RecvBufferState::Open {
                buffer: RingBuffer::new(CAP),
                assembler: Assembler::new(ISS_1),
            },
            timer: None,
            mss: Mss(NonZeroU16::new(TEST_BYTES.len() as u16).unwrap()),
            wnd_scale: WindowScale::default(),
            last_window_update: (ISS_1, WindowSize::new(CAP).unwrap()),
        };

        fn get_buffer(rcv: &mut Recv<FakeInstant, RingBuffer>) -> &mut RingBuffer {
            assert_matches!(
                &mut rcv.buffer,
                RecvBufferState::Open {ref mut buffer, .. } => buffer
            )
        }

        // Initially the entire buffer is advertised.
        assert_eq!(rcv.select_window(), WindowSize::new(CAP).unwrap());

        for _ in 0..MULTIPLE {
            assert_eq!(get_buffer(&mut rcv).enqueue_data(TEST_BYTES), TEST_BYTES.len());
        }
        let assembler = assert_matches!(&mut rcv.buffer,
            RecvBufferState::Open { ref mut assembler, .. } => assembler);
        assert_eq!(assembler.insert(ISS_1..ISS_1 + CAP), CAP);
        // Since the buffer is full, we now get a zero window.
        assert_eq!(rcv.select_window(), WindowSize::ZERO);

        // The user reads 1 byte, but our implementation should not advertise
        // a new window because it is too small;
        assert_eq!(get_buffer(&mut rcv).read_with(|_| 1), 1);
        assert_eq!(rcv.select_window(), WindowSize::ZERO);

        // Now at least there is at least 1 MSS worth of free space in the
        // buffer, advertise it.
        assert_eq!(get_buffer(&mut rcv).read_with(|_| TEST_BYTES.len()), TEST_BYTES.len());
        assert_eq!(rcv.select_window(), WindowSize::new(TEST_BYTES.len() + 1).unwrap());
    }

    #[test]
    // Regression test for https://fxbug.dev/376061162.
    fn correct_window_scale_during_send() {
        let snd_wnd_scale = WindowScale::new(4).unwrap();
        let rcv_wnd_scale = WindowScale::new(8).unwrap();
        let wnd_size = WindowSize::new(1024).unwrap();

        let counters = TcpCountersInner::default();
        // Tests the common behavior when ack segment arrives in states that
        // have a send state.
        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: wnd_size,
            wnd_max: WindowSize::DEFAULT,
            buffer: RingBuffer::with_data(TEST_BYTES.len(), TEST_BYTES),
            wl1: ISS_2 + 1,
            wl2: ISS_1 + 1,
            rtt_estimator: Estimator::default(),
            last_seq_ts: None,
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            ),
            wnd_scale: snd_wnd_scale,
        };
        let new_rcv = || Recv {
            buffer: RecvBufferState::Open {
                buffer: RingBuffer::new(wnd_size.into()),
                assembler: Assembler::new(ISS_2 + 1),
            },
            timer: None,
            mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            wnd_scale: rcv_wnd_scale,
            last_window_update: (ISS_2 + 1, WindowSize::ZERO),
        };
        for mut state in [
            State::Established(Established { snd: new_snd().into(), rcv: new_rcv().into() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin().into(), rcv: new_rcv().into() }),
            State::Closing(Closing {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: wnd_size,
                last_wnd_scale: rcv_wnd_scale,
            }),
            State::CloseWait(CloseWait {
                snd: new_snd().into(),
                last_ack: ISS_2 + 1,
                last_wnd: wnd_size,
                last_wnd_scale: rcv_wnd_scale,
            }),
            State::LastAck(LastAck {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: wnd_size,
                last_wnd_scale: rcv_wnd_scale,
            }),
        ] {
            assert_eq!(
                state.poll_send_with_default_options(
                    u32::try_from(TEST_BYTES.len()).unwrap(),
                    FakeInstant::default(),
                    &counters,
                ),
                Some(Segment::data(
                    ISS_1 + 1,
                    ISS_2 + 1,
                    // We expect this to be wnd_size >> rcv_wnd_scale, which
                    // equals 1024 >> 8 == 4
                    UnscaledWindowSize::from(4),
                    FragmentedPayload::new_contiguous(TEST_BYTES)
                ))
            );
        }
    }

    #[test_case(true; "prompted window update")]
    #[test_case(false; "unprompted window update")]
    fn snd_silly_window_avoidance(prompted_window_update: bool) {
        const CAP: usize = TEST_BYTES.len() * 2;
        let mut snd: Send<FakeInstant, RingBuffer, false> = Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::new(CAP).unwrap(),
            wnd_scale: WindowScale::default(),
            wnd_max: WindowSize::DEFAULT,
            wl1: ISS_1,
            wl2: ISS_2,
            buffer: RingBuffer::new(CAP),
            last_seq_ts: None,
            rtt_estimator: Estimator::default(),
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(Mss(NonZeroU16::new(
                TEST_BYTES.len() as u16,
            )
            .unwrap())),
        };

        let mut clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();

        // We enqueue two copies of TEST_BYTES.
        assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
        assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());

        // The first copy should be sent out since the receiver has the space.
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            )),
        );

        assert_eq!(
            snd.process_ack(
                &counters,
                ISS_2 + 1,
                ISS_1 + 1 + TEST_BYTES.len(),
                UnscaledWindowSize::from(0),
                true,
                ISS_2 + 1,
                WindowSize::DEFAULT,
                clock.now(),
                &KeepAlive::default(),
            ),
            (None, DataAcked::Yes)
        );

        // Now that we received a zero window, we should start probing for the
        // window reopening.
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            None
        );

        assert_eq!(
            snd.timer,
            Some(SendTimer::ZeroWindowProbe(RetransTimer::new(
                clock.now(),
                snd.rtt_estimator.rto(),
                DEFAULT_USER_TIMEOUT,
                DEFAULT_MAX_RETRIES,
            )))
        );

        clock.sleep(Duration::from_millis(100));

        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            Some(Segment::data(
                ISS_1 + 1 + TEST_BYTES.len(),
                ISS_2 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&TEST_BYTES[..1]),
            ))
        );

        if prompted_window_update {
            // Now the receiver sends back a window update, but not enough for a
            // full MSS.
            assert_eq!(
                snd.process_ack(
                    &counters,
                    ISS_2 + 1,
                    ISS_1 + 1 + TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from(3),
                    true,
                    ISS_2 + 1,
                    WindowSize::DEFAULT,
                    clock.now(),
                    &KeepAlive::default(),
                ),
                (None, DataAcked::Yes)
            );
        } else {
            // First probe sees the same empty window.
            assert_eq!(
                snd.process_ack(
                    &counters,
                    ISS_2 + 1,
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(0),
                    true,
                    ISS_2 + 1,
                    WindowSize::DEFAULT,
                    clock.now(),
                    &KeepAlive::default(),
                ),
                (None, DataAcked::No)
            );

            // The receiver freed up buffer space and decided to send a sub-MSS
            // window update (likely a bug).
            assert_eq!(
                snd.process_ack(
                    &counters,
                    ISS_2 + 1,
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(3),
                    true,
                    ISS_2 + 1,
                    WindowSize::DEFAULT,
                    clock.now(),
                    &KeepAlive::default(),
                ),
                (None, DataAcked::No)
            );
        }

        // We would then transition into SWS avoidance.
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            None,
        );
        assert_eq!(
            snd.timer,
            Some(SendTimer::SWSProbe { at: clock.now().panicking_add(SWS_PROBE_TIMEOUT) })
        );
        clock.sleep(SWS_PROBE_TIMEOUT);

        // After the overriding timeout, we should push out whatever the
        // receiver is willing to receive.
        let seq_index = usize::from(prompted_window_update);
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            Some(Segment::data(
                ISS_1 + 1 + TEST_BYTES.len() + seq_index,
                ISS_2 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(&TEST_BYTES[seq_index..3 + seq_index]),
            ))
        );
    }

    #[test]
    fn snd_enter_zwp_on_negative_window_update() {
        const CAP: usize = TEST_BYTES.len() * 2;
        let mut snd: Send<FakeInstant, RingBuffer, false> = Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::new(CAP).unwrap(),
            wnd_scale: WindowScale::default(),
            wnd_max: WindowSize::DEFAULT,
            wl1: ISS_1,
            wl2: ISS_2,
            buffer: RingBuffer::new(CAP),
            last_seq_ts: None,
            rtt_estimator: Estimator::default(),
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(Mss(NonZeroU16::new(
                TEST_BYTES.len() as u16,
            )
            .unwrap())),
        };

        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();

        // We enqueue two copies of TEST_BYTES.
        assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
        assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());

        // The first copy should be sent out since the receiver has the space.
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from(u16::MAX),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            )),
        );

        // We've sent some data, but haven't received an ACK, so the retransmit
        // timer should be set.
        assert_matches!(snd.timer, Some(SendTimer::Retrans(_)));

        // Send an ACK segment that doesn't ACK any data, but does close the
        // window. This is strongly discouraged, but technically allowed by RFC
        // 9293:
        //   A TCP receiver SHOULD NOT shrink the window, i.e., move the right
        //   window edge to the left (SHLD-14). However, a sending TCP peer MUST
        //   be robust against window shrinking, which may cause the "usable
        //   window" (see Section 3.8.6.2.1) to become negative (MUST-34).
        assert_eq!(
            snd.process_ack(
                &counters,
                ISS_2 + 1,
                ISS_1 + 1 + TEST_BYTES.len(),
                UnscaledWindowSize::from(0),
                true,
                ISS_2 + 1,
                WindowSize::DEFAULT,
                clock.now(),
                &KeepAlive::default(),
            ),
            (None, DataAcked::Yes)
        );

        // Trying to send more data should result in no segment being sent, but
        // instead setting up the ZWP timr.
        assert_eq!(
            snd.poll_send(
                &counters,
                ISS_2 + 1,
                WindowSize::DEFAULT >> WindowScale::ZERO,
                u32::MAX,
                clock.now(),
                &SocketOptions::default(),
            ),
            None,
        );
        assert_matches!(snd.timer, Some(SendTimer::ZeroWindowProbe(_)));
    }

    #[test]
    // Regression test for https://fxbug.dev/334926865.
    fn ack_uses_snd_max() {
        let counters = TcpCountersInner::default();
        let mss = Mss(NonZeroU16::new(u16::try_from(TEST_BYTES.len()).unwrap()).unwrap());

        let mut clock = FakeInstantCtx::default();
        let mut buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
        assert_eq!(buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());

        // This connection has the same send and receive seqnum space.
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                wnd_max: WindowSize::DEFAULT,
                buffer,
                wl1: ISS_1,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::NoSample,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(mss),
                wnd_scale: WindowScale::default(),
            }
            .into(),
            rcv: Recv {
                buffer: RecvBufferState::Open {
                    buffer: RingBuffer::new(BUFFER_SIZE),
                    assembler: Assembler::new(ISS_1 + 1),
                },
                timer: None,
                mss,
                wnd_scale: WindowScale::default(),
                last_window_update: (ISS_1 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
            }
            .into(),
        });

        // Send the first two data segments.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::try_from(BUFFER_SIZE).unwrap()),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            )),
        );
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1 + TEST_BYTES.len(),
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::try_from(BUFFER_SIZE).unwrap()),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            )),
        );

        // Retransmit, now snd.nxt = TEST.BYTES.len() + 1.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now(), &counters),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::try_from(BUFFER_SIZE).unwrap()),
                FragmentedPayload::new_contiguous(TEST_BYTES),
            )),
        );

        // the ACK sent should have seq = snd.max (2 * TEST_BYTES.len() + 1) to
        // avoid getting stuck in an ACK cycle.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::try_from(BUFFER_SIZE).unwrap()),
                    TEST_BYTES,
                ),
                clock.now(),
                &counters,
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1 + 2 * TEST_BYTES.len(),
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(
                        u16::try_from(BUFFER_SIZE - TEST_BYTES.len()).unwrap()
                    ),
                )),
                None,
            )
        );
    }

    #[test_case(
        State::Closed(Closed { reason: None }),
        State::Closed(Closed { reason: None }) => NewlyClosed::No; "closed to closed")]
    #[test_case(
        State::SynSent(SynSent {
            iss: ISS_1,
            timestamp: Some(FakeInstant::default()),
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT,
                DEFAULT_MAX_SYN_RETRIES,
            ),
            active_open: (),
            buffer_sizes: BufferSizes::default(),
            default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
        }),
        State::Closed(Closed { reason: None }) => NewlyClosed::Yes; "non-closed to closed")]
    #[test_case(
        State::SynSent(SynSent {
                iss: ISS_1,
                timestamp: Some(FakeInstant::default()),
                retrans_timer: RetransTimer::new(
                    FakeInstant::default(),
                    Estimator::RTO_INIT,
                    DEFAULT_USER_TIMEOUT,
                    DEFAULT_MAX_SYN_RETRIES,
                ),
                active_open: (),
                buffer_sizes: BufferSizes::default(),
                default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                rcv_wnd_scale: WindowScale::default(),
            },
        ),
        State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer::new(
                FakeInstant::default(),
                Estimator::RTO_INIT,
                Duration::from_secs(10),
                DEFAULT_MAX_SYNACK_RETRIES,
            ),
            simultaneous_open: None,
            buffer_sizes: BufferSizes { send: 0, receive: 0 },
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::new(0).unwrap(),
            snd_wnd_scale: WindowScale::new(0),
        }) => NewlyClosed::No; "non-closed to non-closed")]
    fn transition_to_state(
        mut old_state: State<FakeInstant, RingBuffer, RingBuffer, ()>,
        new_state: State<FakeInstant, RingBuffer, RingBuffer, ()>,
    ) -> NewlyClosed {
        old_state.transition_to_state(&Default::default(), new_state)
    }

    #[test_case(true; "more than mss dequeued")]
    #[test_case(false; "less than mss dequeued")]
    fn poll_receive_data_dequeued_state(dequeue_more_than_mss: bool) {
        // NOTE: Just enough room to hold TEST_BYTES, but will end up below the MSS when full.
        const BUFFER_SIZE: usize = 5;
        const MSS: Mss = Mss(NonZeroU16::new(5).unwrap());
        const TEST_BYTES: &[u8] = "Hello".as_bytes();

        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::ZERO,
            wnd_max: WindowSize::ZERO,
            buffer: NullBuffer,
            wl1: ISS_2 + 1,
            wl2: ISS_1 + 1,
            rtt_estimator: Estimator::default(),
            last_seq_ts: None,
            timer: None,
            congestion_control: CongestionControl::cubic_with_mss(MSS),
            wnd_scale: WindowScale::default(),
        };
        let new_rcv = || Recv {
            buffer: RecvBufferState::Open {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
            timer: None,
            mss: MSS,
            wnd_scale: WindowScale::default(),
            last_window_update: (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap()),
        };

        let last_window_update = |state: &State<_, _, _, _>| match state {
            State::Closed(_)
            | State::Listen(_)
            | State::SynRcvd(_)
            | State::SynSent(_)
            | State::CloseWait(_)
            | State::LastAck(_)
            | State::Closing(_)
            | State::TimeWait(_) => unreachable!(),

            State::Established(Established { rcv, .. }) => rcv.last_window_update.clone(),
            State::FinWait1(FinWait1 { rcv, .. }) => rcv.last_window_update.clone(),
            State::FinWait2(FinWait2 { rcv, .. }) => rcv.last_window_update.clone(),
        };

        let clock = FakeInstantCtx::default();
        let counters = TcpCountersInner::default();
        for mut state in [
            State::Established(Established { snd: new_snd().into(), rcv: new_rcv().into() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin().into(), rcv: new_rcv().into() }),
            State::FinWait2(FinWait2 { last_seq: ISS_1 + 1, rcv: new_rcv(), timeout_at: None }),
        ] {
            // At the beginning, our last window update was greater than MSS, so there's no
            // need to send an update.
            assert_matches!(state.poll_receive_data_dequeued(), None);
            assert_eq!(
                last_window_update(&state),
                (ISS_2 + 1, WindowSize::new(BUFFER_SIZE).unwrap())
            );

            // Receive a segment that almost entirely fills the buffer.  We now have less
            // than MSS of available window, which means we won't send an update.
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::data(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0), TEST_BYTES,),
                    clock.now(),
                    &counters,
                ),
                (
                    Some(Segment::ack(
                        ISS_1 + 1,
                        ISS_2 + 1 + TEST_BYTES.len(),
                        UnscaledWindowSize::from((BUFFER_SIZE - TEST_BYTES.len()) as u16),
                    )),
                    None
                ),
            );
            assert_matches!(state.poll_receive_data_dequeued(), None);
            assert_eq!(
                last_window_update(&state),
                (ISS_2 + 1 + TEST_BYTES.len(), WindowSize::new(0).unwrap())
            );

            if dequeue_more_than_mss {
                // Dequeue the data we just received, which frees up enough space in the buffer
                // to receive another MSS worth of data. Since we went from under to over MSS,
                // we expect to immediately send a window update.
                assert_eq!(
                    state.read_with(|available| {
                        assert_eq!(available, &[TEST_BYTES]);
                        available[0].len()
                    }),
                    TEST_BYTES.len()
                );
                assert_eq!(
                    state.poll_receive_data_dequeued(),
                    Some(Segment::ack(
                        ISS_1 + 1,
                        ISS_2 + 1 + TEST_BYTES.len(),
                        UnscaledWindowSize::from(BUFFER_SIZE as u16)
                    ))
                );
                assert_eq!(
                    last_window_update(&state),
                    (ISS_2 + 1 + TEST_BYTES.len(), WindowSize::new(BUFFER_SIZE).unwrap())
                );
            } else {
                // Dequeue data we just received, but just under one MSS worth.
                // We shouldn't expect there to be any window update sent in
                // that case.
                let mss: usize = MSS.get().get().into();
                assert_eq!(
                    state.read_with(|available| {
                        assert_eq!(available, &[TEST_BYTES]);
                        mss - 1
                    }),
                    mss - 1
                );
                assert_eq!(state.poll_receive_data_dequeued(), None,);
                assert_eq!(
                    last_window_update(&state),
                    (ISS_2 + 1 + TEST_BYTES.len(), WindowSize::new(0).unwrap())
                );
            }
        }
    }
}
