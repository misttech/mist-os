// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::{Debug, Display};

/// An extension for common logging patterns for error types.
pub trait ErrorLogExt: Debug {
    fn log_level(&self) -> log::Level;

    #[track_caller]
    fn log(&self, msg: impl Display) {
        let level = self.log_level();
        log::log!(level, "{msg}: {self:?}");
    }
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
    fn log_level(&self) -> log::Level {
        if self.is_closed() {
            log::Level::Debug
        } else {
            log::Level::Error
        }
    }
}

impl ErrorLogExt for fidl_fuchsia_posix::Errno {
    fn log_level(&self) -> log::Level {
        match self {
            // Errnos that indicate the socket API is being called incorrectly.
            fidl_fuchsia_posix::Errno::Einval
            | fidl_fuchsia_posix::Errno::Eafnosupport
            | fidl_fuchsia_posix::Errno::Enoprotoopt => log::Level::Warn,
            // Errnos that may occur under normal operation and are quite noisy.
            fidl_fuchsia_posix::Errno::Enetunreach
            | fidl_fuchsia_posix::Errno::Ehostunreach
            | fidl_fuchsia_posix::Errno::Eagain => log::Level::Trace,
            // All other errnos.
            _ => log::Level::Debug,
        }
    }
}
