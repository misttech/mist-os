// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TCP state machine per [RFC 793](https://tools.ietf.org/html/rfc793).
// Note: All RFC quotes (with two extra spaces at the beginning of each line) in
// this file are from https://tools.ietf.org/html/rfc793#section-3.9 if not
// specified otherwise.

use core::{
    convert::{Infallible, TryFrom as _},
    fmt::Debug,
    num::{NonZeroU32, NonZeroU8, NonZeroUsize, TryFromIntError},
    time::Duration,
};

use assert_matches::assert_matches;
use derivative::Derivative;
use explicit::ResultExt as _;
use packet_formats::utils::NonZeroDuration;

use crate::{
    ip::icmp::IcmpErrorCode,
    transport::tcp::{
        buffer::{Assembler, BufferLimits, IntoBuffers, ReceiveBuffer, SendBuffer, SendPayload},
        congestion::CongestionControl,
        rtt::Estimator,
        segment::{Options, Payload, Segment},
        seqnum::{SeqNum, UnscaledWindowSize, WindowScale, WindowSize},
        BufferSizes, ConnectionError, Control, KeepAlive, Mss, OptionalBufferSizes, SocketOptions,
    },
    Instant,
};

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-81):
/// MSL
///       Maximum Segment Lifetime, the time a TCP segment can exist in
///       the internetwork system.  Arbitrarily defined to be 2 minutes.
pub(super) const MSL: Duration = Duration::from_secs(2 * 60);
// TODO(https://fxbug.dev/117955): With the current usage of netstack3 on mostly
// link-local workloads, these values are large enough to accommodate most cases
// so it can help us detect failure faster. We should make them agree with other
// common implementations once we can configure them through socket options.
const DEFAULT_MAX_RETRIES: NonZeroU8 = nonzero_ext::nonzero!(12_u8);
const DEFAULT_USER_TIMEOUT: Duration = Duration::from_secs(60 * 2);

/// Default maximum SYN's to send before giving up an attempt to connect.
// TODO(https://fxbug.dev/126318): Make these constants configurable.
pub(super) const DEFAULT_MAX_SYN_RETRIES: NonZeroU8 = nonzero_ext::nonzero!(6_u8);
const DEFAULT_MAX_SYNACK_RETRIES: NonZeroU8 = nonzero_ext::nonzero!(5_u8);

/// Per RFC 9293 (https://tools.ietf.org/html/rfc9293#section-3.8.6.3):
///  ... in particular, the delay MUST be less than 0.5 seconds.
const ACK_DELAY_THRESHOLD: Duration = Duration::from_millis(500);

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
pub(crate) struct Closed<Error> {
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
    pub(crate) fn on_segment(
        &self,
        Segment { seq: seg_seq, ack: seg_ack, wnd: _, contents, options: _ }: Segment<impl Payload>,
    ) -> Option<Segment<()>> {
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
        if contents.control() == Some(Control::RST) {
            return None;
        }
        Some(match seg_ack {
            Some(seg_ack) => Segment::rst(seg_ack),
            None => Segment::rst_ack(SeqNum::from(0), seg_seq + contents.len()),
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
pub(crate) struct Listen {
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
        Segment { seq, ack, wnd: _, contents, options }: Segment<impl Payload>,
        now: I,
    ) -> ListenOnSegmentDisposition<I> {
        let Listen { iss, buffer_sizes, device_mss, default_mss, user_timeout } = *self;
        let smss = options.mss.unwrap_or(default_mss).min(device_mss);
        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-65):
        //   first check for an RST
        //   An incoming RST should be ignored.  Return.
        if contents.control() == Some(Control::RST) {
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
        if contents.control() == Some(Control::SYN) {
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
                        /// Per RFC 7323 Section 2.3:
                        ///   If a TCP receives a <SYN> segment containing a
                        ///   Window Scale option, it SHOULD send its own Window
                        ///   Scale option in the <SYN,ACK> segment.
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
pub(crate) struct SynSent<I, ActiveOpen> {
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
enum SynSentOnSegmentDisposition<I: Instant, R: ReceiveBuffer, S: SendBuffer, ActiveOpen> {
    SendAckAndEnterEstablished(Segment<()>, Established<I, R, S>),
    SendSynAckAndEnterSynRcvd(Segment<()>, SynRcvd<I, ActiveOpen>),
    SendRstAndEnterClosed(Segment<()>, Closed<Option<ConnectionError>>),
    EnterClosed(Closed<Option<ConnectionError>>),
    Ignore,
}

impl<I: Instant + 'static, ActiveOpen: Takeable> SynSent<I, ActiveOpen> {
    /// Processes an incoming segment in the SYN-SENT state.
    ///
    /// Transitions to ESTABLSHED if the incoming segment is a proper SYN-ACK.
    /// Transitions to SYN-RCVD if the incoming segment is a SYN. Otherwise,
    /// the segment is dropped or an RST is generated.
    fn on_segment<
        R: ReceiveBuffer,
        S: SendBuffer,
        BP: BufferProvider<R, S, ActiveOpen = ActiveOpen>,
    >(
        &mut self,
        Segment { seq: seg_seq, ack: seg_ack, wnd: seg_wnd, contents, options }: Segment<
            impl Payload,
        >,
        now: I,
    ) -> SynSentOnSegmentDisposition<I, R, S, BP::ActiveOpen>
    where
        ActiveOpen: IntoBuffers<R, S>,
    {
        let SynSent {
            iss,
            timestamp: syn_sent_ts,
            retrans_timer: RetransTimer { user_timeout_until, remaining_retries: _, at: _, rto: _ },
            ref mut active_open,
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
                    return if contents.control() == Some(Control::RST) {
                        SynSentOnSegmentDisposition::Ignore
                    } else {
                        SynSentOnSegmentDisposition::SendRstAndEnterClosed(
                            Segment::rst(ack),
                            Closed { reason: Some(ConnectionError::ConnectionReset) },
                        )
                    };
                }
                true
            }
            None => false,
        };

        match contents.control() {
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
                        reason: Some(ConnectionError::ConnectionReset),
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
                                rtt_estimator.sample(now.duration_since(syn_sent_ts));
                            }
                            let (rcv_buffer, snd_buffer) =
                                active_open.take().into_buffers(buffer_sizes);
                            let (rcv_wnd_scale, snd_wnd_scale) = options
                                .window_scale
                                .map(|snd_wnd_scale| (rcv_wnd_scale, snd_wnd_scale))
                                .unwrap_or_default();
                            let established = Established {
                                snd: Send {
                                    nxt: iss + 1,
                                    max: iss + 1,
                                    una: seg_ack,
                                    wnd: seg_wnd << snd_wnd_scale,
                                    wl1: seg_seq,
                                    wl2: seg_ack,
                                    buffer: snd_buffer,
                                    last_seq_ts: None,
                                    rtt_estimator,
                                    timer: None,
                                    congestion_control: CongestionControl::cubic_with_mss(smss),
                                    wnd_scale: snd_wnd_scale,
                                },
                                rcv: Recv {
                                    buffer: rcv_buffer,
                                    assembler: Assembler::new(irs + 1),
                                    timer: None,
                                    mss: smss,
                                    wnd_scale: rcv_wnd_scale,
                                },
                            };
                            let ack_seg = Segment::ack(
                                established.snd.nxt,
                                established.rcv.nxt(),
                                established.rcv.wnd() >> rcv_wnd_scale,
                            );
                            SynSentOnSegmentDisposition::SendAckAndEnterEstablished(
                                ack_seg,
                                established,
                            )
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
                                        user_timeout_until.duration_since(now),
                                        DEFAULT_MAX_SYNACK_RETRIES,
                                    ),
                                    simultaneous_open: Some(active_open.take()),
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
pub(crate) struct SynRcvd<I, ActiveOpen> {
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
            Some(infallible) => match infallible {},
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
struct Send<I, S, const FIN_QUEUED: bool> {
    nxt: SeqNum,
    max: SeqNum,
    una: SeqNum,
    wnd: WindowSize,
    wnd_scale: WindowScale,
    wl1: SeqNum,
    wl2: SeqNum,
    buffer: S,
    // The last sequence number sent out and its timestamp when sent.
    last_seq_ts: Option<(SeqNum, I)>,
    rtt_estimator: Estimator,
    timer: Option<SendTimer<I>>,
    #[derivative(PartialEq = "ignore")]
    congestion_control: CongestionControl<I>,
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
        let at = now.add(wakeup_after);
        let user_timeout_until = now.add(user_timeout);
        Self { at, rto, user_timeout_until, remaining_retries: Some(max_retries) }
    }

    fn backoff(&mut self, now: I) {
        let Self { at, rto, user_timeout_until, remaining_retries } = self;
        *remaining_retries = remaining_retries.and_then(|r| NonZeroU8::new(r.get() - 1));
        *rto = rto.saturating_mul(2);
        let remaining = if now < *user_timeout_until {
            user_timeout_until.duration_since(now)
        } else {
            // `now` has already passed  `user_timeout_until`, just update the
            // timer to expire as soon as possible.
            Duration::ZERO
        };
        *at = now.add(core::cmp::min(*rto, remaining));
    }

    fn rearm(&mut self, now: I) {
        let Self { at, rto, user_timeout_until: _, remaining_retries: _ } = self;
        *at = now.add(*rto);
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
        let at = now.add(keep_alive.idle.into());
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

/// TCP control block variables that are responsible for receiving.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct Recv<I, R> {
    buffer: R,
    assembler: Assembler,
    timer: Option<ReceiveTimer<I>>,
    mss: Mss,
    wnd_scale: WindowScale,
}

impl<I: Instant, R: ReceiveBuffer> Recv<I, R> {
    fn wnd(&self) -> WindowSize {
        // TODO(https://fxbug.dev/128525): When selecting window sizes, modern
        // implementations tend to take into account factors more than just free
        // space available. We need to also take similar measures to address
        // silly window syndrome, also we need to avoid advertising spurious
        // zero-window-probe when window scaling is in effect.
        let BufferLimits { capacity, len } = self.buffer.limits();
        WindowSize::new(capacity - len).unwrap_or(WindowSize::MAX)
    }

    fn nxt(&self) -> SeqNum {
        self.assembler.nxt()
    }

    fn take(&mut self) -> Self {
        let Self { buffer, assembler, timer, mss, wnd_scale } = self;
        Self {
            buffer: buffer.take(),
            assembler: core::mem::replace(assembler, Assembler::new(SeqNum::new(0))),
            timer: *timer,
            mss: *mss,
            wnd_scale: *wnd_scale,
        }
    }

    fn set_capacity(&mut self, size: usize) {
        let Self { buffer, assembler: _, timer: _, mss: _, wnd_scale: _ } = self;
        buffer.request_capacity(size)
    }

    fn target_capacity(&self) -> usize {
        let Self { buffer, assembler: _, timer: _, mss: _, wnd_scale: _ } = self;
        buffer.target_capacity()
    }

    fn poll_send(&mut self, snd_nxt: SeqNum, now: I) -> Option<Segment<()>> {
        match self.timer {
            Some(ReceiveTimer::DelayedAck { at, received_bytes: _ }) => (at <= now).then(|| {
                self.timer = None;
                Segment::ack(snd_nxt, self.nxt(), self.wnd() >> self.wnd_scale)
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
pub(crate) struct Established<I, R, S> {
    snd: Send<I, S, { FinQueued::NO }>,
    rcv: Recv<I, R>,
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
            None => false,
        }
    }

    /// Polls for new segments with enabled options.
    ///
    /// `limit` is the maximum bytes wanted in the TCP segment (if any). The
    /// returned segment will have payload size up to the smaller of the given
    /// limit or the calculated MSS for the connection.
    fn poll_send(
        &mut self,
        rcv_nxt: SeqNum,
        rcv_wnd: WindowSize,
        limit: u32,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled,
            user_timeout,
            delayed_ack: _,
            fin_wait2_timeout: _,
            max_syn_retries: _,
        }: &SocketOptions,
    ) -> Option<Segment<SendPayload<'_>>> {
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
            wnd_scale,
        } = self;
        let BufferLimits { capacity: _, len: readable_bytes } = buffer.limits();
        let mss = u32::from(congestion_control.mss());
        let mut zero_window_probe = false;
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
                        *at = now.add(keep_alive.interval.into());
                        *already_sent = already_sent.saturating_add(1);
                        // Per RFC 9293 Section 3.8.4:
                        //   Such a segment generally contains SEG.SEQ = SND.NXT-1
                        return Some(
                            Segment::ack(*snd_nxt - 1, rcv_nxt, rcv_wnd >> *wnd_scale).into(),
                        );
                    }
                } else {
                    *timer = None;
                }
            }
            None => {}
        };
        // Find the sequence number for the next segment, we start with snd_nxt
        // unless a fast retransmit is needed.
        let next_seg = congestion_control.fast_retransmit().unwrap_or(*snd_nxt);
        // First calculate the open window, note that if our peer has shrank
        // their window (it is strongly discouraged), the following conversion
        // will fail and we return early.
        // TODO(https://fxbug.dev/93868): Implement zero window probing.
        let cwnd = congestion_control.cwnd();
        let swnd = WindowSize::min(*snd_wnd, cwnd);
        let open_window =
            u32::try_from(*snd_una + swnd - next_seg).ok_checked::<TryFromIntError>()?;
        let offset =
            usize::try_from(next_seg - *snd_una).unwrap_or_else(|TryFromIntError { .. }| {
                panic!("next_seg({:?}) should never fall behind snd.una({:?})", *snd_nxt, *snd_una);
            });
        let available = u32::try_from(readable_bytes + usize::from(FIN_QUEUED) - offset)
            .unwrap_or(WindowSize::MAX.into());
        // We can only send the minimum of the open window and the bytes that
        // are available, additionally, if in zero window probe mode, allow at
        // least one byte past the limit to be sent.
        let can_send =
            open_window.min(available).min(mss).min(limit).max(u32::from(zero_window_probe));
        if can_send == 0 {
            if available == 0 && offset == 0 && timer.is_none() && keep_alive.enabled {
                *timer = Some(SendTimer::KeepAlive(KeepAliveTimer::idle(now, keep_alive)));
            }
            if available != 0 && offset == 0 && timer.is_none() && *snd_wnd == WindowSize::ZERO {
                let user_timeout = user_timeout.get_or_default(DEFAULT_USER_TIMEOUT);
                *timer = Some(SendTimer::ZeroWindowProbe(RetransTimer::new(
                    now,
                    rtt_estimator.rto(),
                    user_timeout,
                    DEFAULT_MAX_RETRIES,
                )))
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
            // Per RFC 9293 Section 3.7.4:
            //   If there is unacknowledged data (i.e., SND.NXT > SND.UNA), then
            //   the sending TCP endpoint buffers all user data (regardless of
            //   the PSH bit) until the outstanding data has been acknowledged
            //   or until the TCP endpoint can send a full-sized segment
            //   (Eff.snd.MSS bytes).
            // Note: There is one special case that isn't covered by the RFC:
            // If the current segment would contain a FIN, then even if it's a
            // small segment, there is no reason to wait for more - there won't
            // be more as the application has indicated.
            if *nagle_enabled && bytes_to_send < mss && !has_fin && snd_nxt.after(*snd_una) {
                return None;
            }
            // Don't send a segment if there isn't any data to read, unless that
            // segment would carry the FIN.
            if bytes_to_send == 0 && !has_fin {
                return None;
            }
            let (seg, discarded) = Segment::with_data(
                next_seg,
                Some(rcv_nxt),
                has_fin.then(|| Control::FIN),
                rcv_wnd >> *wnd_scale,
                readable.slice(0..bytes_to_send),
            );
            debug_assert_eq!(discarded, 0);
            Some(seg)
        })?;
        let seq_max = next_seg + can_send;
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
            Some(SendTimer::KeepAlive(_)) | None => {
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

    fn process_ack(
        &mut self,
        seg_seq: SeqNum,
        seg_ack: SeqNum,
        seg_wnd: UnscaledWindowSize,
        pure_ack: bool,
        rcv_nxt: SeqNum,
        rcv_wnd: WindowSize,
        now: I,
        keep_alive: &KeepAlive,
    ) -> Option<Segment<()>> {
        let Self {
            nxt: snd_nxt,
            max: snd_max,
            una: snd_una,
            wnd: snd_wnd,
            wl1: snd_wl1,
            wl2: snd_wl2,
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
            Some(SendTimer::ZeroWindowProbe(_)) => {}
        }
        // Note: we rewind SND.NXT to SND.UNA on retransmission; if
        // `seg_ack` is after `snd.max`, it means the segment acks
        // something we never sent.
        if seg_ack.after(*snd_max) {
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   If the ACK acks something not yet sent (SEG.ACK >
            //   SND.NXT) then send an ACK, drop the segment, and
            //   return.
            Some(Segment::ack(*snd_nxt, rcv_nxt, rcv_wnd >> *wnd_scale))
        } else if seg_ack.after(*snd_una) {
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
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   If SND.UNA < SEG.ACK =< SND.NXT, the send window should be
            //   updated.  If (SND.WL1 < SEG.SEQ or (SND.WL1 = SEG.SEQ and
            //   SND.WL2 =< SEG.ACK)), set SND.WND <- SEG.WND, set
            //   SND.WL1 <- SEG.SEQ, and set SND.WL2 <- SEG.ACK.
            if snd_wl1.before(seg_seq) || (seg_seq == *snd_wl1 && !snd_wl2.after(seg_ack)) {
                *snd_wnd = seg_wnd;
                *snd_wl1 = seg_seq;
                *snd_wl2 = seg_ack;
                if seg_wnd != WindowSize::ZERO
                    && matches!(timer, Some(SendTimer::ZeroWindowProbe(_)))
                {
                    *timer = None;
                }
            }
            // If the incoming segment acks the sequence number that we used
            // for RTT estimate, feed the sample to the estimator.
            if let Some((seq_max, timestamp)) = *last_seq_ts {
                if !seg_ack.before(seq_max) {
                    rtt_estimator.sample(now.duration_since(timestamp));
                }
            }
            congestion_control.on_ack(acked, now, rtt_estimator.rto());
            None
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
                congestion_control.on_dup_ack(seg_ack);
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   If the ACK is a duplicate (SEG.ACK < SND.UNA), it can be
            //   ignored.
            None
        }
    }

    fn take(&mut self) -> Self {
        Self {
            buffer: self.buffer.take(),
            congestion_control: self.congestion_control.take(),
            ..*self
        }
    }

    fn set_capacity(&mut self, size: usize) {
        let Self {
            nxt: _,
            max: _,
            una: _,
            wnd: _,
            wl1: _,
            wl2: _,
            buffer,
            last_seq_ts: _,
            rtt_estimator: _,
            timer: _,
            congestion_control: _,
            wnd_scale: _,
        } = self;
        buffer.request_capacity(size)
    }

    fn target_capacity(&self) -> usize {
        let Self {
            nxt: _,
            max: _,
            una: _,
            wnd: _,
            wl1: _,
            wl2: _,
            buffer,
            last_seq_ts: _,
            rtt_estimator: _,
            timer: _,
            congestion_control: _,
            wnd_scale: _,
        } = self;
        buffer.target_capacity()
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
pub(crate) struct CloseWait<I, S> {
    snd: Send<I, S, { FinQueued::NO }>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
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
pub(crate) struct LastAck<I, S> {
    snd: Send<I, S, { FinQueued::YES }>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
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
pub(crate) struct FinWait1<I, R, S> {
    snd: Send<I, S, { FinQueued::YES }>,
    rcv: Recv<I, R>,
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
pub(crate) struct FinWait2<I, R> {
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
pub(crate) struct Closing<I, S> {
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
pub(crate) struct TimeWait<I> {
    pub(super) last_seq: SeqNum,
    pub(super) last_ack: SeqNum,
    pub(super) last_wnd: WindowSize,
    pub(super) last_wnd_scale: WindowScale,
    pub(super) expiry: I,
}

fn new_time_wait_expiry<I: Instant>(now: I) -> I {
    now.add(MSL * 2)
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum State<I, R, S, ActiveOpen> {
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
    type ActiveOpen: Takeable + IntoBuffers<R, S>;

    /// Creates new send and receive buffers and an object that needs to be
    /// vended to the application.
    fn new_passive_open_buffers(buffer_sizes: BufferSizes) -> (R, S, Self::PassiveOpen);
}

/// Allows the implementor to be replaced by an empty value. The semantics is
/// similar to [`core::mem::take`], except that for some types it is not
/// sensible to implement [`Default`] for the target type. This trait allows
/// taking ownership of a struct field, which is useful in state transitions.
pub trait Takeable {
    /// Replaces `self` with an implementor-defined "empty" value.
    fn take(&mut self) -> Self;
}

impl<T: Default> Takeable for T {
    fn take(&mut self) -> Self {
        core::mem::take(self)
    }
}

impl<I: Instant + 'static, R: ReceiveBuffer, S: SendBuffer, ActiveOpen: Debug + Takeable>
    State<I, R, S, ActiveOpen>
{
    /// Processes an incoming segment and advances the state machine.
    ///
    /// Returns a segment if one needs to be sent; if a passive open connection
    /// is newly established, the corresponding object that our client needs
    /// will be returned.
    pub(crate) fn on_segment<P: Payload, BP: BufferProvider<R, S, ActiveOpen = ActiveOpen>>(
        &mut self,
        incoming: Segment<P>,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled: _,
            user_timeout: _,
            delayed_ack,
            fin_wait2_timeout,
            max_syn_retries: _,
        }: &SocketOptions,
        defunct: bool,
    ) -> (Option<Segment<()>>, Option<BP::PassiveOpen>)
    where
        BP::PassiveOpen: Debug,
        ActiveOpen: IntoBuffers<R, S>,
    {
        let mut passive_open = None;
        let seg = (|| {
            let (mut rcv_nxt, rcv_wnd, rcv_wnd_scale, snd_nxt) = match self {
                State::Closed(closed) => return closed.on_segment(incoming),
                State::Listen(listen) => {
                    return match listen.on_segment(incoming, now) {
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
                                    *self = State::SynRcvd(SynRcvd {
                                        iss,
                                        irs,
                                        timestamp,
                                        retrans_timer,
                                        simultaneous_open: None,
                                        buffer_sizes,
                                        smss,
                                        rcv_wnd_scale,
                                        snd_wnd_scale,
                                    });
                                }
                                Some(infallible) => match infallible {},
                            }
                            Some(syn_ack)
                        }
                        ListenOnSegmentDisposition::SendRst(rst) => Some(rst),
                        ListenOnSegmentDisposition::Ignore => None,
                    }
                }
                State::SynSent(synsent) => {
                    return match synsent.on_segment::<_, _, BP>(incoming, now) {
                        SynSentOnSegmentDisposition::SendAckAndEnterEstablished(
                            ack,
                            established,
                        ) => {
                            *self = State::Established(established);
                            Some(ack)
                        }
                        SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                            syn_ack,
                            syn_rcvd,
                        ) => {
                            *self = State::SynRcvd(syn_rcvd);
                            Some(syn_ack)
                        }
                        SynSentOnSegmentDisposition::SendRstAndEnterClosed(rst, closed) => {
                            *self = State::Closed(closed);
                            Some(rst)
                        }
                        SynSentOnSegmentDisposition::EnterClosed(closed) => {
                            *self = State::Closed(closed);
                            None
                        }
                        SynSentOnSegmentDisposition::Ignore => None,
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
                    rcv_wnd_scale,
                    snd_wnd_scale: _,
                }) => (*irs + 1, buffer_sizes.rwnd(), *rcv_wnd_scale, *iss + 1),
                State::Established(Established { rcv, snd }) => {
                    (rcv.nxt(), rcv.wnd(), rcv.wnd_scale, snd.nxt)
                }
                State::CloseWait(CloseWait { snd, last_ack, last_wnd }) => {
                    (*last_ack, *last_wnd, WindowScale::default(), snd.nxt)
                }
                State::LastAck(LastAck { snd, last_ack, last_wnd })
                | State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale: _ }) => {
                    (*last_ack, *last_wnd, WindowScale::default(), snd.nxt)
                }
                State::FinWait1(FinWait1 { rcv, snd }) => {
                    (rcv.nxt(), rcv.wnd(), rcv.wnd_scale, snd.nxt)
                }
                State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => {
                    (rcv.nxt(), rcv.wnd(), rcv.wnd_scale, *last_seq)
                }
                State::TimeWait(TimeWait {
                    last_seq,
                    last_ack,
                    last_wnd,
                    expiry: _,
                    last_wnd_scale: _,
                }) => (*last_ack, *last_wnd, WindowScale::default(), *last_seq),
            };
            // Unreachable note(1): The above match returns early for states CLOSED,
            // SYN_SENT and LISTEN, so it is impossible to have the above states
            // past this line.
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-69):
            //   first check sequence number
            let is_rst = incoming.contents.control() == Some(Control::RST);
            // pure ACKs (empty segments) don't need to be ack'ed.
            let pure_ack = incoming.contents.len() == 0;
            let needs_ack = !pure_ack;
            let Segment { seq: seg_seq, ack: seg_ack, wnd: seg_wnd, contents, options: _ } =
                match incoming.overlap(rcv_nxt, rcv_wnd) {
                    Some(incoming) => incoming,
                    None => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-69):
                        //   If an incoming segment is not acceptable, an acknowledgment
                        //   should be sent in reply (unless the RST bit is set, if so drop
                        //   the segment and return):
                        //     <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK>
                        //   After sending the acknowledgment, drop the unacceptable segment
                        //   and return.
                        return if is_rst {
                            None
                        } else {
                            Some(Segment::ack(snd_nxt, rcv_nxt, rcv_wnd >> rcv_wnd_scale))
                        };
                    }
                };
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-70):
            //   second check the RST bit
            //   If the RST bit is set then, any outstanding RECEIVEs and SEND
            //   should receive "reset" responses.  All segment queues should be
            //   flushed.  Users should also receive an unsolicited general
            //   "connection reset" signal.  Enter the CLOSED state, delete the
            //   TCB, and return.
            if contents.control() == Some(Control::RST) {
                *self = State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) });
                return None;
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
            if contents.control() == Some(Control::SYN) {
                *self = State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) });
                return Some(Segment::rst(snd_nxt));
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
                            return Some(Segment::rst(seg_ack));
                        } else {
                            let mut rtt_estimator = Estimator::default();
                            if let Some(syn_rcvd_ts) = syn_rcvd_ts {
                                rtt_estimator.sample(now.duration_since(*syn_rcvd_ts));
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
                            *self = State::Established(Established {
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
                                },
                                rcv: Recv {
                                    buffer: rcv_buffer,
                                    assembler: Assembler::new(*irs + 1),
                                    timer: None,
                                    mss: *smss,
                                    wnd_scale: rcv_wnd_scale,
                                },
                            });
                        }
                        // Unreachable note(2): Because we either return early or
                        // transition to Established for the ack processing, it is
                        // impossible for SYN_RCVD to appear past this line.
                    }
                    State::Established(Established { snd, rcv: _ })
                    | State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                        if let Some(ack) = snd.process_ack(
                            seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now, keep_alive,
                        ) {
                            return Some(ack);
                        }
                    }
                    State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        if let Some(ack) = snd.process_ack(
                            seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now, keep_alive,
                        ) {
                            return Some(ack);
                        } else if seg_ack == fin_seq {
                            *self = State::Closed(Closed { reason: None });
                            return None;
                        }
                    }
                    State::FinWait1(FinWait1 { snd, rcv }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        if let Some(ack) = snd.process_ack(
                            seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now, keep_alive,
                        ) {
                            return Some(ack);
                        } else if seg_ack == fin_seq {
                            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-73):
                            //   In addition to the processing for the ESTABLISHED
                            //   state, if the FIN segment is now acknowledged then
                            //   enter FIN-WAIT-2 and continue processing in that
                            //   state
                            let last_seq = snd.nxt;
                            *self = State::FinWait2(FinWait2 {
                                last_seq,
                                rcv: rcv.take(),
                                // If the connection is already defunct, we set
                                // a timeout to reclaim, but otherwise, a later
                                // `close` call should set the timer.
                                timeout_at: fin_wait2_timeout
                                    .and_then(|timeout| defunct.then_some(now.add(timeout))),
                            });
                        }
                    }
                    State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale }) => {
                        let BufferLimits { len, capacity: _ } = snd.buffer.limits();
                        let fin_seq = snd.una + len + 1;
                        if let Some(ack) = snd.process_ack(
                            seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now, keep_alive,
                        ) {
                            return Some(ack);
                        } else if seg_ack == fin_seq {
                            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-73):
                            //   In addition to the processing for the ESTABLISHED state, if
                            //   the ACK acknowledges our FIN then enter the TIME-WAIT state,
                            //   otherwise ignore the segment.
                            *self = State::TimeWait(TimeWait {
                                last_seq: snd.nxt,
                                last_ack: *last_ack,
                                last_wnd: *last_wnd,
                                expiry: new_time_wait_expiry(now),
                                last_wnd_scale: *last_wnd_scale,
                            });
                        }
                    }
                    State::FinWait2(_) | State::TimeWait(_) => {}
                },
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
                //   if the ACK bit is off drop the segment and return
                None => return None,
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
                match self {
                    State::Closed(_) | State::Listen(_) | State::SynRcvd(_) | State::SynSent(_) => {
                        // This unreachable assert is justified by note (1) and (2).
                        unreachable!("encountered an already-handled state: {:?}", self)
                    }
                    State::Established(Established { snd: _, rcv })
                    | State::FinWait1(FinWait1 { snd: _, rcv })
                    | State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                        // Write the segment data in the buffer and keep track if it fills
                        // any hole in the assembler.
                        let had_out_of_order = rcv.assembler.has_out_of_order();
                        if contents.data().len() > 0 {
                            let offset = usize::try_from(seg_seq - rcv.nxt()).unwrap_or_else(|TryFromIntError {..}| {
                                panic!("The segment was trimmed to fit the window, thus seg.seq({:?}) must not come before rcv.nxt({:?})", seg_seq, rcv.nxt());
                            });
                            let nwritten = rcv.buffer.write_at(offset, contents.data());
                            let readable = rcv.assembler.insert(seg_seq..seg_seq + nwritten);
                            rcv.buffer.make_readable(readable);
                            rcv_nxt = rcv.nxt();
                        }
                        // Per RFC 5681 Section 4.2:
                        //  Out-of-order data segments SHOULD be acknowledged
                        //  immediately, ...  the receiver SHOULD send an
                        //  immediate ACK when it receives a data segment that
                        //  fills in all or part of a gap in the sequence space.
                        let immediate_ack =
                            !*delayed_ack || had_out_of_order || rcv.assembler.has_out_of_order();
                        if immediate_ack {
                            rcv.timer = None;
                        } else {
                            match &mut rcv.timer {
                                Some(ReceiveTimer::DelayedAck { at: _, received_bytes }) => {
                                    *received_bytes = received_bytes.saturating_add(
                                        u32::try_from(contents.data().len()).unwrap_or(u32::MAX),
                                    );
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
                                    if let Some(received_bytes) = NonZeroU32::new(
                                        u32::try_from(contents.data().len()).unwrap_or(u32::MAX),
                                    ) {
                                        rcv.timer = Some(ReceiveTimer::DelayedAck {
                                            at: now.add(ACK_DELAY_THRESHOLD),
                                            received_bytes,
                                        })
                                    }
                                }
                            }
                        }
                        (!matches!(rcv.timer, Some(ReceiveTimer::DelayedAck { .. })))
                            .then_some(Segment::ack(snd_nxt, rcv.nxt(), rcv.wnd() >> rcv.wnd_scale))
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
            let ack_to_fin = if contents.control() == Some(Control::FIN)
                && rcv_nxt == seg_seq + contents.data().len()
            {
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
                        let last_wnd = rcv.wnd().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_wnd = last_wnd >> rcv.wnd_scale;
                        *self = State::CloseWait(CloseWait { snd: snd.take(), last_ack, last_wnd });
                        Some(Segment::ack(snd_nxt, last_ack, scaled_wnd))
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
                        let last_wnd = rcv.wnd().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_wnd = last_wnd >> rcv.wnd_scale;
                        *self = State::Closing(Closing {
                            snd: snd.take(),
                            last_ack,
                            last_wnd,
                            last_wnd_scale: rcv.wnd_scale,
                        });
                        Some(Segment::ack(snd_nxt, last_ack, scaled_wnd))
                    }
                    State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => {
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd = rcv.wnd().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        let scaled_window = last_wnd >> rcv.wnd_scale;
                        *self = State::TimeWait(TimeWait {
                            last_seq: *last_seq,
                            last_ack,
                            last_wnd,
                            expiry: new_time_wait_expiry(now),
                            last_wnd_scale: rcv.wnd_scale,
                        });
                        Some(Segment::ack(snd_nxt, last_ack, scaled_window))
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
            ack_to_fin.or(ack_to_text)
        })();
        (seg, passive_open)
    }

    /// Polls if there are any bytes available to send in the buffer.
    ///
    /// Forms one segment of at most `limit` available bytes, as long as the
    /// receiver window allows.
    pub(crate) fn poll_send(
        &mut self,
        limit: u32,
        now: I,
        socket_options: &SocketOptions,
    ) -> Option<Segment<SendPayload<'_>>> {
        if self.poll_close(now, socket_options) {
            return None;
        }
        fn poll_rcv_then_snd<
            'a,
            I: Instant,
            R: ReceiveBuffer,
            S: SendBuffer,
            const FIN_QUEUED: bool,
        >(
            snd: &'a mut Send<I, S, FIN_QUEUED>,
            rcv: &'a mut Recv<I, R>,
            limit: u32,
            now: I,
            socket_options: &SocketOptions,
        ) -> Option<Segment<SendPayload<'a>>> {
            // We favor `rcv` over `snd` if both are present. The alternative
            // will also work (sending whatever is ready at the same time of
            // sending the delayed ACK) but we need special treatment for
            // FIN_WAIT_1 if we choose the alternative, because otherwise we
            // will have a spurious retransmitted FIN.
            let rcv_nxt = rcv.nxt();
            let rcv_wnd = rcv.wnd();
            let seg = rcv
                .poll_send(snd.nxt, now)
                .map(Into::into)
                .or_else(|| snd.poll_send(rcv_nxt, rcv_wnd, limit, now, socket_options));
            // We must have piggybacked an ACK so we can cancel the timer now.
            if seg.is_some() && matches!(rcv.timer, Some(ReceiveTimer::DelayedAck { .. })) {
                rcv.timer = None;
            }
            seg
        }
        match self {
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
                .into()
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
                .into()
            }),
            State::Established(Established { snd, rcv }) => {
                poll_rcv_then_snd(snd, rcv, limit, now, socket_options)
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd }) => {
                snd.poll_send(*last_ack, *last_wnd, limit, now, socket_options)
            }
            State::LastAck(LastAck { snd, last_ack, last_wnd })
            | State::Closing(Closing { snd, last_ack, last_wnd, last_wnd_scale: _ }) => {
                snd.poll_send(*last_ack, *last_wnd, limit, now, socket_options)
            }
            State::FinWait1(FinWait1 { snd, rcv }) => {
                poll_rcv_then_snd(snd, rcv, limit, now, socket_options)
            }
            State::FinWait2(FinWait2 { last_seq, rcv, timeout_at: _ }) => {
                rcv.poll_send(*last_seq, now).map(Into::into)
            }
            State::Closed(_) | State::Listen(_) | State::TimeWait(_) => None,
        }
    }

    /// Polls the state machine to check if the connection should be closed.
    ///
    /// Returns whether the connection has been closed.
    fn poll_close(
        &mut self,
        now: I,
        SocketOptions {
            keep_alive,
            nagle_enabled: _,
            user_timeout: _,
            delayed_ack: _,
            fin_wait2_timeout: _,
            max_syn_retries: _,
        }: &SocketOptions,
    ) -> bool {
        let timed_out = match self {
            State::Established(Established { snd, rcv: _ }) => snd.timed_out(now, keep_alive),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                snd.timed_out(now, keep_alive)
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ })
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
            *self = State::Closed(Closed { reason: Some(ConnectionError::TimedOut) });
        } else if let State::TimeWait(tw) = self {
            if tw.expiry <= now {
                *self = State::Closed(Closed { reason: None });
            }
        }
        matches!(self, State::Closed(_))
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
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                Some(snd.timer?.expiry())
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ })
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
        close_reason: CloseReason<I>,
        socket_options: &SocketOptions,
    ) -> Result<(), CloseError>
    where
        ActiveOpen: IntoBuffers<R, S>,
    {
        match self {
            State::Closed(_) => Err(CloseError::NoConnection),
            State::Listen(_) | State::SynSent(_) => {
                *self = State::Closed(Closed { reason: None });
                Ok(())
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
                let active_open = simultaneous_open.as_mut().ok_or(CloseError::NoConnection)?;
                let (rcv_buffer, snd_buffer) = active_open.take().into_buffers(*buffer_sizes);
                // Note: `Send` in `FinWait1` always has a FIN queued.
                // Since we don't support sending data when connection
                // isn't established, so enter FIN-WAIT-1 immediately.
                let (snd_wnd_scale, rcv_wnd_scale) = snd_wnd_scale
                    .map(|snd_wnd_scale| (snd_wnd_scale, *rcv_wnd_scale))
                    .unwrap_or_default();
                *self = State::FinWait1(FinWait1 {
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
                    },
                    rcv: Recv {
                        buffer: rcv_buffer,
                        assembler: Assembler::new(*irs + 1),
                        timer: None,
                        mss: *smss,
                        wnd_scale: rcv_wnd_scale,
                    },
                });
                Ok(())
            }
            State::Established(Established { snd, rcv }) => {
                // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-60):
                //   ESTABLISHED STATE
                //     Queue this until all preceding SENDs have been segmentized,
                //     then form a FIN segment and send it.  In any case, enter
                //     FIN-WAIT-1 state.
                *self = State::FinWait1(FinWait1 { snd: snd.take().queue_fin(), rcv: rcv.take() });
                Ok(())
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd }) => {
                *self = State::LastAck(LastAck {
                    snd: snd.take().queue_fin(),
                    last_ack: *last_ack,
                    last_wnd: *last_wnd,
                });
                Ok(())
            }
            State::LastAck(_) | State::FinWait1(_) | State::Closing(_) | State::TimeWait(_) => {
                Err(CloseError::Closing)
            }
            State::FinWait2(FinWait2 { last_seq: _, rcv: _, timeout_at }) => {
                if let (CloseReason::Close { now }, Some(fin_wait2_timeout)) =
                    (close_reason, socket_options.fin_wait2_timeout)
                {
                    assert_eq!(timeout_at.replace(now.add(fin_wait2_timeout)), None);
                }
                Err(CloseError::Closing)
            }
        }
    }

    /// Corresponds to [ABORT](https://tools.ietf.org/html/rfc9293#section-3.10.5)
    /// user call.
    pub(crate) fn abort(&mut self) -> Option<Segment<()>> {
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
            }) => Some(Segment::rst_ack(*iss, *irs + 1)),
            State::Established(Established { snd, rcv }) => {
                Some(Segment::rst_ack(snd.nxt, rcv.nxt()))
            }
            State::FinWait1(FinWait1 { snd, rcv }) => Some(Segment::rst_ack(snd.nxt, rcv.nxt())),
            State::FinWait2(FinWait2 { rcv, last_seq, timeout_at: _ }) => {
                Some(Segment::rst_ack(*last_seq, rcv.nxt()))
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd: _ }) => {
                Some(Segment::rst_ack(snd.nxt, *last_ack))
            }
        };
        *self = State::Closed(Closed { reason: Some(ConnectionError::ConnectionReset) });
        reply
    }

    pub(crate) fn set_send_buffer_size(&mut self, size: usize) {
        match self {
            State::FinWait2(_) | State::TimeWait(_) | State::Closed(_) => (),
            State::Listen(Listen {
                iss: _,
                buffer_sizes: BufferSizes { send, receive: _ },
                device_mss: _,
                default_mss: _,
                user_timeout: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: BufferSizes { send, receive: _ },
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            })
            | State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes: BufferSizes { send, receive: _ },
                device_mss: _,
                default_mss: _,
                rcv_wnd_scale: _,
            }) => *send = size,
            State::Established(Established { snd, rcv: _ }) => snd.set_capacity(size),
            State::FinWait1(FinWait1 { snd, rcv: _ }) => snd.set_capacity(size),
            State::Closing(Closing { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ })
            | State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ }) => snd.set_capacity(size),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => snd.set_capacity(size),
        }
    }

    pub(crate) fn set_receive_buffer_size(&mut self, size: usize) {
        match self {
            State::Closing(_)
            | State::LastAck(_)
            | State::CloseWait(_)
            | State::TimeWait(_)
            | State::Closed(_) => (),
            State::Listen(Listen {
                iss: _,
                buffer_sizes: BufferSizes { send: _, receive },
                device_mss: _,
                default_mss: _,
                user_timeout: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: BufferSizes { send: _, receive },
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            })
            | State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes: BufferSizes { send: _, receive },
                device_mss: _,
                default_mss: _,
                rcv_wnd_scale: _,
            }) => *receive = size,
            State::Established(Established { snd: _, rcv }) => rcv.set_capacity(size),
            State::FinWait1(FinWait1 { snd: _, rcv })
            | State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                rcv.set_capacity(size)
            }
        }
    }

    pub(crate) fn target_buffer_sizes(&self) -> OptionalBufferSizes {
        match self {
            State::TimeWait(_) | State::Closed(_) => {
                OptionalBufferSizes { send: None, receive: None }
            }
            State::Listen(Listen {
                iss: _,
                buffer_sizes,
                device_mss: _,
                default_mss: _,
                user_timeout: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes,
                smss: _,
                rcv_wnd_scale: _,
                snd_wnd_scale: _,
            })
            | State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes,
                device_mss: _,
                default_mss: _,
                rcv_wnd_scale: _,
            }) => buffer_sizes.into_optional(),
            State::Established(Established { snd, rcv }) => OptionalBufferSizes {
                send: Some(snd.target_capacity()),
                receive: Some(rcv.target_capacity()),
            },
            State::FinWait1(FinWait1 { snd, rcv }) => OptionalBufferSizes {
                send: Some(snd.target_capacity()),
                receive: Some(rcv.target_capacity()),
            },
            State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                OptionalBufferSizes { send: None, receive: Some(rcv.target_capacity()) }
            }
            State::Closing(Closing { snd, last_ack: _, last_wnd: _, last_wnd_scale: _ })
            | State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ }) => {
                OptionalBufferSizes { send: Some(snd.target_capacity()), receive: None }
            }
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                OptionalBufferSizes { send: Some(snd.target_capacity()), receive: None }
            }
        }
    }

    /// Processes an incoming ICMP error, returns an soft error that needs to be
    /// recorded in the containing socket.
    pub(super) fn on_icmp_error(
        &mut self,
        err: IcmpErrorCode,
        seq: SeqNum,
    ) -> Option<ConnectionError> {
        let err = Option::<ConnectionError>::from(err)?;
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
        match self {
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
                    *self = State::Closed(Closed { reason: Some(err) });
                }
                None
            }
            State::Established(Established { snd, rcv: _ })
            | State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                (!snd.una.after(seq) && seq.before(snd.nxt)).then_some(err)
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ })
            | State::FinWait1(FinWait1 { snd, rcv: _ }) => {
                (!snd.una.after(seq) && seq.before(snd.nxt)).then_some(err)
            }
            // The following states does not have any outstanding segments, so
            // they don't expect any incoming ICMP error.
            State::FinWait2(_) | State::Closing(_) | State::TimeWait(_) => None,
        }
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
    use core::{fmt::Debug, time::Duration};

    use assert_matches::assert_matches;
    use net_types::ip::Ipv4;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::{
            testutil::{FakeInstant, FakeInstantCtx},
            InstantContext as _,
        },
        transport::tcp::{
            buffer::{Buffer, RingBuffer},
            testutil::{
                DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE, DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE,
            },
            DEFAULT_FIN_WAIT2_TIMEOUT,
        },
    };

    const ISS_1: SeqNum = SeqNum::new(100);
    const ISS_2: SeqNum = SeqNum::new(300);

    const RTT: Duration = Duration::from_millis(500);

    const DEVICE_MAXIMUM_SEGMENT_SIZE: Mss = Mss(nonzero_ext::nonzero!(1400 as u16));

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

    impl<P: Payload> Segment<P> {
        fn data(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize, data: P) -> Segment<P> {
            let (seg, truncated) = Segment::with_data(seq, Some(ack), None, wnd, data);
            assert_eq!(truncated, 0);
            seg
        }

        fn piggybacked_fin(
            seq: SeqNum,
            ack: SeqNum,
            wnd: UnscaledWindowSize,
            data: P,
        ) -> Segment<P> {
            let (seg, truncated) =
                Segment::with_data(seq, Some(ack), Some(Control::FIN), wnd, data);
            assert_eq!(truncated, 0);
            seg
        }
    }

    impl Segment<()> {
        fn fin(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize) -> Self {
            Segment::new(seq, Some(ack), Some(Control::FIN), wnd)
        }
    }

    impl RingBuffer {
        fn with_data<'a>(cap: usize, data: &'a [u8]) -> Self {
            let mut buffer = RingBuffer::new(cap);
            let nwritten = buffer.write_at(0, &data);
            assert_eq!(nwritten, data.len());
            buffer.make_readable(nwritten);
            buffer
        }
    }

    impl UnscaledWindowSize {
        fn from_usize(size: usize) -> Self {
            UnscaledWindowSize::from(u16::try_from(size).unwrap())
        }

        fn from_u32(size: u32) -> Self {
            UnscaledWindowSize::from(u16::try_from(size).unwrap())
        }
    }

    /// A buffer that can't read or write for test purpose.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct NullBuffer;

    impl Buffer for NullBuffer {
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

        fn make_readable(&mut self, count: usize) {
            assert_eq!(count, 0);
        }
    }

    impl SendBuffer for NullBuffer {
        fn mark_read(&mut self, count: usize) {
            assert_eq!(count, 0);
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(SendPayload<'a>) -> R,
        {
            assert_eq!(offset, 0);
            f(SendPayload::Contiguous(&[]))
        }
    }

    impl<R: ReceiveBuffer, S: SendBuffer> State<FakeInstant, R, S, ()> {
        fn poll_send_with_default_options(
            &mut self,
            mss: u32,
            now: FakeInstant,
        ) -> Option<Segment<SendPayload<'_>>> {
            self.poll_send(mss, now, &SocketOptions::default())
        }

        fn on_segment_with_default_options<P: Payload, BP: BufferProvider<R, S, ActiveOpen = ()>>(
            &mut self,
            incoming: Segment<P>,
            now: FakeInstant,
        ) -> (Option<Segment<()>>, Option<BP::PassiveOpen>)
        where
            BP::PassiveOpen: Debug,
            R: Default,
            S: Default,
        {
            // In testing, it is convenient to disable delayed ack by default.
            self.on_segment::<P, BP>(
                incoming,
                now,
                &SocketOptions::default(),
                false, /* defunct */
            )
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
                State::Established(e) => e.rcv.buffer.read_with(f),
                State::FinWait1(FinWait1 { snd: _, rcv })
                | State::FinWait2(FinWait2 { last_seq: _, rcv, timeout_at: _ }) => {
                    rcv.buffer.read_with(f)
                }
            }
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
        closed.on_segment(incoming.into())
    }

    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 - 1), RTT
    => SynSentOnSegmentDisposition::Ignore; "unacceptable ACK with RST")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1 - 1, UnscaledWindowSize::from(u16::MAX)), RTT
    => SynSentOnSegmentDisposition::SendRstAndEnterClosed(
        Segment::rst(ISS_1-1),
        Closed { reason: Some(ConnectionError::ConnectionReset) },
    ); "unacceptable ACK without RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1), RTT
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: Some(ConnectionError::ConnectionReset) },
    ); "acceptable ACK(ISS) with RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 + 1), RTT
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: Some(ConnectionError::ConnectionReset) },
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
            simultaneous_open: Some(()),
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
    ) -> SynSentOnSegmentDisposition<FakeInstant, NullBuffer, NullBuffer, ()> {
        let mut syn_sent = SynSent {
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
        syn_sent.on_segment::<_, _, ClientlessBufferProvider>(incoming, FakeInstant::from(delay))
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
                },
                rcv: Recv {
                    buffer: NullBuffer,
                    assembler: Assembler::new(ISS_1 + 1),
                    timer: None,
                    mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                    wnd_scale: WindowScale::default(),
                },
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
            },
            last_ack: ISS_1 + 2,
            last_wnd: WindowSize::ZERO,
        }))
    => Some(
        Segment::ack(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(0))
    ); "fin")]
    fn segment_arrives_when_syn_rcvd(
        incoming: Segment<()>,
        expected: Option<State<FakeInstant, NullBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let mut clock = FakeInstantCtx::default();
        let mut state = State::SynRcvd(SynRcvd {
            iss: ISS_2,
            irs: ISS_1,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(
                clock.now(),
                Estimator::RTO_INIT,
                DEFAULT_USER_TIMEOUT,
                DEFAULT_MAX_RETRIES,
            ),
            simultaneous_open: Some(()),
            buffer_sizes: Default::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            rcv_wnd_scale: WindowScale::default(),
            snd_wnd_scale: Some(WindowScale::default()),
        });
        clock.sleep(RTT);
        let (seg, _passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(incoming, clock.now());
        match expected {
            Some(new_state) => assert_eq!(new_state, state),
            None => assert_matches!(state, State::SynRcvd(_)),
        };
        seg
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
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
            },
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::new(1).unwrap(),
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
                buffer: NullBuffer,
                wl1: ISS_2 + 1,
                wl2: ISS_1 + 1,
                rtt_estimator: Estimator::default(),
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    wnd_scale: WindowScale::default(),
            },
            last_ack: ISS_2 + 3,
            last_wnd: WindowSize::ZERO,
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
        incoming: Segment<impl Payload>,
        expected: Option<State<FakeInstant, RingBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(2),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        let (seg, passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                incoming,
                FakeInstant::default(),
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
        // Tests the common behavior when data segment arrives in states that
        // have a receive state.
        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::DEFAULT,
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
            buffer: RingBuffer::new(TEST_BYTES.len()),
            assembler: Assembler::new(ISS_2 + 1),
            timer: None,
            mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            wnd_scale: WindowScale::default(),
        };
        for mut state in [
            State::Established(Established { snd: new_snd(), rcv: new_rcv() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin(), rcv: new_rcv() }),
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
        // Tests the common behavior when ack segment arrives in states that
        // have a send state.
        let new_snd = || Send {
            nxt: ISS_1 + 1,
            max: ISS_1 + 1,
            una: ISS_1 + 1,
            wnd: WindowSize::DEFAULT,
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
            buffer: NullBuffer,
            assembler: Assembler::new(ISS_2 + 1),
            timer: None,
            mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            wnd_scale: WindowScale::default(),
        };
        for mut state in [
            State::Established(Established { snd: new_snd(), rcv: new_rcv() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin(), rcv: new_rcv() }),
            State::Closing(Closing {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
                last_wnd_scale: WindowScale::default(),
            }),
            State::CloseWait(CloseWait {
                snd: new_snd(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
            }),
            State::LastAck(LastAck {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
            }),
        ] {
            assert_eq!(
                state.poll_send_with_default_options(
                    u32::try_from(TEST_BYTES.len()).unwrap(),
                    FakeInstant::default()
                ),
                Some(Segment::data(
                    ISS_1 + 1,
                    ISS_2 + 1,
                    UnscaledWindowSize::from(0),
                    SendPayload::Contiguous(TEST_BYTES)
                ))
            );
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::ack(
                        ISS_2 + 1,
                        ISS_1 + 1 + TEST_BYTES.len(),
                        UnscaledWindowSize::from(u16::MAX)
                    ),
                    FakeInstant::default(),
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
                State::Established(e) => e.snd.queue_fin(),
                State::CloseWait(c) => c.snd.queue_fin(),
                State::LastAck(l) => l.snd,
                State::FinWait1(f) => f.snd,
                State::Closing(c) => c.snd,
            };
            assert_eq!(snd.nxt, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.max, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.una, ISS_1 + 1 + TEST_BYTES.len());
            assert_eq!(snd.buffer.limits().len, 0);
        }
    }

    #[test_case(
        Segment::syn(ISS_2 + 2, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: None }),
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
        Segment::data(ISS_2 + 2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), "Hello".as_bytes()),
        None
    => None; "ignore data")]
    fn segment_arrives_when_close_wait(
        incoming: Segment<impl Payload>,
        expected: Option<State<FakeInstant, RingBuffer, NullBuffer, ()>>,
    ) -> Option<Segment<()>> {
        let mut state = State::CloseWait(CloseWait {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::DEFAULT,
        });
        let (seg, _passive_open) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                incoming,
                FakeInstant::default(),
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
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(syn_seg, clock.now());
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
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(syn_ack, clock.now());
        let ack_seg = seg.expect("failed to generate a ack segment");
        assert_eq!(passive_open, None);
        assert_eq!(ack_seg, Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(0)));
        assert_matches!(active, State::Established(ref established) if established == &Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                buffer: NullBuffer,
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,

                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            }
        });
        clock.sleep(RTT / 2);
        assert_eq!(
            passive.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                ack_seg,
                clock.now()
            ),
            (None, Some(())),
        );
        assert_matches!(passive, State::Established(ref established) if established == &Established {
            snd: Send {
                nxt: ISS_2 + 1,
                max: ISS_2 + 1,
                una: ISS_2 + 1,
                wnd: WindowSize::ZERO,
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_1 + 1),
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            }
        })
    }

    #[test]
    fn simultaneous_open() {
        let mut clock = FakeInstantCtx::default();
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
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(syn2, clock.now());
        let syn_ack1 = seg.expect("failed to generate syn ack");
        assert_eq!(passive_open, None);
        let (seg, passive_open) = state2
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(syn1, clock.now());
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
                clock.now()
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(0))), None)
        );
        assert_eq!(
            state2.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack1,
                clock.now()
            ),
            (Some(Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0))), None)
        );

        assert_matches!(state1, State::Established(established) if established == Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                buffer: NullBuffer,
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            }
        });

        assert_matches!(state2, State::Established(established) if established == Established {
            snd: Send {
                nxt: ISS_2 + 1,
                max: ISS_2 + 1,
                una: ISS_2 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_1 + 1),
                timer: None,
                mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            }
        });
    }

    const BUFFER_SIZE: usize = 16;
    const TEST_BYTES: &[u8] = "Hello".as_bytes();

    #[test]
    fn established_receive() {
        let clock = FakeInstantCtx::default();
        let mut established = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::ZERO,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });

        // Received an expected segment at rcv.nxt.
        assert_eq!(
            established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0), TEST_BYTES,),
                clock.now(),
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
                assert_eq!(available, &[&[][..]]);
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
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        let mut established = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1,
                wnd: WindowSize::ZERO,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        // Data queued but the window is not opened, nothing to send.
        assert_eq!(established.poll_send_with_default_options(u32::MAX, clock.now()), None);
        let open_window = |established: &mut State<FakeInstant, RingBuffer, RingBuffer, ()>,
                           ack: SeqNum,
                           win: usize,
                           now: FakeInstant| {
            assert_eq!(
                established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::ack(ISS_2 + 1, ack, UnscaledWindowSize::from_usize(win)),
                    now,
                ),
                (None, None),
            );
        };
        // Open up the window by 1 byte.
        open_window(&mut established, ISS_1 + 1, 1, clock.now());
        assert_eq!(
            established.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[1..2]),
            ))
        );

        // Open up the window by 10 bytes, but the MSS is limited to 2 bytes.
        open_window(&mut established, ISS_1 + 2, 10, clock.now());
        assert_eq!(
            established.poll_send_with_default_options(2, clock.now()),
            Some(Segment::data(
                ISS_1 + 2,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[2..4]),
            ))
        );

        assert_eq!(
            established.poll_send(
                1,
                clock.now(),
                &SocketOptions { nagle_enabled: false, ..SocketOptions::default() }
            ),
            Some(Segment::data(
                ISS_1 + 4,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[4..5]),
            ))
        );

        // We've exhausted our send buffer.
        assert_eq!(established.poll_send_with_default_options(1, clock.now()), None);
    }

    #[test]
    fn self_connect_retransmission() {
        let mut clock = FakeInstantCtx::default();
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
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), Some(syn.into()));

        // Bring the state to SYNRCVD.
        let (seg, passive_open) =
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(syn, clock.now());
        let syn_ack = seg.expect("expected SYN-ACK");
        assert_eq!(passive_open, None);
        // Retransmission timer should be installed.
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));
        clock.sleep(Estimator::RTO_INIT);
        // The SYN-ACK segment should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(syn_ack.into())
        );

        // Bring the state to ESTABLISHED and write some data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack,
                clock.now()
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
            | State::CloseWait(CloseWait { ref mut snd, last_ack: _, last_wnd: _ }) => {
                assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
            }
        }
        // We have no outstanding segments, so there is no retransmission timer.
        assert_eq!(state.poll_send_at(), None);
        // The retransmission timer should backoff exponentially.
        for i in 0..3 {
            assert_eq!(
                state.poll_send_with_default_options(u32::MAX, clock.now()),
                Some(Segment::data(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(TEST_BYTES),
                ))
            );
            assert_eq!(state.poll_send_at(), Some(clock.now() + (1 << i) * Estimator::RTO_INIT));
            clock.sleep((1 << i) * Estimator::RTO_INIT);
        }
        // The receiver acks the first byte of the payload.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + 1,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
            ),
            (None, None),
        );
        // The timer is rearmed, and the current RTO after 3 retransmissions
        // should be 4s (1s, 2s, 4s).
        assert_eq!(state.poll_send_at(), Some(clock.now() + 4 * Estimator::RTO_INIT));
        clock.sleep(4 * Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(1, clock.now()),
            Some(Segment::data(
                ISS_1 + 1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                SendPayload::Contiguous(&TEST_BYTES[1..2]),
            ))
        );
        // Currently, snd.nxt = ISS_1 + 2, snd.max = ISS_1 + 5, a segment
        // with ack number ISS_1 + 4 should bump snd.nxt immediately.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + 3,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
            ),
            (None, None)
        );
        // Since we retransmitted once more, the RTO is now 8s.
        assert_eq!(state.poll_send_at(), Some(clock.now() + 8 * Estimator::RTO_INIT));
        assert_eq!(
            state.poll_send_with_default_options(1, clock.now()),
            Some(Segment::data(
                ISS_1 + 1 + 3,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                SendPayload::Contiguous(&TEST_BYTES[3..4]),
            ))
        );
        // Finally the receiver ACKs all the outstanding data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
            ),
            (None, None)
        );
        // The retransmission timer should be removed.
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn passive_close() {
        let mut clock = FakeInstantCtx::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        let last_wnd = WindowSize::new(BUFFER_SIZE - 1).unwrap();
        // Transition the state machine to CloseWait by sending a FIN.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
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
        assert_eq!(state.close(CloseReason::Shutdown, &SocketOptions::default()), Ok(()));
        assert_eq!(
            state,
            State::LastAck(LastAck {
                snd: Send {
                    nxt: ISS_1 + 1,
                    max: ISS_1 + 1,
                    una: ISS_1 + 1,
                    wnd: WindowSize::DEFAULT,
                    buffer: send_buffer,
                    wl1: ISS_2,
                    wl2: ISS_1,
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
            })
        );
        // When the send window is not big enough, there should be no FIN.
        assert_eq!(
            state.poll_send_with_default_options(2, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 2,
                last_wnd >> WindowScale::default(),
                SendPayload::Contiguous(&TEST_BYTES[..2]),
            ))
        );
        // We should be able to send out all remaining bytes together with a FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 2,
                last_wnd >> WindowScale::default(),
                SendPayload::Contiguous(&TEST_BYTES[2..]),
            ))
        );
        // Now let's test we retransmit correctly by only acking the data.
        clock.sleep(RTT);
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + 2,
                    ISS_1 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));
        clock.sleep(Estimator::RTO_INIT);
        // The FIN should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(
                Segment::fin(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_2 + 2,
                    last_wnd >> WindowScale::default()
                )
                .into()
            )
        );

        // Finally, our FIN is acked.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + 2,
                    ISS_1 + 1 + TEST_BYTES.len() + 1,
                    UnscaledWindowSize::from(u16::MAX),
                ),
                clock.now(),
            ),
            (None, None)
        );
        // The connection is closed.
        assert_eq!(state, State::Closed(Closed { reason: None }));
    }

    #[test]
    fn syn_rcvd_active_close() {
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
        assert_eq!(state.close(CloseReason::Shutdown, &SocketOptions::default()), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, FakeInstant::default()),
            Some(Segment::fin(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX)).into())
        );
    }

    #[test]
    fn established_active_close() {
        let mut clock = FakeInstantCtx::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        assert_eq!(state.close(CloseReason::Shutdown, &SocketOptions::default()), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.close(CloseReason::Shutdown, &SocketOptions::default()),
            Err(CloseError::Closing)
        );

        // Poll for 2 bytes.
        assert_eq!(
            state.poll_send_with_default_options(2, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[..2])
            ))
        );

        // And we should send the rest of the buffer together with the FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[2..])
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
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::piggybacked_fin(
                ISS_1 + 2,
                ISS_2 + TEST_BYTES.len() + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[1..]),
            ))
        );

        // Now our FIN is acked, we should transition to FinWait2.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + TEST_BYTES.len() + 1,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
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
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(
                    ISS_2 + 2 * TEST_BYTES.len() + 1,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now(),
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
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_matches!(state, State::TimeWait(_));
        clock.sleep(SMALLEST_DURATION);
        // The state should become closed.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(state, State::Closed(Closed { reason: None }));
    }

    #[test]
    fn fin_wait_1_fin_ack_to_time_wait() {
        // Test that we can transition from FIN-WAIT-2 to TIME-WAIT directly
        // with one FIN-ACK segment.
        let mut state = State::FinWait1(FinWait1 {
            snd: Send {
                nxt: ISS_1 + 2,
                max: ISS_1 + 2,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                FakeInstant::default(),
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
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_1 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        assert_eq!(state.close(CloseReason::Shutdown, &SocketOptions::default()), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.close(CloseReason::Shutdown, &SocketOptions::default()),
            Err(CloseError::Closing)
        );

        let fin = state.poll_send_with_default_options(u32::MAX, clock.now());
        assert_eq!(
            fin,
            Some(Segment::piggybacked_fin(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(TEST_BYTES),
            ))
        );
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::piggybacked_fin(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE),
                    SendPayload::Contiguous(TEST_BYTES),
                ),
                clock.now(),
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
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_1 + TEST_BYTES.len() + 2,
                    UnscaledWindowSize::from_usize(BUFFER_SIZE - TEST_BYTES.len() - 1),
                ),
                clock.now(),
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
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_matches!(state, State::TimeWait(_));
        clock.sleep(SMALLEST_DURATION);
        // The state should become closed.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(state, State::Closed(Closed { reason: None }));
    }

    #[test]
    fn time_wait_restarts_timer() {
        let mut clock = FakeInstantCtx::default();
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
            time_wait.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 2, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                clock.now(),
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
                buffer: NullBuffer,
                wl1: ISS_2,
                wl2: ISS_1,
                rtt_estimator: Estimator::Measured { srtt: RTT, rtt_var: RTT / 2 },
                last_seq_ts: None,
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                wnd_scale: WindowScale::default(),
            },
            rcv: Recv {
                buffer: RingBuffer::default(),
                assembler: Assembler::new(ISS_2 + 5),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
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
        Segment::syn_ack(ISS_2, ISS_1 + 1, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: Some(WindowScale::default()) }).into() =>
        Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, UnscaledWindowSize::from(u16::MAX))); "retransmit syn_ack"
    )]
    // Regression test for https://fxbug.dev/107597
    fn ack_to_retransmitted_segment(
        mut state: State<FakeInstant, RingBuffer, NullBuffer, ()>,
        seg: Segment<&[u8]>,
    ) -> Option<Segment<()>> {
        let (reply, _): (_, Option<()>) = state
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
                seg,
                FakeInstant::default(),
            );
        reply
    }

    #[test]
    fn fast_retransmit() {
        let mut clock = FakeInstantCtx::default();
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
            },
            rcv: Recv {
                buffer: RingBuffer::default(),
                assembler: Assembler::new(ISS_2),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });

        assert_eq!(
            state.poll_send_with_default_options(
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now()
            ),
            Some(Segment::data(
                ISS_1,
                ISS_2,
                UnscaledWindowSize::from(u16::MAX),
                SendPayload::Contiguous(&[b'A'; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE])
            ))
        );

        let mut dup_ack = |expected_byte: u8| {
            clock.sleep(Duration::from_millis(10));
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)),
                    clock.now()
                ),
                (None, None)
            );

            assert_eq!(
                state.poll_send_with_default_options(
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    clock.now()
                ),
                Some(Segment::data(
                    ISS_1
                        + u32::from(expected_byte - b'A')
                            * u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    ISS_2,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(
                        &[expected_byte; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE]
                    )
                ))
            );
        };

        // The first two dup acks should allow two previously unsent segments
        // into the network.
        dup_ack(b'B');
        dup_ack(b'C');
        // The third dup ack will cause a fast retransmit of the first segment
        // at snd.una.
        dup_ack(b'A');
        // Afterwards, we continue to send previously unsent data if allowed.
        dup_ack(b'D');

        // Make sure the window size is deflated after loss is recovered.
        clock.sleep(Duration::from_millis(10));
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2,
                    ISS_1 + u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    UnscaledWindowSize::from(u16::MAX)
                ),
                clock.now()
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
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1,
                max: ISS_1,
                una: ISS_1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::default(),
                assembler: Assembler::new(ISS_2),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
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
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now(),
                socket_options,
            ),
            None,
        );
        // so the above poll_send call will install a timer, which will fire
        // after `keep_alive.idle`.
        assert_eq!(state.poll_send_at(), Some(clock.now().add(keep_alive.idle.into())));

        // Now we receive an ACK after an hour.
        clock.sleep(Duration::from_secs(60 * 60));
        assert_eq!(
            state.on_segment::<&[u8], ClientlessBufferProvider>(
                Segment::ack(ISS_2, ISS_1, UnscaledWindowSize::from(u16::MAX)).into(),
                clock.now(),
                socket_options,
                false, /* defunct */
            ),
            (None, None),
        );
        // the timer is reset to fire in 2 hours.
        assert_eq!(state.poll_send_at(), Some(clock.now().add(keep_alive.idle.into())),);
        clock.sleep(keep_alive.idle.into());

        // Then there should be `count` probes being sent out after `count`
        // `interval` seconds.
        for _ in 0..keep_alive.count.get() {
            assert_eq!(
                state.poll_send(
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    clock.now(),
                    socket_options,
                ),
                Some(Segment::ack(ISS_1 - 1, ISS_2, UnscaledWindowSize::from(u16::MAX)).into())
            );
            clock.sleep(keep_alive.interval.into());
            assert_matches!(state, State::Established(_));
        }

        // At this time the connection is closed and we don't have anything to
        // send.
        assert_eq!(
            state.poll_send(
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now(),
                socket_options,
            ),
            None,
        );
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
    }

    /// A `SendBuffer` that doesn't allow peeking some number of bytes.
    #[derive(Debug)]
    struct ReservingBuffer<B> {
        buffer: B,
        reserved_bytes: usize,
    }

    impl<B: Buffer> Buffer for ReservingBuffer<B> {
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

    impl<B: Takeable> Takeable for ReservingBuffer<B> {
        fn take(&mut self) -> Self {
            let Self { buffer, reserved_bytes } = self;
            Self { buffer: buffer.take(), reserved_bytes: *reserved_bytes }
        }
    }

    impl<B: SendBuffer> SendBuffer for ReservingBuffer<B> {
        fn mark_read(&mut self, count: usize) {
            self.buffer.mark_read(count)
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(SendPayload<'a>) -> R,
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
            f: impl FnOnce(Segment<SendPayload<'_>>),
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

            f(snd
                .poll_send(
                    ISS_1,
                    WindowSize::DEFAULT,
                    u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                    FakeInstant::default(),
                    &SocketOptions::default(),
                )
                .expect("has data"))
        }

        let f = |segment: Segment<SendPayload<'_>>| {
            let Segment { contents, ack: _, seq: _, wnd: _, options: _ } = segment;
            let contents_len = contents.data().len();

            if has_fin && reserved_bytes == 0 {
                assert_eq!(
                    contents.len(),
                    u32::try_from(contents_len + 1).unwrap(),
                    "FIN not accounted for"
                );
            } else {
                assert_eq!(contents.len(), u32::try_from(contents_len).unwrap());
            }

            let mut target = vec![0; contents_len];
            contents.data().partial_copy(0, target.as_mut_slice());
            assert_eq!(target, vec![VALUE; contents_len]);
        };
        match has_fin {
            true => with_poll_send_result::<true>(f, reserved_bytes),
            false => with_poll_send_result::<false>(f, reserved_bytes),
        }
    }

    #[test]
    fn zero_window_probe() {
        let mut clock = FakeInstantCtx::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::ZERO,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(state.poll_send_at(), Some(clock.now().add(Estimator::RTO_INIT)));

        // Send the first probe after first RTO.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver still has a zero window.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 1, UnscaledWindowSize::from(0)),
                clock.now()
            ),
            (None, None)
        );
        // The timer should backoff exponentially.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(state.poll_send_at(), Some(clock.now().add(Estimator::RTO_INIT * 2)));

        // No probe should be sent before the timeout.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);

        // Probe sent after the timeout.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver now opens its receive window.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 2, UnscaledWindowSize::from(u16::MAX)),
                clock.now()
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), None);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 2,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[1..])
            ))
        );
    }

    #[test]
    fn nagle() {
        let clock = FakeInstantCtx::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        let mut socket_options = SocketOptions::default();
        assert_eq!(
            state.poll_send(3, clock.now(), &socket_options),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[0..3])
            ))
        );
        assert_eq!(state.poll_send(3, clock.now(), &socket_options), None);
        socket_options.nagle_enabled = false;
        assert_eq!(
            state.poll_send(3, clock.now(), &socket_options),
            Some(Segment::data(
                ISS_1 + 4,
                ISS_2 + 1,
                UnscaledWindowSize::from_usize(BUFFER_SIZE),
                SendPayload::Contiguous(&TEST_BYTES[3..5])
            ))
        );
    }

    #[test]
    fn mss_option() {
        let clock = FakeInstantCtx::default();
        let (syn_sent, syn) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            Mss(nonzero_ext::nonzero!(1_u16)),
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            &SocketOptions::default(),
        );
        let mut state = State::<_, RingBuffer, RingBuffer, ()>::SynSent(syn_sent);

        // Bring the state to SYNRCVD.
        let (seg, passive_open) =
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(syn, clock.now());
        let syn_ack = seg.expect("expected SYN-ACK");
        assert_eq!(passive_open, None);

        // Bring the state to ESTABLISHED and write some data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack,
                clock.now()
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
            | State::CloseWait(CloseWait { ref mut snd, last_ack: _, last_wnd: _ }) => {
                assert_eq!(snd.buffer.enqueue_data(TEST_BYTES), TEST_BYTES.len());
            }
        }
        // Since the MSS of the connection is 1, we can only get the first byte.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_1 + 1,
                UnscaledWindowSize::from(u16::MAX),
                SendPayload::Contiguous(&TEST_BYTES[..1]),
            ))
        );
    }

    // We can use a smaller and a larger RTT so that when using the smaller one,
    // we can reach maximum retransmit retires and when using the larger one, we
    // timeout before reaching maximum retries.
    #[test_case(Duration::from_millis(1), false, true; "retrans_max_retries")]
    #[test_case(Duration::from_secs(1), false, false; "retrans_no_max_retries")]
    #[test_case(Duration::from_millis(1), true, true; "zwp_max_retries")]
    #[test_case(Duration::from_secs(1), true, false; "zwp_no_max_retires")]
    fn user_timeout(rtt: Duration, zero_window_probe: bool, max_retries: bool) {
        let mut clock = FakeInstantCtx::default();
        let mut send_buffer = RingBuffer::new(BUFFER_SIZE);
        assert_eq!(send_buffer.enqueue_data(TEST_BYTES), 5);
        // Set up the state machine to start with Established.
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
                buffer: send_buffer.clone(),
                wl1: ISS_2,
                wl2: ISS_1,
                last_seq_ts: None,
                rtt_estimator: Estimator::Measured { srtt: rtt, rtt_var: Duration::ZERO },
                timer: None,
                congestion_control: CongestionControl::cubic_with_mss(
                    DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                ),
                wnd_scale: WindowScale::default(),
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        let mut times = 1;
        let start = clock.now();
        while let Some(seg) = state.poll_send_with_default_options(u32::MAX, clock.now()) {
            if zero_window_probe {
                let zero_window_ack =
                    Segment::ack(seg.ack.unwrap(), seg.seq, UnscaledWindowSize::from(0));
                assert_eq!(
                    state.on_segment_with_default_options::<(), ClientlessBufferProvider>(
                        zero_window_ack,
                        clock.now()
                    ),
                    (None, None)
                );
            }
            let deadline = state.poll_send_at().expect("must have a retransmission timer");
            clock.sleep(deadline.duration_since(clock.now()));
            times += 1;
        }
        let elapsed = clock.now().duration_since(start);
        assert_eq!(elapsed, DEFAULT_USER_TIMEOUT);
        if max_retries {
            assert_eq!(times, DEFAULT_MAX_RETRIES.get());
        } else {
            assert!(times < DEFAULT_MAX_RETRIES.get());
        }
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }))
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(
                    TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                ),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        }); "established")]
    #[test_case(
        State::FinWait1(FinWait1 {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(
                    TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                ),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        }); "fin_wait_1")]
    #[test_case(
        State::FinWait2(FinWait2 {
            last_seq: ISS_1 + 1,
            rcv: Recv {
                buffer: RingBuffer::new(
                    TEST_BYTES.len() + 2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize,
                ),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
            timeout_at: None,
        }); "fin_wait_2")]
    fn delayed_ack(mut state: State<FakeInstant, RingBuffer, RingBuffer, ()>) {
        let mut clock = FakeInstantCtx::default();
        // TODO(https://fxbug.dev/124309): Enable delayed ack by default.
        let socket_options = SocketOptions { delayed_ack: true, ..SocketOptions::default() };
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(TEST_BYTES),
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now().add(ACK_DELAY_THRESHOLD)));
        clock.sleep(ACK_DELAY_THRESHOLD);
        assert_eq!(
            state.poll_send(u32::MAX, clock.now(), &socket_options),
            Some(
                Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    UnscaledWindowSize::from_u32(2 * u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE)),
                )
                .into()
            )
        );
        let full_segment_sized_payload =
            vec![b'0'; u32::from(DEVICE_MAXIMUM_SEGMENT_SIZE) as usize];
        // The first full sized segment should not trigger an immediate ACK,
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(&full_segment_sized_payload[..]),
                ),
                clock.now(),
                &socket_options,
                false, /* defunct */
            ),
            (None, None)
        );
        // ... but just a timer.
        assert_eq!(state.poll_send_at(), Some(clock.now().add(ACK_DELAY_THRESHOLD)));
        // Now the second full sized segment arrives, an ACK should be sent
        // immediately.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1 + TEST_BYTES.len() + full_segment_sized_payload.len(),
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(&full_segment_sized_payload[..]),
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
                None
            )
        );
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn immediate_ack_if_out_of_order_or_fin() {
        let clock = FakeInstantCtx::default();
        // TODO(https://fxbug.dev/124309): Enable delayed ack by default.
        let socket_options = SocketOptions { delayed_ack: true, ..SocketOptions::default() };
        let mut state: State<_, _, _, ()> = State::Established(Established {
            snd: Send {
                nxt: ISS_1 + 1,
                max: ISS_1 + 1,
                una: ISS_1 + 1,
                wnd: WindowSize::DEFAULT,
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(TEST_BYTES.len() + 1),
                assembler: Assembler::new(ISS_2 + 1),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
        });
        // Upon receiving an out-of-order segment, we should send an ACK
        // immediately.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 2,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(&TEST_BYTES[1..])
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
                None
            )
        );
        assert_eq!(state.poll_send_at(), None);
        // The next segment fills a gap, so it should trigger an immediate
        // ACK.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
                Segment::data(
                    ISS_2 + 1,
                    ISS_1 + 1,
                    UnscaledWindowSize::from(u16::MAX),
                    SendPayload::Contiguous(&TEST_BYTES[..1])
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
                None
            )
        );
        assert_eq!(state.poll_send_at(), None);
        // We should also respond immediately with an ACK to a FIN.
        assert_eq!(
            state.on_segment::<_, ClientlessBufferProvider>(
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
                None
            )
        );
        assert_eq!(state.poll_send_at(), None);
    }

    #[test]
    fn fin_wait2_timeout() {
        let mut clock = FakeInstantCtx::default();
        let mut state: State<_, _, NullBuffer, ()> = State::FinWait2(FinWait2 {
            last_seq: ISS_1,
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_2),
                timer: None,
                mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                wnd_scale: WindowScale::default(),
            },
            timeout_at: None,
        });
        assert_eq!(
            state.close(CloseReason::Close { now: clock.now() }, &SocketOptions::default()),
            Err(CloseError::Closing)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now().add(DEFAULT_FIN_WAIT2_TIMEOUT)));
        clock.sleep(DEFAULT_FIN_WAIT2_TIMEOUT);
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
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
        let mut retransmissions = 0;
        clock.sleep(Duration::from_millis(1));
        while let Some(_seg) = state.poll_send_with_default_options(u32::MAX, clock.now()) {
            let deadline = state.poll_send_at().expect("must have a retransmission timer");
            clock.sleep(deadline.duration_since(clock.now()));
            retransmissions += 1;
        }
        assert_eq!(state, State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }));
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
    fn window_scale(
        buffer_size: usize,
        syn_window_scale: WindowScale,
        syn_ack_window_scale: Option<WindowScale>,
    ) -> (WindowScale, WindowScale) {
        let mut clock = FakeInstantCtx::default();
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
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(
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
            );
        assert_eq!(passive_open, None);
        assert_matches!(seg, Some(_));

        let established: Established<FakeInstant, RingBuffer, NullBuffer> =
            assert_matches!(active, State::Established(established) => established);

        (established.rcv.wnd_scale, established.snd.wnd_scale)
    }
}
