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
    num::{NonZeroU32, NonZeroUsize, TryFromIntError},
    time::Duration,
};

use assert_matches::assert_matches;
use derivative::Derivative;
use explicit::ResultExt as _;

use crate::{
    transport::tcp::{
        buffer::{Assembler, IntoBuffers, ReceiveBuffer, SendBuffer, SendPayload},
        congestion::CongestionControl,
        rtt::Estimator,
        segment::{Options, Payload, Segment},
        seqnum::{SeqNum, WindowSize},
        BufferSizes, Control, KeepAlive, Mss, SocketOptions, UserError,
    },
    Instant,
};

/// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-81):
/// MSL
///       Maximum Segment Lifetime, the time a TCP segment can exist in
///       the internetwork system.  Arbitrarily defined to be 2 minutes.
const MSL: Duration = Duration::from_secs(2 * 60);

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
    // TODO(https://fxbug.dev/103982): Read from the field.
    #[allow(dead_code)]
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
    ) -> (SynSent<I, ActiveOpen>, Segment<()>) {
        (
            SynSent {
                iss,
                timestamp: Some(now),
                retrans_timer: RetransTimer::new(now, Estimator::RTO_INIT),
                active_open,
                buffer_sizes,
                device_mss,
                default_mss,
            },
            Segment::syn(iss, WindowSize::DEFAULT, Options { mss: Some(device_mss) }),
        )
    }

    pub(crate) fn listen(
        iss: SeqNum,
        buffer_sizes: BufferSizes,
        device_mss: Mss,
        default_mss: Mss,
    ) -> Listen {
        Listen { iss, buffer_sizes, device_mss, default_mss }
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
        let Listen { iss, buffer_sizes, device_mss, default_mss } = *self;
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
            return ListenOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                Segment::syn_ack(iss, seq + 1, WindowSize::DEFAULT, Options { mss: Some(smss) }),
                SynRcvd {
                    iss,
                    irs: seq,
                    timestamp: Some(now),
                    retrans_timer: RetransTimer::new(now, Estimator::RTO_INIT),
                    simultaneous_open: None,
                    buffer_sizes,
                    smss,
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
pub(crate) struct SynSent<I: Instant, ActiveOpen> {
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
}

/// Dispositions of [`SynSent::on_segment`].
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum SynSentOnSegmentDisposition<I: Instant, R: ReceiveBuffer, S: SendBuffer, ActiveOpen> {
    SendAckAndEnterEstablished(Segment<()>, Established<I, R, S>),
    SendSynAckAndEnterSynRcvd(Segment<()>, SynRcvd<I, ActiveOpen>),
    SendRstAndEnterClosed(Segment<()>, Closed<UserError>),
    EnterClosed(Closed<UserError>),
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
            retrans_timer: _,
            ref mut active_open,
            buffer_sizes,
            device_mss,
            default_mss,
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
                            Closed { reason: UserError::ConnectionReset },
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
                        reason: UserError::ConnectionReset,
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
                            let established = Established {
                                snd: Send {
                                    nxt: iss + 1,
                                    max: iss + 1,
                                    una: seg_ack,
                                    wnd: seg_wnd,
                                    wl1: seg_seq,
                                    wl2: seg_ack,
                                    buffer: snd_buffer,
                                    last_seq_ts: None,
                                    rtt_estimator,
                                    timer: None,
                                    congestion_control: CongestionControl::cubic_with_mss(smss),
                                },
                                rcv: Recv {
                                    buffer: rcv_buffer,
                                    assembler: Assembler::new(irs + 1),
                                },
                            };
                            let ack_seg = Segment::ack(
                                established.snd.nxt,
                                established.rcv.nxt(),
                                established.rcv.wnd(),
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
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-68):
                        //   Otherwise enter SYN-RECEIVED, form a SYN,ACK
                        //   segment
                        //     <SEQ=ISS><ACK=RCV.NXT><CTL=SYN,ACK>
                        //   and send it.  If there are other controls or text
                        //   in the segment, queue them for processing after the
                        //   ESTABLISHED state has been reached, return.
                        SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
                            Segment::syn_ack(
                                iss,
                                seg_seq + 1,
                                WindowSize::DEFAULT,
                                Options { mss: Some(smss) },
                            ),
                            SynRcvd {
                                iss,
                                irs: seg_seq,
                                timestamp: Some(now),
                                retrans_timer: RetransTimer::new(now, Estimator::RTO_INIT),
                                simultaneous_open: Some(active_open.take()),
                                buffer_sizes,
                                smss,
                            },
                        )
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
pub(crate) struct SynRcvd<I: Instant, ActiveOpen> {
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
}

impl<I: Instant, R: ReceiveBuffer, S: SendBuffer, ActiveOpen> From<SynRcvd<I, Infallible>>
    for State<I, R, S, ActiveOpen>
{
    fn from(
        SynRcvd { iss, irs, timestamp, retrans_timer, simultaneous_open, buffer_sizes, smss }: SynRcvd<
            I,
            Infallible,
        >,
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
struct Send<I: Instant, S: SendBuffer, const FIN_QUEUED: bool> {
    nxt: SeqNum,
    max: SeqNum,
    una: SeqNum,
    wnd: WindowSize,
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
struct RetransTimer<I: Instant> {
    at: I,
    rto: Duration,
}

impl<I: Instant> RetransTimer<I> {
    fn new(now: I, rto: Duration) -> Self {
        let at = now.checked_add(rto).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", rto, now);
        });
        Self { at, rto }
    }

    fn backoff(&mut self, now: I) {
        let Self { at, rto } = self;
        *rto *= 2;
        *at = now.checked_add(*rto).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", rto, now);
        });
    }

    fn rearm(&mut self, now: I) {
        let Self { at: _, rto } = *self;
        *self = Self::new(now, rto);
    }
}

/// Possible timers for a sender.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum SendTimer<I: Instant> {
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
struct KeepAliveTimer<I: Instant> {
    at: I,
    already_sent: u8,
}

impl<I: Instant> KeepAliveTimer<I> {
    fn idle(now: I, keep_alive: &KeepAlive) -> Self {
        let at = now.checked_add(keep_alive.idle.into()).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", keep_alive.idle, now);
        });
        Self { at, already_sent: 0 }
    }
}

impl<I: Instant> SendTimer<I> {
    fn expiry(&self) -> I {
        match self {
            SendTimer::Retrans(RetransTimer { at, rto: _ })
            | SendTimer::KeepAlive(KeepAliveTimer { at, already_sent: _ })
            | SendTimer::ZeroWindowProbe(RetransTimer { at, rto: _ }) => *at,
        }
    }
}

/// TCP control block variables that are responsible for receiving.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct Recv<R: ReceiveBuffer> {
    buffer: R,
    assembler: Assembler,
}

impl<R: ReceiveBuffer> Recv<R> {
    fn wnd(&self) -> WindowSize {
        WindowSize::new(self.buffer.capacity() - self.buffer.len()).unwrap_or(WindowSize::MAX)
    }

    fn nxt(&self) -> SeqNum {
        self.assembler.nxt()
    }

    fn take(&mut self) -> Self {
        let Self { buffer, assembler } = self;
        Self {
            buffer: buffer.take(),
            assembler: core::mem::replace(assembler, Assembler::new(SeqNum::new(0))),
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
pub(crate) struct Established<I: Instant, R: ReceiveBuffer, S: SendBuffer> {
    snd: Send<I, S, { FinQueued::NO }>,
    rcv: Recv<R>,
}

impl<I: Instant, S: SendBuffer, const FIN_QUEUED: bool> Send<I, S, FIN_QUEUED> {
    /// Returns true if the connection should still be alive per the send state.
    fn still_alive(&self, keep_alive: &KeepAlive) -> bool {
        match self.timer {
            Some(SendTimer::KeepAlive(keep_alive_timer)) => {
                !keep_alive.enabled || keep_alive_timer.already_sent < keep_alive.count.get()
            }
            Some(SendTimer::Retrans(_)) | Some(SendTimer::ZeroWindowProbe(_)) | None => true,
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
        SocketOptions { keep_alive, nagle_enabled }: &SocketOptions,
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
        } = self;
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
                debug_assert!(buffer.len() > 0 || FIN_QUEUED);
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
                if keep_alive.enabled && !FIN_QUEUED && buffer.len() == 0 {
                    if *at <= now {
                        *at = now.checked_add(keep_alive.interval.into()).unwrap_or_else(|| {
                            panic!(
                                "clock wraps around when adding {:?} to {:?}",
                                keep_alive.interval, now
                            );
                        });
                        *already_sent = already_sent.saturating_add(1);
                        // Per RFC 9293 Section 3.8.4:
                        //   Such a segment generally contains SEG.SEQ = SND.NXT-1
                        return Some(Segment::ack(*snd_nxt - 1, rcv_nxt, rcv_wnd).into());
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
        let available = u32::try_from(buffer.len() + usize::from(FIN_QUEUED) - offset)
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
                *timer =
                    Some(SendTimer::ZeroWindowProbe(RetransTimer::new(now, rtt_estimator.rto())))
            }
            return None;
        }
        let has_fin = FIN_QUEUED && can_send == available;
        let seg = buffer.peek_with(offset, |readable| {
            let bytes_to_send = u32::min(
                can_send - u32::from(has_fin),
                u32::try_from(readable.len()).unwrap_or(u32::MAX),
            );
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
                rcv_wnd,
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
                *timer = Some(SendTimer::Retrans(RetransTimer::new(now, rtt_estimator.rto())))
            }
        }
        Some(seg)
    }

    fn process_ack(
        &mut self,
        seg_seq: SeqNum,
        seg_ack: SeqNum,
        seg_wnd: WindowSize,
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
        } = self;
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
            Some(Segment::ack(*snd_nxt, rcv_nxt, rcv_wnd))
        } else if seg_ack.after(*snd_una) {
            // The unwrap is safe because the result must be positive.
            let acked = u32::try_from(seg_ack - *snd_una)
                .ok_checked::<TryFromIntError>()
                .and_then(NonZeroU32::new)
                .unwrap_or_else(|| {
                    panic!("seg_ack({:?}) - snd_una({:?}) must be positive", seg_ack, snd_una);
                });
            let fin_acked = FIN_QUEUED && seg_ack == *snd_una + buffer.len() + 1;
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
pub(crate) struct CloseWait<I: Instant, S: SendBuffer> {
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
pub(crate) struct LastAck<I: Instant, S: SendBuffer> {
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
pub(crate) struct FinWait1<I: Instant, R: ReceiveBuffer, S: SendBuffer> {
    snd: Send<I, S, { FinQueued::YES }>,
    rcv: Recv<R>,
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
pub(crate) struct FinWait2<R: ReceiveBuffer> {
    last_seq: SeqNum,
    rcv: Recv<R>,
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
pub(crate) struct Closing<I: Instant, S: SendBuffer> {
    snd: Send<I, S, { FinQueued::YES }>,
    last_ack: SeqNum,
    last_wnd: WindowSize,
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
pub(crate) struct TimeWait<I: Instant> {
    last_seq: SeqNum,
    last_ack: SeqNum,
    last_wnd: WindowSize,
    expiry: I,
}

fn new_time_wait_expiry<I: Instant>(now: I) -> I {
    now.checked_add(MSL * 2).unwrap_or_else(|| {
        panic!("clock wraps around when adding {:?} * 2 ({:?}) to {:?}", MSL, MSL * 2, now);
    })
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum State<I: Instant, R: ReceiveBuffer, S: SendBuffer, ActiveOpen> {
    Closed(Closed<UserError>),
    Listen(Listen),
    SynRcvd(SynRcvd<I, ActiveOpen>),
    SynSent(SynSent<I, ActiveOpen>),
    Established(Established<I, R, S>),
    CloseWait(CloseWait<I, S>),
    LastAck(LastAck<I, S>),
    FinWait1(FinWait1<I, R, S>),
    FinWait2(FinWait2<R>),
    Closing(Closing<I, S>),
    TimeWait(TimeWait<I>),
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
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
        keep_alive: &KeepAlive,
    ) -> (Option<Segment<()>>, Option<BP::PassiveOpen>)
    where
        BP::PassiveOpen: Debug,
        ActiveOpen: IntoBuffers<R, S>,
    {
        let mut passive_open = None;
        let seg = (|| {
            let (mut rcv_nxt, rcv_wnd, snd_nxt) = match self {
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
                    buffer_sizes: _,
                    smss: _,
                }) => (*irs + 1, WindowSize::DEFAULT, *iss + 1),
                State::Established(Established { rcv, snd }) => (rcv.nxt(), rcv.wnd(), snd.nxt),
                State::CloseWait(CloseWait { snd, last_ack, last_wnd }) => {
                    (*last_ack, *last_wnd, snd.nxt)
                }
                State::LastAck(LastAck { snd, last_ack, last_wnd })
                | State::Closing(Closing { snd, last_ack, last_wnd }) => {
                    (*last_ack, *last_wnd, snd.nxt)
                }
                State::FinWait1(FinWait1 { rcv, snd }) => (rcv.nxt(), rcv.wnd(), snd.nxt),
                State::FinWait2(FinWait2 { last_seq, rcv }) => (rcv.nxt(), rcv.wnd(), *last_seq),
                State::TimeWait(TimeWait { last_seq, last_ack, last_wnd, expiry: _ }) => {
                    (*last_ack, *last_wnd, *last_seq)
                }
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
                            Some(Segment::ack(snd_nxt, rcv_nxt, rcv_wnd))
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
                *self = State::Closed(Closed { reason: UserError::ConnectionReset });
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
                *self = State::Closed(Closed { reason: UserError::ConnectionReset });
                return Some(Segment::rst(snd_nxt));
            }
            // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-72):
            //   fifth check the ACK field
            match seg_ack {
                Some(seg_ack) => match self {
                    State::Closed(_) | State::Listen(_) | State::SynSent(_) => {
                        // This unreachable assert is justified by note (1).
                        unreachable!("encountered an alread-handled state: {:?}", self)
                    }
                    State::SynRcvd(SynRcvd {
                        iss,
                        irs,
                        timestamp: syn_rcvd_ts,
                        retrans_timer: _,
                        simultaneous_open,
                        buffer_sizes,
                        smss,
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
                            *self = State::Established(Established {
                                snd: Send {
                                    nxt: *iss + 1,
                                    max: *iss + 1,
                                    una: seg_ack,
                                    wnd: seg_wnd,
                                    wl1: seg_seq,
                                    wl2: seg_ack,
                                    buffer: snd_buffer,
                                    last_seq_ts: None,
                                    rtt_estimator,
                                    timer: None,
                                    congestion_control: CongestionControl::cubic_with_mss(*smss),
                                },
                                rcv: Recv {
                                    buffer: rcv_buffer,
                                    assembler: Assembler::new(*irs + 1),
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
                        let fin_seq = snd.una + snd.buffer.len() + 1;
                        if let Some(ack) = snd.process_ack(
                            seg_seq, seg_ack, seg_wnd, pure_ack, rcv_nxt, rcv_wnd, now, keep_alive,
                        ) {
                            return Some(ack);
                        } else if seg_ack == fin_seq {
                            *self = State::Closed(Closed { reason: UserError::ConnectionClosed });
                            return None;
                        }
                    }
                    State::FinWait1(FinWait1 { snd, rcv }) => {
                        let fin_seq = snd.una + snd.buffer.len() + 1;
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
                            *self = State::FinWait2(FinWait2 { last_seq, rcv: rcv.take() });
                        }
                    }
                    State::Closing(Closing { snd, last_ack, last_wnd }) => {
                        let fin_seq = snd.una + snd.buffer.len() + 1;
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
                        unreachable!("encountered an alread-handled state: {:?}", self)
                    }
                    State::Established(Established { snd: _, rcv })
                    | State::FinWait1(FinWait1 { snd: _, rcv })
                    | State::FinWait2(FinWait2 { last_seq: _, rcv }) => {
                        // Write the segment data in the buffer and keep track if it fills
                        // any hole in the assembler.
                        if contents.data().len() > 0 {
                            let offset = usize::try_from(seg_seq - rcv.nxt()).unwrap_or_else(|TryFromIntError {..}| {
                                panic!("The segment was trimmed to fit the window, thus seg.seq({:?}) must not come before rcv.nxt({:?})", seg_seq, rcv.nxt());
                            });
                            let nwritten = rcv.buffer.write_at(offset, contents.data());
                            let readable = rcv.assembler.insert(seg_seq..seg_seq + nwritten);
                            rcv.buffer.make_readable(readable);
                            rcv_nxt = rcv.nxt();
                        }
                        Some(Segment::ack(snd_nxt, rcv.nxt(), rcv.wnd()))
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
                        unreachable!("encountered an alread-handled state: {:?}", self)
                    }
                    State::Established(Established { snd, rcv }) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-75):
                        //   Enter the CLOSE-WAIT state.
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd = rcv.wnd().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        *self = State::CloseWait(CloseWait { snd: snd.take(), last_ack, last_wnd });
                        Some(Segment::ack(snd_nxt, last_ack, last_wnd))
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
                        *self = State::Closing(Closing { snd: snd.take(), last_ack, last_wnd });
                        Some(Segment::ack(snd_nxt, last_ack, last_wnd))
                    }
                    State::FinWait2(FinWait2 { last_seq, rcv }) => {
                        let last_ack = rcv.nxt() + 1;
                        let last_wnd = rcv.wnd().checked_sub(1).unwrap_or(WindowSize::ZERO);
                        *self = State::TimeWait(TimeWait {
                            last_seq: *last_seq,
                            last_ack,
                            last_wnd,
                            expiry: new_time_wait_expiry(now),
                        });
                        Some(Segment::ack(snd_nxt, last_ack, last_wnd))
                    }
                    State::TimeWait(TimeWait { last_seq, last_ack, last_wnd, expiry }) => {
                        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-76):
                        //   TIME-WAIT STATE
                        //     Remain in the TIME-WAIT state.  Restart the 2 MSL time-wait
                        //     timeout.
                        *expiry = new_time_wait_expiry(now);
                        Some(Segment::ack(*last_seq, *last_ack, *last_wnd))
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
        match self {
            State::SynSent(SynSent {
                iss,
                timestamp,
                retrans_timer,
                active_open: _,
                buffer_sizes: _,
                device_mss,
                default_mss: _,
            }) => (retrans_timer.at <= now).then(|| {
                *timestamp = None;
                retrans_timer.backoff(now);
                Segment::syn(*iss, WindowSize::DEFAULT, Options { mss: Some(*device_mss) }).into()
            }),
            State::SynRcvd(SynRcvd {
                iss,
                irs,
                timestamp,
                retrans_timer,
                simultaneous_open: _,
                buffer_sizes: _,
                smss,
            }) => (retrans_timer.at <= now).then(|| {
                *timestamp = None;
                retrans_timer.backoff(now);
                Segment::syn_ack(*iss, *irs + 1, WindowSize::DEFAULT, Options { mss: Some(*smss) })
                    .into()
            }),
            State::Established(Established { snd, rcv }) => {
                snd.poll_send(rcv.nxt(), rcv.wnd(), limit, now, socket_options)
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd }) => {
                snd.poll_send(*last_ack, *last_wnd, limit, now, socket_options)
            }
            State::LastAck(LastAck { snd, last_ack, last_wnd })
            | State::Closing(Closing { snd, last_ack, last_wnd }) => {
                snd.poll_send(*last_ack, *last_wnd, limit, now, socket_options)
            }
            State::FinWait1(FinWait1 { snd, rcv }) => {
                snd.poll_send(rcv.nxt(), rcv.wnd(), limit, now, socket_options)
            }
            State::Closed(_) | State::Listen(_) | State::FinWait2(_) | State::TimeWait(_) => None,
        }
    }

    /// Polls the state machine to check if the connection should be closed.
    fn poll_close(
        &mut self,
        now: I,
        SocketOptions { keep_alive, nagle_enabled: _ }: &SocketOptions,
    ) -> bool {
        let alive = match self {
            State::Established(Established { snd, rcv: _ }) => snd.still_alive(keep_alive),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                snd.still_alive(keep_alive)
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _ }) => {
                snd.still_alive(keep_alive)
            }
            State::FinWait1(FinWait1 { snd, rcv: _ }) => snd.still_alive(keep_alive),
            State::TimeWait(TimeWait { last_seq: _, last_ack: _, last_wnd: _, expiry }) => {
                *expiry > now
            }
            State::SynSent(_)
            | State::SynRcvd(_)
            | State::Closed(_)
            | State::Listen(_)
            | State::FinWait2(_) => true,
        };
        if !alive {
            *self = State::Closed(Closed { reason: UserError::ConnectionClosed });
        }
        !alive
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
        match self {
            State::Established(Established { snd, rcv: _ })
            | State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                Some(snd.timer?.expiry())
            }
            State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _ }) => {
                Some(snd.timer?.expiry())
            }
            State::FinWait1(FinWait1 { snd, rcv: _ }) => Some(snd.timer?.expiry()),
            State::SynRcvd(syn_rcvd) => Some(syn_rcvd.retrans_timer.at),
            State::SynSent(syn_sent) => Some(syn_sent.retrans_timer.at),
            State::Closed(_) | State::Listen(_) | State::FinWait2(_) => None,
            State::TimeWait(TimeWait { last_seq: _, last_ack: _, last_wnd: _, expiry }) => {
                Some(*expiry)
            }
        }
    }

    /// Corresponds to the [CLOSE](https://tools.ietf.org/html/rfc793#page-60)
    /// user call.
    pub(super) fn close(&mut self) -> Result<(), CloseError>
    where
        ActiveOpen: IntoBuffers<R, S>,
    {
        match self {
            State::Closed(_) => Err(CloseError::NoConnection),
            State::Listen(_) | State::SynSent(_) => {
                *self = State::Closed(Closed { reason: UserError::ConnectionClosed });
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
                    },
                    rcv: Recv { buffer: rcv_buffer, assembler: Assembler::new(*irs + 1) },
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
            State::LastAck(_)
            | State::FinWait1(_)
            | State::FinWait2(_)
            | State::Closing(_)
            | State::TimeWait(_) => Err(CloseError::Closing),
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
            }) => Some(Segment::rst_ack(*iss, *irs + 1)),
            State::Established(Established { snd, rcv }) => {
                Some(Segment::rst_ack(snd.nxt, rcv.nxt()))
            }
            State::FinWait1(FinWait1 { snd, rcv }) => Some(Segment::rst_ack(snd.nxt, rcv.nxt())),
            State::FinWait2(FinWait2 { rcv, last_seq }) => {
                Some(Segment::rst_ack(*last_seq, rcv.nxt()))
            }
            State::CloseWait(CloseWait { snd, last_ack, last_wnd: _ }) => {
                Some(Segment::rst_ack(snd.nxt, *last_ack))
            }
        };
        *self = State::Closed(Closed { reason: UserError::ConnectionReset });
        reply
    }

    pub(crate) fn set_send_buffer_size(&mut self, size: usize) {
        match self {
            State::FinWait2(_) | State::TimeWait(_) | State::Closed(_) => (),
            State::Listen(Listen {
                iss: _,
                buffer_sizes: BufferSizes { send },
                device_mss: _,
                default_mss: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: BufferSizes { send },
                smss: _,
            })
            | State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes: BufferSizes { send },
                device_mss: _,
                default_mss: _,
            }) => *send = size,
            State::Established(Established { snd, rcv: _ }) => snd.set_capacity(size),
            State::FinWait1(FinWait1 { snd, rcv: _ }) => snd.set_capacity(size),
            State::Closing(Closing { snd, last_ack: _, last_wnd: _ })
            | State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ }) => snd.set_capacity(size),
            State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => snd.set_capacity(size),
        }
    }

    pub(crate) fn send_buffer_size(&self) -> Option<usize> {
        match self {
            State::FinWait2(_) | State::TimeWait(_) | State::Closed(_) => None,
            State::Listen(Listen {
                iss: _,
                buffer_sizes: BufferSizes { send },
                device_mss: _,
                default_mss: _,
            })
            | State::SynRcvd(SynRcvd {
                iss: _,
                irs: _,
                timestamp: _,
                retrans_timer: _,
                simultaneous_open: _,
                buffer_sizes: BufferSizes { send },
                smss: _,
            })
            | State::SynSent(SynSent {
                iss: _,
                timestamp: _,
                retrans_timer: _,
                active_open: _,
                buffer_sizes: BufferSizes { send },
                device_mss: _,
                default_mss: _,
            }) => Some(*send),
            State::Established(Established { snd, rcv: _ })
            | State::CloseWait(CloseWait { snd, last_ack: _, last_wnd: _ }) => {
                Some(snd.target_capacity())
            }
            State::FinWait1(FinWait1 { snd, rcv: _ })
            | State::Closing(Closing { snd, last_ack: _, last_wnd: _ })
            | State::LastAck(LastAck { snd, last_ack: _, last_wnd: _ }) => {
                Some(snd.target_capacity())
            }
        }
    }
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
        fn data(seq: SeqNum, ack: SeqNum, wnd: WindowSize, data: P) -> Segment<P> {
            let (seg, truncated) = Segment::with_data(seq, Some(ack), None, wnd, data);
            assert_eq!(truncated, 0);
            seg
        }

        fn piggybacked_fin(seq: SeqNum, ack: SeqNum, wnd: WindowSize, data: P) -> Segment<P> {
            let (seg, truncated) =
                Segment::with_data(seq, Some(ack), Some(Control::FIN), wnd, data);
            assert_eq!(truncated, 0);
            seg
        }
    }

    impl Segment<()> {
        fn fin(seq: SeqNum, ack: SeqNum, wnd: WindowSize) -> Self {
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

    /// A buffer that can't read or write for test purpose.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct NullBuffer;

    impl Buffer for NullBuffer {
        fn len(&self) -> usize {
            0
        }

        fn capacity(&self) -> usize {
            0
        }

        fn target_capacity(&self) -> usize {
            0
        }
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
        fn request_capacity(&mut self, _size: usize) {}

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
            self.on_segment::<P, BP>(incoming, now, &KeepAlive::default())
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
                | State::FinWait2(FinWait2 { last_seq: _, rcv }) => rcv.buffer.read_with(f),
            }
        }
    }

    #[test_case(Segment::rst(ISS_1) => None; "drop RST")]
    #[test_case(Segment::rst_ack(ISS_1, ISS_2) => None; "drop RST|ACK")]
    #[test_case(Segment::syn(ISS_1, WindowSize::ZERO, Options { mss: None }) => Some(Segment::rst_ack(SeqNum::new(0), ISS_1 + 1)); "reset SYN")]
    #[test_case(Segment::syn_ack(ISS_1, ISS_2, WindowSize::ZERO, Options { mss: None }) => Some(Segment::rst(ISS_2)); "reset SYN|ACK")]
    #[test_case(Segment::data(ISS_1, ISS_2, WindowSize::ZERO, &[0, 1, 2][..]) => Some(Segment::rst(ISS_2)); "reset data segment")]
    fn segment_arrives_when_closed(
        incoming: impl Into<Segment<&'static [u8]>>,
    ) -> Option<Segment<()>> {
        let closed = Closed { reason: () };
        closed.on_segment(incoming.into())
    }

    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 - 1)
    => SynSentOnSegmentDisposition::Ignore; "unacceptable ACK with RST")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1 - 1, WindowSize::DEFAULT)
    => SynSentOnSegmentDisposition::SendRstAndEnterClosed(
        Segment::rst(ISS_1-1),
        Closed { reason: UserError::ConnectionReset },
    ); "unacceptable ACK without RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1)
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: UserError::ConnectionReset },
    ); "acceptable ACK(ISS) with RST")]
    #[test_case(
        Segment::rst_ack(ISS_2, ISS_1 + 1)
    => SynSentOnSegmentDisposition::EnterClosed(
        Closed { reason: UserError::ConnectionReset },
    ); "acceptable ACK(ISS+1) with RST")]
    #[test_case(
        Segment::rst(ISS_2)
    => SynSentOnSegmentDisposition::Ignore; "RST without ack")]
    #[test_case(
        Segment::syn(ISS_2, WindowSize::DEFAULT, Options { mss: None })
    => SynSentOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
        Segment::syn_ack(ISS_1, ISS_2 + 1, WindowSize::DEFAULT, Options { mss: Some(Mss::default::<Ipv4>()) }),
        SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: Some(FakeInstant::from(RTT)),
            retrans_timer: RetransTimer::new(FakeInstant::from(RTT), Estimator::RTO_INIT),
            simultaneous_open: Some(()),
            buffer_sizes: BufferSizes::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        }
    ); "SYN only")]
    #[test_case(
        Segment::fin(ISS_2, ISS_1 + 1, WindowSize::DEFAULT)
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK with FIN")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1 + 1, WindowSize::DEFAULT)
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK(ISS+1) with nothing")]
    #[test_case(
        Segment::ack(ISS_2, ISS_1, WindowSize::DEFAULT)
    => SynSentOnSegmentDisposition::Ignore; "acceptable ACK(ISS) without RST")]
    fn segment_arrives_when_syn_sent(
        incoming: Segment<()>,
    ) -> SynSentOnSegmentDisposition<FakeInstant, NullBuffer, NullBuffer, ()> {
        let mut syn_sent = SynSent {
            iss: ISS_1,
            timestamp: Some(FakeInstant::default()),
            retrans_timer: RetransTimer::new(FakeInstant::default(), Estimator::RTO_INIT),
            active_open: (),
            buffer_sizes: BufferSizes::default(),
            default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
        };
        syn_sent.on_segment::<_, _, ClientlessBufferProvider>(incoming, FakeInstant::from(RTT))
    }

    #[test_case(Segment::rst(ISS_2) => ListenOnSegmentDisposition::Ignore; "ignore RST")]
    #[test_case(Segment::ack(ISS_2, ISS_1, WindowSize::DEFAULT) =>
        ListenOnSegmentDisposition::SendRst(Segment::rst(ISS_1)); "reject ACK")]
    #[test_case(Segment::syn(ISS_2, WindowSize::DEFAULT, Options { mss: None }) =>
        ListenOnSegmentDisposition::SendSynAckAndEnterSynRcvd(
            Segment::syn_ack(ISS_1, ISS_2 + 1, WindowSize::DEFAULT, Options { mss: Some(Mss::default::<Ipv4>()) }),
            SynRcvd {
                iss: ISS_1,
                irs: ISS_2,
                timestamp: Some(FakeInstant::default()),
                retrans_timer: RetransTimer::new(FakeInstant::default(), Estimator::RTO_INIT),
                simultaneous_open: None,
                buffer_sizes: BufferSizes::default(),
                smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
            }); "accept syn")]
    fn segment_arrives_when_listen(
        incoming: Segment<()>,
    ) -> ListenOnSegmentDisposition<FakeInstant> {
        let listen = Closed::<Initial>::listen(
            ISS_1,
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        );
        listen.on_segment(incoming, FakeInstant::default())
    }

    #[test_case(
        Segment::ack(ISS_1, ISS_2, WindowSize::DEFAULT),
        None
    => Some(
        Segment::ack(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT)
    ); "OTW segment")]
    #[test_case(
        Segment::rst_ack(ISS_1, ISS_2),
        None
    => None; "OTW RST")]
    #[test_case(
        Segment::rst_ack(ISS_1 + 1, ISS_2),
        Some(State::Closed(Closed { reason: UserError::ConnectionReset }))
    => None; "acceptable RST")]
    #[test_case(
        Segment::syn(ISS_1 + 1, WindowSize::DEFAULT, Options { mss: None }),
        Some(State::Closed(Closed { reason: UserError::ConnectionReset }))
    => Some(
        Segment::rst(ISS_2 + 1)
    ); "duplicate syn")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2, WindowSize::DEFAULT),
        None
    => Some(
        Segment::rst(ISS_2)
    ); "unacceptable ack (ISS)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 + 1, WindowSize::DEFAULT),
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
                },
                rcv: Recv { buffer: NullBuffer, assembler: Assembler::new(ISS_1 + 1) },
            }
        ))
    => None; "acceptable ack (ISS + 1)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 + 2, WindowSize::DEFAULT),
        None
    => Some(
        Segment::rst(ISS_2 + 2)
    ); "unacceptable ack (ISS + 2)")]
    #[test_case(
        Segment::ack(ISS_1 + 1, ISS_2 - 1, WindowSize::DEFAULT),
        None
    => Some(
        Segment::rst(ISS_2 - 1)
    ); "unacceptable ack (ISS - 1)")]
    #[test_case(
        Segment::new(ISS_1 + 1, None, None, WindowSize::DEFAULT),
        None
    => None; "no ack")]
    #[test_case(
        Segment::fin(ISS_1 + 1, ISS_2 + 1, WindowSize::DEFAULT),
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
            },
            last_ack: ISS_1 + 2,
            last_wnd: WindowSize::ZERO,
        }))
    => Some(
        Segment::ack(ISS_2 + 1, ISS_1 + 2, WindowSize::ZERO)
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
            retrans_timer: RetransTimer::new(clock.now(), Estimator::RTO_INIT),
            simultaneous_open: Some(()),
            buffer_sizes: Default::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
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
        Segment::syn(ISS_2 + 1, WindowSize::DEFAULT, Options { mss: None }),
        Some(State::Closed (
            Closed { reason: UserError::ConnectionReset },
        ))
    => Some(Segment::rst(ISS_1 + 1)); "duplicate syn")]
    #[test_case(
        Segment::rst(ISS_2 + 1),
        Some(State::Closed (
            Closed { reason: UserError::ConnectionReset },
        ))
    => None; "accepatable rst")]
    #[test_case(
        Segment::ack(ISS_2 + 1, ISS_1 + 2, WindowSize::DEFAULT),
        None
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 1, WindowSize::new(2).unwrap())
    ); "unacceptable ack")]
    #[test_case(
        Segment::ack(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT),
        None
    => None; "pure ack")]
    #[test_case(
        Segment::fin(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT),
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
            },
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::new(1).unwrap(),
        }))
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 2, WindowSize::new(1).unwrap())
    ); "pure fin")]
    #[test_case(
        Segment::piggybacked_fin(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT, "A".as_bytes()),
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
            },
            last_ack: ISS_2 + 3,
            last_wnd: WindowSize::ZERO,
        }))
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 3, WindowSize::ZERO)
    ); "fin with 1 byte")]
    #[test_case(
        Segment::piggybacked_fin(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT, "AB".as_bytes()),
        None
    => Some(
        Segment::ack(ISS_1 + 1, ISS_2 + 3, WindowSize::ZERO)
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
            },
            rcv: Recv { buffer: RingBuffer::new(2), assembler: Assembler::new(ISS_2 + 1) },
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
        };
        let new_rcv = || Recv {
            buffer: RingBuffer::new(TEST_BYTES.len()),
            assembler: Assembler::new(ISS_2 + 1),
        };
        for mut state in [
            State::Established(Established { snd: new_snd(), rcv: new_rcv() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin(), rcv: new_rcv() }),
            State::FinWait2(FinWait2 { last_seq: ISS_1 + 1, rcv: new_rcv() }),
        ] {
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::data(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT, TEST_BYTES),
                    FakeInstant::default(),
                ),
                (
                    Some(Segment::ack(ISS_1 + 1, ISS_2 + 1 + TEST_BYTES.len(), WindowSize::ZERO)),
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
        };
        let new_rcv = || Recv { buffer: NullBuffer, assembler: Assembler::new(ISS_2 + 1) };
        for mut state in [
            State::Established(Established { snd: new_snd(), rcv: new_rcv() }),
            State::FinWait1(FinWait1 { snd: new_snd().queue_fin(), rcv: new_rcv() }),
            State::Closing(Closing {
                snd: new_snd().queue_fin(),
                last_ack: ISS_2 + 1,
                last_wnd: WindowSize::ZERO,
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
                    WindowSize::ZERO,
                    SendPayload::Contiguous(TEST_BYTES)
                ))
            );
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::ack(ISS_2 + 1, ISS_1 + 1 + TEST_BYTES.len(), WindowSize::DEFAULT),
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
            assert_eq!(snd.buffer.len(), 0);
        }
    }

    #[test_case(
        Segment::syn(ISS_2 + 2, WindowSize::DEFAULT, Options { mss: None }),
        Some(State::Closed (
            Closed { reason: UserError::ConnectionReset },
        ))
    => Some(Segment::rst(ISS_1 + 1)); "syn")]
    #[test_case(
        Segment::rst(ISS_2 + 2),
        Some(State::Closed (
            Closed { reason: UserError::ConnectionReset },
        ))
    => None; "rst")]
    #[test_case(
        Segment::fin(ISS_2 + 2, ISS_1 + 1, WindowSize::DEFAULT),
        None
    => None; "ignore fin")]
    #[test_case(
        Segment::data(ISS_2 + 2, ISS_1 + 1, WindowSize::DEFAULT, "Hello".as_bytes()),
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
        );
        assert_eq!(
            syn_seg,
            Segment::syn(
                ISS_1,
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
            )
        );
        assert_eq!(
            syn_sent,
            SynSent {
                iss: ISS_1,
                timestamp: Some(clock.now()),
                retrans_timer: RetransTimer::new(clock.now(), Estimator::RTO_INIT),
                active_open: (),
                buffer_sizes: BufferSizes::default(),
                default_mss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
                device_mss: DEVICE_MAXIMUM_SEGMENT_SIZE,
            }
        );
        let mut active = State::SynSent(syn_sent);
        let mut passive = State::Listen(Closed::<Initial>::listen(
            ISS_2,
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
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
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
            )
        );
        assert_matches!(passive, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_2,
            irs: ISS_1,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(clock.now(), Estimator::RTO_INIT),
            simultaneous_open: None,
            buffer_sizes: Default::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
        });
        clock.sleep(RTT / 2);
        let (seg, passive_open) = active
            .on_segment_with_default_options::<_, ClientlessBufferProvider>(syn_ack, clock.now());
        let ack_seg = seg.expect("failed to generate a ack segment");
        assert_eq!(passive_open, None);
        assert_eq!(ack_seg, Segment::ack(ISS_1 + 1, ISS_2 + 1, WindowSize::ZERO));
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
            },
            rcv: Recv { buffer: NullBuffer, assembler: Assembler::new(ISS_2 + 1) }
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
            },
            rcv: Recv { buffer: NullBuffer, assembler: Assembler::new(ISS_1 + 1) }
        })
    }

    #[test]
    fn simultaneous_open() {
        let mut clock = FakeInstantCtx::default();
        let (syn_sent1, syn1) = Closed::<Initial>::connect(
            ISS_1,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        );
        let (syn_sent2, syn2) = Closed::<Initial>::connect(
            ISS_2,
            clock.now(),
            (),
            Default::default(),
            DEVICE_MAXIMUM_SEGMENT_SIZE,
            DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        );

        assert_eq!(
            syn1,
            Segment::syn(
                ISS_1,
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
            )
        );
        assert_eq!(
            syn2,
            Segment::syn(
                ISS_2,
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
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
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
            )
        );
        assert_eq!(
            syn_ack2,
            Segment::syn_ack(
                ISS_2,
                ISS_1 + 1,
                WindowSize::DEFAULT,
                Options { mss: Some(DEVICE_MAXIMUM_SEGMENT_SIZE) }
            )
        );

        assert_matches!(state1, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(clock.now(), Estimator::RTO_INIT),
            simultaneous_open: Some(()),
            buffer_sizes: BufferSizes::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
        });
        assert_matches!(state2, State::SynRcvd(ref syn_rcvd) if syn_rcvd == &SynRcvd {
            iss: ISS_2,
            irs: ISS_1,
            timestamp: Some(clock.now()),
            retrans_timer: RetransTimer::new(clock.now(), Estimator::RTO_INIT),
            simultaneous_open: Some(()),
            buffer_sizes: BufferSizes::default(),
            smss: DEVICE_MAXIMUM_SEGMENT_SIZE,
        });

        clock.sleep(RTT);
        assert_eq!(
            state1.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack2,
                clock.now()
            ),
            (Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, WindowSize::ZERO)), None)
        );
        assert_eq!(
            state2.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                syn_ack1,
                clock.now()
            ),
            (Some(Segment::ack(ISS_2 + 1, ISS_1 + 1, WindowSize::ZERO)), None)
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_2 + 1),
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
            },
            rcv: Recv {
                buffer: NullBuffer,
                assembler: Assembler::new(ISS_1 + 1),
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });

        // Received an expected segment at rcv.nxt.
        assert_eq!(
            established.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(ISS_2 + 1, ISS_1 + 1, WindowSize::ZERO, TEST_BYTES,),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    WindowSize::new(BUFFER_SIZE - TEST_BYTES.len()).unwrap(),
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
                    WindowSize::ZERO,
                    TEST_BYTES,
                ),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + TEST_BYTES.len(),
                    WindowSize::new(BUFFER_SIZE).unwrap(),
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
                    WindowSize::ZERO,
                    TEST_BYTES,
                ),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + 1,
                    ISS_2 + 1 + 3 * TEST_BYTES.len(),
                    WindowSize::new(BUFFER_SIZE - 2 * TEST_BYTES.len()).unwrap(),
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
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
                    Segment::ack(ISS_2 + 1, ack, WindowSize::new(win).unwrap()),
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
                WindowSize::new(BUFFER_SIZE).unwrap(),
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
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[2..4]),
            ))
        );

        assert_eq!(
            established.poll_send(
                1,
                clock.now(),
                &SocketOptions { keep_alive: KeepAlive::default(), nagle_enabled: false }
            ),
            Some(Segment::data(
                ISS_1 + 4,
                ISS_2 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
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
            (Some(Segment::ack(ISS_1 + 1, ISS_1 + 1, WindowSize::DEFAULT)), None)
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
                    WindowSize::DEFAULT,
                    SendPayload::Contiguous(TEST_BYTES),
                ))
            );
            assert_eq!(state.poll_send_at(), Some(clock.now() + (1 << i) * Estimator::RTO_INIT));
            clock.sleep((1 << i) * Estimator::RTO_INIT);
        }
        // The receiver acks the first byte of the payload.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_1 + 1 + TEST_BYTES.len(), ISS_1 + 1 + 1, WindowSize::DEFAULT),
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
                WindowSize::DEFAULT,
                SendPayload::Contiguous(&TEST_BYTES[1..2]),
            ))
        );
        // Currently, snd.nxt = ISS_1 + 2, snd.max = ISS_1 + 5, a segment
        // with ack number ISS_1 + 4 should bump snd.nxt immediately.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_1 + 1 + TEST_BYTES.len(), ISS_1 + 1 + 3, WindowSize::DEFAULT),
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
                WindowSize::DEFAULT,
                SendPayload::Contiguous(&TEST_BYTES[3..4]),
            ))
        );
        // Finally the receiver ACKs all the outstanding data.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_1 + 1 + TEST_BYTES.len(),
                    ISS_1 + 1 + TEST_BYTES.len(),
                    WindowSize::DEFAULT
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });
        let last_wnd = WindowSize::new(BUFFER_SIZE - 1).unwrap();
        // Transition the state machine to CloseWait by sending a FIN.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 1, WindowSize::DEFAULT),
                clock.now(),
            ),
            (
                Some(Segment::ack(ISS_1 + 1, ISS_2 + 2, WindowSize::new(BUFFER_SIZE - 1).unwrap())),
                None
            )
        );
        // Then call CLOSE to transition the state machine to LastAck.
        assert_eq!(state.close(), Ok(()));
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
                last_wnd,
                SendPayload::Contiguous(&TEST_BYTES[..2]),
            ))
        );
        // We should be able to send out all remaining bytes together with a FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 2,
                last_wnd,
                SendPayload::Contiguous(&TEST_BYTES[2..]),
            ))
        );
        // Now let's test we retransmit correctly by only acking the data.
        clock.sleep(RTT);
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 2, ISS_1 + 1 + TEST_BYTES.len(), WindowSize::DEFAULT),
                clock.now(),
            ),
            (None, None)
        );
        assert_eq!(state.poll_send_at(), Some(clock.now() + Estimator::RTO_INIT));
        clock.sleep(Estimator::RTO_INIT);
        // The FIN should be retransmitted.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::fin(ISS_1 + 1 + TEST_BYTES.len(), ISS_2 + 2, last_wnd,).into())
        );

        // Finally, our FIN is acked.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 2, ISS_1 + 1 + TEST_BYTES.len() + 1, WindowSize::DEFAULT,),
                clock.now(),
            ),
            (None, None)
        );
        // The connection is closed.
        assert_eq!(state, State::Closed(Closed { reason: UserError::ConnectionClosed }));
    }

    #[test]
    fn syn_rcvd_active_close() {
        let mut state: State<_, RingBuffer, NullBuffer, ()> = State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer { at: FakeInstant::default(), rto: Duration::new(0, 0) },
            simultaneous_open: Some(()),
            buffer_sizes: Default::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        });
        assert_eq!(state.close(), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, FakeInstant::default()),
            Some(Segment::fin(ISS_1 + 1, ISS_2 + 1, WindowSize::DEFAULT).into())
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });
        assert_eq!(state.close(), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(state.close(), Err(CloseError::Closing));

        // Poll for 2 bytes.
        assert_eq!(
            state.poll_send_with_default_options(2, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[..2])
            ))
        );

        // And we should send the rest of the buffer together with the FIN.
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::piggybacked_fin(
                ISS_1 + 3,
                ISS_2 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[2..])
            ))
        );

        // Test that the recv state works in FIN_WAIT_1.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::data(ISS_2 + 1, ISS_1 + 1 + 1, WindowSize::DEFAULT, TEST_BYTES),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + TEST_BYTES.len() + 1,
                    WindowSize::new(BUFFER_SIZE - TEST_BYTES.len()).unwrap()
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
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[1..]),
            ))
        );

        // Now our FIN is acked, we should transition to FinWait2.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(
                    ISS_2 + TEST_BYTES.len() + 1,
                    ISS_1 + TEST_BYTES.len() + 2,
                    WindowSize::DEFAULT
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
                    WindowSize::DEFAULT,
                    TEST_BYTES
                ),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + 2 * TEST_BYTES.len() + 1,
                    WindowSize::new(BUFFER_SIZE - TEST_BYTES.len()).unwrap()
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
                    WindowSize::DEFAULT
                ),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_2 + 2 * TEST_BYTES.len() + 2,
                    WindowSize::new(BUFFER_SIZE - 1).unwrap()
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
        assert_eq!(state, State::Closed(Closed { reason: UserError::ConnectionClosed }));
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 1, ISS_1 + 2, WindowSize::DEFAULT),
                FakeInstant::default(),
            ),
            (
                Some(Segment::ack(ISS_1 + 2, ISS_2 + 2, WindowSize::new(BUFFER_SIZE - 1).unwrap())),
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_1 + 1),
            },
        });
        assert_eq!(state.close(), Ok(()));
        assert_matches!(state, State::FinWait1(_));
        assert_eq!(state.close(), Err(CloseError::Closing));

        let fin = state.poll_send_with_default_options(u32::MAX, clock.now());
        assert_eq!(
            fin,
            Some(Segment::piggybacked_fin(
                ISS_1 + 1,
                ISS_1 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(TEST_BYTES),
            ))
        );
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::piggybacked_fin(
                    ISS_1 + 1,
                    ISS_1 + 1,
                    WindowSize::new(BUFFER_SIZE).unwrap(),
                    SendPayload::Contiguous(TEST_BYTES),
                ),
                clock.now(),
            ),
            (
                Some(Segment::ack(
                    ISS_1 + TEST_BYTES.len() + 2,
                    ISS_1 + TEST_BYTES.len() + 2,
                    WindowSize::new(BUFFER_SIZE - TEST_BYTES.len() - 1).unwrap()
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
                    WindowSize::new(BUFFER_SIZE - TEST_BYTES.len() - 1).unwrap()
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
        assert_eq!(state, State::Closed(Closed { reason: UserError::ConnectionClosed }));
    }

    #[test]
    fn time_wait_restarts_timer() {
        let mut clock = FakeInstantCtx::default();
        let mut time_wait = State::<_, NullBuffer, NullBuffer, ()>::TimeWait(TimeWait {
            last_seq: ISS_1 + 2,
            last_ack: ISS_2 + 2,
            last_wnd: WindowSize::DEFAULT,
            expiry: new_time_wait_expiry(clock.now()),
        });

        assert_eq!(time_wait.poll_send_at(), Some(clock.now() + MSL * 2));
        clock.sleep(Duration::from_secs(1));
        assert_eq!(
            time_wait.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::fin(ISS_2 + 2, ISS_1 + 2, WindowSize::DEFAULT),
                clock.now(),
            ),
            (Some(Segment::ack(ISS_1 + 2, ISS_2 + 2, WindowSize::DEFAULT)), None),
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
            },
            rcv: Recv { buffer: RingBuffer::default(), assembler: Assembler::new(ISS_2 + 5) },
        }),
        Segment::data(ISS_2, ISS_1, WindowSize::DEFAULT, TEST_BYTES) =>
        Some(Segment::ack(ISS_1, ISS_2 + 5, WindowSize::DEFAULT)); "retransmit data"
    )]
    #[test_case(
        State::SynRcvd(SynRcvd {
            iss: ISS_1,
            irs: ISS_2,
            timestamp: None,
            retrans_timer: RetransTimer { at: FakeInstant::default(), rto: Duration::new(0, 0) },
            simultaneous_open: None,
            buffer_sizes: BufferSizes::default(),
            smss: DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE,
        }),
        Segment::syn_ack(ISS_2, ISS_1 + 1, WindowSize::DEFAULT, Options { mss: None }).into() =>
        Some(Segment::ack(ISS_1 + 1, ISS_2 + 1, WindowSize::DEFAULT)); "retransmit syn_ack"
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
            },
            rcv: Recv { buffer: RingBuffer::default(), assembler: Assembler::new(ISS_2) },
        });

        assert_eq!(
            state.poll_send_with_default_options(
                u32::from(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE),
                clock.now()
            ),
            Some(Segment::data(
                ISS_1,
                ISS_2,
                WindowSize::DEFAULT,
                SendPayload::Contiguous(&[b'A'; DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE])
            ))
        );

        let mut dup_ack = |expected_byte: u8| {
            clock.sleep(Duration::from_millis(10));
            assert_eq!(
                state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                    Segment::ack(ISS_2, ISS_1, WindowSize::DEFAULT),
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
                    WindowSize::DEFAULT,
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
                    WindowSize::DEFAULT
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
            },
            rcv: Recv { buffer: RingBuffer::default(), assembler: Assembler::new(ISS_2) },
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
        assert_eq!(
            state.poll_send_at(),
            Some(clock.now().checked_add(keep_alive.idle.into())).unwrap()
        );

        // Now we receive an ACK after an hour.
        clock.sleep(Duration::from_secs(60 * 60));
        assert_eq!(
            state.on_segment::<&[u8], ClientlessBufferProvider>(
                Segment::ack(ISS_2, ISS_1, WindowSize::DEFAULT).into(),
                clock.now(),
                keep_alive,
            ),
            (None, None),
        );
        // the timer is reset to fire in 2 hours.
        assert_eq!(
            state.poll_send_at(),
            Some(clock.now().checked_add(keep_alive.idle.into()).unwrap()),
        );
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
                Some(Segment::ack(ISS_1 - 1, ISS_2, WindowSize::DEFAULT).into())
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
        assert_eq!(state, State::Closed(Closed { reason: UserError::ConnectionClosed }));
    }

    /// A `SendBuffer` that doesn't allow peeking some number of bytes.
    #[derive(Debug)]
    struct ReservingBuffer<B> {
        buffer: B,
        reserved_bytes: usize,
    }

    impl<B: Buffer> Buffer for ReservingBuffer<B> {
        fn len(&self) -> usize {
            self.buffer.len()
        }

        fn capacity(&self) -> usize {
            self.buffer.capacity()
        }

        fn target_capacity(&self) -> usize {
            self.buffer.target_capacity()
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

        fn request_capacity(&mut self, size: usize) {
            self.buffer.request_capacity(size)
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

    #[test_case(true)]
    #[test_case(false)]
    fn poll_send_len(has_fin: bool) {
        const NUM_RESERVED: usize = 16;
        const VALUE: u8 = 0xaa;

        fn with_poll_send_result<const HAS_FIN: bool>(f: impl FnOnce(Segment<SendPayload<'_>>)) {
            const DATA_LEN: usize = 40;
            let buffer = ReservingBuffer {
                buffer: RingBuffer::with_data(DATA_LEN, &vec![VALUE; DATA_LEN]),
                reserved_bytes: NUM_RESERVED,
            };
            assert_eq!(buffer.len(), DATA_LEN);

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

            if has_fin {
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
            true => with_poll_send_result::<true>(f),
            false => with_poll_send_result::<false>(f),
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(
            state.poll_send_at(),
            Some(clock.now().checked_add(Estimator::RTO_INIT).unwrap())
        );

        // Send the first probe after first RTO.
        clock.sleep(Estimator::RTO_INIT);
        assert_eq!(
            state.poll_send_with_default_options(u32::MAX, clock.now()),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver still has a zero window.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 1, WindowSize::ZERO),
                clock.now()
            ),
            (None, None)
        );
        // The timer should backoff exponentially.
        assert_eq!(state.poll_send_with_default_options(u32::MAX, clock.now()), None);
        assert_eq!(
            state.poll_send_at(),
            Some(clock.now().checked_add(Estimator::RTO_INIT * 2).unwrap())
        );

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
                WindowSize::new(BUFFER_SIZE).unwrap(),
                SendPayload::Contiguous(&TEST_BYTES[0..1])
            ))
        );

        // The receiver now opens its receive window.
        assert_eq!(
            state.on_segment_with_default_options::<_, ClientlessBufferProvider>(
                Segment::ack(ISS_2 + 1, ISS_1 + 2, WindowSize::DEFAULT),
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
                WindowSize::new(BUFFER_SIZE).unwrap(),
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
            },
            rcv: Recv {
                buffer: RingBuffer::new(BUFFER_SIZE),
                assembler: Assembler::new(ISS_2 + 1),
            },
        });
        let mut socket_options = SocketOptions::default();
        assert_eq!(
            state.poll_send(3, clock.now(), &socket_options),
            Some(Segment::data(
                ISS_1 + 1,
                ISS_2 + 1,
                WindowSize::new(BUFFER_SIZE).unwrap(),
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
                WindowSize::new(BUFFER_SIZE).unwrap(),
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
            (Some(Segment::ack(ISS_1 + 1, ISS_1 + 1, WindowSize::DEFAULT)), None)
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
                WindowSize::DEFAULT,
                SendPayload::Contiguous(&TEST_BYTES[..1]),
            ))
        );
    }
}
