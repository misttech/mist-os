// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    filter::LogFilterCriteria,
    log_socket_stream::{JsonDeserializeError, LogsDataStream},
    DetailedDateTime, LogCommand, LogError, LogProcessingResult, TimeFormat,
};
use anyhow::Result;
use async_trait::async_trait;
use diagnostics_data::{
    Data, LogTextColor, LogTextDisplayOptions, LogTextPresenter, LogTimeDisplayFormat, Logs,
    LogsData, Timestamp, Timezone,
};
use ffx_writer::ToolIO;
use futures_util::{future::Either, select, stream::FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::{fmt::Display, io::Write, time::Duration};
use thiserror::Error;

pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S.%3f";

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
}

impl LogData {
    /// Gets the LogData as a target log.
    pub fn as_target_log(&self) -> Option<&LogsData> {
        match self {
            LogData::TargetLog(log) => Some(log),
        }
    }

    pub fn as_target_log_mut(&mut self) -> Option<&mut LogsData> {
        match self {
            LogData::TargetLog(log) => Some(log),
        }
    }
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
    /// Symbolizes a LogEntry and optionally produces a result.
    /// The symbolizer may choose to discard the result.
    /// This method may be called multiple times concurrently.
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry>;
}

async fn handle_value<S>(one: Data<Logs>, boot_ts: i64, symbolizer: &S) -> Option<LogEntry>
where
    S: Symbolize + ?Sized,
{
    let entry = LogEntry {
        timestamp: {
            let monotonic = one.metadata.timestamp;
            Timestamp::from(monotonic + boot_ts)
        },
        data: one.into(),
    };
    symbolizer.symbolize(entry).await
}

/// Reads logs from a socket and formats them using the given formatter and symbolizer.
pub async fn dump_logs_from_socket<F, S>(
    socket: fuchsia_async::Socket,
    formatter: &mut F,
    symbolizer: &S,
) -> Result<LogProcessingResult, JsonDeserializeError>
where
    F: LogFormatter + BootTimeAccessor,
    S: Symbolize + ?Sized,
{
    let boot_ts = formatter.get_boot_timestamp();
    let mut decoder = Box::pin(LogsDataStream::new(socket).fuse());
    let mut symbolize_pending = FuturesUnordered::new();
    while let Some(value) = select! {
        res = decoder.next() => Some(Either::Left(res)),
        res = symbolize_pending.next() => Some(Either::Right(res)),
        complete => None,
    } {
        match value {
            Either::Left(Some(log)) => {
                symbolize_pending.push(handle_value(log, boot_ts, symbolizer));
            }
            Either::Right(Some(Some(symbolized))) => match formatter.push_log(symbolized).await? {
                LogProcessingResult::Exit => {
                    return Ok(LogProcessingResult::Exit);
                }
                LogProcessingResult::Continue => {}
            },
            _ => {}
        }
    }
    Ok(LogProcessingResult::Continue)
}

pub trait BootTimeAccessor {
    /// Sets the boot timestamp in nanoseconds since the Unix epoch.
    fn set_boot_timestamp(&mut self, _boot_ts_nanos: i64);

    /// Returns the boot timestamp in nanoseconds since the Unix epoch.
    fn get_boot_timestamp(&self) -> i64;
}

/// Timestamp filter which is either either monotonic-based or UTC-based.
#[derive(Clone, Debug)]
pub struct DeviceOrLocalTimestamp {
    /// Timestamp in monotonic time
    pub timestamp: Timestamp,
    /// True if this filter should be applied to monotonic time,
    /// false if UTC time.
    pub is_monotonic: bool,
}

impl DeviceOrLocalTimestamp {
    /// Creates a DeviceOrLocalTimestamp from a real-time date/time or
    /// a monotonic date/time. Returns None if both rtc and monotonic are None.
    /// Returns None if the timestamp is "now".
    pub fn new(
        rtc: Option<&DetailedDateTime>,
        monotonic: Option<&Duration>,
    ) -> Option<DeviceOrLocalTimestamp> {
        rtc.as_ref()
            .filter(|value| !value.is_now)
            .map(|value| DeviceOrLocalTimestamp {
                timestamp: Timestamp::from(value.naive_utc().timestamp_nanos_opt().unwrap()),
                is_monotonic: false,
            })
            .or_else(|| {
                monotonic.map(|value| DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from(value.as_nanos() as i64),
                    is_monotonic: true,
                })
            })
    }
}

/// Log formatter options
#[derive(Clone, Debug)]
pub struct LogFormatterOptions {
    /// Text display options
    pub display: Option<LogTextDisplayOptions>,
    /// Only display logs since the specified time.
    pub since: Option<DeviceOrLocalTimestamp>,
    /// Only display logs until the specified time.
    pub until: Option<DeviceOrLocalTimestamp>,
}

impl Default for LogFormatterOptions {
    fn default() -> Self {
        LogFormatterOptions { display: Some(Default::default()), since: None, until: None }
    }
}

/// Log formatter error
#[derive(Error, Debug)]
pub enum FormatterError {
    /// An unknown error occurred
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    /// An IO error occurred
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// Default formatter implementation
pub struct DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    writer: W,
    filters: LogFilterCriteria,
    options: LogFormatterOptions,
    boot_ts_nanos: Option<i64>,
}

/// Converts from UTC time to monotonic time.
fn utc_to_monotonic(boot_ts: i64, utc: i64) -> Timestamp {
    Timestamp::from(utc - boot_ts)
}

#[async_trait(?Send)]
impl<W> LogFormatter for DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    async fn push_log(&mut self, log_entry: LogEntry) -> Result<LogProcessingResult, LogError> {
        if self.filter_by_timestamp(&log_entry, self.options.since.as_ref(), |a, b| a <= b) {
            return Ok(LogProcessingResult::Continue);
        }

        if self.filter_by_timestamp(&log_entry, self.options.until.as_ref(), |a, b| a >= b) {
            return Ok(LogProcessingResult::Exit);
        }

        if !self.filters.matches(&log_entry) {
            return Ok(LogProcessingResult::Continue);
        }
        match self.options.display {
            Some(text_options) => {
                let mut options_for_this_line_only = self.options.clone();
                options_for_this_line_only.display = Some(text_options);
                self.format_text_log(options_for_this_line_only, log_entry)?;
            }
            None => {
                match log_entry {
                    _ => {}
                }
                self.writer.item(&log_entry).map_err(|err| LogError::UnknownError(err.into()))?;
            }
        };

        Ok(LogProcessingResult::Continue)
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
        self.boot_ts_nanos = Some(boot_ts_nanos);
    }
    fn get_boot_timestamp(&self) -> i64 {
        debug_assert!(self.boot_ts_nanos.is_some());
        self.boot_ts_nanos.unwrap_or(0)
    }
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
    /// Creates a new DefaultLogFormatter with the given writer and options.
    pub fn new(filters: LogFilterCriteria, writer: W, options: LogFormatterOptions) -> Self {
        Self { filters, writer, options, boot_ts_nanos: None }
    }

    /// Creates a new DefaultLogFormatter from command-line arguments.
    pub fn new_from_args(cmd: &LogCommand, writer: W) -> Self {
        let is_json = writer.is_machine();
        let formatter = DefaultLogFormatter::new(
            LogFilterCriteria::try_from(cmd.clone()).unwrap(),
            writer,
            LogFormatterOptions {
                display: if is_json {
                    None
                } else {
                    Some(LogTextDisplayOptions {
                        show_tags: !cmd.hide_tags,
                        color: if cmd.no_color {
                            LogTextColor::None
                        } else {
                            LogTextColor::BySeverity
                        },
                        show_metadata: cmd.show_metadata,
                        time_format: match cmd.clock {
                            TimeFormat::Monotonic => LogTimeDisplayFormat::Original,
                            TimeFormat::Local => LogTimeDisplayFormat::WallTime {
                                tz: Timezone::Local,
                                // This will receive a correct value when logging actually starts,
                                // see `set_boot_timestamp()` method on the log formatter.
                                offset: 0,
                            },
                            TimeFormat::Utc => LogTimeDisplayFormat::WallTime {
                                tz: Timezone::Utc,
                                // This will receive a correct value when logging actually starts,
                                // see `set_boot_timestamp()` method on the log formatter.
                                offset: 0,
                            },
                        },
                        show_file: !cmd.hide_file,
                        show_full_moniker: cmd.show_full_moniker,
                        ..Default::default()
                    })
                },
                since: DeviceOrLocalTimestamp::new(
                    cmd.since.as_ref(),
                    cmd.since_monotonic.as_ref(),
                ),
                until: DeviceOrLocalTimestamp::new(
                    cmd.until.as_ref(),
                    cmd.until_monotonic.as_ref(),
                ),
            },
        );
        formatter
    }

    fn filter_by_timestamp(
        &self,
        log_entry: &LogEntry,
        timestamp: Option<&DeviceOrLocalTimestamp>,
        callback: impl Fn(&Timestamp, &Timestamp) -> bool,
    ) -> bool {
        let Some(timestamp) = timestamp else {
            return false;
        };
        if timestamp.is_monotonic {
            callback(
                &utc_to_monotonic(self.get_boot_timestamp(), *log_entry.timestamp),
                &timestamp.timestamp,
            )
        } else {
            callback(&log_entry.timestamp, &timestamp.timestamp)
        }
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
                // TODO(https://fxbug.dev/42072442): Add support for log spam redaction and other
                // features listed in the design doc.
                writeln!(self.writer, "{}", LogTextPresenter::new(&data, text_options))?;
            }
        })
    }
}

/// Symbolizer that does nothing.
pub struct NoOpSymbolizer;

#[async_trait(?Send)]
impl Symbolize for NoOpSymbolizer {
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry> {
        Some(entry)
    }
}

/// Trait for formatting logs one at a time.
#[async_trait(?Send)]
pub trait LogFormatter {
    /// Formats a log entry and writes it to the output.
    async fn push_log(&mut self, log_entry: LogEntry) -> Result<LogProcessingResult, LogError>;
}

#[cfg(test)]
mod test {
    use crate::parse_time;
    use assert_matches::assert_matches;
    use diagnostics_data::{LogsDataBuilder, Severity};
    use ffx_writer::{Format, MachineWriter, TestBuffers};
    use std::cell::Cell;

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
        async fn push_log(&mut self, log_entry: LogEntry) -> Result<LogProcessingResult, LogError> {
            self.logs.push(log_entry);
            Ok(LogProcessingResult::Continue)
        }
    }

    /// Symbolizer that prints "Fuchsia".
    pub struct FakeFuchsiaSymbolizer;

    fn set_log_msg(entry: &mut LogEntry, msg: impl Into<String>) {
        *entry.data.as_target_log_mut().unwrap().msg_mut().unwrap() = msg.into();
    }

    #[async_trait(?Send)]
    impl Symbolize for FakeFuchsiaSymbolizer {
        async fn symbolize(&self, mut entry: LogEntry) -> Option<LogEntry> {
            set_log_msg(&mut entry, "Fuchsia");
            Some(entry)
        }
    }

    struct FakeSymbolizerCallback {
        should_discard: Cell<bool>,
    }

    impl FakeSymbolizerCallback {
        fn new() -> Self {
            Self { should_discard: Cell::new(true) }
        }
    }

    #[async_trait(?Send)]
    impl Symbolize for FakeSymbolizerCallback {
        async fn symbolize(&self, mut input: LogEntry) -> Option<LogEntry> {
            self.should_discard.set(!self.should_discard.get());
            if self.should_discard.get() {
                None
            } else {
                set_log_msg(&mut input, "symbolized log");
                Some(input)
            }
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

        // Boot timestamp is supported when using JSON output (for filtering)
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), output, options);
        formatter.set_boot_timestamp(1234);
        assert_eq!(formatter.get_boot_timestamp(), 1234);
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
            fuchsia_async::Socket::from_socket(receiver),
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
        .set_pid(1)
        .set_tid(2)
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
            fuchsia_async::Socket::from_socket(receiver),
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

    #[fuchsia::test]
    async fn test_format_timestamp_filter() {
        // test since and until args for the LogFormatter
        let symbolizer = NoOpSymbolizer {};
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut formatter = DefaultLogFormatter::new(
            LogFilterCriteria::default(),
            stdout,
            LogFormatterOptions {
                since: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from(1),
                    is_monotonic: true,
                }),
                until: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from(3),
                    is_monotonic: true,
                }),
                ..Default::default()
            },
        );
        formatter.set_boot_timestamp(0);

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
        let target_log_2 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(2),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_pid(1)
        .set_tid(2)
        .set_message("Hello world 3!")
        .build();
        let target_log_3 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(3),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world 4!")
        .set_pid(1)
        .set_tid(2)
        .build();
        sender
            .write(
                serde_json::to_string(&vec![
                    &target_log_0,
                    &target_log_1,
                    &target_log_2,
                    &target_log_3,
                ])
                .unwrap()
                .as_bytes(),
            )
            .expect("failed to write target log");
        drop(sender);
        assert_matches!(
            dump_logs_from_socket(
                fuchsia_async::Socket::from_socket(receiver),
                &mut formatter,
                &symbolizer,
            )
            .await,
            Ok(LogProcessingResult::Exit)
        );
        assert_eq!(
            buffers.stdout.into_string(),
            "[00000.000000][1][2][ffx] INFO: Hello world 3!\n"
        );
    }

    fn make_log_with_timestamp(timestamp: i32) -> LogsData {
        LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(timestamp),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message(format!("Hello world {timestamp}!"))
        .set_pid(1)
        .set_tid(2)
        .build()
    }

    #[fuchsia::test]
    async fn test_format_timestamp_filter_utc() {
        // test since and until args for the LogFormatter
        let symbolizer = NoOpSymbolizer {};
        let buffers = TestBuffers::default();
        let stdout = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut formatter = DefaultLogFormatter::new(
            LogFilterCriteria::default(),
            stdout,
            LogFormatterOptions {
                since: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from(1),
                    is_monotonic: false,
                }),
                until: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from(3),
                    is_monotonic: false,
                }),
                display: Some(LogTextDisplayOptions {
                    time_format: LogTimeDisplayFormat::WallTime { tz: Timezone::Utc, offset: 1 },
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        formatter.set_boot_timestamp(1);

        let (sender, receiver) = fuchsia_zircon::Socket::create_stream();
        let logs = (0..4).map(|i| make_log_with_timestamp(i)).collect::<Vec<_>>();
        sender
            .write(serde_json::to_string(&logs).unwrap().as_bytes())
            .expect("failed to write target log");
        drop(sender);
        assert_matches!(
            dump_logs_from_socket(
                fuchsia_async::Socket::from_socket(receiver),
                &mut formatter,
                &symbolizer,
            )
            .await,
            Ok(LogProcessingResult::Exit)
        );
        assert_eq!(
            buffers.stdout.into_string(),
            "[1970-01-01 00:00:00.000][1][2][ffx] INFO: Hello world 1!\n"
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
            buffers.into_stdout_str(),
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
            buffers.into_stdout_str(),
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
            serde_json::from_str::<LogEntry>(&buffers.into_stdout_str()).unwrap(),
            log_entry()
        );
    }

    fn emit_log(
        sender: &mut fuchsia_zircon::Socket,
        msg: &str,
        timestamp_nanos: i32,
    ) -> Data<Logs> {
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(timestamp_nanos),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message(msg)
        .build();

        sender
            .write(serde_json::to_string(&target_log).unwrap().as_bytes())
            .expect("failed to write target log");
        target_log
    }

    #[fuchsia::test]
    async fn test_default_formatter_discards_when_told_by_symbolizer() {
        let mut formatter = FakeFormatter::new();
        let (mut sender, receiver) = fuchsia_zircon::Socket::create_stream();
        let mut target_log_0 = emit_log(&mut sender, "Hello world!", 0);
        emit_log(&mut sender, "Dropped world!", 1);
        let mut target_log_2 = emit_log(&mut sender, "Hello world!", 2);
        emit_log(&mut sender, "Dropped world!", 3);
        let mut target_log_4 = emit_log(&mut sender, "Hello world!", 4);
        drop(sender);
        // Drop every other log.
        let symbolizer = FakeSymbolizerCallback::new();
        *target_log_0.msg_mut().unwrap() = "symbolized log".into();
        *target_log_2.msg_mut().unwrap() = "symbolized log".into();
        *target_log_4.msg_mut().unwrap() = "symbolized log".into();
        dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver),
            &mut formatter,
            &symbolizer,
        )
        .await
        .unwrap();
        assert_eq!(
            formatter.logs,
            vec![
                LogEntry { data: LogData::TargetLog(target_log_0), timestamp: Timestamp::from(0) },
                LogEntry { data: LogData::TargetLog(target_log_2), timestamp: Timestamp::from(2) },
                LogEntry { data: LogData::TargetLog(target_log_4), timestamp: Timestamp::from(4) }
            ],
        );
    }

    #[fuchsia::test]
    async fn test_symbolized_output() {
        let symbolizer = FakeFuchsiaSymbolizer;
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut formatter = DefaultLogFormatter::new(
            LogFilterCriteria::default(),
            output,
            LogFormatterOptions { ..Default::default() },
        );
        formatter.set_boot_timestamp(0);
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".into(),
            timestamp_nanos: Timestamp::from(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_pid(1)
        .set_tid(2)
        .set_message("Hello world!")
        .build();
        let (sender, receiver) = fuchsia_zircon::Socket::create_stream();
        sender
            .write(serde_json::to_string(&target_log).unwrap().as_bytes())
            .expect("failed to write target log");
        drop(sender);
        dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver),
            &mut formatter,
            &symbolizer,
        )
        .await
        .unwrap();
        assert_eq!(buffers.stdout.into_string(), "[00000.000000][1][2][ffx] INFO: Fuchsia\n");
    }

    #[test]
    fn test_device_or_local_timestamp_returns_none_if_now_is_passed() {
        assert_matches!(DeviceOrLocalTimestamp::new(Some(&parse_time("now").unwrap()), None), None);
    }
}
