// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    filter::LogFilterCriteria,
    log_socket_stream::{JsonDeserializeError, LogsDataStream},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{Local, TimeZone};
use diagnostics_data::{
    LogTextColor, LogTextDisplayOptions, LogTextPresenter, LogTimeDisplayFormat, LogsData,
    Timestamp,
};
use ffx_writer::ToolIO;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, io::Write, time::SystemTime};
use thiserror::Error;

const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S.%3f";
const NANOS_IN_SECOND: i64 = 1_000_000_000;
const MALFORMED_TARGET_LOG: &str = "malformed target log: ";
const LOGGER_STARTED: &str = "logger started.";
const LOGGER_DISCONNECTED: &str = "Logger lost connection to target. Retrying...";

/// Type of an FFX event
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum EventType {
    /// Overnet connection to logger started
    LoggingStarted,
    /// Overnet connection to logger lost
    TargetDisconnected,
}

/// Type of data in a log entry
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// The log
    pub data: LogData,
    /// The timestamp of the log translated to UTC
    pub timestamp: Timestamp,
}

// Required if we want to use ffx's built-in I/O, but
// this isn't really applicable to us because we have
// custom formatting rules.
impl Display for LogEntry {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unreachable!("UNSUPPORTED -- This type cannot be formatted with std format.");
    }
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

/// Log formatter options
#[derive(Clone, Debug)]
pub struct LogFormatterOptions {
    /// Text display options
    pub display: Option<LogTextDisplayOptions>,
    /// If true, highlights spam, if false, filters it out.
    pub highlight_spam: bool,
}

impl Default for LogFormatterOptions {
    fn default() -> Self {
        LogFormatterOptions { display: Some(Default::default()), highlight_spam: false }
    }
}

/// Trait used to filter spam from log messages. An implementation
/// will check the file, line number, and message against a set of
/// detection rules to determine if it is spam.
pub trait LogSpamFilter {
    /// Returns true if the message containing the given msg content
    /// in the given file and line is considered to be spam.
    fn is_spam(&self, file: Option<&str>, line: Option<u64>, msg: &str) -> bool;
}

#[derive(Error, Debug)]
pub enum FormatterError {
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

/// Default formatter implementation
pub struct DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    writer: W,
    filters: LogFilterCriteria,
    options: LogFormatterOptions,
}

#[async_trait(?Send)]
impl<W> LogFormatter for DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    async fn push_log(&mut self, log_entry: LogEntry) -> Result<()> {
        let is_spam = self.filters.is_spam(&log_entry);

        if (!self.options.highlight_spam && is_spam) || !self.filters.matches(&log_entry) {
            return Ok(());
        }
        match self.options.display {
            Some(mut text_options) => {
                if self.options.highlight_spam && is_spam {
                    text_options.color = LogTextColor::Highlight;
                }
                let mut options_for_this_line_only = self.options.clone();
                options_for_this_line_only.display = Some(text_options);
                self.format_text_log(options_for_this_line_only, log_entry)?;
            }
            None => {
                match log_entry {
                    LogEntry { data: LogData::SymbolizedTargetLog(_, ref symbolized), .. } => {
                        if symbolized.is_empty() {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                self.writer.item(&log_entry)?;
            }
        };

        Ok(())
    }
}

impl<W> BootTimeAccessor for DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    fn set_boot_timestamp(&mut self, boot_ts_nanos: i64) {
        match &mut self.options.display {
            Some(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { ref mut offset, .. },
                ..
            }) => {
                *offset = boot_ts_nanos;
            }
            _ => (),
        }
    }
    fn get_boot_timestamp(&self) -> i64 {
        match &self.options.display {
            Some(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { ref offset, .. },
                ..
            }) => *offset,
            _ => 0,
        }
    }
}

pub enum ColorOverride {
    SpamHighlight,
}

// TODO(https://fxbug.dev/129280): Add unit tests once this is possible
// to test.
fn get_timestamp() -> Result<Timestamp> {
    Ok(Timestamp::from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("system time before Unix epoch")?
            .as_nanos() as i64,
    ))
}

fn format_ffx_event(msg: &str, timestamp: Option<Timestamp>) -> String {
    let ts: i64 = timestamp.unwrap_or_else(|| get_timestamp().unwrap()).into();
    let dt = Local
        .timestamp(ts / NANOS_IN_SECOND, (ts % NANOS_IN_SECOND) as u32)
        .format(TIMESTAMP_FORMAT)
        .to_string();
    format!("[{}][<ffx>]: {}", dt, msg)
}

/// Object which contains a Writer that can be borrowed
pub trait WriterContainer<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    fn writer(&mut self) -> &mut W;
}

impl<W> WriterContainer<W> for DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    fn writer(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W> DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    pub fn new(filters: LogFilterCriteria, writer: W, options: LogFormatterOptions) -> Self {
        Self { filters, writer, options }
    }

    // This function's arguments are copied to make lifetimes in push_log easier since borrowing
    // &self would complicate spam highlighting.
    fn format_text_log(
        &mut self,
        options: LogFormatterOptions,
        log_entry: LogEntry,
    ) -> Result<(), FormatterError> {
        let text_options = match options.display {
            Some(o) => o,
            None => {
                unreachable!("If we are here, we can only be formatting text");
            }
        };
        Ok(match log_entry {
            LogEntry { data: LogData::TargetLog(data), .. } => {
                // TODO(https://fxbug.dev/121413): Add support for log spam redaction and other
                // features listed in the design doc.
                writeln!(self.writer, "{}", LogTextPresenter::new(&data, text_options))?;
            }
            LogEntry { data: LogData::SymbolizedTargetLog(mut data, symbolized), .. } => {
                *data
                    .msg_mut()
                    .expect("if a symbolized message is provided then the payload has a message") =
                    symbolized;
                writeln!(self.writer, "{}", LogTextPresenter::new(&data, text_options))?;
            }
            LogEntry { data: LogData::MalformedTargetLog(raw), timestamp } => {
                writeln!(
                    self.writer,
                    "{}",
                    format_ffx_event(&format!("{MALFORMED_TARGET_LOG}{}", raw), Some(timestamp))
                )?;
            }
            LogEntry { data: LogData::FfxEvent(etype), timestamp, .. } => match etype {
                EventType::LoggingStarted => {
                    writeln!(self.writer, "{}", format_ffx_event(LOGGER_STARTED, Some(timestamp)))?;
                }
                EventType::TargetDisconnected => writeln!(
                    self.writer,
                    "{}",
                    format_ffx_event(LOGGER_DISCONNECTED, Some(timestamp),)
                )?,
            },
        })
    }
}

/// Symbolizer that does nothing.
pub struct NoOpSymbolizer;

#[async_trait(?Send)]
impl Symbolize for NoOpSymbolizer {
    async fn symbolize(&self, entry: LogEntry) -> LogEntry {
        entry
    }
}

#[async_trait(?Send)]
pub trait LogFormatter {
    async fn push_log(&mut self, log_entry: LogEntry) -> anyhow::Result<()>;
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use diagnostics_data::{LogsDataBuilder, Severity, Timezone};
    use ffx_writer::{Format, MachineWriter, TestBuffers};
    use std::{cell::Cell, time::Duration};

    use super::*;

    const DEFAULT_TS_NANOS: u64 = 1615535969000000000;

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
    async fn test_boot_timestamp_setter() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions {
            display: Some(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { tz: Timezone::Utc, offset: 0 },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        formatter.set_boot_timestamp(1234);
        assert_eq!(formatter.get_boot_timestamp(), 1234);

        // Boot timestamp not supported when using JSON output.
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), output, options);
        formatter.set_boot_timestamp(1234);
        assert_eq!(formatter.get_boot_timestamp(), 0);
    }

    struct AlternatingSpamFilter {
        last_message_was_spam: Cell<bool>,
    }
    impl LogSpamFilter for AlternatingSpamFilter {
        fn is_spam(&self, _file: Option<&str>, _line: Option<u64>, _msg: &str) -> bool {
            let prev = self.last_message_was_spam.get();
            self.last_message_was_spam.set(!prev);
            prev
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

    fn logs_data_builder() -> LogsDataBuilder {
        diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            timestamp_nanos: Timestamp::from(default_ts().as_nanos() as i64),
            component_url: Some("component_url".to_string()),
            moniker: "some/moniker".to_string(),
            severity: diagnostics_data::Severity::Warn,
        })
        .set_pid(1)
        .set_tid(2)
    }

    fn default_ts() -> Duration {
        Duration::from_nanos(DEFAULT_TS_NANOS)
    }

    fn log_entry() -> LogEntry {
        LogEntry {
            timestamp: 0.into(),
            data: LogData::TargetLog(
                logs_data_builder().add_tag("tag1").add_tag("tag2").set_message("message").build(),
            ),
        }
    }

    #[fuchsia::test]
    async fn test_default_formatter() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions::default();
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        formatter.push_log(log_entry()).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\n"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_with_hidden_metadata() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut options = LogFormatterOptions::default();
        options.display =
            Some(LogTextDisplayOptions { show_metadata: false, ..Default::default() });
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        formatter.push_log(log_entry()).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            "[1615535969.000000][some/moniker][tag1,tag2] WARN: message\n"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_with_json() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(Some(Format::Json), &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        {
            let mut formatter =
                DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
            formatter.push_log(log_entry()).await.unwrap();
        }
        assert_eq!(
            serde_json::from_str::<LogEntry>(&buffers.stdout.clone().into_string()).unwrap(),
            log_entry()
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_symbolized_log_message() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions::default();
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options);
        let mut entry = log_entry();
        entry.data = assert_matches!(entry.data.clone(), LogData::TargetLog(d)=>LogData::SymbolizedTargetLog(d, "symbolized".to_string()));
        formatter.push_log(entry).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: symbolized\n"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_symbolized_json_log_message() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(Some(Format::Json), &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options);
        let mut entry = log_entry();
        entry.data = assert_matches!(entry.data.clone(), LogData::TargetLog(d)=>LogData::SymbolizedTargetLog(d, "symbolized".to_string()));
        formatter.push_log(entry.clone()).await.unwrap();
        drop(formatter);
        assert_eq!(
            serde_json::from_str::<LogEntry>(&buffers.stdout.clone().into_string()).unwrap(),
            entry
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_symbolize_failed_json_log_message() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options);
        let mut entry = log_entry();
        entry.data = assert_matches!(entry.data.clone(), LogData::TargetLog(d)=>LogData::SymbolizedTargetLog(d, "".to_string()));
        formatter.push_log(entry.clone()).await.unwrap();
        drop(formatter);
        assert_eq!(buffers.stdout.clone().into_string().is_empty(), true);
    }

    #[fuchsia::test]
    async fn test_default_formatter_disconnect_event() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions::default();
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        let mut entry = log_entry();
        entry.data = LogData::FfxEvent(EventType::TargetDisconnected);
        formatter.push_log(entry).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            format!("[1970-01-01 00:00:00.000][<ffx>]: {LOGGER_DISCONNECTED}\n")
        );
    }

    #[fuchsia::test]
    async fn test_spam_list_applies_highlighting_only_to_spam_line() {
        let options =
            LogFormatterOptions { display: Some(Default::default()), highlight_spam: true };

        let mut filter = LogFilterCriteria::default();
        filter.with_spam_filter(AlternatingSpamFilter { last_message_was_spam: Cell::new(false) });
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        {
            let mut formatter = DefaultLogFormatter::new(filter, output, options.clone());
            formatter.push_log(log_entry()).await.unwrap();
            formatter.push_log(log_entry()).await.unwrap();
            formatter.push_log(log_entry()).await.unwrap();
        }
        assert_eq!(
            buffers.stdout.into_string(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message
\u{1b}[38;5;11m[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\u{1b}[m
[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\n",
            "first message should be uncolored, second should be yellow, third should be uncolored"
        );
    }

    #[fuchsia::test]
    async fn test_spam_filter_filters_spam_if_highlighting_is_disabled() {
        let options = LogFormatterOptions::default();

        let mut filter = LogFilterCriteria::default();
        filter.with_spam_filter(AlternatingSpamFilter { last_message_was_spam: Cell::new(false) });
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        {
            let mut formatter = DefaultLogFormatter::new(filter, output, options.clone());
            formatter.push_log(log_entry()).await.unwrap();
            formatter.push_log(log_entry()).await.unwrap();
            formatter.push_log(log_entry()).await.unwrap();
        }
        assert_eq!(
            buffers.stdout.into_string(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message
[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\n",
            "should only get two messages"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_started_event() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions::default();
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        let mut entry = log_entry();
        entry.data = LogData::FfxEvent(EventType::LoggingStarted);
        formatter.push_log(entry).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            "[1970-01-01 00:00:00.000][<ffx>]: logger started.\n"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_malformed_log() {
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions::default();
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), stdout, options.clone());
        let mut entry = log_entry();
        entry.data = LogData::MalformedTargetLog("Invalid log".to_string());
        formatter.push_log(entry).await.unwrap();
        drop(formatter);
        assert_eq!(
            buffers.stdout.clone().into_string(),
            format!("[1970-01-01 00:00:00.000][<ffx>]: {MALFORMED_TARGET_LOG}Invalid log\n")
        );
    }
}
