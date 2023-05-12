// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Emits a log message at the [`tracing::Level`] provided in the first argument.
///
/// Example usage:
/// ```
/// let level = crate::bindings::util::fidl_err_log_level(&e);
/// log_error!(level, "A log message: {:?}", e);
/// ```
macro_rules! log_error {
    ($lvl:expr, $($args:expr),*) => {
        match $lvl {
            ::tracing::Level::ERROR => ::tracing::error!($($args),*),
            ::tracing::Level::WARN => ::tracing::warn!($($args),*),
            ::tracing::Level::INFO => ::tracing::info!($($args),*),
            ::tracing::Level::DEBUG => ::tracing::debug!($($args),*),
            _ => ::tracing::trace!($($args),*)
        }
    };
}

// Errors from `$responder.send` can be safely ignored during regular operation;
// they are handled only by logging to error.
macro_rules! responder_send {
    ($responder:expr, $arg:expr) => {
        $responder.send($arg).unwrap_or_else(|e| {
            log_error!(
                crate::bindings::util::fidl_err_log_level(&e),
                "Responder send error: {:?}",
                e
            )
        })
    };
}
