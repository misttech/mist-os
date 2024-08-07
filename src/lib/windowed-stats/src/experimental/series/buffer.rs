// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Ring buffers and compression.

use num::Unsigned;
use std::io::{self, Write};
use tracing::warn;

use crate::experimental::clock::Duration;
use crate::experimental::ring_buffer::Simple8bRleRingBuffer;
use crate::experimental::series::interpolation::{
    Constant, Interpolation, LastAggregation, LastSample,
};
use crate::experimental::series::statistic::Aggregation;
use crate::experimental::series::{SamplingInterval, SamplingProfile};
use crate::experimental::vec1::Vec1;

/// A type that can construct a [`RingBuffer`] associated with an aggregation type and
/// interpolation.
///
/// [`RingBuffer`]: crate::experimental::series::buffer::RingBuffer
pub trait BufferStrategy<A, P>
where
    P: Interpolation,
{
    type Buffer: Clone + RingBuffer<A>;

    /// Constructs a ring buffer with the given fixed capacity.
    fn buffer(interval: &SamplingInterval) -> Self::Buffer {
        Self::Buffer::with_capacity(interval.capacity() as usize)
    }
    /// Retrieve the sampling intervals given the sampling profile.
    fn sampling_intervals(profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        Self::Buffer::sampling_intervals(profile)
    }
}

pub type Buffer<F, P> = <F as BufferStrategy<Aggregation<F>, P>>::Buffer;

// The following `BufferStrategy` implementations associate an encoding with an aggregation type
// and interpolation (with no other considerations). More sophisticated types like data semantics
// and statistics may forward their implementations to these.

impl BufferStrategy<i64, LastAggregation> for i64 {
    type Buffer = DeltaZigZagSimple8bRle;
}

impl BufferStrategy<i64, LastSample> for i64 {
    type Buffer = DeltaZigZagSimple8bRle;
}

impl BufferStrategy<i64, Constant> for i64 {
    type Buffer = ZigZagSimple8bRle;
}

impl<P> BufferStrategy<f32, P> for f32
where
    P: Interpolation,
{
    type Buffer = Uncompressed<f32>;
}

impl<P> BufferStrategy<f64, P> for f64
where
    P: Interpolation,
{
    type Buffer = Uncompressed<f64>;
}

impl BufferStrategy<u64, Constant> for u64 {
    type Buffer = Simple8bRle;
}

impl BufferStrategy<u64, LastAggregation> for u64 {
    type Buffer = DeltaZigZagSimple8bRle;
}

impl BufferStrategy<u64, LastSample> for u64 {
    type Buffer = DeltaZigZagSimple8bRle;
}

/// A fixed-capacity circular ring buffer.
pub trait RingBuffer<A> {
    fn with_capacity(capacity: usize) -> Self
    where
        Self: Sized;

    fn sampling_intervals(profile: &SamplingProfile) -> Vec1<SamplingInterval>;

    fn push(&mut self, item: A) {
        let _ = item;
    }

    fn serialize(&self, write: impl Write) -> io::Result<()> {
        let _ = write;
        Ok(())
    }
}

// TODO(https://fxbug.dev/352614838): Implement uncompressed ring buffer
/// A ring buffer that stores arbitrary items in their immediate representation.
#[derive(Clone, Debug)]
pub struct Uncompressed<A> {
    buffer: Vec<A>,
}

impl<A> RingBuffer<A> for Uncompressed<A> {
    fn with_capacity(capacity: usize) -> Self {
        warn!("Uncompressed ring buffer is unimplemented. No data will be stored.");
        Uncompressed { buffer: Vec::with_capacity(capacity) }
    }
    fn sampling_intervals(profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        match profile {
            SamplingProfile::Granular => [
                SamplingInterval::new(240, 1, Duration::from_seconds(10)),
                SamplingInterval::new(240, 1, Duration::from_minutes(1)),
            ]
            .into(),
            SamplingProfile::Balanced => [
                SamplingInterval::new(120, 1, Duration::from_seconds(10)),
                SamplingInterval::new(120, 1, Duration::from_minutes(1)),
                SamplingInterval::new(120, 1, Duration::from_minutes(10)),
                SamplingInterval::new(120, 1, Duration::from_hours(1)),
            ]
            .into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Simple8bRle(Simple8bRleRingBuffer);

impl<A> RingBuffer<A> for Simple8bRle
where
    A: Into<u64> + Unsigned,
{
    fn with_capacity(capacity: usize) -> Self {
        let ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(capacity);
        Simple8bRle(ring_buffer)
    }

    fn sampling_intervals(profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        match profile {
            SamplingProfile::Granular => [
                SamplingInterval::new(116, 1, Duration::from_seconds(10)),
                SamplingInterval::new(116, 1, Duration::from_minutes(1)),
            ]
            .into(),
            SamplingProfile::Balanced => [
                SamplingInterval::new(58, 1, Duration::from_seconds(10)),
                SamplingInterval::new(58, 1, Duration::from_minutes(1)),
                SamplingInterval::new(58, 1, Duration::from_minutes(10)),
                SamplingInterval::new(58, 1, Duration::from_hours(1)),
            ]
            .into(),
        }
    }

    fn push(&mut self, item: A) {
        self.0.push(item.into());
    }

    fn serialize(&self, mut write: impl Write) -> io::Result<()> {
        self.0.serialize(&mut write)
    }
}

// TODO(https://fxbug.dev/352614791): Implement ZigZagSimple8bRle ring buffer
/// A ring buffer that stores signed integer items using zig-zag, Simple8B, and run length
/// encoding.
#[derive(Clone, Debug)]
pub struct ZigZagSimple8bRle;

impl<A> RingBuffer<A> for ZigZagSimple8bRle
where
    A: Into<i64>,
{
    fn with_capacity(capacity: usize) -> Self {
        warn!("ZigZagSimple8bRle ring buffer is unimplemented. No data will be stored.");
        let _ = capacity;
        ZigZagSimple8bRle
    }
    fn sampling_intervals(_profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        let placeholder = SamplingInterval::new(1, 1, Duration::from_hours(1));
        [placeholder].into()
    }
}

// TODO(https://fxbug.dev/352614791): Implement DeltaSimple8bRle ring buffer
/// A ring buffer that stores unsigned integer items using delta, Simple8B, and run length encoding.
#[derive(Clone, Debug)]
pub struct DeltaSimple8bRle;

impl<A> RingBuffer<A> for DeltaSimple8bRle
where
    A: Into<u64> + Unsigned,
{
    fn with_capacity(capacity: usize) -> Self {
        warn!("DeltaSimple8bRle ring buffer is unimplemented. No data will be stored.");
        let _ = capacity;
        DeltaSimple8bRle
    }
    fn sampling_intervals(_profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        let placeholder = SamplingInterval::new(1, 1, Duration::from_hours(1));
        [placeholder].into()
    }
}

// TODO(https://fxbug.dev/352614791): Implement DeltaZigZagSimple8bRle ring buffer
/// A ring buffer that stores integer items using delta, zig-zag, Simple8B, and run length
/// encoding.
#[derive(Clone, Debug)]
pub struct DeltaZigZagSimple8bRle;

impl RingBuffer<i64> for DeltaZigZagSimple8bRle {
    fn with_capacity(capacity: usize) -> Self {
        warn!("DeltaZigZagSimple8bRle ring buffer is unimplemented. No data will be stored.");
        let _ = capacity;
        DeltaZigZagSimple8bRle
    }
    fn sampling_intervals(_profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        let placeholder = SamplingInterval::new(1, 1, Duration::from_hours(1));
        [placeholder].into()
    }
}

impl RingBuffer<u64> for DeltaZigZagSimple8bRle {
    fn with_capacity(capacity: usize) -> Self {
        warn!("DeltaZigZagSimple8bRle ring buffer is unimplemented. No data will be stored.");
        let _ = capacity;
        DeltaZigZagSimple8bRle
    }
    fn sampling_intervals(_profile: &SamplingProfile) -> Vec1<SamplingInterval> {
        let placeholder = SamplingInterval::new(1, 1, Duration::from_hours(1));
        [placeholder].into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experimental::series::Gauge;
    use test_case::test_case;

    #[test_case(
        SamplingProfile::Granular,
        [
            SamplingInterval::new(240, 1, Duration::from_seconds(10)),
            SamplingInterval::new(240, 1, Duration::from_minutes(1)),
        ]
        .into();
        "granular_sampling_profile"
    )]
    #[test_case(
        SamplingProfile::Balanced,
        [
            SamplingInterval::new(120, 1, Duration::from_seconds(10)),
            SamplingInterval::new(120, 1, Duration::from_minutes(1)),
            SamplingInterval::new(120, 1, Duration::from_minutes(10)),
            SamplingInterval::new(120, 1, Duration::from_hours(1)),
        ]
        .into();
        "balanced_sampling_profile"
    )]
    fn uncompressed_buffer_sampling_intervals(p: SamplingProfile, expect: Vec1<SamplingInterval>) {
        assert_eq!(<f32 as BufferStrategy<f32, Constant>>::sampling_intervals(&p), expect);
        assert_eq!(<f32 as BufferStrategy<f32, LastSample>>::sampling_intervals(&p), expect);
        assert_eq!(<f32 as BufferStrategy<f32, LastAggregation>>::sampling_intervals(&p), expect);
        assert_eq!(<f64 as BufferStrategy<f64, Constant>>::sampling_intervals(&p), expect);
        assert_eq!(<f64 as BufferStrategy<f64, LastSample>>::sampling_intervals(&p), expect);
        assert_eq!(<f64 as BufferStrategy<f64, LastAggregation>>::sampling_intervals(&p), expect);
        assert_eq!(<Gauge<f32> as BufferStrategy<f32, Constant>>::sampling_intervals(&p), expect);
        assert_eq!(<Gauge<f32> as BufferStrategy<f32, LastSample>>::sampling_intervals(&p), expect);
        assert_eq!(
            <Gauge<f32> as BufferStrategy<f32, LastAggregation>>::sampling_intervals(&p),
            expect
        );
        assert_eq!(<Gauge<f64> as BufferStrategy<f64, Constant>>::sampling_intervals(&p), expect);
        assert_eq!(<Gauge<f64> as BufferStrategy<f64, LastSample>>::sampling_intervals(&p), expect);
        assert_eq!(
            <Gauge<f64> as BufferStrategy<f64, LastAggregation>>::sampling_intervals(&p),
            expect
        );
    }

    #[test]
    fn simple8b_rle_buffer() {
        let mut buffer = <Simple8bRle as RingBuffer<u64>>::with_capacity(2);
        buffer.push(22u64);
        let mut data = vec![];
        let result = RingBuffer::<u64>::serialize(&buffer, &mut data);
        assert!(result.is_ok());
        assert!(!data.is_empty());
    }

    #[test_case(
        SamplingProfile::Granular,
        [
            SamplingInterval::new(116, 1, Duration::from_seconds(10)),
            SamplingInterval::new(116, 1, Duration::from_minutes(1)),
        ]
        .into();
        "granular_sampling_profile"
    )]
    #[test_case(
        SamplingProfile::Balanced,
        [
            SamplingInterval::new(58, 1, Duration::from_seconds(10)),
            SamplingInterval::new(58, 1, Duration::from_minutes(1)),
            SamplingInterval::new(58, 1, Duration::from_minutes(10)),
            SamplingInterval::new(58, 1, Duration::from_hours(1)),
    ]
        .into();
        "balanced_sampling_profile"
    )]
    fn simple8b_rle_buffer_sampling_intervals(p: SamplingProfile, expect: Vec1<SamplingInterval>) {
        assert_eq!(<u64 as BufferStrategy<u64, Constant>>::sampling_intervals(&p), expect);
        assert_eq!(<Gauge<u64> as BufferStrategy<u64, Constant>>::sampling_intervals(&p), expect);
    }
}
