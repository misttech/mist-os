// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TCP state tracking.

use core::time::Duration;

use netstack3_base::{Control, SegmentHeader, SeqNum, WindowScale, WindowSize};

use super::{ConnectionDirection, ConnectionUpdateAction, ConnectionUpdateError};

/// The time since the last seen packet after which an unestablished TCP
/// connection is considered expired and is eligible for garbage collection.
///
/// This is small because it's just meant to be the time between the initial SYN
/// and response SYN/ACK packet.
const CONNECTION_EXPIRY_TIME_TCP_UNESTABLISHED: Duration = Duration::from_secs(30);

/// The time since the last seen packet after which an established TCP
/// connection is considered expired and is eligible for garbage collection.
///
/// Until we have TCP tracking, this is a large value to ensure that connections
/// that are still valid aren't cleaned up prematurely.
const CONNECTION_EXPIRY_TIME_TCP_ESTABLISHED: Duration = Duration::from_secs(6 * 60 * 60);

/// A struct that completely encapsulates tracking a bidirectional TCP
/// connection.
#[derive(Debug, Clone)]
pub(crate) struct Connection {
    /// TODO(https://fxbug.dev/328064736): Remove once we have implemented the
    /// ESTABLISHED state.
    established: bool,
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
            established: false,
        })
    }

    pub fn expiry_duration(&self) -> Duration {
        if self.is_established() {
            CONNECTION_EXPIRY_TIME_TCP_ESTABLISHED
        } else {
            CONNECTION_EXPIRY_TIME_TCP_UNESTABLISHED
        }
    }

    pub fn is_established(&self) -> bool {
        self.established
    }

    pub fn update(
        &mut self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        // TODO(https://fxbug.dev/328064736): Remove once we have implemented
        // the ESTABLISHED state.
        match dir {
            ConnectionDirection::Original => {}
            ConnectionDirection::Reply => self.established = true,
        }

        self.state.update(segment, payload_len, dir)
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
}

impl State {
    pub fn new(segment: &SegmentHeader, payload_len: usize) -> Option<Self> {
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
                advertised_window_scale: segment.options.window_scale,
                // This unwrap cannot fail because WindowSize::MAX is 2^30-1,
                // which is larger than the largest possible unscaled window
                // size (2^16).
                window_size: WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap(),
            }
            .into(),
        )
    }

    pub fn update(
        &mut self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        let (new_state, action) = match self {
            State::Untracked(s) => s.update(segment, payload_len, dir),
            State::SynSent(s) => s.update(segment, payload_len, dir),
            State::WaitingOnOpeningAck(s) => s.update(segment, payload_len, dir),
        }?;

        if let Some(new_state) = new_state {
            *self = new_state;
        }

        Ok(action)
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
/// the largest possible window (without scaling).
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
}

/// State for the Untracked state.
///
/// This state never transitions to another state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Untracked {}
state_from_state_struct!(Untracked);

impl Untracked {
    fn update(
        &self,
        _segment: &SegmentHeader,
        _payload_len: usize,
        _dir: ConnectionDirection,
    ) -> Result<(Option<State>, ConnectionUpdateAction), ConnectionUpdateError> {
        Ok((None, ConnectionUpdateAction::NoAction))
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
        &self,
        segment: &SegmentHeader,
        payload_len: usize,
        dir: ConnectionDirection,
    ) -> Result<(Option<State>, ConnectionUpdateAction), ConnectionUpdateError> {
        let Self { iss, logical_len, advertised_window_scale, window_size } = self;

        match dir {
            // This is another packet in the same direction as the first one.
            // Update existing parameters for initial SYN, but only for packets
            // that look to be valid retransmits of the initial SYN. This
            // behavior is copied over from gVisor.
            ConnectionDirection::Original => {
                if let Some(_) = segment.ack {
                    return Err(ConnectionUpdateError::InvalidPacket);
                }

                match segment.control {
                    None | Some(Control::FIN) | Some(Control::RST) => {
                        return Err(ConnectionUpdateError::InvalidPacket)
                    }
                    Some(Control::SYN) => {}
                };

                if segment.seq != *iss || segment.options.window_scale != *advertised_window_scale {
                    return Err(ConnectionUpdateError::InvalidPacket);
                }

                // If it's a valid retransmit of the original SYN, update
                // any state that changed and let it through.
                let seg_window_size = WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap();

                Ok((
                    Some(
                        SynSent {
                            iss: *iss,
                            logical_len: u32::max(segment.len(payload_len), *logical_len),
                            advertised_window_scale: *advertised_window_scale,
                            window_size: core::cmp::max(seg_window_size, *window_size),
                        }
                        .into(),
                    ),
                    ConnectionUpdateAction::NoAction,
                ))
            }

            ConnectionDirection::Reply => {
                // RFC 9293 3.10.7.3:
                //   If SND.UNA < SEG.ACK =< SND.NXT, then the ACK is
                //   acceptable.
                match segment.ack {
                    None => {}
                    Some(ack) => {
                        if !(ack.after(*iss) && ack.before(*iss + *logical_len + 1)) {
                            return Err(ConnectionUpdateError::InvalidPacket);
                        }
                    }
                };

                match segment.control {
                    None | Some(Control::FIN) => Err(ConnectionUpdateError::InvalidPacket),
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
                        None => Err(ConnectionUpdateError::InvalidPacket),
                        Some(_) => Ok((None, ConnectionUpdateAction::RemoveEntry)),
                    },

                    Some(Control::SYN) => {
                        let Some(ack) = segment.ack else {
                            // TODO(https://fxbug.dev/355200767): Support
                            // simultaneous open.
                            log::warn!(
                                "Unsupported TCP simultaneous open. Giving up on detailed tracking"
                            );

                            return Ok((
                                Some(Untracked {}.into()),
                                ConnectionUpdateAction::NoAction,
                            ));
                        };

                        let reply_window_scale = segment.options.window_scale;
                        let reply_window_size =
                            WindowSize::from_u32(u16::from(segment.wnd).into()).unwrap();

                        // RFC 1323 2.2:
                        //   This option is an offer, not a promise; both sides
                        //   must send Window Scale options in their SYN
                        //   segments to enable window scaling in either
                        //   direction.
                        let (original_window_scale, reply_window_scale) =
                            match (*advertised_window_scale, reply_window_scale) {
                                (Some(original), Some(reply)) => (original, reply),
                                _ => (WindowScale::ZERO, WindowScale::ZERO),
                            };

                        let original_max_next_seq = *iss + *logical_len;

                        Ok((
                            Some(
                                WaitingOnOpeningAck {
                                    original: Peer {
                                        window_scale: original_window_scale,
                                        max_wnd: *window_size,
                                        // We're still waiting on an ACK from
                                        // the original stack, so this is
                                        // slightly different from the normal
                                        // calculation. It's still valid because
                                        // we can assume that the implicit ACK
                                        // number is the reply ISS (no data
                                        // ACKed).
                                        max_wnd_seq: segment.seq + *window_size,
                                        max_next_seq: original_max_next_seq,
                                        unacked_data: ack.before(original_max_next_seq),
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
                                    },
                                }
                                .into(),
                            ),
                            ConnectionUpdateAction::NoAction,
                        ))
                    }
                }
            }
        }
    }
}

/// State for the WaitingOnOpeningAck state.
///
/// Note that we're always expecting the ACK to come from the original
/// direction.
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
        &self,
        _segment: &SegmentHeader,
        _payload_len: usize,
        _dir: ConnectionDirection,
    ) -> Result<(Option<State>, ConnectionUpdateAction), ConnectionUpdateError> {
        Ok((None, ConnectionUpdateAction::NoAction))
    }
}

#[cfg(test)]
mod tests {
    use super::{Peer, State, SynSent, Untracked};

    use assert_matches::assert_matches;
    use netstack3_base::{
        Control, Options, SegmentHeader, SeqNum, UnscaledWindowSize, WindowScale, WindowSize,
    };
    use test_case::test_case;

    use crate::conntrack::tcp::WaitingOnOpeningAck;
    use crate::conntrack::{ConnectionDirection, ConnectionUpdateAction, ConnectionUpdateError};

    const ORIGINAL_ISS: SeqNum = SeqNum::new(0);
    const REPLY_ISS: SeqNum = SeqNum::new(8192);
    const ORIGINAL_WND: u16 = 16;
    const REPLY_WND: u16 = 17;
    const ORIGINAL_WS: u8 = 3;
    const REPLY_WS: u8 = 4;
    const ORIGINAL_PAYLOAD_LEN: usize = 12;
    const REPLY_PAYLOAD_LEN: usize = 13;

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

        assert_matches!(
            state.update(&segment, ORIGINAL_PAYLOAD_LEN, ConnectionDirection::Original),
            Err(ConnectionUpdateError::InvalidPacket)
        );
    }

    #[test_case(SegmentHeader {
        // Different from existing.
        seq: ORIGINAL_ISS + 1,
        ack: None,
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: Options {
            mss: None,
            // Same as existing.
            window_scale: WindowScale::new(ORIGINAL_WS),
        },
    }; "different ISS")]
    #[test_case(SegmentHeader {
        // Same as existing.
        seq: ORIGINAL_ISS,
        ack: None,
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: Options {
            mss: None,
            // Different from existing.
            window_scale: WindowScale::new(ORIGINAL_WS + 1),
        },
    }; "different window scale")]
    #[test_case(SegmentHeader {
        seq: ORIGINAL_ISS,
        // ACK here is invalid.
        ack: Some(SeqNum::new(10)),
        wnd: UnscaledWindowSize::from(ORIGINAL_WND),
        control: Some(Control::SYN),
        options: Options {
            mss: None,
            window_scale: WindowScale::new(2),
        },
    }; "ack not allowed")]
    fn syn_sent_original_syn_not_retransmit(segment: SegmentHeader) {
        let state = SynSent {
            iss: ORIGINAL_ISS,
            logical_len: ORIGINAL_PAYLOAD_LEN as u32 + 1,
            advertised_window_scale: WindowScale::new(ORIGINAL_WS),
            window_size: WindowSize::from_u32(ORIGINAL_WND as u32).unwrap(),
        };

        assert_matches!(
            state.update(&segment, ORIGINAL_PAYLOAD_LEN, ConnectionDirection::Original),
            Err(ConnectionUpdateError::InvalidPacket)
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
            options: Options { mss: None, window_scale: WindowScale::new(ORIGINAL_WS) },
        };

        let result = assert_matches!(
            state.update(
                &segment,
                ORIGINAL_PAYLOAD_LEN + 10,
                ConnectionDirection::Original
            ),
            Ok((Some(State::SynSent(s)), ConnectionUpdateAction::NoAction)) => s
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

        assert_matches!(
            state.update(&segment, REPLY_PAYLOAD_LEN, ConnectionDirection::Reply),
            Err(ConnectionUpdateError::InvalidPacket)
        );
    }

    #[test_case(ORIGINAL_ISS => Err(ConnectionUpdateError::InvalidPacket); "small invalid")]
    #[test_case(
        ORIGINAL_ISS + 1 => Ok((None, ConnectionUpdateAction::RemoveEntry));
        "smallest valid"
    )]
    #[test_case(
        ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN as u32 + 1 =>
            Ok((None, ConnectionUpdateAction::RemoveEntry));
        "largest valid"
    )]
    #[test_case(
        ORIGINAL_ISS + ORIGINAL_PAYLOAD_LEN as u32 + 2 => Err(ConnectionUpdateError::InvalidPacket);
        "large invalid"
    )]
    fn syn_sent_reply_rst_segment(
        ack: SeqNum,
    ) -> Result<(Option<State>, ConnectionUpdateAction), ConnectionUpdateError> {
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

        state.update(&segment, /*payload_len*/ 0, ConnectionDirection::Reply)
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
            Ok((Some(Untracked {}.into()), ConnectionUpdateAction::NoAction))
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

        assert_matches!(
            state.update(&segment, REPLY_PAYLOAD_LEN, ConnectionDirection::Reply),
            Err(ConnectionUpdateError::InvalidPacket)
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
            options: Options { mss: None, window_scale: reply_window_scale },
        };

        let new_state = assert_matches!(
            state.update(
                &segment,
                REPLY_PAYLOAD_LEN,
                ConnectionDirection::Reply
            ),
            Ok((Some(State::WaitingOnOpeningAck(s)), ConnectionUpdateAction::NoAction)) => s
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
                    unacked_data: true
                },
                reply: Peer {
                    window_scale: reply_window_scale,
                    max_wnd: WindowSize::from_u32(REPLY_WND as u32).unwrap(),
                    max_wnd_seq: ORIGINAL_ISS + 1 + REPLY_WND as u32,
                    max_next_seq: REPLY_ISS + REPLY_PAYLOAD_LEN + 1,
                    unacked_data: true
                }
            }
        );
    }
}
