// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use openthread::prelude::*;
use std::ffi::CStr;

pub const fn tracing_level_from(log_level: ot::LogLevel) -> tracing::Level {
    match log_level {
        ot::LogLevel::None => tracing::Level::ERROR,
        ot::LogLevel::Crit => tracing::Level::ERROR,
        ot::LogLevel::Warn => tracing::Level::WARN,
        ot::LogLevel::Note => tracing::Level::INFO,
        ot::LogLevel::Info => tracing::Level::INFO,
        ot::LogLevel::Debg => tracing::Level::DEBUG,
    }
}

pub const fn ot_log_level_from(log_level: tracing::Level) -> ot::LogLevel {
    match log_level {
        tracing::Level::ERROR => ot::LogLevel::Crit,
        tracing::Level::WARN => ot::LogLevel::Warn,
        tracing::Level::INFO => ot::LogLevel::Info,
        tracing::Level::DEBUG => ot::LogLevel::Debg,
        tracing::Level::TRACE => ot::LogLevel::Debg,
    }
}

#[no_mangle]
unsafe extern "C" fn otPlatLogLine(
    log_level: otsys::otLogLevel,
    _: otsys::otLogRegion, // otLogRegion is deprecated.
    line: *const ::std::os::raw::c_char,
) {
    let line = CStr::from_ptr(line);
    let tracing_level = tracing_level_from(log_level.into());

    match line.to_str() {
        // Log line isn't valid UTF-8, display it directly.
        Ok(line) => match tracing_level {
            tracing::Level::ERROR => tracing::error!(tag = "ot", "{line}"),
            tracing::Level::WARN => tracing::warn!(tag = "ot", "{line}"),
            tracing::Level::INFO => tracing::info!(tag = "ot", "{line}"),
            tracing::Level::DEBUG => tracing::debug!(tag = "ot", "{line}"),
            tracing::Level::TRACE => tracing::trace!(tag = "ot", "{line}"),
        },

        // Log line isn't valid UTF-8, try rendering with escaping.
        Err(_) => match tracing_level {
            tracing::Level::ERROR => tracing::error!(tag = "ot", "{line:?}"),
            tracing::Level::WARN => tracing::warn!(tag = "ot", "{line:?}"),
            tracing::Level::INFO => tracing::info!(tag = "ot", "{line:?}"),
            tracing::Level::DEBUG => tracing::debug!(tag = "ot", "{line:?}"),
            tracing::Level::TRACE => tracing::trace!(tag = "ot", "{line:?}"),
        },
    }
}

#[no_mangle]
unsafe extern "C" fn otPlatLogHandleLevelChanged(log_level: otsys::otLogLevel) {
    let tracing_level = tracing_level_from(log_level.into());

    tracing::info!(
        tag = "openthread_fuchsia",
        "otPlatLogHandleLevelChanged: {log_level} (Tracing: {tracing_level})"
    );
}
