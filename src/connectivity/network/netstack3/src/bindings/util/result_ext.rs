// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Display;
use std::panic::Location;

use log::{debug, error, trace, warn};

/// An extension for common logging patterns for error types.
pub trait ErrorLogExt {
    /// Logs this error with `msg`.
    fn log(&self, msg: impl Display);
}

/// Extensions to `Result` types used in bindings.
///
/// It is implemented for well known result types.
pub trait ResultExt {
    /// The Ok type of this result.
    type Ok;
    /// The error type of this result.
    type Error: ErrorLogExt;

    fn as_result(self) -> Result<Self::Ok, Self::Error>;

    /// Consumes self, logs the error with `msg` if it's an error and returns
    /// back `Self` unmodified.
    fn log_error(self, msg: impl Display) -> Self;

    //// Logs the error in `self` if it's an error variant.
    fn inspect_log_error(&self, msg: impl Display);

    /// Consumes this result and logs an error if it's the `Err`` variant.
    ///
    /// This allows many common logsites to be coalesced into a single function,
    /// which reduces the size of generated code.
    fn unwrap_or_log(self, msg: impl Display)
    where
        Self: ResultExt<Ok = ()>;
}

impl<O, E: ErrorLogExt> ResultExt for Result<O, E> {
    type Ok = O;
    type Error = E;

    fn as_result(self) -> Result<O, E> {
        self
    }

    #[track_caller]
    fn log_error(self, msg: impl Display) -> Self {
        match self {
            Ok(v) => Ok(v),
            Err(e) => {
                e.log(msg);
                Err(e)
            }
        }
    }

    #[track_caller]
    fn inspect_log_error(&self, msg: impl Display) {
        match self {
            Ok(_) => (),
            Err(e) => {
                e.log(msg);
            }
        }
    }

    #[track_caller]
    fn unwrap_or_log(self, msg: impl Display)
    where
        Self: ResultExt<Ok = ()>,
    {
        match self.as_result() {
            Ok(()) => (),
            Err(e) => {
                e.log(msg);
            }
        }
    }
}

impl ErrorLogExt for fidl::Error {
    #[track_caller]
    fn log(&self, msg: impl Display) {
        // TODO(https://fxbug.dev/343992493): don't embed location in the log
        // message when better track_caller support is available.
        let location = Location::caller();
        if self.is_closed() {
            debug!("{location}: {msg}: {self:?}");
        } else {
            error!("{location}: {msg}: {self:?}");
        }
    }
}

impl ErrorLogExt for fidl_fuchsia_posix::Errno {
    #[track_caller]
    fn log(&self, msg: impl Display) {
        let location = Location::caller();
        // TODO(https://fxbug.dev/343992493): don't embed location in the log
        // message when better track_caller support is available.
        match self {
            // Errnos that indicate the socket API is being called incorrectly.
            fidl_fuchsia_posix::Errno::Einval
            | fidl_fuchsia_posix::Errno::Eafnosupport
            | fidl_fuchsia_posix::Errno::Enoprotoopt => warn!("{location}: {msg}: {self:?}"),
            // Errnos that may occur under normal operation and are quite noisy.
            fidl_fuchsia_posix::Errno::Enetunreach
            | fidl_fuchsia_posix::Errno::Ehostunreach
            | fidl_fuchsia_posix::Errno::Eagain => trace!("{location}: {msg}: {self:?}"),
            // All other errnos.
            _ => debug!("{location}: {msg}: {self:?}"),
        }
    }
}
