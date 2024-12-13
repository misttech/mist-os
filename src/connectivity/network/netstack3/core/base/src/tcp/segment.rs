// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The definition of a TCP segment.

use crate::alloc::borrow::ToOwned;
use core::borrow::Borrow;
use core::convert::TryFrom as _;
use core::fmt::Debug;
use core::mem::MaybeUninit;
use core::num::{NonZeroU16, TryFromIntError};
use core::ops::Range;

use log::info;
use net_types::ip::IpAddress;
use packet::records::options::OptionSequenceBuilder;
use packet::InnerSerializer;
use packet_formats::tcp::options::TcpOption;
use packet_formats::tcp::{TcpSegment, TcpSegmentBuilder, TcpSegmentBuilderWithOptions};
use thiserror::Error;

use super::base::{Control, Mss};
use super::seqnum::{SeqNum, UnscaledWindowSize, WindowScale, WindowSize};

/// A TCP segment.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Segment<P> {
    /// The non-payload information of the segment.
    pub header: SegmentHeader,
    /// The data carried by the segment.
    ///
    /// It is guaranteed that data.len() plus the length of the control flag
    /// (SYN or FIN) is <= MAX_PAYLOAD_AND_CONTROL_LEN
    pub data: P,
}

/// All non-data portions of a TCP segment.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SegmentHeader {
    /// The sequence number of the segment.
    pub seq: SeqNum,
    /// The acknowledge number of the segment. [`None`] if not present.
    pub ack: Option<SeqNum>,
    /// The advertised window size.
    pub wnd: UnscaledWindowSize,
    /// The control flag of the segment.
    pub control: Option<Control>,
    /// Options carried by this segment.
    pub options: Options,
}

/// Contains all supported TCP options.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Options {
    /// The MSS option (only set on handshake).
    pub mss: Option<Mss>,

    /// The WS option (only set on handshake).
    pub window_scale: Option<WindowScale>,
}

impl Options {
    /// Returns an iterator over the contained options.
    pub fn iter(&self) -> impl Iterator<Item = TcpOption<'static>> + core::fmt::Debug + Clone {
        self.mss
            .map(|mss| TcpOption::Mss(mss.get().get()))
            .into_iter()
            .chain(self.window_scale.map(|ws| TcpOption::WindowScale(ws.get())))
    }

    /// Creates a new [`Options`] from an iterator of TcpOption.
    pub fn from_iter<'a>(iter: impl IntoIterator<Item = TcpOption<'a>>) -> Self {
        let mut options = Options::default();
        for option in iter {
            match option {
                TcpOption::Mss(mss) => {
                    options.mss = NonZeroU16::new(mss).map(Mss);
                }
                TcpOption::WindowScale(ws) => {
                    // Per RFC 7323 Section 2.3:
                    //   If a Window Scale option is received with a shift.cnt
                    //   value larger than 14, the TCP SHOULD log the error but
                    //   MUST use 14 instead of the specified value.
                    if ws > WindowScale::MAX.get() {
                        info!(
                            "received an out-of-range window scale: {}, want < {}",
                            ws,
                            WindowScale::MAX.get()
                        );
                    }
                    options.window_scale = Some(WindowScale::new(ws).unwrap_or(WindowScale::MAX));
                }
                // TODO(https://fxbug.dev/42072902): We don't support these yet.
                TcpOption::SackPermitted
                | TcpOption::Sack(_)
                | TcpOption::Timestamp { ts_val: _, ts_echo_reply: _ } => {}
            }
        }
        options
    }
}

/// The maximum length that the sequence number doesn't wrap around.
pub const MAX_PAYLOAD_AND_CONTROL_LEN: usize = 1 << 31;
// The following `as` is sound because it is representable by `u32`.
const MAX_PAYLOAD_AND_CONTROL_LEN_U32: u32 = MAX_PAYLOAD_AND_CONTROL_LEN as u32;

impl<P: Payload> Segment<P> {
    /// Creates a new segment with data and options.
    ///
    /// Returns the segment along with how many bytes were removed to make sure
    /// sequence numbers don't wrap around, i.e., `seq.before(seq + seg.len())`.
    pub fn with_data_options(
        seq: SeqNum,
        ack: Option<SeqNum>,
        control: Option<Control>,
        wnd: UnscaledWindowSize,
        data: P,
        options: Options,
    ) -> (Self, usize) {
        let has_control_len = control.map(Control::has_sequence_no).unwrap_or(false);

        let discarded_len =
            data.len().saturating_sub(MAX_PAYLOAD_AND_CONTROL_LEN - usize::from(has_control_len));

        let (control, data) = if discarded_len > 0 {
            // If we have to truncate the segment, the FIN flag must be removed
            // because it is logically the last octet of the segment.
            let (control, control_len) = if control == Some(Control::FIN) {
                (None, 0)
            } else {
                (control, has_control_len.into())
            };
            // The following slice will not panic because `discarded_len > 0`,
            // thus `data.len() > MAX_PAYLOAD_AND_CONTROL_LEN - control_len`.
            (control, data.slice(0..MAX_PAYLOAD_AND_CONTROL_LEN_U32 - control_len))
        } else {
            (control, data)
        };

        (
            Segment { header: SegmentHeader { seq, ack, wnd, control, options }, data: data },
            discarded_len,
        )
    }

    /// Creates a new segment with data.
    ///
    /// Returns the segment along with how many bytes were removed to make sure
    /// sequence numbers don't wrap around, i.e., `seq.before(seq + seg.len())`.
    pub fn with_data(
        seq: SeqNum,
        ack: Option<SeqNum>,
        control: Option<Control>,
        wnd: UnscaledWindowSize,
        data: P,
    ) -> (Self, usize) {
        Self::with_data_options(seq, ack, control, wnd, data, Options::default())
    }

    /// Maps the payload in the segment with `f`.
    pub fn map_payload<R, F: FnOnce(P) -> R>(self, f: F) -> Segment<R> {
        let Segment { header, data } = self;
        Segment { header, data: f(data) }
    }

    /// Returns the length of the segment in sequence number space.
    ///
    /// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-25):
    ///   SEG.LEN = the number of octets occupied by the data in the segment
    ///   (counting SYN and FIN)
    pub fn len(&self) -> u32 {
        self.header.len(self.data.len())
    }

    /// Returns the part of the incoming segment within the receive window.
    pub fn overlap(self, rnxt: SeqNum, rwnd: WindowSize) -> Option<Segment<P>> {
        let len = self.len();
        let Segment { header: SegmentHeader { seq, ack, wnd, control, options }, data } = self;

        // RFC 793 (https://tools.ietf.org/html/rfc793#page-69):
        //   There are four cases for the acceptability test for an incoming
        //   segment:
        //       Segment Receive  Test
        //       Length  Window
        //       ------- -------  -------------------------------------------
        //          0       0     SEG.SEQ = RCV.NXT
        //          0      >0     RCV.NXT =< SEG.SEQ < RCV.NXT+RCV.WND
        //         >0       0     not acceptable
        //         >0      >0     RCV.NXT =< SEG.SEQ < RCV.NXT+RCV.WND
        //                     or RCV.NXT =< SEG.SEQ+SEG.LEN-1 < RCV.NXT+RCV.WND
        let overlap = match (len, rwnd) {
            (0, WindowSize::ZERO) => seq == rnxt,
            (0, rwnd) => !rnxt.after(seq) && seq.before(rnxt + rwnd),
            (_len, WindowSize::ZERO) => false,
            (len, rwnd) => {
                (!rnxt.after(seq) && seq.before(rnxt + rwnd))
                    // Note: here we use RCV.NXT <= SEG.SEQ+SEG.LEN instead of
                    // the condition as quoted above because of the following
                    // text immediately after the above table:
                    //   One could tailor actual segments to fit this assumption by
                    //   trimming off any portions that lie outside the window
                    //   (including SYN and FIN), and only processing further if
                    //   the segment then begins at RCV.NXT.
                    // This is essential for TCP simultaneous open to work,
                    // otherwise, the state machine would reject the SYN-ACK
                    // sent by the peer.
                    || (!(seq + len).before(rnxt) && !(seq + len).after(rnxt + rwnd))
            }
        };
        overlap.then(move || {
            // We deliberately don't define `PartialOrd` for `SeqNum`, so we use
            // `cmp` below to utilize `cmp::{max,min}_by`.
            let cmp = |lhs: &SeqNum, rhs: &SeqNum| (*lhs - *rhs).cmp(&0);
            let new_seq = core::cmp::max_by(seq, rnxt, cmp);
            let new_len = core::cmp::min_by(seq + len, rnxt + rwnd, cmp) - new_seq;
            // The following unwrap won't panic because:
            // 1. if `seq` is after `rnxt`, then `start` would be 0.
            // 2. the interesting case is when `rnxt` is after `seq`, in that
            // case, we have `rnxt - seq > 0`, thus `new_seq - seq > 0`.
            let start = u32::try_from(new_seq - seq).unwrap();
            // The following unwrap won't panic because:
            // 1. The witness on `Segment` and `WindowSize` guarantees that
            // `len <= 2^31` and `rwnd <= 2^30-1` thus
            // `seq <= seq + len` and `rnxt <= rnxt + rwnd`.
            // 2. We are in the closure because `overlap` is true which means
            // `seq <= rnxt + rwnd` and `rnxt <= seq + len`.
            // With these two conditions combined, `new_len` can't be negative
            // so the unwrap can't panic.
            let new_len = u32::try_from(new_len).unwrap();
            let (new_control, new_data) = {
                match control {
                    Some(Control::SYN) => {
                        if start == 0 {
                            (Some(Control::SYN), data.slice(start..start + new_len - 1))
                        } else {
                            (None, data.slice(start - 1..start + new_len - 1))
                        }
                    }
                    Some(Control::FIN) => {
                        if len == start + new_len {
                            if new_len > 0 {
                                (Some(Control::FIN), data.slice(start..start + new_len - 1))
                            } else {
                                (None, data.slice(start - 1..start - 1))
                            }
                        } else {
                            (None, data.slice(start..start + new_len))
                        }
                    }
                    Some(Control::RST) | None => (control, data.slice(start..start + new_len)),
                }
            };
            Segment {
                header: SegmentHeader { seq: new_seq, ack, wnd, control: new_control, options },
                data: new_data,
            }
        })
    }

    /// Creates a segment with no data.
    pub fn new(
        seq: SeqNum,
        ack: Option<SeqNum>,
        control: Option<Control>,
        wnd: UnscaledWindowSize,
    ) -> Self {
        Self::with_options(seq, ack, control, wnd, Options::default())
    }

    /// Creates a new segment with options but no data.
    pub fn with_options(
        seq: SeqNum,
        ack: Option<SeqNum>,
        control: Option<Control>,
        wnd: UnscaledWindowSize,
        options: Options,
    ) -> Self {
        // All of the checks on lengths are optimized out:
        // https://godbolt.org/z/KPd537G6Y
        let (seg, truncated) =
            Segment::with_data_options(seq, ack, control, wnd, P::new_empty(), options);
        debug_assert_eq!(truncated, 0);
        seg
    }

    /// Creates an ACK segment.
    pub fn ack(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize) -> Self {
        Segment::new(seq, Some(ack), None, wnd)
    }

    /// Creates a SYN segment.
    pub fn syn(seq: SeqNum, wnd: UnscaledWindowSize, options: Options) -> Self {
        Segment::with_options(seq, None, Some(Control::SYN), wnd, options)
    }

    /// Creates a SYN-ACK segment.
    pub fn syn_ack(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize, options: Options) -> Self {
        Segment::with_options(seq, Some(ack), Some(Control::SYN), wnd, options)
    }

    /// Creates a RST segment.
    pub fn rst(seq: SeqNum) -> Self {
        Segment::new(seq, None, Some(Control::RST), UnscaledWindowSize::from(0))
    }

    /// Creates a RST-ACK segment.
    pub fn rst_ack(seq: SeqNum, ack: SeqNum) -> Self {
        Segment::new(seq, Some(ack), Some(Control::RST), UnscaledWindowSize::from(0))
    }
}

impl Segment<()> {
    /// Converts this segment with `()` data into any `P` payload's `new_empty`
    /// form.
    pub fn into_empty<P: Payload>(self) -> Segment<P> {
        self.map_payload(|()| P::new_empty())
    }
}

impl SegmentHeader {
    /// Returns the length of the segment in sequence number space.
    ///
    /// Per RFC 793 (https://tools.ietf.org/html/rfc793#page-25):
    ///   SEG.LEN = the number of octets occupied by the data in the segment
    ///   (counting SYN and FIN)
    pub fn len(&self, payload_len: usize) -> u32 {
        // The following unwrap and addition are fine because:
        // - `u32::from(has_control_len)` is 0 or 1.
        // - `self.data.len() <= 2^31`.
        let has_control_len = self.control.map(Control::has_sequence_no).unwrap_or(false);
        u32::try_from(payload_len).unwrap() + u32::from(has_control_len)
    }

    /// Create a `SegmentHeader` from the provided builder and data length.  The
    /// options will be set to their default values.
    pub fn from_builder<A: IpAddress>(
        builder: &TcpSegmentBuilder<A>,
    ) -> Result<Self, MalformedFlags> {
        Self::from_builder_options(builder, Options::default())
    }

    /// Create a `SegmentHeader` from the provided builder, options, and data length.
    pub fn from_builder_options<A: IpAddress>(
        builder: &TcpSegmentBuilder<A>,
        options: Options,
    ) -> Result<Self, MalformedFlags> {
        Ok(SegmentHeader {
            seq: SeqNum::new(builder.seq_num()),
            ack: builder.ack_num().map(SeqNum::new),
            control: Flags {
                syn: builder.syn_set(),
                fin: builder.fin_set(),
                rst: builder.rst_set(),
            }
            .control()?,
            wnd: UnscaledWindowSize::from(builder.window_size()),
            options: options,
        })
    }
}

/// A TCP payload that only allows for getting the length of the payload.
pub trait PayloadLen {
    /// Returns the length of the payload.
    fn len(&self) -> usize;
}

/// A TCP payload that operates around `u32` instead of `usize`.
pub trait Payload: PayloadLen + Sized {
    /// Creates a slice of the payload, reducing it to only the bytes within
    /// `range`.
    ///
    /// # Panics
    ///
    /// Panics if the provided `range` is not within the bounds of this
    /// `Payload`, or if the range is nonsensical (the end precedes
    /// the start).
    fn slice(self, range: Range<u32>) -> Self;

    /// Copies part of the payload beginning at `offset` into `dst`.
    ///
    /// # Panics
    ///
    /// Panics if offset is too large or we couldn't fill the `dst` slice.
    fn partial_copy(&self, offset: usize, dst: &mut [u8]);

    /// Copies part of the payload beginning at `offset` into `dst`.
    ///
    /// # Panics
    ///
    /// Panics if offset is too large or we couldn't fill the `dst` slice.
    fn partial_copy_uninit(&self, offset: usize, dst: &mut [MaybeUninit<u8>]);

    /// Creates a new empty payload.
    ///
    /// An empty payload must report 0 as its length.
    fn new_empty() -> Self;
}

impl PayloadLen for &[u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }
}

impl Payload for &[u8] {
    fn slice(self, Range { start, end }: Range<u32>) -> Self {
        // The following `unwrap`s are ok because:
        // `usize::try_from(x)` fails when `x > usize::MAX`; given that
        // `self.len() <= usize::MAX`, panic would be expected because `range`
        // exceeds the bound of `self`.
        let start = usize::try_from(start).unwrap_or_else(|TryFromIntError { .. }| {
            panic!("range start index {} out of range for slice of length {}", start, self.len())
        });
        let end = usize::try_from(end).unwrap_or_else(|TryFromIntError { .. }| {
            panic!("range end index {} out of range for slice of length {}", end, self.len())
        });
        &self[start..end]
    }

    fn partial_copy(&self, offset: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self[offset..offset + dst.len()])
    }

    fn partial_copy_uninit(&self, offset: usize, dst: &mut [MaybeUninit<u8>]) {
        // TODO(https://github.com/rust-lang/rust/issues/79995): Replace unsafe
        // with copy_from_slice when stabiliized.
        let src = &self[offset..offset + dst.len()];
        // SAFETY: &[T] and &[MaybeUninit<T>] have the same layout.
        let uninit_src: &[MaybeUninit<u8>] = unsafe { core::mem::transmute(src) };
        dst.copy_from_slice(&uninit_src);
    }

    fn new_empty() -> Self {
        &[]
    }
}

impl PayloadLen for () {
    fn len(&self) -> usize {
        0
    }
}

impl Payload for () {
    fn slice(self, Range { start, end }: Range<u32>) -> Self {
        if start != 0 {
            panic!("range start index {} out of range for slice of length 0", start);
        }
        if end != 0 {
            panic!("range end index {} out of range for slice of length 0", end);
        }
        ()
    }

    fn partial_copy(&self, offset: usize, dst: &mut [u8]) {
        if dst.len() != 0 || offset != 0 {
            panic!(
                "source slice length (0) does not match destination slice length ({})",
                dst.len()
            );
        }
    }

    fn partial_copy_uninit(&self, offset: usize, dst: &mut [MaybeUninit<u8>]) {
        if dst.len() != 0 || offset != 0 {
            panic!(
                "source slice length (0) does not match destination slice length ({})",
                dst.len()
            );
        }
    }

    fn new_empty() -> Self {
        ()
    }
}

impl<I: PayloadLen, B> PayloadLen for InnerSerializer<I, B> {
    fn len(&self) -> usize {
        PayloadLen::len(self.inner())
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
#[error("multiple mutually exclusive flags are set: syn: {syn}, fin: {fin}, rst: {rst}")]
pub struct MalformedFlags {
    syn: bool,
    fin: bool,
    rst: bool,
}

struct Flags {
    syn: bool,
    fin: bool,
    rst: bool,
}

impl Flags {
    fn control(&self) -> Result<Option<Control>, MalformedFlags> {
        if usize::from(self.syn) + usize::from(self.fin) + usize::from(self.rst) > 1 {
            return Err(MalformedFlags { syn: self.syn, fin: self.fin, rst: self.rst });
        }

        let syn = self.syn.then(|| Control::SYN);
        let fin = self.fin.then(|| Control::FIN);
        let rst = self.rst.then(|| Control::RST);

        Ok(syn.or(fin).or(rst))
    }
}

impl<'a> TryFrom<TcpSegment<&'a [u8]>> for Segment<&'a [u8]> {
    type Error = MalformedFlags;

    fn try_from(from: TcpSegment<&'a [u8]>) -> Result<Self, Self::Error> {
        let options = Options::from_iter(from.iter_options());
        let (to, discarded) = Segment::with_data_options(
            from.seq_num().into(),
            from.ack_num().map(Into::into),
            Flags { syn: from.syn(), fin: from.fin(), rst: from.rst() }.control()?,
            UnscaledWindowSize::from(from.window_size()),
            from.into_body(),
            options,
        );
        debug_assert_eq!(discarded, 0);
        Ok(to)
    }
}

impl<A> TryFrom<&TcpSegmentBuilder<A>> for SegmentHeader
where
    A: IpAddress,
{
    type Error = MalformedFlags;

    fn try_from(from: &TcpSegmentBuilder<A>) -> Result<Self, Self::Error> {
        SegmentHeader::from_builder(from)
    }
}

impl<'a, A, I> TryFrom<&TcpSegmentBuilderWithOptions<A, OptionSequenceBuilder<TcpOption<'a>, I>>>
    for SegmentHeader
where
    A: IpAddress,
    I: Iterator + Clone,
    I::Item: Borrow<TcpOption<'a>>,
{
    type Error = MalformedFlags;

    fn try_from(
        from: &TcpSegmentBuilderWithOptions<A, OptionSequenceBuilder<TcpOption<'a>, I>>,
    ) -> Result<Self, Self::Error> {
        Self::from_builder_options(
            from.prefix_builder(),
            Options::from_iter(from.iter_options().map(|option| option.borrow().to_owned())),
        )
    }
}

#[cfg(feature = "testutils")]
mod testutils {
    use super::*;

    impl<'a> Segment<&'a [u8]> {
        /// Create a new segment with the given seq, ack, and data.
        pub fn with_fake_data(seq: SeqNum, ack: SeqNum, data: &'a [u8]) -> Self {
            let (segment, discarded) =
                Self::with_data(seq, Some(ack), None, UnscaledWindowSize::from(u16::MAX), data);
            assert_eq!(discarded, 0);
            segment
        }
    }

    impl<P: Payload> Segment<P> {
        /// Creates a new segment with the provided data.
        pub fn data(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize, data: P) -> Segment<P> {
            let (seg, truncated) = Segment::with_data(seq, Some(ack), None, wnd, data);
            assert_eq!(truncated, 0);
            seg
        }

        /// Creates a new FIN segment with the provided data.
        pub fn piggybacked_fin(
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

        /// Creates a new FIN segment.
        pub fn fin(seq: SeqNum, ack: SeqNum, wnd: UnscaledWindowSize) -> Self {
            Segment::new(seq, Some(ack), Some(Control::FIN), wnd)
        }
    }
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ipv4, Ipv6};
    use packet_formats::ip::IpExt;
    use test_case::test_case;

    use super::*;

    #[test_case(None, &[][..] => (0, &[][..]); "empty")]
    #[test_case(None, &[1][..] => (1, &[1][..]); "no control")]
    #[test_case(Some(Control::SYN), &[][..] => (1, &[][..]); "empty slice with syn")]
    #[test_case(Some(Control::SYN), &[1][..] => (2, &[1][..]); "non-empty slice with syn")]
    #[test_case(Some(Control::FIN), &[][..] => (1, &[][..]); "empty slice with fin")]
    #[test_case(Some(Control::FIN), &[1][..] => (2, &[1][..]); "non-empty slice with fin")]
    #[test_case(Some(Control::RST), &[][..] => (0, &[][..]); "empty slice with rst")]
    #[test_case(Some(Control::RST), &[1][..] => (1, &[1][..]); "non-empty slice with rst")]
    fn segment_len(control: Option<Control>, data: &[u8]) -> (u32, &[u8]) {
        let (seg, truncated) = Segment::with_data(
            SeqNum::new(1),
            Some(SeqNum::new(1)),
            control,
            UnscaledWindowSize::from(0),
            data,
        );
        assert_eq!(truncated, 0);
        (seg.len(), seg.data)
    }

    #[test_case(&[1, 2, 3, 4, 5][..], 0..4 => [1, 2, 3, 4])]
    #[test_case((), 0..0 => [0, 0, 0, 0])]
    fn payload_slice_copy(data: impl Payload, range: Range<u32>) -> [u8; 4] {
        let sliced = data.slice(range);
        let mut buffer = [0; 4];
        sliced.partial_copy(0, &mut buffer[..sliced.len()]);
        buffer
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TestPayload(Range<u32>);

    impl TestPayload {
        fn new(len: usize) -> Self {
            Self(0..u32::try_from(len).unwrap())
        }
    }

    impl PayloadLen for TestPayload {
        fn len(&self) -> usize {
            self.0.len()
        }
    }

    impl Payload for TestPayload {
        fn slice(self, range: Range<u32>) -> Self {
            let Self(this) = self;
            assert!(range.start >= this.start && range.end <= this.end);
            TestPayload(range)
        }

        fn partial_copy(&self, _offset: usize, _dst: &mut [u8]) {
            unimplemented!("TestPayload doesn't carry any data");
        }

        fn partial_copy_uninit(&self, _offset: usize, _dst: &mut [MaybeUninit<u8>]) {
            unimplemented!("TestPayload doesn't carry any data");
        }

        fn new_empty() -> Self {
            Self(0..0)
        }
    }

    #[test_case(100, Some(Control::SYN) => (100, Some(Control::SYN), 0))]
    #[test_case(100, Some(Control::FIN) => (100, Some(Control::FIN), 0))]
    #[test_case(100, Some(Control::RST) => (100, Some(Control::RST), 0))]
    #[test_case(100, None => (100, None, 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::SYN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::SYN), 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::FIN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::FIN), 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::RST)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::RST), 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN - 1, None
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, None, 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN, Some(Control::SYN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::SYN), 1))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN, Some(Control::FIN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN, None, 1))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN, Some(Control::RST)
    => (MAX_PAYLOAD_AND_CONTROL_LEN, Some(Control::RST), 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN, None
    => (MAX_PAYLOAD_AND_CONTROL_LEN, None, 0))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN + 1, Some(Control::SYN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::SYN), 2))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN + 1, Some(Control::FIN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN, None, 2))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN + 1, Some(Control::RST)
    => (MAX_PAYLOAD_AND_CONTROL_LEN, Some(Control::RST), 1))]
    #[test_case(MAX_PAYLOAD_AND_CONTROL_LEN + 1, None
    => (MAX_PAYLOAD_AND_CONTROL_LEN, None, 1))]
    #[test_case(u32::MAX as usize, Some(Control::SYN)
    => (MAX_PAYLOAD_AND_CONTROL_LEN - 1, Some(Control::SYN), 1 << 31))]
    fn segment_truncate(len: usize, control: Option<Control>) -> (usize, Option<Control>, usize) {
        let (seg, truncated) = Segment::with_data(
            SeqNum::new(0),
            None,
            control,
            UnscaledWindowSize::from(0),
            TestPayload::new(len),
        );
        (seg.data.len(), seg.header.control, truncated)
    }

    struct OverlapTestArgs {
        seg_seq: u32,
        control: Option<Control>,
        data_len: u32,
        rcv_nxt: u32,
        rcv_wnd: usize,
    }
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 0,
        rcv_nxt: 0,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 0,
    } => Some((SeqNum::new(1), None, 0..0)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 0,
        rcv_nxt: 2,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::SYN),
        data_len: 0,
        rcv_nxt: 2,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::SYN),
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::SYN),
        data_len: 0,
        rcv_nxt: 0,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::FIN),
        data_len: 0,
        rcv_nxt: 2,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::FIN),
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::FIN),
        data_len: 0,
        rcv_nxt: 0,
        rcv_wnd: 0,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: None,
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 0..0)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 2,
        control: None,
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: None,
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 1..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: Some(Control::SYN),
        data_len: 0,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 0..0)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 2,
        control: None,
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => None)]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: None,
        data_len: 2,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 1..2)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 2,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 0..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: Some(Control::SYN),
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 0..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::SYN),
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), Some(Control::SYN), 0..0)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 0,
        control: Some(Control::FIN),
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), Some(Control::FIN), 1..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::FIN),
        data_len: 1,
        rcv_nxt: 1,
        rcv_wnd: 1,
    } => Some((SeqNum::new(1), None, 0..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: MAX_PAYLOAD_AND_CONTROL_LEN_U32,
        rcv_nxt: 1,
        rcv_wnd: 10,
    } => Some((SeqNum::new(1), None, 0..10)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 10,
        control: None,
        data_len: MAX_PAYLOAD_AND_CONTROL_LEN_U32,
        rcv_nxt: 1,
        rcv_wnd: 10,
    } => Some((SeqNum::new(10), None, 0..1)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: None,
        data_len: 10,
        rcv_nxt: 1,
        rcv_wnd: 1 << 30 - 1,
    } => Some((SeqNum::new(1), None, 0..10)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 10,
        control: None,
        data_len: 10,
        rcv_nxt: 1,
        rcv_wnd: 1 << 30 - 1,
    } => Some((SeqNum::new(10), None, 0..10)))]
    #[test_case(OverlapTestArgs{
        seg_seq: 1,
        control: Some(Control::FIN),
        data_len: 1,
        rcv_nxt: 3,
        rcv_wnd: 10,
    } => Some((SeqNum::new(3), None, 1..1)); "regression test for https://fxbug.dev/42061750")]
    fn segment_overlap(
        OverlapTestArgs { seg_seq, control, data_len, rcv_nxt, rcv_wnd }: OverlapTestArgs,
    ) -> Option<(SeqNum, Option<Control>, Range<u32>)> {
        let (seg, discarded) = Segment::with_data(
            SeqNum::new(seg_seq),
            None,
            control,
            UnscaledWindowSize::from(0),
            TestPayload(0..data_len),
        );
        assert_eq!(discarded, 0);
        seg.overlap(SeqNum::new(rcv_nxt), WindowSize::new(rcv_wnd).unwrap()).map(
            |Segment { header: SegmentHeader { seq, control, .. }, data: TestPayload(range) }| {
                (seq, control, range)
            },
        )
    }

    pub trait TestIpExt: IpExt {
        const SRC_IP: Self::Addr;
        const DST_IP: Self::Addr;
    }

    impl TestIpExt for Ipv4 {
        const SRC_IP: Self::Addr = net_ip_v4!("192.0.2.1");
        const DST_IP: Self::Addr = net_ip_v4!("192.0.2.2");
    }

    impl TestIpExt for Ipv6 {
        const SRC_IP: Self::Addr = net_ip_v6!("2001:db8::1");
        const DST_IP: Self::Addr = net_ip_v6!("2001:db8::2");
    }

    const SRC_PORT: NonZeroU16 = NonZeroU16::new(1234).unwrap();
    const DST_PORT: NonZeroU16 = NonZeroU16::new(9876).unwrap();

    #[ip_test(I)]
    fn from_segment_builder<I: TestIpExt>() {
        let mut builder =
            TcpSegmentBuilder::new(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT, 1, Some(2), 3);
        builder.syn(true);

        let converted_header =
            SegmentHeader::try_from(&builder).expect("failed to convert serializer");

        let expected_header = SegmentHeader {
            seq: SeqNum::new(1),
            ack: Some(SeqNum::new(2)),
            wnd: UnscaledWindowSize::from(3u16),
            control: Some(Control::SYN),
            options: Options { mss: None, window_scale: None },
        };

        assert_eq!(converted_header, expected_header);
    }

    #[ip_test(I)]
    fn from_segment_builder_failure<I: TestIpExt>() {
        let mut builder =
            TcpSegmentBuilder::new(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT, 1, Some(2), 3);
        builder.syn(true);
        builder.fin(true);

        assert_matches!(
            SegmentHeader::try_from(&builder),
            Err(MalformedFlags { syn: true, fin: true, rst: false })
        );
    }

    #[ip_test(I)]
    fn from_segment_builder_with_options<I: TestIpExt>() {
        let mut builder =
            TcpSegmentBuilder::new(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT, 1, Some(2), 3);
        builder.syn(true);

        let builder = TcpSegmentBuilderWithOptions::new(
            builder,
            [TcpOption::Mss(1024), TcpOption::WindowScale(10)],
        )
        .expect("failed to create tcp segment builder");

        let converted_header =
            SegmentHeader::try_from(&builder).expect("failed to convert serializer");

        let expected_header = SegmentHeader {
            seq: SeqNum::new(1),
            ack: Some(SeqNum::new(2)),
            wnd: UnscaledWindowSize::from(3u16),
            control: Some(Control::SYN),
            options: Options {
                mss: Some(Mss(NonZeroU16::new(1024).unwrap())),
                window_scale: Some(WindowScale::new(10).unwrap()),
            },
        };

        assert_eq!(converted_header, expected_header);
    }

    #[ip_test(I)]
    fn from_segment_builder_with_options_failure<I: TestIpExt>() {
        let mut builder =
            TcpSegmentBuilder::new(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT, 1, Some(2), 3);
        builder.syn(true);
        builder.fin(true);

        let builder = TcpSegmentBuilderWithOptions::new(
            builder,
            [TcpOption::Mss(1024), TcpOption::WindowScale(10)],
        )
        .expect("failed to create tcp segment builder");

        assert_matches!(
            SegmentHeader::try_from(&builder),
            Err(MalformedFlags { syn: true, fin: true, rst: false })
        );
    }

    #[test_case(Flags {
            syn: false,
            fin: false,
            rst: false,
        } => Ok(None))]
    #[test_case(Flags {
            syn: true,
            fin: false,
            rst: false,
        } => Ok(Some(Control::SYN)))]
    #[test_case(Flags {
            syn: false,
            fin: true,
            rst: false,
        } => Ok(Some(Control::FIN)))]
    #[test_case(Flags {
            syn: false,
            fin: false,
            rst: true,
        } => Ok(Some(Control::RST)))]
    #[test_case(Flags {
            syn: true,
            fin: true,
            rst: false,
        } => Err(MalformedFlags {
            syn: true,
            fin: true,
            rst: false,
        }))]
    #[test_case(Flags {
            syn: true,
            fin: false,
            rst: true,
        } => Err(MalformedFlags {
            syn: true,
            fin: false,
            rst: true,
        }))]
    #[test_case(Flags {
            syn: false,
            fin: true,
            rst: true,
        } => Err(MalformedFlags {
            syn: false,
            fin: true,
            rst: true,
        }))]
    #[test_case(Flags {
            syn: true,
            fin: true,
            rst: true,
        } => Err(MalformedFlags {
            syn: true,
            fin: true,
            rst: true,
        }))]
    fn flags_to_control(input: Flags) -> Result<Option<Control>, MalformedFlags> {
        input.control()
    }
}
