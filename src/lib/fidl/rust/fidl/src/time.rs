// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_os = "fuchsia")]
mod fuchsia {
    pub use zx::{BootInstant, MonotonicInstant};
}
#[cfg(target_os = "fuchsia")]
pub use fuchsia::*;

#[cfg(not(target_os = "fuchsia"))]
mod host {
    // This module provides a small subset of our Zircon Time bindings for the purposes of
    // encoding/decoding times in the host without losing the type safety we get on the target.
    use std::marker::PhantomData;
    use zx_types::zx_time_t;

    /// A marker type representing nanoseconds.
    #[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct NsUnit;

    /// A marker type for the system's monotonic timeline which pauses during suspend.
    #[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct MonotonicTimeline;

    /// A marker type for the system's boot timeline which continues running during suspend.
    #[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct BootTimeline;

    /// Time generic over the Timeline.
    #[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
    #[repr(transparent)]
    pub struct Instant<T, U = NsUnit>(zx_time_t, PhantomData<(T, U)>);

    /// Monotonic timestamp (ns).
    pub type MonotonicInstant = Instant<MonotonicTimeline, NsUnit>;
    /// Boot timestamp (ns).
    pub type BootInstant = Instant<BootTimeline, NsUnit>;

    impl<T> Instant<T> {
        /// Zero timestamp.
        pub const ZERO: Instant<T, NsUnit> = Instant(0, PhantomData);

        /// Create a timestamp from nanoseconds.
        pub const fn from_nanos(nanos: zx_time_t) -> Self {
            Self(nanos, PhantomData)
        }

        /// Return the number of nanoseconds associated with this timestamp.
        pub fn into_nanos(self) -> zx_time_t {
            self.0
        }
    }
}
#[cfg(not(target_os = "fuchsia"))]
pub use host::*;
