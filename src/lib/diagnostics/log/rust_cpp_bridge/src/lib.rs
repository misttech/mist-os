// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrapper around the C++ syslog library for Fuchsia that allows initializing it with the same
//! initial settings as the Rust logging library.

/// Initialize the bridge, configuring the C++ logging backend with the same settings as those for
/// Rust at the time of this call.
pub fn init() {
    let cpp_severity = if log::log_enabled!(log::Level::Trace) {
        FUCHSIA_LOG_TRACE
    } else if log::log_enabled!(log::Level::Debug) {
        FUCHSIA_LOG_DEBUG
    } else if log::log_enabled!(log::Level::Info) {
        FUCHSIA_LOG_INFO
    } else if log::log_enabled!(log::Level::Warn) {
        FUCHSIA_LOG_WARNING
    } else if log::log_enabled!(log::Level::Error) {
        FUCHSIA_LOG_ERROR
    } else {
        FUCHSIA_LOG_FATAL
    };

    // SAFETY: basic FFI call with no invariants
    unsafe {
        init_cpp_logging(cpp_severity);
    }
}

// Mirrored from //sdk/lib/syslog/structured_backend/fuchsia_syslog.h
const FUCHSIA_LOG_TRACE: u8 = 0x10;
const FUCHSIA_LOG_DEBUG: u8 = 0x20;
const FUCHSIA_LOG_INFO: u8 = 0x30;
const FUCHSIA_LOG_WARNING: u8 = 0x40;
const FUCHSIA_LOG_ERROR: u8 = 0x50;
const FUCHSIA_LOG_FATAL: u8 = 0x60;

extern "C" {
    fn init_cpp_logging(min_severity: u8);
}

#[cfg(test)]
mod tests {
    use diagnostics_reader::{ArchiveReader, Logs, Severity};
    use futures::StreamExt;

    extern "C" {
        fn emit_trace_log_for_testing();
        fn emit_debug_log_for_testing();
        fn emit_info_log_for_testing();
        fn emit_warning_log_for_testing();
        fn emit_error_log_for_testing();
    }

    #[fuchsia::test(logging_minimum_severity = "TRACE")]
    async fn cpp_trace_log_appears_after_init() {
        super::init();
        let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();

        // SAFETY: basic FFI call with no invariants
        unsafe { emit_trace_log_for_testing() };

        let message = loop {
            let message = logs.next().await.unwrap().unwrap();
            if message.msg().unwrap() == "TRACE TEST MESSAGE FROM C++"
                && message.severity() == Severity::Trace
            {
                break message;
            }
        };
        assert_eq!(message.moniker.to_string(), ".", "messages must come from this component");
        let file_path = message.file_path().unwrap();
        assert!(file_path.ends_with(".cc"), "messages must come from C++, got {file_path}");
    }

    #[fuchsia::test(logging_minimum_severity = "DEBUG")]
    async fn cpp_debug_log_appears_after_init() {
        super::init();
        let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();

        // SAFETY: basic FFI call with no invariants
        unsafe { emit_debug_log_for_testing() };

        let message = loop {
            let message = logs.next().await.unwrap().unwrap();
            if message.msg().unwrap() == "DEBUG TEST MESSAGE FROM C++"
                && message.severity() == Severity::Debug
            {
                break message;
            }
        };
        assert_eq!(message.moniker.to_string(), ".", "messages must come from this component");
        let file_path = message.file_path().unwrap();
        assert!(file_path.ends_with(".cc"), "messages must come from C++, got {file_path}");
    }

    #[fuchsia::test(logging_minimum_severity = "INFO")]
    async fn cpp_info_log_appears_after_init() {
        super::init();
        let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();

        // SAFETY: basic FFI call with no invariants
        unsafe { emit_info_log_for_testing() };

        let message = loop {
            let message = logs.next().await.unwrap().unwrap();
            if message.msg().unwrap() == "INFO TEST MESSAGE FROM C++"
                && message.severity() == Severity::Info
            {
                break message;
            }
        };
        assert_eq!(message.moniker.to_string(), ".", "messages must come from this component");
        let file_path = message.file_path().unwrap();
        assert!(file_path.ends_with(".cc"), "messages must come from C++, got {file_path}");
    }

    #[fuchsia::test(logging_minimum_severity = "WARN")]
    async fn cpp_warn_log_appears_after_init() {
        super::init();
        let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();

        // SAFETY: basic FFI call with no invariants
        unsafe { emit_warning_log_for_testing() };

        let message = loop {
            let message = logs.next().await.unwrap().unwrap();
            if message.msg().unwrap() == "WARNING TEST MESSAGE FROM C++"
                && message.severity() == Severity::Warn
            {
                break message;
            }
        };
        assert_eq!(message.moniker.to_string(), ".", "messages must come from this component");
        let file_path = message.file_path().unwrap();
        assert!(file_path.ends_with(".cc"), "messages must come from C++, got {file_path}");
    }

    #[fuchsia::test(logging_minimum_severity = "ERROR")]
    async fn cpp_error_log_appears_after_init() {
        super::init();
        let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();

        // SAFETY: basic FFI call with no invariants
        unsafe { emit_error_log_for_testing() };

        let message = loop {
            let message = logs.next().await.unwrap().unwrap();
            if message.msg().unwrap() == "ERROR TEST MESSAGE FROM C++"
                && message.severity() == Severity::Error
            {
                break message;
            }
        };
        assert_eq!(message.moniker.to_string(), ".", "messages must come from this component");
        let file_path = message.file_path().unwrap();
        assert!(file_path.ends_with(".cc"), "messages must come from C++, got {file_path}");
    }
}
