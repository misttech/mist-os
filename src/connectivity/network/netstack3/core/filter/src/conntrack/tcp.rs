// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TCP state tracking.

use core::time::Duration;

use netstack3_base::{Control, SegmentHeader, SeqNum, UnscaledWindowSize, WindowScale, WindowSize};
use replace_with::replace_with_and;

use super::{
    ConnectionDirection, ConnectionUpdateAction, ConnectionUpdateError, EstablishmentLifecycle,
};

/// A struct that completely encapsulates tracking a bidirectional TCP
/// connection.
#[derive(Debug, Clone)]
pub(crate) struct Connection {
    /// The current state of the TCP connection.
    state: State,
}

impl Connection {
    pub fn new(segment: &SegmentHeader, payload_len: usize, self_connected: bool) -> Option<Self> {
        Some(Self {
            // TODO(https://fxbug.dev/355699182): Properly support self-connected
            // connections.
            state: if self_connected {
                Untracked {}.into()
            } else {
                State::new(segment, payload_len)?
            },
        })
    }

    pub fn expiry_duration(&self, establishment_lifecycle: EstablishmentLifecycle) -> Duration {
        self.state.expiry_duration(establishment_lifecycle)
    }

    pub fn update(
        &mut self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        let valid =
            replace_with_and(&mut self.state, |state| state.update(segment, payload_len, dir));

        if !valid {
            return Err(ConnectionUpdateError::InvalidPacket);
        }

        match self.state {
            State::Closed(_) => Ok(ConnectionUpdateAction::RemoveEntry),
            State::Untracked(_)
            | State::SynSent(_)
            | State::WaitingOnOpeningAck(_)
            | State::Closing(_)
            | State::Established(_) => Ok(ConnectionUpdateAction::NoAction),
        }
    }
}

/// States for a TCP connection as a whole.
///
/// These vaguely correspond to states from RFC 9293, but since they apply to
/// the whole connection, and we're just snooping in the middle, they can't line
/// up perfectly 1:1. See the doc comments on each state for the expected state
/// of each of the peers assuming that all packets are received and are valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// The connection has properties that break standard state tracking. This
    /// state does a good-enough job tracking the connection.
    ///
    /// This is a short-circuit state that can never be left.
    Untracked(Untracked),

    /// The connection has been closed, either by the FIN handshake or valid
    /// RST.
    ///
    /// This is a short-circuit state that can never be left.
    Closed(Closed),

    /// The initial SYN for this connection has been sent. State contained
    /// within is everything that can be gleaned from the initial SYN packet
    /// sent in the original direction.
    ///
    /// Expected peer states:
    /// - Original: `SYN_SENT`
    /// - Reply: `SYN_RECEIVED` (upon receipt)
    SynSent(SynSent),

    /// The reply peer has sent a valid SYN/ACK segment. All that remains is for
    /// the original peer to ACK.
    ///
    /// Expected peer states.
    /// - Original: ESTABLISHED
    /// - Reply: SYN_RECEIVED
    WaitingOnOpeningAck(WaitingOnOpeningAck),

    /// The handshake has completed and data may begin flowing at any time. This
    /// is where most connections will spend the vast majority of their time.
    ///
    /// Expected peer states:
    /// - Original: ESTABLISHED
    /// - Reply: ESTABLISHED
    Established(Established),

    /// The process of closing down a connection starting from when the first
    /// FIN is seen and until FINs from both peers have been ACKed.
    ///
    /// Expected peer states are any of:
    /// - FIN_WAIT_1
    /// - FIN_WAIT_2
    /// - CLOSING
    /// - CLOSE_WAIT
    /// - LAST_ACK
    Closing(Closing),
}

impl State {
    fn new(segment: &SegmentHeader, payload_len: usize) -> Option<Self> {
        // We explicitly don't want to track any connections that we haven't
        // seen from the beginning because:
        //
        // a) This shouldn't happen, since we run from boot
        // b) Window scale is only negotiated during the initial handshake.
        match segment.control {
            Some(Control::SYN) => {}
            None | Some(Control::FIN) | Some(Control::RST) => return None,
        }

        Some(
            SynSent {
                iss: segment.seq,
                logical_len: segment.len(payload_len),
                advertised_window_scale: segment.options.window_scale(),
                // This unwrap cannot fail because WindowSize::MAX is 2^30-1,
                // which is larger than the largest possible unscaled window
                // size (2^16).
                window_size: WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap(),
            }
            .into(),
        )
    }

    fn expiry_duration(&self, establishment_lifecycle: EstablishmentLifecycle) -> Duration {
        const MAXIMUM_SEGMENT_LIFETIME: Duration = Duration::from_secs(120);

        // These are all picked to optimize purging connections from the table
        // as soon as is reasonable. Unlike Linux, we are choosing to be more
        // conservative with our timeouts and setting the most aggressive one to
        // the standard MSL of 120 seconds.
        match self {
            State::Untracked(_) => {
                match establishment_lifecycle {
                    // This is small because it's just meant to be the time for
                    // the initial handshake.
                    EstablishmentLifecycle::SeenOriginal | EstablishmentLifecycle::SeenReply => {
                        MAXIMUM_SEGMENT_LIFETIME
                    }
                    EstablishmentLifecycle::Established => Duration::from_secs(6 * 60 * 60),
                }
            }
            State::Closed(_) => Duration::ZERO,
            State::SynSent(_) => MAXIMUM_SEGMENT_LIFETIME,
            State::WaitingOnOpeningAck(_) => MAXIMUM_SEGMENT_LIFETIME,
            State::Established(Established { original, reply }) => {
                // If there is no data outstanding, make the timeout large, and
                // otherwise small so we can purge the connection quickly if one
                // of the endpoints disappears.
                if original.unacked_data || reply.unacked_data {
                    MAXIMUM_SEGMENT_LIFETIME
                } else {
                    Duration::from_secs(5 * 60 * 60 * 24)
                }
            }
            State::Closing(_) => MAXIMUM_SEGMENT_LIFETIME,
        }
    }

    /// Returns a new state that unconditionally replaces the previous one. The
    /// boolean represents whether the segment was valid or not.
    ///
    /// In the case where the segment was invalid, the returned state will be
    /// equivalent to the one that `update` was called on.
    fn update(
        self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> (State, bool) {
        match self {
            State::Untracked(s) => s.update(segment, payload_len, dir),
            State::Closed(s) => s.update(segment, payload_len, dir),
            State::SynSent(s) => s.update(segment, payload_len, dir),
            State::WaitingOnOpeningAck(s) => s.update(segment, payload_len, dir),
            State::Established(s) => s.update(segment, payload_len, dir),
            State::Closing(s) => s.update(segment, payload_len, dir),
        }
    }
}

macro_rules! state_from_state_struct {
    ($struct:ident) => {
        impl From<$struct> for State {
            fn from(value: $struct) -> Self {
                Self::$struct(value)
            }
        }
    };
}

/// Contains all of the information required for a single peer in an established
/// TCP connection.
///
/// Packets are valid with the following equations (taken from ["Real Stateful
/// TCP Packet Filtering in IP Filter"][paper] and updated to allow for segments
/// intersecting the valid ranges, rather than needing to be entirely contained
/// within them).
///
/// Definitions:
/// - s: The sequence number of the first octet of the segment
/// - n: The (virtual) length of the segment in octets
/// - a: The ACK number of the segment
///
/// I:   Data upper bound: s   <= receiver.max_wnd_seq
/// II:  Data lower bound: s+n >= sender.max_next_seq - receiver.max_wnd
/// III: ACK  upper bound: a   <= receiver.max_next_seq
/// IV:  ACK  lower bound: a   >= receiver.max_next_seq - MAXACKWINDOW
///
/// MAXACKWINDOW is defined in the paper to be 66000, which is larger than
/// the largest possible window (without scaling). We scale by
/// sender.window_scale to ensure this property remains true.
///
/// [paper]: https://www.usenix.org/legacy/events/sec01/invitedtalks/rooij.pdf
#[derive(Debug, Clone, PartialEq, Eq)]
struct Peer {
    /// How much to scale window updates from this peer.
    window_scale: WindowScale,

    /// The maximum window size ever sent by this peer.
    ///
    /// On every packet sent by this peer, the larger of itself and the
    /// (scaled) window from the packet is taken.
    max_wnd: WindowSize,

    /// The maximum sequence number that is in the window for this peer.
    ///
    /// On every packet sent by this peer, this is updated to be the larger of
    /// itself or the current advertised window (scaled) plus the ACK number in
    /// the packet.
    max_wnd_seq: SeqNum,

    /// The largest "next octet" ever sent by this peer. This is equivalent to
    /// one more than the largest sequence number ever sent by this peer.
    ///
    /// On every packet sent by this peer, this is updated to be the larger of
    /// itself or the sequence number plus the length of the packet.
    max_next_seq: SeqNum,

    /// Has this peer sent data that has yet to be ACKed?
    ///
    /// Set when max_next_seq is increased.
    ///
    /// Unset when a reply segment is seen that has an ACK number equal to
    /// `max_next_seq` (larger would mean an invalid packet).
    unacked_data: bool,

    /// The state of the first FIN segment sent by this peer.
    fin_state: FinState,
}

impl Peer {
    /// Checks that an ACK segment is within the windows defined in the comment on [`Peer`].
    fn ack_segment_valid(
        sender: &Self,
        receiver: &Self,
        seq: SeqNum,
        len: u32,
        ack: SeqNum,
    ) -> bool {
        // All checks below are for the negation of the equation referenced in
        // the associated comment.

        // I: Segment sequence numbers upper bound.
        if seq.after(receiver.max_wnd_seq) {
            return false;
        }

        // II: Segment sequence numbers lower bound.
        if (seq + len).before(sender.max_next_seq - receiver.max_wnd) {
            return false;
        }

        // III: ACK upper bound.
        if ack.after(receiver.max_next_seq) {
            return false;
        }

        // IV: ACK lower bound.
        if ack.before(receiver.max_next_seq - receiver.max_ack_window()) {
            return false;
        }

        true
    }

    /// Returns a new `Peer` updated using the provided information from a
    /// segment which it has sent.
    fn update_sender(
        self,
        seq: SeqNum,
        len: u32,
        ack: SeqNum,
        wnd: UnscaledWindowSize,
        fin_seen: bool,
    ) -> Self {
        let Self { window_scale, max_wnd, max_wnd_seq, max_next_seq, unacked_data, fin_state } =
            self;

        // The minimum window size is assumed to be 1. From the paper:
        //   On BSD systems, a window probing is always done with a packet
        //   containing one octet of data.
        let window_size = {
            let window_size = wnd << window_scale;
            // The unwrap below won't fail because 1 is less than
            // WindowSize::MAX.
            core::cmp::max(window_size, WindowSize::from_u32(1).unwrap())
        };

        // The largest sequence number allowed by the current window.
        let wnd_seq = ack + window_size;
        // The octet one past the last one in the segment.
        let end = seq + len;
        // The largest `end` value sent by this peer.
        let sender_max_next_seq = if max_next_seq.before(end) { end } else { max_next_seq };

        Peer {
            window_scale,
            max_wnd: core::cmp::max(max_wnd, window_size),
            max_wnd_seq: if max_wnd_seq.before(wnd_seq) { wnd_seq } else { max_wnd_seq },
            max_next_seq: sender_max_next_seq,
            unacked_data: if sender_max_next_seq.after(max_next_seq) { true } else { unacked_data },
            fin_state: if fin_seen { fin_state.update_fin_sent(end - 1) } else { fin_state },
        }
    }

    /// Returns a new `Peer` updated using the provided information from a
    /// segment which it received.
    fn update_receiver(self, ack: SeqNum) -> Self {
        let Self { window_scale, max_wnd, max_wnd_seq, max_next_seq, unacked_data, fin_state } =
            self;

        Peer {
            window_scale,
            max_wnd,
            max_wnd_seq,
            max_next_seq,
            // It's not possible for ack to be > self.max_next_seq due to
            // equation III, which is checked on every segment.
            unacked_data: if ack == max_next_seq { false } else { unacked_data },
            fin_state: fin_state.update_ack_received(ack),
        }
    }

    fn max_ack_window(&self) -> u32 {
        // The paper gives 66000 as the MAXACKWINDOW constant because it's a
        // little larger than the largest possible TCP window. With winow
        // scaling in effect, this constant is no longer valid, so we scale it
        // up by that scaling factor.
        //
        // This shift is guaranteed to never overflow because the maximum value
        // for self.window_scale is 14 and 66000 << 14 < u32::MAX.
        66000u32 << (self.window_scale.get() as u32)
    }
}

/// Tracks the state of the FIN process for a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FinState {
    /// This peer has not sent a FIN yet.
    NotSent,

    /// This peer has sent a FIN with the provided sequence number.
    ///
    /// Updated to ensure it is the sequence number of the first FIN sent.
    Sent(SeqNum),

    /// The FIN sent by this peer has been ACKed.
    Acked,
}

impl FinState {
    /// To be called when the peer has sent a FIN segment with the sequence
    /// number of the FIN.
    ///
    /// Returns an updated `FinState`.
    fn update_fin_sent(self, seq: SeqNum) -> Self {
        match self {
            FinState::NotSent => FinState::Sent(seq),
            FinState::Sent(s) => {
                // NOTE: We want to track the first FIN in the sequence
                // space, not the first one we saw.
                if s.before(seq) {
                    FinState::Sent(s)
                } else {
                    FinState::Sent(seq)
                }
            }
            FinState::Acked => FinState::Acked,
        }
    }

    /// To be called when the peer has received an ACK.
    ///
    /// Returns an updated `FinState`.
    fn update_ack_received(self, ack: SeqNum) -> Self {
        match self {
            FinState::NotSent => FinState::NotSent,
            FinState::Sent(seq) => {
                if ack.after(seq) {
                    FinState::Acked
                } else {
                    FinState::Sent(seq)
                }
            }
            FinState::Acked => FinState::Acked,
        }
    }

    /// Has this FIN been acked?
    fn acked(&self) -> bool {
        match self {
            FinState::NotSent => false,
            FinState::Sent(_) => false,
            FinState::Acked => true,
        }
    }
}

/// The return value from [`do_established_update`] indicating further action.
#[derive(Debug, PartialEq, Eq)]
enum EstablishedUpdateResult {
    /// The update was successful.
    ///
    /// `new_original` and `new_reply` are the updated versions of the original
    /// and reply peers that were provided to the function.
    Success { new_original: Peer, new_reply: Peer, fin_seen: bool },

    /// A valid RST was seen.
    Reset,

    /// The segment was invalid.
    ///
    /// Includes the non-updated peers so the state can be reset.
    Invalid { original: Peer, reply: Peer },
}

fn swap_peers(original: Peer, reply: Peer, dir: ConnectionDirection) -> (Peer, Peer) {
    match dir {
        ConnectionDirection::Original => (original, reply),
        ConnectionDirection::Reply => (reply, original),
    }
}

/// Holds and names the peers passed into [`do_established_update`] to avoid
/// ambiguous argument order.
struct UpdatePeers {
    original: Peer,
    reply: Peer,
}

/// The core functionality for handling segments once the handshake is complete.
///
/// Validates that segments fall within the bounds described in the comment on
/// [`Peer`].
fn do_established_update(
    UpdatePeers { original, reply }: UpdatePeers,
    segment: &SegmentHeader,
    payload_len: usize,
    dir: ConnectionDirection,
) -> EstablishedUpdateResult {
    let logical_len = segment.len(payload_len);
    let SegmentHeader { seq, ack, wnd, control, options: _ } = segment;

    let (sender, receiver) = swap_peers(original, reply, dir);

    // From RFC 9293:
    //   If the ACK control bit is set, this field contains the value of the
    //   next sequence number the sender of the segment is expecting to receive.
    //   Once a connection is established, this is always sent.
    let ack = match ack {
        Some(ack) => ack,
        None => {
            let (original, reply) = swap_peers(sender, receiver, dir);
            return EstablishedUpdateResult::Invalid { original, reply };
        }
    };

    if !Peer::ack_segment_valid(&sender, &receiver, *seq, logical_len, *ack) {
        let (original, reply) = swap_peers(sender, receiver, dir);
        return EstablishedUpdateResult::Invalid { original, reply };
    }

    let fin_seen = match control {
        Some(Control::SYN) => {
            let (original, reply) = swap_peers(sender, receiver, dir);
            return EstablishedUpdateResult::Invalid { original, reply };
        }
        Some(Control::RST) => return EstablishedUpdateResult::Reset,
        Some(Control::FIN) => true,
        None => false,
    };

    let new_sender = sender.update_sender(*seq, logical_len, *ack, *wnd, fin_seen);
    let new_receiver = receiver.update_receiver(*ack);

    let (new_original, new_reply) = swap_peers(new_sender, new_receiver, dir);
    EstablishedUpdateResult::Success { new_original, new_reply, fin_seen }
}

/// State for the Untracked state.
///
/// This state never transitions to another state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Untracked {}
state_from_state_struct!(Untracked);

impl Untracked {
    fn update(
        self,
        _segment: &SegmentHeader,
        _payload_len: usize,
        _dir: ConnectionDirection,
    ) -> (State, bool) {
        (Self {}.into(), true)
    }
}

/// State for the Closed state.
///
/// This state never transitions to another state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Closed {}
state_from_state_struct!(Closed);

impl Closed {
    fn update(
        self,
        _segment: &SegmentHeader,
        _payload_len: usize,
        _dir: ConnectionDirection,
    ) -> (State, bool) {
        (self.into(), true)
    }
}

/// State for the SynSent state.
///
/// State transitions for in-range segments by direction:
/// - Original
///   - SYN: Update data, stay in SynSent
///   - SYN/ACK: Invalid
///   - RST: Invalid
///   - FIN: Invalid
///   - ACK: Invalid
/// - Reply
///   - SYN: Untracked, until we support simultaneous open.
///   - SYN/ACK: WaitingOnOpeningAck
///   - RST: Delete connection
///   - FIN: Invalid
///   - ACK: Invalid
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SynSent {
    /// The ISS (initial send sequence number) for the original TCP stack.
    iss: SeqNum,

    /// The logical length of the segment sent by the original TCP stack.
    logical_len: u32,

    /// The window scale (if set in the initial SYN) for the original TCP stack.
    advertised_window_scale: Option<WindowScale>,

    /// The window size of the original TCP stack. Will be converted into a
    /// sequence number once we know what the reply stack's ISN is.
    ///
    /// RFC 1323 2.2:
    ///   The Window field in a SYN (i.e., a <SYN> or <SYN,ACK>) segment itself
    ///   is never scaled.
    window_size: WindowSize,
}
state_from_state_struct!(SynSent);

impl SynSent {
    fn update(
        self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> (State, bool) {
        let Self { iss, logical_len, advertised_window_scale, window_size } = self;

        match dir {
            // This is another packet in the same direction as the first one.
            // Update existing parameters for initial SYN, but only for packets
            // that look to be valid retransmits of the initial SYN. This
            // behavior is copied over from gVisor.
            ConnectionDirection::Original => {
                if let Some(_) = segment.ack {
                    return (self.into(), false);
                }

                match segment.control {
                    None | Some(Control::FIN) | Some(Control::RST) => {
                        return (self.into(), false);
                    }
                    Some(Control::SYN) => {}
                };

                if segment.seq != iss || segment.options.window_scale() != advertised_window_scale {
                    return (self.into(), false);
                }

                // If it's a valid retransmit of the original SYN, update
                // any state that changed and let it through.
                let seg_window_size = WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap();

                (
                    SynSent {
                        iss: iss,
                        logical_len: u32::max(segment.len(payload_len), logical_len),
                        advertised_window_scale: advertised_window_scale,
                        window_size: core::cmp::max(seg_window_size, window_size),
                    }
                    .into(),
                    true,
                )
            }

            ConnectionDirection::Reply => {
                // RFC 9293 3.10.7.3:
                //   If SND.UNA < SEG.ACK =< SND.NXT, then the ACK is
                //   acceptable.
                match segment.ack {
                    None => {}
                    Some(ack) => {
                        if !(ack.after(iss) && ack.before(iss + logical_len + 1)) {
                            return (self.into(), false);
                        }
                    }
                };

                match segment.control {
                    None | Some(Control::FIN) => (self.into(), false),
                    // RFC 9293 3.10.7.3:
                    //   If the RST bit is set,
                    //   If the ACK was acceptable, then signal to the user
                    //   "error: connection reset", drop the segment, enter
                    //   CLOSED state, delete TCB, and return. Otherwise (no
                    //   ACK), drop the segment and return.
                    //
                    // For our purposes, we delete the connection because we
                    // know the receiver will tear down the connection.
                    Some(Control::RST) => match segment.ack {
                        None => (self.into(), false),
                        Some(_) => (Closed {}.into(), true),
                    },

                    Some(Control::SYN) => {
                        let Some(ack) = segment.ack else {
                            // TODO(https://fxbug.dev/355200767): Support
                            // simultaneous open.
                            log::warn!(
                                "Unsupported TCP simultaneous open. Giving up on detailed tracking"
                            );

                            return (Untracked {}.into(), true);
                        };

                        let reply_window_scale = segment.options.window_scale();
                        let reply_window_size =
                            WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap();

                        // RFC 1323 2.2:
                        //   This option is an offer, not a promise; both sides
                        //   must send Window Scale options in their SYN
                        //   segments to enable window scaling in either
                        //   direction.
                        let (original_window_scale, reply_window_scale) =
                            match (advertised_window_scale, reply_window_scale) {
                                (Some(original), Some(reply)) => (original, reply),
                                _ => (WindowScale::ZERO, WindowScale::ZERO),
                            };

                        let original_max_next_seq = iss + logical_len;

                        (
                            WaitingOnOpeningAck {
                                original: Peer {
                                    window_scale: original_window_scale,
                                    max_wnd: window_size,
                                    // We're still waiting on an ACK from
                                    // the original stack, so this is
                                    // slightly different from the normal
                                    // calculation. It's still valid because
                                    // we can assume that the implicit ACK
                                    // number is the reply ISS (no data
                                    // ACKed).
                                    max_wnd_seq: segment.seq + window_size,
                                    max_next_seq: original_max_next_seq,
                                    unacked_data: ack.before(original_max_next_seq),
                                    fin_state: FinState::NotSent,
                                },
                                reply: Peer {
                                    window_scale: reply_window_scale,
                                    max_wnd: reply_window_size,
                                    max_wnd_seq: ack + reply_window_size,
                                    max_next_seq: segment.seq + segment.len(payload_len),
                                    // The reply peer will always have
                                    // unacked data here because the SYN we
                                    // just saw sent needs to be acked.
                                    unacked_data: true,
                                    fin_state: FinState::NotSent,
                                },
                            }
                            .into(),
                            true,
                        )
                    }
                }
            }
        }
    }
}

/// State for the WaitingOnOpeningAck state.
///
/// Note this expects the ACK to come from the original direction.
///
/// State transitions for in-range segments by direction:
/// - Original
///   - SYN: Invalid
///   - RST: Delete connection
///   - FIN: Closing
///   - ACK: Established
/// - Reply
///   - SYN: Invalid
///   - RST: Delete connection
///   - FIN: Closing
///   - ACK: WaitingOnOpeningAck
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitingOnOpeningAck {
    /// State for the "original" TCP stack (the one that we first saw a packet
    /// for).
    original: Peer,

    /// State for the "reply" TCP stack.
    reply: Peer,
}
state_from_state_struct!(WaitingOnOpeningAck);

impl WaitingOnOpeningAck {
    fn update(
        self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> (State, bool) {
        let Self { original, reply } = self;

        let (original, reply, fin_seen) = match do_established_update(
            UpdatePeers { original, reply },
            segment,
            payload_len,
            dir.clone(),
        ) {
            EstablishedUpdateResult::Success { new_original, new_reply, fin_seen } => {
                (new_original, new_reply, fin_seen)
            }
            EstablishedUpdateResult::Invalid { original, reply } => {
                return (Self { original, reply }.into(), false)
            }
            EstablishedUpdateResult::Reset => return (Closed {}.into(), true),
        };

        let new_state = if fin_seen {
            Closing { original, reply }.into()
        } else {
            match dir {
                // We move to Established because we know that if the ACK was
                // valid (checked by do_established_update), the ACK must
                // include the SYN, which is the first possible octet.
                ConnectionDirection::Original => Established { original, reply }.into(),
                ConnectionDirection::Reply => WaitingOnOpeningAck { original, reply }.into(),
            }
        };

        (new_state, true)
    }
}

/// State for the Established state.
///
/// State transitions for in-range segments, regardless of direction:
/// - SYN: Invalid
/// - RST: Delete connection
/// - FIN: Closing
/// - ACK: Established
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Established {
    original: Peer,
    reply: Peer,
}
state_from_state_struct!(Established);

impl Established {
    fn update(
        self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> (State, bool) {
        let Self { original, reply } = self;

        let (original, reply, fin_seen) = match do_established_update(
            UpdatePeers { original, reply },
            segment,
            payload_len,
            dir.clone(),
        ) {
            EstablishedUpdateResult::Success { new_original, new_reply, fin_seen } => {
                (new_original, new_reply, fin_seen)
            }
            EstablishedUpdateResult::Invalid { original, reply } => {
                return (Self { original, reply }.into(), false)
            }
            EstablishedUpdateResult::Reset => return (Closed {}.into(), true),
        };

        let new_state = if fin_seen {
            Closing { original, reply }.into()
        } else {
            Established { original, reply }.into()
        };

        (new_state, true)
    }
}

/// State for the Closing state.
///
/// State transitions for in-range segments regardless of direction:
/// - SYN: Invalid
/// - RST: Delete connection
/// - FIN: Closing
/// - ACK: Closing
///
/// The Closing state deletes the connection once FINs from both peers have been
/// ACKed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Closing {
    original: Peer,
    reply: Peer,
}
state_from_state_struct!(Closing);

impl Closing {
    fn update(
        self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> (State, bool) {
        let Self { original, reply } = self;

        // NOTE: Segments after a FIN are somewhat invalid, but we do not
        // attempt to handle them specially. Per RFC 9293 3.10.7.4:
        //
        //   Seventh, process the segment text. [After FIN,] this should not
        //   occur since a FIN has been received from the remote side. Ignore
        //   the segment text.
        //
        // Because these segments aren't completely invalid, handling them
        // properly (and consistently with the endpoints) is difficult. It is
        // not needed for correctness, since the connection will be torn down as
        // soon as there's an ACK for both FINs anyway. This extra invalid data
        // does not change that.
        //
        // Neither Linux nor gVisor do anything special for these segments.

        let (original, reply) = match do_established_update(
            UpdatePeers { original, reply },
            segment,
            payload_len,
            dir.clone(),
        ) {
            EstablishedUpdateResult::Success { new_original, new_reply, fin_seen: _ } => {
                (new_original, new_reply)
            }
            EstablishedUpdateResult::Invalid { original, reply } => {
                return (Self { original, reply }.into(), false)
            }
            EstablishedUpdateResult::Reset => return (Closed {}.into(), true),
        };

        if original.fin_state.acked() && reply.fin_state.acked() {
            // Removing the entry immediately is not expected to break any
            // use-cases. The endpoints are ultimately responsible for
            // respecting the TIME_WAIT state.
            //
            // The NAT entry will be removed as a consequence, but this is only
            // a problem if a server wanted to reopen the connection with the
            // client (but only during TIME_WAIT).
            //
            // TODO(https://fxbug.dev/355200767): Add TimeWait and reopening
            // connections once simultaneous open is supported.
            (Closed {}.into(), true)
        } else {
            (Closing { original, reply }.into(), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        do_established_update, Closed, Closing, Established, EstablishedUpdateResult, FinState,
        Peer, State, SynSent, Untracked, UpdatePeers, WaitingOnOpeningAck,
    };

    use assert_matches::assert_matches;
    use netstack3_base::{
        Control, HandshakeOptions, Options, SegmentHeader, SeqNum, UnscaledWindowSize, WindowScale,
        WindowSize,
    };
    use test_case::test_case;

    use crate::conntrack::ConnectionDirection;

    const ORIGINAL_ISS: SeqNum = SeqNum::new(0);
    const REPLY_ISS: SeqNum = SeqNum::new(8192);
    const ORIGINAL_WND: u16 = 16;
    const REPLY_WND: u16 = 17;
    const ORIGINAL_WS: u8 = 3;
    const REPLY_WS: u8 = 4;
    const ORIGINAL_PAYLOAD_LEN: usize = 12;
    const REPLY_PAYLOAD_LEN: usize = 13;

    impl Peer {
        pub fn arbitrary() -> Peer {
            Peer {
                max_next_seq: SeqNum::new(0),
                window_scale: WindowScale::new(0).unwrap(),
                max_wnd_seq: SeqNum::new(0),
                unacked_data: false,
                max_wnd: WindowSize::new(0).unwrap(),
                fin_state: FinState::NotSent,
            }
        }
    }

    #[test_case(None)]
    #[test_case(Some(Control::FIN))]
    #[test_case(Some(Control::RST))]
    fn syn_sent_original_non_syn_segment(control: Option<Control>) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: 3,
            advertised_window_scale: None,
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: ORIGINAL_ISS,
            ack: None,
            wnd: UnscaledWindowSize::from(ORIGINAL_WND),
            control,
            options: Options::default(),
        };

        let expected_state = state.clone().into();
        assert_eq!(
            state.update(&segment, ORIGINAL_PAYLOAD_LEN, ConnectionDirection::Original),
            (expected_state, false)
        );
    }

    #[test_case(SegmentHeader {
        // Different from existing.
        seq: ORIGINAL_ISS + 1,
        ack: None,
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: HandshakeOptions {
            // Same as existing.
            window_scale: WindowScale::new(ORIGINAL_WS),
            ..Default::default()
        }.into(),
    }; "different ISS")]
    #[test_case(SegmentHeader {
        // Same as existing.
        seq: ORIGINAL_ISS,
        ack: None,
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: HandshakeOptions {
            // Different from existing.
            window_scale: WindowScale::new(ORIGINAL_WS + 1),
            ..Default::default()
        }.into(),
    }; "different window scale")]
    #[test_case(SegmentHeader {
        seq: ORIGINAL_ISS,
        // ACK here is invalid.
        ack: Some(SeqNum::new(10)),
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: HandshakeOptions {
            window_scale: WindowScale::new(2),
            ..Default::default()
        }.into(),
    }; "ack not allowed")]
    fn syn_sent_original_syn_not_retransmit(segment: SegmentHeader) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: WindowScale::new(ORIGINAL_WS),
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let expected_state = state.clone().into();
        assert_eq!(
            state.update(&segment, ORIGINAL_PAYLOAD_LEN, ConnectionDirection::Original),
            (expected_state, false)
        );
    }

    #[test]
    fn syn_sent_original_syn_retransmit() {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: WindowScale::new(ORIGINAL_WS),
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: ORIGINAL_ISS,
            ack: None,
            wnd: UnscaledWindowSize::from(ORIGINAL_WND + 10),
            control: Some(Control::SYN),
            options: HandshakeOptions {
                window_scale: WindowScale::new(ORIGINAL_WS),
                ..Default::default()
            }
            .into(),
        };

        let result = assert_matches!(
            state.update(
                &segment,
                ORIGINAL_PAYLOAD_LEN + 10,
                ConnectionDirection::Original
            ),
            (State::SynSent(s), true) => s
        );

        assert_eq!(
            result,
            SynSent {
                iss: ORIGINAL_ISS,
                logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 10 + 1,
                advertised_window_scale: WindowScale::new(ORIGINAL_WS),
                window_size: WindowSize::from_u32(ORIGINAL_WND as u32 + 10).unwrap(),
            }
        )
    }

    #[test_case(None)]
    #[test_case(Some(Control::FIN))]
    fn syn_sent_reply_non_syn_segment(control: Option<Control>) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: None,
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: ORIGINAL_ISS,
            ack: None,
            wnd: UnscaledWindowSize::from(ORIGINAL_WND),
            control,
            options: Options::default(),
        };

        let expected_state = state.clone().into();
        assert_eq!(
            state.update(&segment, REPLY_PAYLOAD_LEN, ConnectionDirection::Reply),
            (expected_state, false)
        );
    }

    #[test_case(ORIGINAL_ISS, None; "small invalid")]
    #[test_case(
        ORIGINAL_ISS + 1, Some(Closed {}.into());
        "smallest valid"
    )]
    #[test_case(
        ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN as u32 + 1,
        Some(Closed {}.into());
        "largest valid"
    )]
    #[test_case(
        ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN as u32 + 2,
        None;
        "large invalid"
    )]
    fn syn_sent_reply_rst_segment(ack: SeqNum, new_state: Option<State>) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: None,
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: ORIGINAL_ISS,
            ack: Some(ack),
            wnd: UnscaledWindowSize::from(ORIGINAL_WND),
            control: Some(Control::RST),
            options: Options::default(),
        };

        let (expected_state, valid) = match new_state {
            Some(state) => (state, true),
            None => (state.clone().into(), false),
        };

        assert_eq!(
            state.update(&segment, /*payload_len*/ 0, ConnectionDirection::Reply),
            (expected_state, valid)
        );
    }

    #[test]
    fn syn_sent_reply_simultaneous_open() {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: WindowScale::new(ORIGINAL_WS),
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: ORIGINAL_ISS,
            ack: None,
            wnd: UnscaledWindowSize::from(ORIGINAL_WND + 10),
            control: Some(Control::SYN),
            options: Options::default(),
        };

        assert_eq!(
            state.update(&segment, /*payload_len*/ 0, ConnectionDirection::Reply),
            (Untracked {}.into(), true)
        );
    }

    #[test_case(ORIGINAL_ISS; "too low")]
    #[test_case(ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN + 2; "too high")]
    fn syn_sent_reply_syn_ack_not_in_range(ack: SeqNum) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: None,
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: REPLY_ISS,
            ack: Some(ack),
            wnd: UnscaledWindowSize::from(REPLY_WND),
            control: Some(Control::SYN),
            options: Options::default(),
        };

        let expected_state = state.clone().into();
        assert_eq!(
            state.update(&segment, REPLY_PAYLOAD_LEN, ConnectionDirection::Reply),
            (expected_state, false)
        );
    }

    #[test_case(None)]
    #[test_case(Some(WindowScale::new(REPLY_WS).unwrap()))]
    fn syn_sent_reply_syn_ack(reply_window_scale: Option<WindowScale>) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: WindowScale::new(ORIGINAL_WS),
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        let segment = SegmentHeader {
            seq: REPLY_ISS,
            ack: Some(ORIGINAL_ISS + 1),
            wnd: UnscaledWindowSize::from(REPLY_WND),
            control: Some(Control::SYN),
            options: HandshakeOptions { window_scale: reply_window_scale, ..Default::default() }
                .into(),
        };

        let new_state = assert_matches!(
            state.update(
                &segment,
                REPLY_PAYLOAD_LEN,
                ConnectionDirection::Reply
            ),
            (State::WaitingOnOpeningAck(s), true) => s
        );

        let (original_window_scale, reply_window_scale) = match reply_window_scale {
            Some(s) => (WindowScale::new(ORIGINAL_WS).unwrap(), s),
            None => (WindowScale::ZERO, WindowScale::ZERO),
        };

        assert_eq!(
            new_state,
            WaitingOnOpeningAck {
                original: Peer {
                    window_scale: original_window_scale,
                    max_wnd: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
                    max_wnd_seq: REPLY_ISS + ORIGINAL_WND as u32,
                    max_next_seq: ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN + 1,
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: reply_window_scale,
                    max_wnd: WindowSize::from_u32(REPLY_WND as u32).unwrap(),
                    max_wnd_seq: ORIGINAL_ISS + 1 + REPLY_WND as u32,
                    max_next_seq: REPLY_ISS + REPLY_PAYLOAD_LEN + 1,
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                }
            }
        );
    }

    #[test_case(FinState::NotSent, SeqNum::new(9) => FinState::Sent(SeqNum::new(9)))]
    #[test_case(FinState::Sent(SeqNum::new(9)), SeqNum::new(8) => FinState::Sent(SeqNum::new(8)))]
    #[test_case(FinState::Sent(SeqNum::new(9)), SeqNum::new(9) => FinState::Sent(SeqNum::new(9)))]
    #[test_case(FinState::Sent(SeqNum::new(9)), SeqNum::new(10) => FinState::Sent(SeqNum::new(9)))]
    #[test_case(FinState::Acked, SeqNum::new(9) => FinState::Acked)]
    fn fin_state_update_fin_sent(fin_state: FinState, seq: SeqNum) -> FinState {
        fin_state.update_fin_sent(seq)
    }

    #[test_case(FinState::NotSent, SeqNum::new(10) => FinState::NotSent)]
    #[test_case(FinState::Sent(SeqNum::new(9)), SeqNum::new(9) => FinState::Sent(SeqNum::new(9)))]
    #[test_case(FinState::Sent(SeqNum::new(9)), SeqNum::new(10) => FinState::Acked)]
    #[test_case(FinState::Acked, SeqNum::new(10) => FinState::Acked)]
    fn fin_state_update_ack_received(fin_state: FinState, ack: SeqNum) -> FinState {
        fin_state.update_ack_received(ack)
    }

    const RECV_MAX_NEXT_SEQ: SeqNum = SeqNum::new(66_001);
    const RECV_MAX_WND_SEQ: SeqNum = SeqNum::new(1424);

    #[test_case(SeqNum::new(424), 200, SeqNum::new(1) => true; "success low seq/ack")]
    #[test_case(RECV_MAX_WND_SEQ, 0, RECV_MAX_NEXT_SEQ => true; "success high seq/ack")]
    #[test_case(RECV_MAX_WND_SEQ + 1, 0, SeqNum::new(1) => false; "bad equation I")]
    #[test_case(SeqNum::new(424), 199, SeqNum::new(1) => false; "bad equation II")]
    #[test_case(SeqNum::new(424), 200, RECV_MAX_NEXT_SEQ + 1 => false; "bad equation III")]
    #[test_case(SeqNum::new(424), 200, SeqNum::new(0) => false; "bad equation IV")]
    fn ack_segment_valid_test(seq: SeqNum, len: u32, ack: SeqNum) -> bool {
        let sender = Peer { max_next_seq: SeqNum::new(1024), ..Peer::arbitrary() };

        // MAXACKWINDOW is going to be 66000 due to window shift of 0.
        let receiver = Peer {
            window_scale: WindowScale::new(0).unwrap(),
            max_wnd: WindowSize::new(400).unwrap(),
            max_next_seq: RECV_MAX_NEXT_SEQ,
            max_wnd_seq: RECV_MAX_WND_SEQ,
            ..Peer::arbitrary()
        };

        Peer::ack_segment_valid(&sender, &receiver, seq, len, ack)
    }

    struct PeerUpdateSenderArgs {
        seq: SeqNum,
        len: u32,
        ack: SeqNum,
        wnd: UnscaledWindowSize,
        fin_seen: bool,
    }

    #[test_case(
        Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(16).unwrap(),
            max_wnd_seq: SeqNum::new(127),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::NotSent,
        },
        PeerUpdateSenderArgs {
            seq: SeqNum::new(1025),
            len: 10,
            ack: SeqNum::new(100),
            wnd: UnscaledWindowSize::from_u32(4),
            fin_seen: false
        } => Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(32).unwrap(),
            max_wnd_seq: SeqNum::new(132),
            max_next_seq: SeqNum::new(1035),
            unacked_data: true,
            fin_state: FinState::NotSent,
        }; "packet larger"
    )]
    #[test_case(
        Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(16).unwrap(),
            max_wnd_seq: SeqNum::new(127),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::NotSent,
        },
        PeerUpdateSenderArgs {
            seq: SeqNum::new(1000),
            len: 10,
            ack: SeqNum::new(0),
            wnd: UnscaledWindowSize::from_u32(0),
            fin_seen: false
        } => Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(16).unwrap(),
            max_wnd_seq: SeqNum::new(127),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::NotSent,
        }; "packet smaller"
    )]
    #[test_case(
        Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(16).unwrap(),
            max_wnd_seq: SeqNum::new(127),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::NotSent,
        },
        PeerUpdateSenderArgs {
            seq: SeqNum::new(1000),
            len: 10,
            ack: SeqNum::new(0),
            wnd: UnscaledWindowSize::from_u32(0),
            fin_seen: true
        } => Peer {
            window_scale: WindowScale::new(3).unwrap(),
            max_wnd: WindowSize::new(16).unwrap(),
            max_wnd_seq: SeqNum::new(127),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::Sent(SeqNum::new(1000 + 9)),
        }; "fin sent"
    )]
    fn peer_update_sender_test(peer: Peer, args: PeerUpdateSenderArgs) -> Peer {
        peer.update_sender(args.seq, args.len, args.ack, args.wnd, args.fin_seen)
    }

    #[test_case(
        Peer { max_next_seq: SeqNum::new(1024), ..Peer::arbitrary() },
        SeqNum::new(1024) => Peer { max_next_seq: SeqNum::new(1024), ..Peer::arbitrary() };
        "unset unacked data"
    )]
    #[test_case(
        Peer { max_next_seq: SeqNum::new(1024), ..Peer::arbitrary() },
        SeqNum::new(1023) => Peer { max_next_seq: SeqNum::new(1024), ..Peer::arbitrary() };
        "don't unset unacked data"
    )]
    #[test_case(
        Peer { fin_state: FinState::Sent(SeqNum::new(9)), ..Peer::arbitrary() },
        SeqNum::new(10) => Peer { fin_state: FinState::Acked, ..Peer::arbitrary() };
        "update fin state"
    )]
    fn peer_update_receiver_test(peer: Peer, ack: SeqNum) -> Peer {
        peer.update_receiver(ack)
    }

    // This is mostly to ensure that we don't accidentally overflow a u32, since
    // it's such a basic calculation.
    #[test]
    fn peer_max_ack_window() {
        let max_peer = Peer { window_scale: WindowScale::MAX, ..Peer::arbitrary() };
        let min_peer = Peer { window_scale: WindowScale::new(0).unwrap(), ..Peer::arbitrary() };

        assert_eq!(max_peer.max_ack_window(), 1_081_344_000u32);
        assert_eq!(min_peer.max_ack_window(), 66_000u32);
    }

    enum EstablishedUpdateTestResult {
        Success { new_original: Peer, new_reply: Peer, fin_seen: bool },
        Invalid,
        Reset,
    }

    struct EstablishedUpdateTestArgs {
        segment: SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
        expected: EstablishedUpdateTestResult,
    }

    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: EstablishedUpdateTestResult::Success {
                new_original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    // Changed.
                    max_wnd: WindowSize::new(40).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    // Changed.
                    max_next_seq: SeqNum::new(1424),
                    // Changed.
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
                new_reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_001),
                    // Changed.
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
                fin_seen: false,
            }
        }; "success original"
    )]
    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(66_100),
                ack: Some(SeqNum::new(1024)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::FIN),
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Reply,
            expected: EstablishedUpdateTestResult::Success {
              // No changes in new_original.
              new_original: Peer {
                  window_scale: WindowScale::new(2).unwrap(),
                  max_wnd: WindowSize::new(0).unwrap(),
                  max_wnd_seq: SeqNum::new(70_000),
                  max_next_seq: SeqNum::new(1024),
                  unacked_data: false,
                  fin_state: FinState::NotSent,
              },
              new_reply: Peer {
                  window_scale: WindowScale::new(0).unwrap(),
                  max_wnd: WindowSize::new(400).unwrap(),
                  max_wnd_seq: SeqNum::new(1424),
                  max_next_seq: SeqNum::new(66_101),
                  unacked_data: true,
                  fin_state: FinState::Sent(SeqNum::new(66_100)),
              },
              fin_seen: true,
            }
        }; "success reply"
    )]
    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::RST),
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: EstablishedUpdateTestResult::Reset,
        }; "RST"
    )]
    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: None,
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            // These don't matter for the test
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: EstablishedUpdateTestResult::Invalid,
        }; "missing ack"
    )]
    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                // Too low. Doesn't meet equation II.
                seq: SeqNum::new(0),
                ack: None,
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            // These don't matter for the test
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: EstablishedUpdateTestResult::Invalid,
        }; "invalid equation bounds"
    )]
    #[test_case(
        EstablishedUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::SYN),
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: EstablishedUpdateTestResult::Invalid,
        }; "SYN not allowed"
    )]
    fn do_established_update_test(args: EstablishedUpdateTestArgs) {
        let original = Peer {
            window_scale: WindowScale::new(2).unwrap(),
            max_wnd: WindowSize::new(0).unwrap(),
            max_wnd_seq: SeqNum::new(70_000),
            max_next_seq: SeqNum::new(1024),
            unacked_data: false,
            fin_state: FinState::NotSent,
        };

        let reply = Peer {
            window_scale: WindowScale::new(0).unwrap(),
            max_wnd: WindowSize::new(400).unwrap(),
            max_wnd_seq: SeqNum::new(1424),
            max_next_seq: SeqNum::new(66_001),
            unacked_data: true,
            fin_state: FinState::NotSent,
        };

        let expected_result = match args.expected {
            EstablishedUpdateTestResult::Success { new_original, new_reply, fin_seen } => {
                EstablishedUpdateResult::Success { new_original, new_reply, fin_seen }
            }
            EstablishedUpdateTestResult::Invalid => EstablishedUpdateResult::Invalid {
                original: original.clone(),
                reply: reply.clone(),
            },
            EstablishedUpdateTestResult::Reset => EstablishedUpdateResult::Reset,
        };

        assert_eq!(
            do_established_update(
                UpdatePeers { original: original, reply: reply },
                &args.segment,
                args.payload_len,
                args.dir,
            ),
            expected_result
        );
    }

    struct StateUpdateTestArgs {
        segment: SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
        expected: Option<State>,
    }

    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: Some(Established {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(40).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1424),
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_001),
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
            }.into()),
        }; "established"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(66_100),
                ack: Some(SeqNum::new(1024)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::FIN),
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Reply,
            expected: Some(Closing {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(0).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1024),
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_101),
                    unacked_data: true,
                    fin_state: FinState::Sent(SeqNum::new(66_100)),
                },
            }.into()),
        }; "closing"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(66_100),
                ack: Some(SeqNum::new(1024)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Reply,
            expected: Some(WaitingOnOpeningAck {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(0).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1024),
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_100),
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
            }.into())
        }; "update in place"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                // Fails equation I.
                seq: SeqNum::new(100_000),
                ack: Some(SeqNum::new(1024)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Reply,
            expected: None
        }; "invalid"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::RST),
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: Some(Closed {}.into()),
        }; "rst"
    )]
    fn waiting_on_opening_ack_test(args: StateUpdateTestArgs) {
        let state = WaitingOnOpeningAck {
            original: Peer {
                window_scale: WindowScale::new(2).unwrap(),
                max_wnd: WindowSize::new(0).unwrap(),
                max_wnd_seq: SeqNum::new(70_000),
                max_next_seq: SeqNum::new(1024),
                unacked_data: false,
                fin_state: FinState::NotSent,
            },
            reply: Peer {
                window_scale: WindowScale::new(0).unwrap(),
                max_wnd: WindowSize::new(400).unwrap(),
                max_wnd_seq: SeqNum::new(1424),
                max_next_seq: SeqNum::new(66_001),
                unacked_data: true,
                fin_state: FinState::NotSent,
            },
        };

        let (new_state, valid) = match args.expected {
            Some(new_state) => (new_state, true),
            None => (state.clone().into(), false),
        };

        assert_eq!(state.update(&args.segment, args.payload_len, args.dir), (new_state, valid));
    }

    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: Some(Established {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(40).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1424),
                    // This is becoming true because `segment.seq > original.max_next_seq`.
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_001),
                    // This is becoming true because `segment.ack == reply.max_next_seq`.
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
            }.into()),
        }; "update original"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(66_100),
                ack: Some(SeqNum::new(1024)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::FIN),
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Reply,
            expected: Some(Closing {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(0).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1024),
                    unacked_data: false,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_101),
                    unacked_data: true,
                    fin_state: FinState::Sent(SeqNum::new(66_100)),
                },
            }.into()),
        }; "closing"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                // Fails equation III.
                ack: Some(SeqNum::new(100_000)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: None,
        }; "invalid"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::RST),
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: Some(Closed {}.into()),
        }; "rst"
    )]
    fn established_test(args: StateUpdateTestArgs) {
        let state = Established {
            original: Peer {
                window_scale: WindowScale::new(2).unwrap(),
                max_wnd: WindowSize::new(0).unwrap(),
                max_wnd_seq: SeqNum::new(70_000),
                max_next_seq: SeqNum::new(1024),
                unacked_data: false,
                fin_state: FinState::NotSent,
            },
            reply: Peer {
                window_scale: WindowScale::new(0).unwrap(),
                max_wnd: WindowSize::new(400).unwrap(),
                max_wnd_seq: SeqNum::new(1424),
                max_next_seq: SeqNum::new(66_001),
                unacked_data: true,
                fin_state: FinState::NotSent,
            },
        };

        let (new_state, valid) = match args.expected {
            Some(new_state) => (new_state, true),
            None => (state.clone().into(), false),
        };

        assert_eq!(state.update(&args.segment, args.payload_len, args.dir), (new_state, valid));
    }

    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: Some(Closing {
                original: Peer {
                    window_scale: WindowScale::new(2).unwrap(),
                    max_wnd: WindowSize::new(40).unwrap(),
                    max_wnd_seq: SeqNum::new(70_000),
                    max_next_seq: SeqNum::new(1424),
                    unacked_data: true,
                    fin_state: FinState::NotSent,
                },
                reply: Peer {
                    window_scale: WindowScale::new(0).unwrap(),
                    max_wnd: WindowSize::new(400).unwrap(),
                    max_wnd_seq: SeqNum::new(1424),
                    max_next_seq: SeqNum::new(66_001),
                    unacked_data: false,
                    fin_state: FinState::Acked,
                },
            }.into()),
        }; "update original"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                // Fails equation III.
                ack: Some(SeqNum::new(100_000)),
                wnd: UnscaledWindowSize::from(10),
                control: None,
                options: Options::default(),
            },
            payload_len: 24,
            dir: ConnectionDirection::Original,
            expected: None,
        }; "invalid"
    )]
    #[test_case(
        StateUpdateTestArgs {
            segment: SegmentHeader {
                seq: SeqNum::new(1400),
                ack: Some(SeqNum::new(66_001)),
                wnd: UnscaledWindowSize::from(10),
                control: Some(Control::RST),
                options: Options::default(),
            },
            payload_len: 0,
            dir: ConnectionDirection::Original,
            expected: Some (Closed {}.into())
        }; "rst"
    )]
    fn closing_test(args: StateUpdateTestArgs) {
        let state = Closing {
            original: Peer {
                window_scale: WindowScale::new(2).unwrap(),
                max_wnd: WindowSize::new(0).unwrap(),
                max_wnd_seq: SeqNum::new(70_000),
                max_next_seq: SeqNum::new(1024),
                unacked_data: true,
                fin_state: FinState::NotSent,
            },
            reply: Peer {
                window_scale: WindowScale::new(0).unwrap(),
                max_wnd: WindowSize::new(400).unwrap(),
                max_wnd_seq: SeqNum::new(1424),
                max_next_seq: SeqNum::new(66_001),
                unacked_data: false,
                fin_state: FinState::Acked,
            },
        };

        let (new_state, valid) = match args.expected {
            Some(new_state) => (new_state, true),
            None => (state.clone().into(), false),
        };

        assert_eq!(state.update(&args.segment, args.payload_len, args.dir), (new_state, valid));
    }

    #[test]
    fn closing_complete_test() {
        let state = Closing {
            original: Peer {
                window_scale: WindowScale::new(2).unwrap(),
                max_wnd: WindowSize::new(0).unwrap(),
                max_wnd_seq: SeqNum::new(70_000),
                max_next_seq: SeqNum::new(1024),
                unacked_data: true,
                fin_state: FinState::Sent(SeqNum::new(1023)),
            },
            reply: Peer {
                window_scale: WindowScale::new(0).unwrap(),
                max_wnd: WindowSize::new(400).unwrap(),
                max_wnd_seq: SeqNum::new(1424),
                max_next_seq: SeqNum::new(66_001),
                unacked_data: false,
                fin_state: FinState::Acked,
            },
        };

        let segment = SegmentHeader {
            seq: SeqNum::new(66_100),
            ack: Some(SeqNum::new(1024)),
            wnd: UnscaledWindowSize::from(10),
            control: None,
            options: Options::default(),
        };

        assert_matches!(
            state.update(&segment, /* payload_len */ 0, ConnectionDirection::Reply),
            (State::Closed(_), true)
        );
    }
}
