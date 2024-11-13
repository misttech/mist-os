// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::filter::LogFilterCriteria;
use crate::log_socket_stream::{JsonDeserializeError, LogsDataStream};
use crate::{DetailedDateTime, LogCommand, LogError, LogProcessingResult, TimeFormat};
use anyhow::Result;
use async_trait::async_trait;
use diagnostics_data::{
    Data, LogTextColor, LogTextDisplayOptions, LogTextPresenter, LogTimeDisplayFormat, Logs,
    LogsData, LogsDataBuilder, LogsField, LogsProperty, Severity, Timezone,
};
use ffx_writer::ToolIO;
use futures_util::future::Either;
use futures_util::stream::FuturesUnordered;
use futures_util::{select, StreamExt};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::io::Write;
use std::time::Duration;
use thiserror::Error;

pub use diagnostics_data::Timestamp;

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
    #[serde(
        serialize_with = "diagnostics_data::serialize_timestamp",
        deserialize_with = "diagnostics_data::deserialize_timestamp"
    )]
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

async fn handle_value<S>(one: Data<Logs>, boot_ts: Timestamp, symbolizer: &S) -> Option<LogEntry>
where
    S: Symbolize + ?Sized,
{
    let entry = LogEntry {
        timestamp: Timestamp::from_nanos(
            one.metadata.timestamp.into_nanos() + boot_ts.into_nanos(),
        ),
        data: one.into(),
    };
    symbolizer.symbolize(entry).await
}

fn generate_timestamp_message(boot_timestamp: Timestamp) -> LogEntry {
    LogEntry {
        data: LogData::TargetLog(
            LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                moniker: "ffx".try_into().unwrap(),
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("ffx".into()),
                severity: Severity::Info,
            })
            .set_message("Logging started")
            .add_key(LogsProperty::String(
                LogsField::Other("utc_time_now".into()),
                chrono::Utc::now().to_rfc3339(),
            ))
            .add_key(LogsProperty::Int(
                LogsField::Other("current_boot_timestamp".to_string()),
                boot_timestamp.into_nanos(),
            ))
            .build(),
        ),
        timestamp: Timestamp::from_nanos(0),
    }
}

/// Reads logs from a socket and formats them using the given formatter and symbolizer.
pub async fn dump_logs_from_socket<F, S>(
    socket: fuchsia_async::Socket,
    formatter: &mut F,
    symbolizer: &S,
    include_timestamp: bool,
) -> Result<LogProcessingResult, JsonDeserializeError>
where
    F: LogFormatter + BootTimeAccessor,
    S: Symbolize + ?Sized,
{
    let boot_ts = formatter.get_boot_timestamp();
    let mut decoder = Box::pin(LogsDataStream::new(socket).fuse());
    let mut symbolize_pending = FuturesUnordered::new();
    if include_timestamp && !formatter.is_utc_time_format() {
        formatter.push_log(generate_timestamp_message(formatter.get_boot_timestamp())).await?;
    }
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
    fn set_boot_timestamp(&mut self, _boot_ts_nanos: Timestamp);

    /// Returns the boot timestamp in nanoseconds since the Unix epoch.
    fn get_boot_timestamp(&self) -> Timestamp;
}

/// Timestamp filter which is either either boot-based or UTC-based.
#[derive(Clone, Debug)]
pub struct DeviceOrLocalTimestamp {
    /// Timestamp in boot time
    pub timestamp: Timestamp,
    /// True if this filter should be applied to boot time,
    /// false if UTC time.
    pub is_boot: bool,
}

impl DeviceOrLocalTimestamp {
    /// Creates a DeviceOrLocalTimestamp from a real-time date/time or
    /// a boot date/time. Returns None if both rtc and boot are None.
    /// Returns None if the timestamp is "now".
    pub fn new(
        rtc: Option<&DetailedDateTime>,
        boot: Option<&Duration>,
    ) -> Option<DeviceOrLocalTimestamp> {
        rtc.as_ref()
            .filter(|value| !value.is_now)
            .map(|value| DeviceOrLocalTimestamp {
                timestamp: Timestamp::from_nanos(value.naive_utc().timestamp_nanos_opt().unwrap()),
                is_boot: false,
            })
            .or_else(|| {
                boot.map(|value| DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from_nanos(value.as_nanos() as i64),
                    is_boot: true,
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
    boot_ts_nanos: Option<Timestamp>,
}

/// Converts from UTC time to boot time.
fn utc_to_boot(boot_ts: Timestamp, utc: i64) -> Timestamp {
    Timestamp::from_nanos(utc - boot_ts.into_nanos())
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
                self.writer.item(&log_entry).map_err(|err| LogError::UnknownError(err.into()))?;
            }
        };

        Ok(LogProcessingResult::Continue)
    }

    fn is_utc_time_format(&self) -> bool {
        self.options.display.iter().any(|options| match options.time_format {
            LogTimeDisplayFormat::Original => false,
            LogTimeDisplayFormat::WallTime { tz, offset: _ } => tz == Timezone::Utc,
        })
    }
}

impl<W> BootTimeAccessor for DefaultLogFormatter<W>
where
    W: Write + ToolIO<OutputItem = LogEntry>,
{
    fn set_boot_timestamp(&mut self, boot_ts_nanos: Timestamp) {
        if let Some(LogTextDisplayOptions {
            time_format: LogTimeDisplayFormat::WallTime { ref mut offset, .. },
            ..
        }) = &mut self.options.display
        {
            *offset = boot_ts_nanos.into_nanos();
        }
        self.boot_ts_nanos = Some(boot_ts_nanos);
    }
    fn get_boot_timestamp(&self) -> Timestamp {
        debug_assert!(self.boot_ts_nanos.is_some());
        self.boot_ts_nanos.unwrap_or_else(|| Timestamp::from_nanos(0))
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
            LogFilterCriteria::from(cmd.clone()),
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
                            TimeFormat::Boot => LogTimeDisplayFormat::Original,
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
                    })
                },
                since: DeviceOrLocalTimestamp::new(cmd.since.as_ref(), cmd.since_boot.as_ref()),
                until: DeviceOrLocalTimestamp::new(cmd.until.as_ref(), cmd.until_boot.as_ref()),
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
        if timestamp.is_boot {
            callback(
                &utc_to_boot(self.get_boot_timestamp(), log_entry.timestamp.into_nanos()),
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
        match log_entry {
            LogEntry { data: LogData::TargetLog(data), .. } => {
                // TODO(https://fxbug.dev/42072442): Add support for log spam redaction and other
                // features listed in the design doc.
                writeln!(self.writer, "{}", LogTextPresenter::new(&data, text_options))?;
            }
        }
        Ok(())
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

    /// Returns true if the formatter is configured to output in UTC time format.
    fn is_utc_time_format(&self) -> bool;
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
        boot_timestamp: Timestamp,
        is_utc_time_format: bool,
    }

    impl FakeFormatter {
        fn new() -> Self {
            Self {
                logs: Vec::new(),
                boot_timestamp: Timestamp::from_nanos(0),
                is_utc_time_format: false,
            }
        }
    }

    impl BootTimeAccessor for FakeFormatter {
        fn set_boot_timestamp(&mut self, boot_ts_nanos: Timestamp) {
            self.boot_timestamp = boot_ts_nanos;
        }

        fn get_boot_timestamp(&self) -> Timestamp {
            self.boot_timestamp
        }
    }

    #[async_trait(?Send)]
    impl LogFormatter for FakeFormatter {
        async fn push_log(&mut self, log_entry: LogEntry) -> Result<LogProcessingResult, LogError> {
            self.logs.push(log_entry);
            Ok(LogProcessingResult::Continue)
        }

        fn is_utc_time_format(&self) -> bool {
            self.is_utc_time_format
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

    async fn dump_logs_from_socket<F, S>(
        socket: fuchsia_async::Socket,
        formatter: &mut F,
        symbolizer: &S,
    ) -> Result<LogProcessingResult, JsonDeserializeError>
    where
        F: LogFormatter + BootTimeAccessor,
        S: Symbolize + ?Sized,
    {
        super::dump_logs_from_socket(socket, formatter, symbolizer, false).await
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
        formatter.set_boot_timestamp(Timestamp::from_nanos(1234));
        assert_eq!(formatter.get_boot_timestamp(), Timestamp::from_nanos(1234));

        // Boot timestamp is supported when using JSON output (for filtering)
        let buffers = TestBuffers::default();
        let output = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let options = LogFormatterOptions { display: None, ..Default::default() };
        let mut formatter = DefaultLogFormatter::new(LogFilterCriteria::default(), output, options);
        formatter.set_boot_timestamp(Timestamp::from_nanos(1234));
        assert_eq!(formatter.get_boot_timestamp(), Timestamp::from_nanos(1234));
    }

    #[fuchsia::test]
    async fn test_format_single_message() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world!")
        .build();
        let (sender, receiver) = zx::Socket::create_stream();
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
            vec![LogEntry {
                data: LogData::TargetLog(target_log),
                timestamp: Timestamp::from_nanos(0)
            }]
        );
    }

    #[fuchsia::test]
    async fn test_format_utc_timestamp() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        formatter.set_boot_timestamp(Timestamp::from_nanos(DEFAULT_TS_NANOS as i64));
        let (_, receiver) = zx::Socket::create_stream();
        super::dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver),
            &mut formatter,
            &symbolizer,
            true,
        )
        .await
        .unwrap();
        let target_log = formatter.logs[0].data.as_target_log().unwrap();
        let properties = target_log.payload_keys().unwrap();
        assert_eq!(target_log.msg().unwrap(), "Logging started");

        // Ensure the end has a valid timestamp
        chrono::DateTime::parse_from_rfc3339(
            properties.get_property("utc_time_now").unwrap().string().unwrap(),
        )
        .unwrap();
        assert_eq!(
            properties.get_property("current_boot_timestamp").unwrap().int().unwrap(),
            DEFAULT_TS_NANOS as i64
        );
    }

    #[fuchsia::test]
    async fn test_format_utc_timestamp_does_not_print_if_utc_time() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        formatter.is_utc_time_format = true;
        formatter.set_boot_timestamp(Timestamp::from_nanos(DEFAULT_TS_NANOS as i64));
        let (_, receiver) = zx::Socket::create_stream();
        super::dump_logs_from_socket(
            fuchsia_async::Socket::from_socket(receiver),
            &mut formatter,
            &symbolizer,
            true,
        )
        .await
        .unwrap();
        assert_eq!(formatter.logs.len(), 0);
    }

    #[fuchsia::test]
    async fn test_format_multiple_messages() {
        let symbolizer = NoOpSymbolizer {};
        let mut formatter = FakeFormatter::new();
        let (sender, receiver) = zx::Socket::create_stream();
        let target_log_0 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world!")
        .set_pid(1)
        .set_tid(2)
        .build();
        let target_log_1 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(1),
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
                LogEntry {
                    data: LogData::TargetLog(target_log_0),
                    timestamp: Timestamp::from_nanos(0)
                },
                LogEntry {
                    data: LogData::TargetLog(target_log_1),
                    timestamp: Timestamp::from_nanos(1)
                }
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
                    timestamp: Timestamp::from_nanos(1),
                    is_boot: true,
                }),
                until: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from_nanos(3),
                    is_boot: true,
                }),
                ..Default::default()
            },
        );
        formatter.set_boot_timestamp(Timestamp::from_nanos(0));

        let (sender, receiver) = zx::Socket::create_stream();
        let target_log_0 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world!")
        .build();
        let target_log_1 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(1),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_message("Hello world 2!")
        .build();
        let target_log_2 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(2),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_pid(1)
        .set_tid(2)
        .set_message("Hello world 3!")
        .build();
        let target_log_3 = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(3),
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

    fn make_log_with_timestamp(timestamp: i64) -> LogsData {
        LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(timestamp),
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
                    timestamp: Timestamp::from_nanos(1),
                    is_boot: false,
                }),
                until: Some(DeviceOrLocalTimestamp {
                    timestamp: Timestamp::from_nanos(3),
                    is_boot: false,
                }),
                display: Some(LogTextDisplayOptions {
                    time_format: LogTimeDisplayFormat::WallTime { tz: Timezone::Utc, offset: 1 },
                    ..Default::default()
                }),
            },
        );
        formatter.set_boot_timestamp(Timestamp::from_nanos(1));

        let (sender, receiver) = zx::Socket::create_stream();
        let logs = (0..4).map(make_log_with_timestamp).collect::<Vec<_>>();
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
            timestamp: Timestamp::from_nanos(default_ts().as_nanos() as i64),
            component_url: Some("component_url".into()),
            moniker: "some/moniker".try_into().unwrap(),
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
            timestamp: Timestamp::from_nanos(0),
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
        let options = LogFormatterOptions {
            display: Some(LogTextDisplayOptions { show_metadata: false, ..Default::default() }),
            ..LogFormatterOptions::default()
        };
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

    fn emit_log(sender: &mut zx::Socket, msg: &str, timestamp: i64) -> Data<Logs> {
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(timestamp),
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
        let (mut sender, receiver) = zx::Socket::create_stream();
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
                LogEntry {
                    data: LogData::TargetLog(target_log_0),
                    timestamp: Timestamp::from_nanos(0)
                },
                LogEntry {
                    data: LogData::TargetLog(target_log_2),
                    timestamp: Timestamp::from_nanos(2)
                },
                LogEntry {
                    data: LogData::TargetLog(target_log_4),
                    timestamp: Timestamp::from_nanos(4)
                }
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
        formatter.set_boot_timestamp(Timestamp::from_nanos(0));
        let target_log = LogsDataBuilder::new(diagnostics_data::BuilderArgs {
            moniker: "ffx".try_into().unwrap(),
            timestamp: Timestamp::from_nanos(0),
            component_url: Some("ffx".into()),
            severity: Severity::Info,
        })
        .set_pid(1)
        .set_tid(2)
        .set_message("Hello world!")
        .build();
        let (sender, receiver) = zx::Socket::create_stream();
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
