// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Transmission Control Protocol (TCP).

use core::iter::FromIterator;
use core::num::NonZeroU16;
use core::ops::Range;

use alloc::vec::Vec;
use core::mem::MaybeUninit;
use net_types::ip::{Ip, IpVersion};
use packet::InnerPacketBuilder;
use packet_formats::ip::IpExt;

use crate::ip::Mms;
use crate::tcp::segment::{Payload, PayloadLen};

/// Control flags that can alter the state of a TCP control block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Corresponds to the SYN bit in a TCP segment.
    SYN,
    /// Corresponds to the FIN bit in a TCP segment.
    FIN,
    /// Corresponds to the RST bit in a TCP segment.
    RST,
}

impl Control {
    /// Returns whether the control flag consumes one byte from the sequence
    /// number space.
    pub fn has_sequence_no(self) -> bool {
        match self {
            Control::SYN | Control::FIN => true,
            Control::RST => false,
        }
    }
}

const TCP_HEADER_LEN: u32 = packet_formats::tcp::HDR_PREFIX_LEN as u32;

/// Maximum segment size, that is the maximum TCP payload one segment can carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Mss(pub NonZeroU16);

impl Mss {
    /// Creates MSS from the maximum message size of the IP layer.
    pub fn from_mms<I: IpExt>(mms: Mms) -> Option<Self> {
        NonZeroU16::new(
            u16::try_from(mms.get().get().saturating_sub(TCP_HEADER_LEN)).unwrap_or(u16::MAX),
        )
        .map(Self)
    }

    /// Create a new [`Mss`] with the IP-version default value, as defined by RFC 9293.
    pub const fn default<I: Ip>() -> Self {
        // Per RFC 9293 Section 3.7.1:
        //  If an MSS Option is not received at connection setup, TCP
        //  implementations MUST assume a default send MSS of 536 (576 - 40) for
        //  IPv4 or 1220 (1280 - 60) for IPv6 (MUST-15).
        match I::VERSION {
            IpVersion::V4 => Mss(NonZeroU16::new(536).unwrap()),
            IpVersion::V6 => Mss(NonZeroU16::new(1220).unwrap()),
        }
    }

    /// Gets the numeric value of the MSS.
    pub const fn get(&self) -> NonZeroU16 {
        let Self(mss) = *self;
        mss
    }
}

impl From<Mss> for u32 {
    fn from(Mss(mss): Mss) -> Self {
        u32::from(mss.get())
    }
}

/// An implementation of [`Payload`] backed by up to `N` byte slices.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FragmentedPayload<'a, const N: usize> {
    storage: [&'a [u8]; N],
    // NB: Not using `Range` because it is not `Copy`.
    //
    // Start is inclusive, end is exclusive; so this is equivalent to
    // `start..end` ranges.
    start: usize,
    end: usize,
}

/// Creates a new `FragmentedPayload` possibly without using the entire
/// storage capacity `N`.
///
/// # Panics
///
/// Panics if the iterator contains more than `N` items.
impl<'a, const N: usize> FromIterator<&'a [u8]> for FragmentedPayload<'a, N> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = &'a [u8]>,
    {
        let Self { storage, start, end } = Self::new_empty();
        let (storage, end) = iter.into_iter().fold((storage, end), |(mut storage, end), sl| {
            storage[end] = sl;
            (storage, end + 1)
        });
        Self { storage, start, end }
    }
}

impl<'a, const N: usize> FragmentedPayload<'a, N> {
    /// Creates a new `FragmentedPayload` with the slices in `values`.
    pub fn new(values: [&'a [u8]; N]) -> Self {
        Self { storage: values, start: 0, end: N }
    }

    /// Creates a new `FragmentedPayload` with a single contiguous slice.
    pub fn new_contiguous(value: &'a [u8]) -> Self {
        core::iter::once(value).collect()
    }

    /// Converts this [`FragmentedPayload`] into an owned `Vec`.
    pub fn to_vec(self) -> Vec<u8> {
        self.slices().concat()
    }

    fn slices(&self) -> &[&'a [u8]] {
        let Self { storage, start, end } = self;
        &storage[*start..*end]
    }

    /// Extracted function to implement [`Payload::partial_copy`] and
    /// [`Payload::partial_copy_uninit`].
    fn apply_copy<T, F: Fn(&[u8], &mut [T])>(
        &self,
        mut offset: usize,
        mut dst: &mut [T],
        apply: F,
    ) {
        let mut slices = self.slices().into_iter();
        while let Some(sl) = slices.next() {
            let l = sl.len();
            if offset >= l {
                offset -= l;
                continue;
            }
            let sl = &sl[offset..];
            let cp = sl.len().min(dst.len());
            let (target, new_dst) = dst.split_at_mut(cp);
            apply(&sl[..cp], target);

            // We're done.
            if new_dst.len() == 0 {
                return;
            }

            dst = new_dst;
            offset = 0;
        }
        assert_eq!(dst.len(), 0, "failed to fill dst");
    }
}

impl<'a, const N: usize> PayloadLen for FragmentedPayload<'a, N> {
    fn len(&self) -> usize {
        self.slices().into_iter().map(|s| s.len()).sum()
    }
}

impl<'a, const N: usize> Payload for FragmentedPayload<'a, N> {
    fn slice(self, byte_range: Range<u32>) -> Self {
        let Self { mut storage, start: mut self_start, end: mut self_end } = self;
        let Range { start: byte_start, end: byte_end } = byte_range;
        let byte_start =
            usize::try_from(byte_start).expect("range start index out of range for usize");
        let byte_end = usize::try_from(byte_end).expect("range end index out of range for usize");
        assert!(byte_end >= byte_start);
        let mut storage_iter =
            (&mut storage[self_start..self_end]).into_iter().scan(0, |total_len, slice| {
                let slice_len = slice.len();
                let item = Some((*total_len, slice));
                *total_len += slice_len;
                item
            });

        // Keep track of whether the start was inside the range, we should panic
        // even on an empty range out of start bounds.
        let mut start_offset = None;
        let mut final_len = 0;
        while let Some((sl_offset, sl)) = storage_iter.next() {
            let orig_len = sl.len();

            // Advance until the start of the specified range, discarding unused
            // slices.
            if sl_offset + orig_len < byte_start {
                *sl = &[];
                self_start += 1;
                continue;
            }
            // Discard any empty slices at the end.
            if sl_offset >= byte_end {
                *sl = &[];
                self_end -= 1;
                continue;
            }

            let sl_start = byte_start.saturating_sub(sl_offset);
            let sl_end = sl.len().min(byte_end - sl_offset);
            *sl = &sl[sl_start..sl_end];

            match start_offset {
                Some(_) => (),
                None => {
                    // Keep track of the start offset of the first slice.
                    start_offset = Some(sl_offset + sl_start);
                    // Avoid producing an empty slice if we haven't added
                    // anything yet.
                    if sl.len() == 0 {
                        self_start += 1;
                    }
                }
            }
            final_len += sl.len();
        }
        // Verify that the entire range was consumed.
        assert_eq!(
            // If we didn't use start_offset the only valid value for
            // `byte_start` is zero.
            start_offset.unwrap_or(0),
            byte_start,
            "range start index out of range {byte_range:?}"
        );
        assert_eq!(byte_start + final_len, byte_end, "range end index out of range {byte_range:?}");

        // Canonicalize an empty payload.
        if self_start == self_end {
            self_start = 0;
            self_end = 0;
        }
        Self { storage, start: self_start, end: self_end }
    }

    fn new_empty() -> Self {
        Self { storage: [&[]; N], start: 0, end: 0 }
    }

    fn partial_copy(&self, offset: usize, dst: &mut [u8]) {
        self.apply_copy(offset, dst, |src, dst| {
            dst.copy_from_slice(src);
        });
    }

    fn partial_copy_uninit(&self, offset: usize, dst: &mut [MaybeUninit<u8>]) {
        self.apply_copy(offset, dst, |src, dst| {
            // TODO(https://github.com/rust-lang/rust/issues/79995): Replace unsafe
            // with copy_from_slice when stabiliized.
            // SAFETY: &[T] and &[MaybeUninit<T>] have the same layout.
            let uninit_src: &[MaybeUninit<u8>] = unsafe { core::mem::transmute(src) };
            dst.copy_from_slice(&uninit_src);
        });
    }
}

impl<'a, const N: usize> InnerPacketBuilder for FragmentedPayload<'a, N> {
    fn bytes_len(&self) -> usize {
        self.len()
    }

    fn serialize(&self, buffer: &mut [u8]) {
        self.partial_copy(0, buffer);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use alloc::format;

    use packet::Serializer as _;
    use proptest::test_runner::Config;
    use proptest::{prop_assert_eq, proptest};
    use proptest_support::failed_seeds_no_std;
    use test_case::test_case;

    const EXAMPLE_DATA: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    #[test_case(FragmentedPayload::new([&EXAMPLE_DATA[..]]); "contiguous")]
    #[test_case(FragmentedPayload::new([&EXAMPLE_DATA[0..2], &EXAMPLE_DATA[2..]]); "split once")]
    #[test_case(FragmentedPayload::new([
        &EXAMPLE_DATA[0..2],
        &EXAMPLE_DATA[2..5],
        &EXAMPLE_DATA[5..],
    ]); "split twice")]
    #[test_case(FragmentedPayload::<4>::from_iter([
        &EXAMPLE_DATA[0..2],
        &EXAMPLE_DATA[2..5],
        &EXAMPLE_DATA[5..],
    ]); "partial twice")]
    fn fragmented_payload_serializer_data<const N: usize>(payload: FragmentedPayload<'_, N>) {
        let serialized = payload
            .into_serializer()
            .serialize_vec_outer()
            .expect("should serialize")
            .unwrap_b()
            .into_inner();
        assert_eq!(&serialized[..], EXAMPLE_DATA);
    }

    #[test]
    #[should_panic(expected = "range start index out of range")]
    fn slice_start_out_of_bounds() {
        let len = u32::try_from(EXAMPLE_DATA.len()).unwrap();
        let bad_len = len + 1;
        // Like for standard slices, this shouldn't succeed if the start length
        // is out of bounds, even if the total range is empty.
        let _ = FragmentedPayload::<2>::new_contiguous(&EXAMPLE_DATA).slice(bad_len..bad_len);
    }

    #[test]
    #[should_panic(expected = "range end index out of range")]
    fn slice_end_out_of_bounds() {
        let len = u32::try_from(EXAMPLE_DATA.len()).unwrap();
        let bad_len = len + 1;
        let _ = FragmentedPayload::<2>::new_contiguous(&EXAMPLE_DATA).slice(0..bad_len);
    }

    #[test]
    fn canon_empty_payload() {
        let len = u32::try_from(EXAMPLE_DATA.len()).unwrap();
        assert_eq!(
            FragmentedPayload::<1>::new_contiguous(&EXAMPLE_DATA).slice(len..len),
            FragmentedPayload::new_empty()
        );
        assert_eq!(
            FragmentedPayload::<2>::new_contiguous(&EXAMPLE_DATA).slice(len..len),
            FragmentedPayload::new_empty()
        );
        assert_eq!(
            FragmentedPayload::<2>::new_contiguous(&EXAMPLE_DATA).slice(2..2),
            FragmentedPayload::new_empty()
        );
    }

    const TEST_BYTES: &'static [u8] = b"Hello World!";
    proptest! {
        #![proptest_config(Config {
            // Add all failed seeds here.
            failure_persistence: failed_seeds_no_std!(),
            ..Config::default()
        })]

        #[test]
        fn fragmented_payload_to_vec(payload in fragmented_payload::with_payload()) {
            prop_assert_eq!(payload.to_vec(), &TEST_BYTES[..]);
        }

        #[test]
        fn fragmented_payload_len(payload in fragmented_payload::with_payload()) {
            prop_assert_eq!(payload.len(), TEST_BYTES.len())
        }

        #[test]
        fn fragmented_payload_slice((payload, (start, end)) in fragmented_payload::with_range()) {
            let want = &TEST_BYTES[start..end];
            let start = u32::try_from(start).unwrap();
            let end = u32::try_from(end).unwrap();
            prop_assert_eq!(payload.clone().slice(start..end).to_vec(), want);
        }

        #[test]
        fn fragmented_payload_partial_copy((payload, (start, end)) in fragmented_payload::with_range()) {
            let mut buffer = [0; TEST_BYTES.len()];
            let buffer = &mut buffer[0..(end-start)];
            payload.partial_copy(start, buffer);
            prop_assert_eq!(buffer, &TEST_BYTES[start..end]);
        }
    }

    mod fragmented_payload {
        use super::*;

        use proptest::strategy::{Just, Strategy};
        use rand::Rng as _;

        const TEST_STORAGE: usize = 5;
        type TestFragmentedPayload = FragmentedPayload<'static, TEST_STORAGE>;
        pub(super) fn with_payload() -> impl Strategy<Value = TestFragmentedPayload> {
            (1..=TEST_STORAGE).prop_perturb(|slices, mut rng| {
                (0..slices)
                    .scan(0, |st, slice| {
                        let len = if slice == slices - 1 {
                            TEST_BYTES.len() - *st
                        } else {
                            rng.gen_range(0..=(TEST_BYTES.len() - *st))
                        };
                        let start = *st;
                        *st += len;
                        Some(&TEST_BYTES[start..*st])
                    })
                    .collect()
            })
        }

        pub(super) fn with_range() -> impl Strategy<Value = (TestFragmentedPayload, (usize, usize))>
        {
            (
                with_payload(),
                (0..TEST_BYTES.len()).prop_flat_map(|start| (Just(start), start..TEST_BYTES.len())),
            )
        }
    }
}
