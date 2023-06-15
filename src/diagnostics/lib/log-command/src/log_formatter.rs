// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::log_socket_stream::{JsonDeserializeError, LogsDataStream};
use anyhow::Result;
use async_trait::async_trait;
use diagnostics_data::{LogsData, Timestamp};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

/// Type of an FFX event
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum EventType {
    /// Overnet connection to logger started
    LoggingStarted,
    /// Overnet connection to logger lost
    TargetDisconnected,
}

/// Type of data in a log entry
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum LogData {
    /// A log entry from the target
    TargetLog(LogsData),
    /// A symbolized log (Original log, Symbolizer output)
    SymbolizedTargetLog(LogsData, String),
    /// A malformed log (invalid JSON)
    MalformedTargetLog(String),
    /// An FFX event
    FfxEvent(EventType),
}

impl From<LogsData> for LogData {
    fn from(data: LogsData) -> Self {
        Self::TargetLog(data)
    }
}

/// A log entry from either the host, target, or
/// a symbolized log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogEntry {
    /// The log
    pub data: LogData,
    /// The timestamp of the log translated to UTC
    pub timestamp: Timestamp,
}

/// A trait for symbolizing log entries
#[async_trait(?Send)]
pub trait Symbolize {
    async fn symbolize(&self, entry: LogEntry) -> LogEntry;
}

async fn handle_value<F, S>(
    one: diagnostics_data::Data<diagnostics_data::Logs>,
    formatter: &mut F,
    symbolizer: &S,
) -> Result<(), JsonDeserializeError>
where
    F: LogFormatter + BootTimeAccessor,
    S: Symbolize,
{
    let boot_ts = formatter.get_boot_timestamp();

    let entry = LogEntry {
        timestamp: {
            let monotonic = one.metadata.timestamp;
            Timestamp::from(monotonic + boot_ts)
        },
        data: one.into(),
    };
    formatter.push_log(symbolizer.symbolize(entry).await).await?;
    Ok(())
}

/// Reads logs from a socket and formats them using the given formatter and symbolizer.
pub async fn dump_logs_from_socket<F, S>(
    socket: fuchsia_async::Socket,
    formatter: &mut F,
    symbolizer: &S,
) -> Result<(), JsonDeserializeError>
where
    F: LogFormatter + BootTimeAccessor,
    S: Symbolize,
{
    let mut decoder = Box::pin(LogsDataStream::new(socket));
    while let Some(log) = decoder.next().await {
        handle_value(log, formatter, symbolizer).await?;
    }
    Ok(())
}

pub trait BootTimeAccessor {
    /// Sets the boot timestamp in nanoseconds since the Unix epoch.
    fn set_boot_timestamp(&mut self, _boot_ts_nanos: i64);

    /// Returns the boot timestamp in nanoseconds since the Unix epoch.
    fn get_boot_timestamp(&self) -> i64;
}

#[async_trait(?Send)]
pub trait LogFormatter {
    async fn push_log(&mut self, log_entry: LogEntry) -> anyhow::Result<()>;
}

#[cfg(test)]
mod test {
    use diagnostics_data::{LogsDataBuilder, Severity};

    use super::*;

    struct NoOpSymbolizer {}

    #[async_trait(?Send)]
    impl Symbolize for NoOpSymbolizer {
        async fn symbolize(&self, entry: LogEntry) -> LogEntry {
            entry
        }
    }

    struct FakeFormatter {
        logs: Vec<LogEntry>,
    }

    impl FakeFormatter {
        fn new() -> Self {
            Self { logs: Vec::new() }
        }
    }

    impl BootTimeAccessor for FakeFormatter {
        fn set_boot_timestamp(&mut self, _boot_ts_nanos: i64) {}

        fn get_boot_timestamp(&self) -> i64 {
            0
        }
    }

    #[async_trait(?Send)]
    impl LogFormatter for FakeFormatter {
        async fn push_log(&mut self, log_entry: LogEntry) -> anyhow::Result<()> {
            self.logs.push(log_entry);
            Ok(())
        }
    }

    #[fuchsia::test]
    async fn test_format_single_message() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world!")
        .build();
        let (sender, receiver) = fuchsia_zircon::Socket::create_stream();
        sender
            .write(serde_json::to_string(&target_log).unwrap().as_bytes())
            .expect("failed to write target log");
        drop(sender);
        dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver).unwrap(),
            &mut formatter,
            &symbolizer,
        )
        .await
        .unwrap();
        assert_eq!(
            formatter.logs,
            vec![LogEntry { data: LogData::TargetLog(target_log), timestamp: Timestamp::from(0) }]
        );
    }

    #[fuchsia::test]
    async fn test_format_multiple_messages() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        let (sender, receiver) = fuchsia_zircon::Socket::create_stream();
        let target_log_0 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world!")
        .build();
        let target_log_1 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(1),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world 2!")
        .build();
        sender
            .write(serde_json::to_string(&vec![&target_log_0, &target_log_1]).unwrap().as_bytes())
            .expect("failed to write target log");
        drop(sender);
        dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver).unwrap(),
            &mut formatter,
            &symbolizer,
        )
        .await
        .unwrap();
        assert_eq!(
            formatter.logs,
            vec![
                LogEntry { data: LogData::TargetLog(target_log_0), timestamp: Timestamp::from(0) },
                LogEntry { data: LogData::TargetLog(target_log_1), timestamp: Timestamp::from(1) }
            ]
        );
    }
}
