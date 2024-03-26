// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fmt::Debug, ops::Range};

/// Hints provided to the `AllocatorStrategy::allocate()` call about where the allocation should
/// start.
#[derive(Debug)]
pub enum LocationHint {
    /// The allocation can be anywhere.
    None,
    /// The allocation MUST start at the provided offset and be the requested length or fail.
    /// This is only for allocating the super-block at a fixed device offset.
    Require(u64),
    /// Returns the next free space at or after the provided offset (even if it is smaller than
    /// requested). This is used when TRIMing to enumerate all free space.
    /// Note that this hint ignores the strategy being used.
    NextAvailable(u64),
}

/// Tracks free space and decides where allocations are to be made.
///
/// Note that this strategy *excludes*:
///
///   * get/set byte limits
///   * reservation tracking of future allocations.
///   * deallocated but not yet usable regions (awaiting flush).
///   * TRIM-specific logic
///   * Volume Deletion
///
/// High level concepts such as these should be implemented in `Allocator`.
///
/// Implementors of this trait should scan the device at initialization time and build
/// a map of free extents.
///
/// This trait currently assumes all free extents are cached in RAM.
/// (FSCK, which is done at every boot, currently makes a similar assumption for allocated extents.)
/// TODO(b/316827348): Should support capped RAM usage at some point.
pub trait AllocatorStrategy: Send + Sync + Debug {
    /// Tries to assign a set of contiguous `bytes` and returns the range, removing it
    /// from the pool of available bytes and returning it.
    ///
    /// If insufficient contiguous space is available, the largest available range will
    /// be returned. If no bytes are available, None will be returned.
    ///
    /// LocationHint provides limited control over the range in which an allocation should be
    /// made.
    ///
    /// There are no special requirements on alignment of `bytes` but the caller is generally
    /// encouraged to align to device block size..
    fn allocate(&mut self, bytes: u64, hint: LocationHint) -> Option<Range<u64>>;

    /// Adds a range of bytes to the pool of available ranges.
    ///
    /// There are no special requirements on the alignment of `range` but
    /// callers should should generally ensure ranges are block aligned.
    fn free(&mut self, range: Range<u64>);
}

pub mod best_fit;
