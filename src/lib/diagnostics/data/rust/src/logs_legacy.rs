// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(target_os = "fuchsia")]

use crate::{Data, Logs};
use fidl_fuchsia_logger::LogMessage;
use fuchsia_zircon as zx;
use std::fmt::Write;

/// Convert this `Message` to a FIDL representation suitable for sending to `LogListenerSafe`.
impl Into<LogMessage> for &Data<Logs> {
    fn into(self) -> LogMessage {
        let mut msg = self.msg().unwrap_or("").to_string();

        if let Some(payload) = self.payload_keys() {
            for property in payload.properties.iter() {
                write!(&mut msg, " {property}").expect("allocations have to fail for this to fail");
            }
        }
        let file = self.metadata.file.as_ref();
        let line = self.metadata.line.as_ref();
        if let (Some(file), Some(line)) = (file, line) {
            msg = format!("[{}({})] {}", file, line, msg);
        }

        let tags = match &self.metadata.tags {
            None => vec![self.component_name().to_string()],
            Some(tags) if tags.is_empty() => vec![self.component_name().to_string()],
            Some(tags) => tags.clone(),
        };

        LogMessage {
            pid: self.pid().unwrap_or(zx::sys::ZX_KOID_INVALID),
            tid: self.tid().unwrap_or(zx::sys::ZX_KOID_INVALID),
            time: self.metadata.timestamp.into_nanos(),
            severity: self.metadata.raw_severity() as i32,
            dropped_logs: self.dropped_logs().unwrap_or(0) as _,
            tags,
            msg,
        }
    }
}

/// Convert this `Message` to a FIDL representation suitable for sending to `LogListenerSafe`.
impl Into<LogMessage> for Data<Logs> {
    fn into(self) -> LogMessage {
        let mut msg = self.msg().unwrap_or("").to_string();

        if let Some(payload) = self.payload_keys() {
            for property in payload.properties.iter() {
                write!(&mut msg, " {property}").expect("allocations have to fail for this to fail");
            }
        }
        let file = self.metadata.file.as_ref();
        let line = self.metadata.line.as_ref();
        if let (Some(file), Some(line)) = (file, line) {
            msg = format!("[{}({})] {}", file, line, msg);
        }

        LogMessage {
            pid: self.pid().unwrap_or(zx::sys::ZX_KOID_INVALID),
            tid: self.tid().unwrap_or(zx::sys::ZX_KOID_INVALID),
            time: self.metadata.timestamp.into_nanos(),
            severity: self.metadata.raw_severity() as i32,
            dropped_logs: self.dropped_logs().unwrap_or(0) as _,
            tags: match self.metadata.tags {
                Some(tags) => tags,
                None => vec![self.component_name().to_string()],
            },
            msg,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use fidl_fuchsia_diagnostics as fdiagnostics;

    const TEST_URL: &'static str = "fuchsia-pkg://test";
    const TEST_MONIKER: &'static str = "fake-test/moniker";

    macro_rules! severity_roundtrip_test {
        ($raw:expr, $expected:expr) => {
            let severity = Severity::from(u8::from($raw));
            let msg = LogsDataBuilder::new(BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some(TEST_URL.into()),
                moniker: moniker::ExtendedMoniker::parse_str(TEST_MONIKER).unwrap(),
                severity,
            })
            .build();

            let legacy_msg: LogMessage = (&msg).into();
            assert_eq!(
                legacy_msg.severity,
                i32::from($expected),
                "failed to round trip severity for {:?} (raw {}), intermediates: {:#?}\n{:#?}",
                severity,
                $raw,
                msg,
                legacy_msg
            );
        };
    }

    #[fuchsia::test]
    fn raw_severity_roundtrip_trace() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Trace.into_primitive() - 1,
            fdiagnostics::Severity::Trace.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Trace.into_primitive(),
            fdiagnostics::Severity::Trace.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Trace.into_primitive() + 1,
            fdiagnostics::Severity::Debug.into_primitive()
        );
    }

    #[fuchsia::test]
    fn severity_roundtrip_debug() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Debug.into_primitive() - 1,
            fdiagnostics::Severity::Debug.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Debug.into_primitive(),
            fdiagnostics::Severity::Debug.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Debug.into_primitive() + 1,
            fdiagnostics::Severity::Info.into_primitive()
        );
    }

    #[fuchsia::test]
    fn severity_roundtrip_info() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Info.into_primitive() - 1,
            fdiagnostics::Severity::Info.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Info.into_primitive(),
            fdiagnostics::Severity::Info.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Info.into_primitive() + 1,
            fdiagnostics::Severity::Warn.into_primitive()
        );
    }

    #[fuchsia::test]
    fn severity_roundtrip_warn() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Warn.into_primitive() - 1,
            fdiagnostics::Severity::Warn.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Warn.into_primitive(),
            fdiagnostics::Severity::Warn.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Warn.into_primitive() + 1,
            fdiagnostics::Severity::Error.into_primitive()
        );
    }

    #[fuchsia::test]
    fn severity_roundtrip_error() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Error.into_primitive() - 1,
            fdiagnostics::Severity::Error.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Error.into_primitive(),
            fdiagnostics::Severity::Error.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Error.into_primitive() + 1,
            fdiagnostics::Severity::Fatal.into_primitive()
        );
    }

    #[fuchsia::test]
    fn severity_roundtrip_fatal() {
        severity_roundtrip_test!(
            fdiagnostics::Severity::Fatal.into_primitive() - 1,
            fdiagnostics::Severity::Fatal.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Fatal.into_primitive(),
            fdiagnostics::Severity::Fatal.into_primitive()
        );
        severity_roundtrip_test!(
            fdiagnostics::Severity::Fatal.into_primitive() + 1,
            fdiagnostics::Severity::Fatal.into_primitive()
        );
    }
}
