// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Helpers for logging.
//!
//! Logging using the macros exported from this module will always include
//! the [`NETLINK_LOG_TAG`] tag.
//!
//! [`NETLINK_LOG_TAG`]: crate::NETLINK_LOG_TAG

/// Emits a log at the specified level.
///
/// This macro should not be used directly and the other `log_*` macros should
/// be used instead.
macro_rules! __log_inner {
    (level = $level:ident, $($arg:tt)*) => {
        tracing::$level!(tag = $crate::NETLINK_LOG_TAG, $($arg)*)
    }
}

/// Emits a debug log.
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logging::__log_inner!(level = debug, $($arg)*)
    }
}

/// Emits an error log.
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::__log_inner!(level = error, $($arg)*)
    }
}

/// Emits a warning log.
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::__log_inner!(level = warn, $($arg)*)
    }
}

// Re-exporting macros allows them to be used like regular rust items.
//
// We re-export `__log_inner` so that invocation sites of the `log_*` macros can
// access `__log_inner` as it is invoked by the `log_*` implementations. See
// https://doc.rust-lang.org/reference/macros-by-example.html#hygiene for more
// details.
pub(crate) use __log_inner;
pub(crate) use log_debug;
pub(crate) use log_error;
pub(crate) use log_warn;
