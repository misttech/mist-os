// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod error;

use anyhow::{anyhow, Context, Error, Result};
use async_trait::async_trait;
use blocking::Unblock;
use chrono::{Local, TimeZone};
use diagnostics_data::{
    LogTextColor, LogTextDisplayOptions, LogTextPresenter, LogTimeDisplayFormat, LogsData,
    Severity, Timestamp, Timezone,
};
use error::LogError;
use errors::{ffx_bail, ffx_error};
use ffx_config::{get, keys::TARGET_DEFAULT_KEY};
use ffx_core::macro_deps::fidl::endpoints::create_proxy;
use ffx_log_args::{DumpCommand, LogCommand, LogSubCommand, TimeFormat, WatchCommand};
use ffx_log_data::{EventType, LogData, LogEntry};
use ffx_log_frontend::{exec_log_cmd, LogCommandParameters, LogFormatter};
use fho::{daemon_protocol, deferred, moniker, Deferred, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_ffx::{
    DiagnosticsProxy, StreamMode, TargetCollectionProxy, TargetQuery, TimeBound,
};
use fidl_fuchsia_developer_remotecontrol::{ArchiveIteratorError, RemoteControlProxy};
use fidl_fuchsia_diagnostics::LogSettingsProxy;
use futures::{AsyncWrite, AsyncWriteExt};
use moniker::Moniker;
use std::{fs, iter::Iterator, str::FromStr, sync::Arc, time::SystemTime};

use ffx_log_frontend::{RemoteDiagnosticsBridgeProxyWrapper, StreamDiagnostics};
use ffx_tool_log_ng::spam_filter;

type ArchiveIteratorResult = Result<LogEntry, ArchiveIteratorError>;
const COLOR_CONFIG_NAME: &str = "log_cmd.color";
const LOG_SPAM_CONFIG_NAME: &str = "log_cmd.spam_filepath";
const SYMBOLIZE_ENABLED_CONFIG: &str = "proactive_log.symbolize.enabled";
const NANOS_IN_SECOND: i64 = 1_000_000_000;
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S.%3f";
const STREAM_TARGET_CHOICE_HELP: &str = "Unable to connect to any target. There must be a target connected to stream logs.

If you expect a target to be connected, verify that it is listed in `ffx target list`. If it remains disconnected, try running `ffx doctor`.

Alternatively, you can dump historical logs from a target using `ffx [--target <nodename or IP>] log dump`.";

const DUMP_TARGET_CHOICE_HELP: &str = "There is no target connected and there is no default target set.

To view logs for an offline target, provide a target explicitly using `ffx --target <nodename or IP> log dump`, \
or set a default with `ffx target default set <nodename or IP>` and try again.

Alternatively, if you expected a target to be connected, verify that it is listed in `ffx target list`. If it remains disconnected, try running `ffx doctor`.";

const SELECT_FAILURE_MESSAGE: &str = "--select was provided, but ffx could not get a proxy to the LogSettings service.

Confirm that your chosen target is online with `ffx target list`. Note that you cannot use `--select` with an offline target.";
pub enum ColorOverride {
    SpamHighlight,
}

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

struct LogFilterCriteria {
    min_severity: Severity,
    filters: Vec<String>,
    moniker_filters: Vec<String>,
    excludes: Vec<String>,
    tags: Vec<String>,
    exclude_tags: Vec<String>,
    kernel: bool,
    spam_filter: Option<Box<dyn spam_filter::LogSpamFilter>>,
}

impl Default for LogFilterCriteria {
    fn default() -> Self {
        Self {
            min_severity: Severity::Info,
            filters: vec![],
            excludes: vec![],
            tags: vec![],
            moniker_filters: vec![],
            exclude_tags: vec![],
            kernel: false,
            spam_filter: None,
        }
    }
}

impl LogFilterCriteria {
    fn new(
        min_severity: Severity,
        filters: Vec<String>,
        moniker_filters: Vec<String>,
        excludes: Vec<String>,
        tags: Vec<String>,
        exclude_tags: Vec<String>,
        kernel: bool,
        spam_filter: Option<Box<dyn spam_filter::LogSpamFilter>>,
    ) -> Self {
        Self {
            min_severity: min_severity,
            filters,
            moniker_filters,
            excludes,
            tags,
            exclude_tags,
            kernel,
            spam_filter,
        }
    }

    fn try_from_cmd(
        cmd: &LogCommand,
        default_log_spam_path: Option<String>,
    ) -> Result<Self, Error> {
        let mut spam_filter: Option<Box<dyn spam_filter::LogSpamFilter>> = None;
        if !cmd.disable_spam_filter {
            let spam_list_path: Option<String> = match cmd.spam_list_path.as_ref() {
                Some(path) => Some(path.to_string()),
                None => default_log_spam_path,
            };
            if let Some(spam_list_path) = spam_list_path.as_ref() {
                tracing::info!(
                    "Filtering Log spam based on spam definition file {:?}",
                    spam_list_path
                );
                let contents = fs::read_to_string(spam_list_path)
                    .context(format!("Failed to read log spam JSON: {:?}", spam_list_path))?;
                let spam_list: spam_filter::LogSpamList = serde_json::from_str(&contents)
                    .context(format!("Failed to parse log spam JSON: {:?}", spam_list_path))?;
                spam_filter = Some(Box::new(spam_filter::LogSpamFilterImpl::new(spam_list)))
            }
        }

        Ok(Self::new(
            cmd.severity,
            cmd.filter.clone(),
            cmd.moniker.clone(),
            cmd.exclude.clone(),
            cmd.tags.clone(),
            cmd.exclude_tags.clone(),
            cmd.kernel,
            spam_filter,
        ))
    }

    fn data_matches_spam(&self, data: &LogsData, msg: &str) -> bool {
        match &self.spam_filter {
            None => false,
            Some(f) => f.is_spam(
                data.metadata.file.as_ref().map(|value| value.as_str()),
                data.metadata.line,
                msg,
            ),
        }
    }

    fn matches_filter_string(filter_string: &str, message: &str, log: &LogsData) -> bool {
        let filter_moniker = Moniker::from_str(filter_string);
        let moniker = Moniker::from_str(log.moniker.as_str());
        return message.contains(filter_string)
            || log.moniker.contains(filter_string)
            || moniker.clone().map(|m| m.to_string().contains(filter_string)).unwrap_or(false)
            || match (moniker, filter_moniker) {
                (Ok(m), Ok(f)) => m.to_string().contains(&f.to_string()),
                _ => false,
            }
            || log.metadata.component_url.as_ref().map_or(false, |s| s.contains(filter_string));
    }

    fn matches_filter_by_moniker_string(filter_string: &str, log: &LogsData) -> bool {
        let filter_moniker = Moniker::from_str(filter_string);
        let moniker = Moniker::from_str(log.moniker.as_str());
        matches!((moniker, filter_moniker), (Ok(a), Ok(b)) if a == b)
    }

    fn match_filters_to_event(&self) -> bool {
        self.moniker_filters.is_empty()
    }

    fn match_filters_to_log_data(&self, data: &LogsData, msg: &str) -> bool {
        if data.metadata.severity < self.min_severity {
            return false;
        }

        if self.kernel && data.moniker != "klog" {
            return false;
        }

        if !self.moniker_filters.is_empty()
            && !self
                .moniker_filters
                .iter()
                .any(|f| Self::matches_filter_by_moniker_string(f, &data))
        {
            return false;
        }

        if !self.filters.is_empty()
            && !self.filters.iter().any(|f| Self::matches_filter_string(f, msg, &data))
        {
            return false;
        }

        if self.excludes.iter().any(|f| Self::matches_filter_string(f, msg, &data)) {
            return false;
        }

        if !self.tags.is_empty()
            && !self.tags.iter().any(|f| data.tags().map(|t| t.contains(f)).unwrap_or(false))
        {
            return false;
        }

        if self.exclude_tags.iter().any(|f| data.tags().map(|t| t.contains(f)).unwrap_or(false)) {
            return false;
        }

        true
    }

    fn matches_filters_to_log_entry(&self, entry: &LogEntry) -> bool {
        match entry {
            LogEntry { data: LogData::TargetLog(data), .. } => {
                self.match_filters_to_log_data(data, data.msg().unwrap_or(""))
            }
            LogEntry { data: LogData::SymbolizedTargetLog(data, message), .. } => {
                self.match_filters_to_log_data(data, message)
            }
            LogEntry { data: LogData::FfxEvent(_), .. } => self.match_filters_to_event(),
            _ => true,
        }
    }

    fn matches_spam(&self, entry: &LogEntry) -> bool {
        match entry {
            LogEntry { data: LogData::TargetLog(data), .. } => {
                self.data_matches_spam(data, data.msg().unwrap_or(""))
            }
            LogEntry { data: LogData::SymbolizedTargetLog(data, message), .. } => {
                self.data_matches_spam(data, message)
            }
            _ => false,
        }
    }
}

/// display options
#[derive(Clone, Debug)]
pub enum DisplayOption {
    Text(LogTextDisplayOptions),
    Json,
}

/// additional log options
#[derive(Default)]
pub struct LogOpts {
    /// true if machine output (JSON) is enabled
    is_machine: bool,
}

#[derive(Clone, Debug)]
pub struct LogFormatterOptions {
    display: DisplayOption,
    raw: bool,
    highlight_spam: bool,
}

pub struct DefaultLogFormatter<'a> {
    writer: Box<dyn AsyncWrite + Unpin + 'a>,
    has_previous_log: bool,
    filters: LogFilterCriteria,
    options: LogFormatterOptions,
}

#[async_trait(?Send)]
impl<'a> LogFormatter for DefaultLogFormatter<'_> {
    fn set_boot_timestamp(&mut self, boot_ts_nanos: i64) {
        match &mut self.options.display {
            DisplayOption::Text(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { ref mut offset, .. },
                ..
            }) => {
                *offset = boot_ts_nanos;
            }
            _ => (),
        }
    }
    async fn push_log(&mut self, log_entry_result: ArchiveIteratorResult) -> Result<()> {
        let mut s = match log_entry_result {
            Ok(log_entry) => {
                let is_spam = self.filters.matches_spam(&log_entry);

                if (!self.options.highlight_spam && is_spam)
                    || !self.filters.matches_filters_to_log_entry(&log_entry)
                {
                    return Ok(());
                }

                match self.options.display.clone() {
                    // If spam highlighting is required we'll need to update these options but only
                    // for this particular message. Make a copy of the options to use in local
                    // formatting calls.
                    DisplayOption::Text(mut text_options) => {
                        if self.options.highlight_spam && is_spam {
                            text_options.color = LogTextColor::Highlight;
                        }
                        let mut options_for_this_line_only = self.options.clone();
                        options_for_this_line_only.display = DisplayOption::Text(text_options);
                        match Self::format_text_log(
                            options_for_this_line_only,
                            log_entry,
                            self.has_previous_log,
                        )? {
                            Some(s) => s,
                            None => return Ok(()),
                        }
                    }
                    DisplayOption::Json => {
                        match log_entry {
                            LogEntry {
                                data: LogData::SymbolizedTargetLog(_, ref symbolized),
                                ..
                            } => {
                                if !self.options.raw && symbolized.is_empty() {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        }
                        serde_json::to_string(&log_entry)?
                    }
                }
            }
            Err(e) => format!("got an error fetching next log: {:?}", e),
        };

        s.push('\n');

        self.has_previous_log = true;

        let s = self.writer.write(s.as_bytes());
        s.await.map(|_| ()).map_err(|e| anyhow!(e))
    }
}

impl<'a> DefaultLogFormatter<'a> {
    fn new(
        filters: LogFilterCriteria,
        writer: impl AsyncWrite + Unpin + 'a,
        options: LogFormatterOptions,
    ) -> Self {
        Self { filters, writer: Box::new(writer), has_previous_log: false, options }
    }

    // This function's arguments are copied to make lifetimes in push_log easier since borrowing
    // &self would complicate spam highlighting.
    fn format_text_log(
        options: LogFormatterOptions,
        log_entry: LogEntry,
        has_previous_log: bool,
    ) -> Result<Option<String>, Error> {
        let text_options = match options.display {
            DisplayOption::Text(o) => o,
            DisplayOption::Json => {
                anyhow::bail!("format_text_log called with a json configuration")
            }
        };
        Ok(match log_entry {
            LogEntry { data: LogData::TargetLog(data), .. } => {
                Some(format!("{}", LogTextPresenter::new(&data, text_options)))
            }
            LogEntry { data: LogData::SymbolizedTargetLog(mut data, symbolized), .. } => {
                if !options.raw && symbolized.is_empty() {
                    return Ok(None);
                }
                if !options.raw {
                    *data.msg_mut().expect(
                        "if a symbolized message is provided then the payload has a message",
                    ) = symbolized;
                }

                Some(format!("{}", LogTextPresenter::new(&data, text_options)))
            }
            LogEntry { data: LogData::MalformedTargetLog(raw), .. } => {
                Some(format!("malformed target log: {}", raw))
            }
            LogEntry { data: LogData::FfxEvent(etype), timestamp, .. } => match etype {
                EventType::LoggingStarted => {
                    let mut s = String::from("logger started.");
                    if has_previous_log {
                        s.push_str(" Logs before this may have been dropped if they were not cached on the target. There may be a brief delay while we catch up...");
                    }
                    Some(format_ffx_event(&s, Some(timestamp)))
                }
                EventType::TargetDisconnected => Some(format_ffx_event(
                    "Logger lost connection to target. Retrying...",
                    Some(timestamp),
                )),
            },
        })
    }
}

fn should_color(config_color: bool, cmd_no_color: bool) -> bool {
    if cmd_no_color {
        return false;
    }

    return config_color;
}

async fn print_symbolizer_warning(err: Error) {
    eprintln!(
        "Warning: attempting to get the symbolizer binary failed.
This likely means that your logs will not be symbolized."
    );
    eprintln!("\nThe failure was: {}", err);

    let sdk_type: Result<String, _> = get("sdk.type").await;
    if sdk_type.is_err() || sdk_type.unwrap() == "" {
        eprintln!("If you are working in-tree, ensure that the sdk.type config setting is set accordingly:");
        eprintln!("  ffx config set sdk.type in-tree");
    }
}

type Writer = MachineWriter<Vec<LogEntry>>;

#[derive(FfxTool)]
pub struct LogTool {
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
    #[with(daemon_protocol())]
    diagnostics_proxy: DiagnosticsProxy,
    rcs_proxy: fho::Result<RemoteControlProxy>,
    #[with(deferred(moniker("/bootstrap/archivist")))]
    log_settings: Deferred<LogSettingsProxy>,
    #[command]
    cmd: LogCommand,
}

fho::embedded_plugin!(LogTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for LogTool {
    type Writer = Writer;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        log(
            self.diagnostics_proxy,
            writer,
            self.rcs_proxy.ok(),
            self.log_settings.await.ok(),
            self.target_collection,
            self.cmd,
        )
        .await
        .map_err(Into::into)
    }
}

async fn log_internal(
    diagnostics_proxy: DiagnosticsProxy,
    writer: Writer,
    mut rcs_proxy: Option<RemoteControlProxy>,
    log_settings: Option<LogSettingsProxy>,
    bridge_proxy: TargetCollectionProxy,
    cmd: LogCommand,
) -> Result<(), LogError> {
    // TODO(https://fxbug.dev/125266): Fix coverage bots.
    let proactive_enabled: bool = get::<bool, &str>("proactive_log.enabled").await?;
    if !proactive_enabled {
        if rcs_proxy.is_none() {
            let (client, server) = create_proxy()?;
            bridge_proxy.open_target(&TargetQuery::default(), server).await??;
            let (rcs_client, rcs_server) = create_proxy()?;
            client.open_remote_control(rcs_server).await??;
            rcs_proxy = Some(rcs_client);
        }
        let host =
            rcs_proxy.as_ref().ok_or(LogError::NoRcsProxyAvailable)?.identify_host().await??;
        log_impl(
            Arc::new(RemoteDiagnosticsBridgeProxyWrapper::new(
                bridge_proxy,
                host.nodename.ok_or(LogError::NoHostname)?,
            )),
            rcs_proxy,
            &log_settings,
            cmd,
            &mut std::io::stdout(),
            LogOpts { is_machine: writer.is_machine() },
        )
        .await?;
        Ok(())
    } else {
        // TODO(fxbug.dev/121413): this will be removed as we remove proactive logging.
        log_impl(
            diagnostics_proxy,
            rcs_proxy,
            &log_settings,
            cmd,
            &mut std::io::stdout(),
            LogOpts { is_machine: writer.is_machine() },
        )
        .await?;
        Ok(())
    }
}

async fn log(
    diagnostics_proxy: DiagnosticsProxy,
    writer: Writer,
    rcs_proxy: Option<RemoteControlProxy>,
    log_settings: Option<LogSettingsProxy>,
    bridge_proxy: TargetCollectionProxy,
    cmd: LogCommand,
) -> Result<()> {
    log_internal(diagnostics_proxy, writer, rcs_proxy, log_settings, bridge_proxy, cmd).await?;
    Ok(())
}

pub async fn log_impl<W: std::io::Write>(
    diagnostics_proxy: impl StreamDiagnostics,
    rcs_proxy: Option<RemoteControlProxy>,
    log_settings: &Option<LogSettingsProxy>,
    cmd: LogCommand,
    writer: &mut W,
    opts: LogOpts,
) -> Result<()> {
    let config_color: bool = get(COLOR_CONFIG_NAME).await?;
    let default_log_spam_path = get(LOG_SPAM_CONFIG_NAME).await.ok();

    let mut stdout = Unblock::new(std::io::stdout());
    let mut formatter = DefaultLogFormatter::new(
        LogFilterCriteria::try_from_cmd(&cmd, default_log_spam_path)?,
        &mut stdout,
        LogFormatterOptions {
            raw: cmd.raw,
            display: if opts.is_machine {
                DisplayOption::Json
            } else {
                DisplayOption::Text(LogTextDisplayOptions {
                    show_tags: !cmd.hide_tags,
                    show_file: !cmd.hide_file,
                    color: if should_color(config_color, cmd.no_color) {
                        LogTextColor::BySeverity
                    } else {
                        LogTextColor::None
                    },
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
                    show_metadata: cmd.show_metadata,
                    show_full_moniker: cmd.show_full_moniker,
                })
            },
            highlight_spam: cmd.enable_spam_highlight,
        },
    );

    if get(SYMBOLIZE_ENABLED_CONFIG).await.unwrap_or(true) {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await;
        match sdk {
            Ok(s) => match s.get_host_tool("symbolizer") {
                Err(e) => {
                    print_symbolizer_warning(e).await;
                }
                Ok(_) => {}
            },
            Err(e) => {
                print_symbolizer_warning(e).await;
            }
        };
    }

    log_cmd(diagnostics_proxy, rcs_proxy, &log_settings, &mut formatter, cmd, writer).await
}

async fn log_cmd<W: std::io::Write>(
    diagnostics_proxy: impl StreamDiagnostics,
    rcs_opt: Option<RemoteControlProxy>,
    // NOTE: The fact that this is a reference is load-bearing.
    // It needs to be kept alive to prevent the connection from being dropped
    // and reverting to the previous log settings.
    log_settings: &Option<LogSettingsProxy>,
    log_formatter: &mut impl LogFormatter,
    cmd: LogCommand,
    writer: &mut W,
) -> Result<()> {
    if !cmd.select.is_empty() {
        if let Some(log_settings) = log_settings {
            log_settings
                .set_interest(&cmd.select)
                .await
                .map_err(|e| anyhow!("failed to register log interest selector: {}", e))?;
        } else {
            ffx_bail!("{}", SELECT_FAILURE_MESSAGE);
        }
    }

    let sub_command = cmd.sub_command.unwrap_or(LogSubCommand::Watch(WatchCommand {}));
    let stream_mode = if matches!(sub_command, LogSubCommand::Dump(..)) {
        StreamMode::SnapshotAll
    } else {
        cmd.since
            .as_ref()
            .map(|value| {
                if value.is_now {
                    StreamMode::Subscribe
                } else {
                    StreamMode::SnapshotAllThenSubscribe
                }
            })
            .unwrap_or(StreamMode::SnapshotRecentThenSubscribe)
    };

    let nodename = if let Some(rcs) = rcs_opt {
        let target_info_result = rcs.identify_host().await?;
        let target_info =
            target_info_result.map_err(|e| anyhow!("failed to get target info: {:?}", e))?;
        target_info.nodename.context("missing nodename")?
    } else if let LogSubCommand::Dump(..) = sub_command {
        let default: String = get(TARGET_DEFAULT_KEY)
            .await
            .map_err(|e| ffx_error!("{}\n\nError was: {}", DUMP_TARGET_CHOICE_HELP, e))?;
        if default.is_empty() {
            ffx_bail!("{}", DUMP_TARGET_CHOICE_HELP);
        }

        default
    } else {
        ffx_bail!("{}", STREAM_TARGET_CHOICE_HELP);
    };

    let session = if let LogSubCommand::Dump(DumpCommand { session }) = sub_command {
        Some(session)
    } else {
        None
    };

    if !(cmd.since.is_none() || cmd.since_monotonic.is_none()) {
        ffx_bail!("only one of --from or --from-monotonic may be provided at once.");
    }
    if !(cmd.until.is_none() || cmd.until_monotonic.is_none()) {
        ffx_bail!("only one of --to or --to-monotonic may be provided at once.");
    }

    let from_bound = if let Some(since) = cmd.since {
        Some(TimeBound::Absolute(since.timestamp_nanos() as u64))
    } else if let Some(since_monotonic) = cmd.since_monotonic {
        Some(TimeBound::Monotonic(since_monotonic.as_nanos() as u64))
    } else {
        None
    };
    let to_bound = if let Some(until) = cmd.until {
        Some(TimeBound::Absolute(until.timestamp_nanos() as u64))
    } else if let Some(until_monotonic) = cmd.until_monotonic {
        Some(TimeBound::Monotonic(until_monotonic.as_nanos() as u64))
    } else {
        None
    };

    exec_log_cmd(
        LogCommandParameters {
            target_identifier: nodename,
            session: session,
            from_bound: from_bound,
            to_bound: to_bound,
            stream_mode,
        },
        diagnostics_proxy,
        log_formatter,
        writer,
    )
    .await
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::spam_filter::LogSpamFilter;
    use diagnostics_data::{LogsDataBuilder, LogsField, LogsProperty, Timestamp};
    use errors::ResultExt as _;
    use ffx_log_args::{parse_time, DumpCommand};
    use ffx_log_test_utils::{
        setup_fake_archive_iterator, ArchiveIteratorParameters, FakeArchiveIteratorResponse,
    };
    use fidl_fuchsia_developer_ffx::{
        DaemonDiagnosticsStreamParameters, DiagnosticsRequest, LogSession, SessionSpec,
    };
    use fidl_fuchsia_developer_remotecontrol::{
        ArchiveIteratorError, IdentifyHostResponse, RemoteControlRequest,
    };
    use fidl_fuchsia_diagnostics::{
        Interest, LogInterestSelector, LogSettingsRequest, Severity as FidlSeverity,
    };
    use selectors::{parse_component_selector, VerboseError};
    use std::{cell::Cell, io::Write, sync::Arc, time::Duration};
    use tempfile::NamedTempFile;

    const DEFAULT_TS_NANOS: u64 = 1615535969000000000;
    const BOOT_TS: u64 = 98765432000000000;
    const FAKE_START_TIMESTAMP: i64 = 1614669138;
    const NODENAME: &str = "some-nodename";

    fn default_ts() -> Duration {
        Duration::from_nanos(DEFAULT_TS_NANOS)
    }
    struct FakeLogFormatter {
        pushed_logs: Vec<ArchiveIteratorResult>,
    }

    #[async_trait(?Send)]
    impl LogFormatter for FakeLogFormatter {
        async fn push_log(&mut self, log_entry: ArchiveIteratorResult) -> Result<()> {
            self.pushed_logs.push(log_entry);
            Ok(())
        }

        fn set_boot_timestamp(&mut self, boot_ts_nanos: i64) {
            assert_eq!(boot_ts_nanos, BOOT_TS as i64)
        }
    }

    impl FakeLogFormatter {
        fn new() -> Self {
            Self { pushed_logs: vec![] }
        }

        fn assert_same_logs(&self, expected: Vec<ArchiveIteratorResult>) {
            assert_eq!(
                self.pushed_logs.len(),
                expected.len(),
                "got different number of log entries. \ngot: {:?}\nexpected: {:?}",
                self.pushed_logs,
                expected
            );
            for (got, expected_log) in self.pushed_logs.iter().zip(expected.iter()) {
                assert_eq!(
                    got, expected_log,
                    "got different log entries. \ngot: {:?}\nexpected: {:?}\n",
                    got, expected_log
                );
            }
        }
    }

    struct FakeLogSpamFilter {
        is_spam_result: bool,
    }

    impl spam_filter::LogSpamFilter for FakeLogSpamFilter {
        fn is_spam(&self, _: Option<&str>, _: Option<u64>, _: &str) -> bool {
            self.is_spam_result
        }
    }

    fn setup_fake_log_settings_proxy(
        expected_selectors: Vec<LogInterestSelector>,
    ) -> Option<LogSettingsProxy> {
        Some(fho::testing::fake_proxy(move |req| match req {
            LogSettingsRequest::SetInterest { selectors, responder } => {
                assert_eq!(selectors, expected_selectors);
                let _ = responder.send();
            }
            other => panic!("Got unexpected request: {other:?}"),
        }))
    }

    fn setup_fake_rcs() -> Option<RemoteControlProxy> {
        Some(fho::testing::fake_proxy(move |req| match req {
            RemoteControlRequest::IdentifyHost { responder } => {
                responder
                    .send(Ok(&IdentifyHostResponse {
                        boot_timestamp_nanos: Some(BOOT_TS),
                        nodename: Some(NODENAME.to_string()),
                        ..Default::default()
                    }))
                    .context("sending identify host response")
                    .unwrap();
            }
            _ => assert!(false),
        }))
    }

    fn setup_fake_daemon_server(
        expected_parameters: DaemonDiagnosticsStreamParameters,
        expected_responses: Arc<Vec<FakeArchiveIteratorResponse>>,
    ) -> DiagnosticsProxy {
        fho::testing::fake_proxy(move |req| match req {
            DiagnosticsRequest::StreamDiagnostics {
                target: t,
                parameters,
                iterator,
                responder,
            } => {
                assert_eq!(parameters, expected_parameters);
                setup_fake_archive_iterator(
                    iterator,
                    ArchiveIteratorParameters {
                        legacy_format: false,
                        responses: expected_responses.clone(),
                        use_socket: false,
                    },
                )
                .unwrap();
                responder
                    .send(Ok(&LogSession {
                        target_identifier: t,
                        session_timestamp_nanos: Some(BOOT_TS),
                        ..Default::default()
                    }))
                    .context("error sending response")
                    .expect("should send")
            }
        })
    }

    fn make_log_entry(log_data: LogData) -> LogEntry {
        LogEntry {
            version: 1,
            timestamp: Timestamp::from(default_ts().as_nanos() as i64),
            data: log_data,
        }
    }

    fn empty_dump_command() -> LogCommand {
        LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        }
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

    fn log_entry() -> LogEntry {
        LogEntry {
            timestamp: 0.into(),
            version: 1,
            data: LogData::TargetLog(
                logs_data_builder().add_tag("tag1").add_tag("tag2").set_message("message").build(),
            ),
        }
    }

    impl Default for LogFormatterOptions {
        fn default() -> Self {
            LogFormatterOptions {
                raw: false,
                display: DisplayOption::Text(Default::default()),
                highlight_spam: false,
            }
        }
    }

    #[fuchsia::test]
    async fn test_dump_empty() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = empty_dump_command();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };
        let expected_responses = vec![];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![])
    }

    #[fuchsia::test]
    async fn test_watch() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand::default();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotRecentThenSubscribe),
            ..Default::default()
        };
        let log1 = make_log_entry(LogData::FfxEvent(EventType::LoggingStarted));
        let log2 = make_log_entry(LogData::MalformedTargetLog("text".to_string()));
        let log3 = make_log_entry(LogData::MalformedTargetLog("text2".to_string()));

        let expected_responses = vec![
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log1).unwrap(),
                serde_json::to_string(&log2).unwrap(),
            ]),
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log3).unwrap()
            ]),
        ];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2), Ok(log3)])
    }

    #[fuchsia::test]
    async fn test_watch_since_arbitrary_timestamp() {
        let mut formatter = FakeLogFormatter::new();
        let mut cmd = LogCommand::default();
        let date_string = "04/20/2020";
        let ts = parse_time(date_string).unwrap();
        cmd.since = Some(ts.clone());
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAllThenSubscribe),
            min_timestamp_nanos: Some(TimeBound::Absolute(ts.timestamp_nanos() as u64)),
            ..Default::default()
        };
        let log1 = make_log_entry(LogData::FfxEvent(EventType::LoggingStarted));
        let log2 = make_log_entry(LogData::MalformedTargetLog("text".to_string()));
        let log3 = make_log_entry(LogData::MalformedTargetLog("text2".to_string()));

        let expected_responses = vec![
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log1).unwrap(),
                serde_json::to_string(&log2).unwrap(),
            ]),
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log3).unwrap()
            ]),
        ];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2), Ok(log3)])
    }

    #[fuchsia::test]
    async fn test_watch_since_now() {
        let mut formatter = FakeLogFormatter::new();
        let mut cmd = LogCommand::default();
        let now_ts = parse_time("now").unwrap();
        cmd.since = Some(now_ts.clone());
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
            min_timestamp_nanos: Some(TimeBound::Absolute(now_ts.timestamp_nanos() as u64)),
            ..Default::default()
        };
        let log1 = make_log_entry(LogData::FfxEvent(EventType::LoggingStarted));
        let log2 = make_log_entry(LogData::MalformedTargetLog("text".to_string()));
        let log3 = make_log_entry(LogData::MalformedTargetLog("text2".to_string()));

        let expected_responses = vec![
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log1).unwrap(),
                serde_json::to_string(&log2).unwrap(),
            ]),
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log3).unwrap()
            ]),
        ];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2), Ok(log3)])
    }

    #[fuchsia::test]
    async fn test_watch_with_error() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand::default();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotRecentThenSubscribe),
            ..Default::default()
        };
        let log1 = make_log_entry(LogData::FfxEvent(EventType::LoggingStarted));
        let log2 = make_log_entry(LogData::MalformedTargetLog("text".to_string()));
        let log3 = make_log_entry(LogData::MalformedTargetLog("text2".to_string()));

        let expected_responses = vec![
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log1).unwrap(),
                serde_json::to_string(&log2).unwrap(),
            ]),
            FakeArchiveIteratorResponse::new_with_error(ArchiveIteratorError::GenericError),
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log3).unwrap()
            ]),
        ];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![
            Ok(log1),
            Ok(log2),
            Err(ArchiveIteratorError::GenericError),
            Ok(log3),
        ])
    }

    #[fuchsia::test]
    async fn test_dump_with_to_timestamp() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand { until_monotonic: Some(default_ts()), ..empty_dump_command() };
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };
        let log1 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: Timestamp::from(
                    (default_ts() - Duration::from_nanos(1)).as_nanos() as i64,
                ),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("log1")
            .build()
            .into(),
        );
        let log2 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: Timestamp::from(default_ts().as_nanos() as i64),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("log2")
            .build()
            .into(),
        );
        let log3 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: Timestamp::from(
                    (default_ts() + Duration::from_nanos(1)).as_nanos() as i64,
                ),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("log3")
            .build()
            .into(),
        );

        let expected_responses = vec![FakeArchiveIteratorResponse::new_with_values(vec![
            serde_json::to_string(&log1).unwrap(),
            serde_json::to_string(&log2).unwrap(),
            serde_json::to_string(&log3).unwrap(),
        ])];

        let mut writer = Vec::new();
        assert_matches::assert_matches!(
            log_cmd(
                setup_fake_daemon_server(params, Arc::new(expected_responses)),
                setup_fake_rcs(),
                &setup_fake_log_settings_proxy(vec![]),
                &mut formatter,
                cmd,
                &mut writer,
            )
            .await,
            Ok(_)
        );

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2)])
    }

    #[fuchsia::test]
    async fn test_watch_with_select() {
        let mut formatter = FakeLogFormatter::new();
        let selectors = vec![LogInterestSelector {
            selector: parse_component_selector::<VerboseError>("core/my_component").unwrap(),
            interest: Interest { min_severity: Some(FidlSeverity::Info), ..Default::default() },
        }];
        let cmd = LogCommand { select: selectors.clone(), ..LogCommand::default() };
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotRecentThenSubscribe),
            ..Default::default()
        };
        let log1 = make_log_entry(LogData::FfxEvent(EventType::LoggingStarted));
        let log2 = make_log_entry(LogData::MalformedTargetLog("text".to_string()));
        let log3 = make_log_entry(LogData::MalformedTargetLog("text2".to_string()));

        let expected_responses = vec![
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log1).unwrap(),
                serde_json::to_string(&log2).unwrap(),
            ]),
            FakeArchiveIteratorResponse::new_with_values(vec![
                serde_json::to_string(&log3).unwrap()
            ]),
        ];

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(expected_responses)),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(selectors),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2), Ok(log3)])
    }

    #[fuchsia::test]
    async fn test_watch_with_select_params_but_no_proxy() {
        let mut formatter = FakeLogFormatter::new();
        let selectors = vec![LogInterestSelector {
            selector: parse_component_selector::<VerboseError>("core/my_component").unwrap(),
            interest: Interest { min_severity: Some(FidlSeverity::Info), ..Default::default() },
        }];
        let cmd = LogCommand { select: selectors.clone(), ..LogCommand::default() };
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotRecentThenSubscribe),
            ..Default::default()
        };

        let mut writer = Vec::new();
        assert!(log_cmd(
            setup_fake_daemon_server(params, Arc::new(vec![])),
            setup_fake_rcs(),
            &None,
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .is_err())
    }

    #[fuchsia::test]
    async fn test_criteria_moniker_message_and_severity_matches() {
        let cmd = LogCommand {
            filter: vec!["included".to_string()],
            exclude: vec!["not this".to_string()],
            severity: Severity::Error,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                // Include a "/" prefix on the moniker to test filter permissiveness.
                moniker: "/included/moniker".to_string(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "not/this/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Warn,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("not this message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_message_severity_symbolized_log() {
        let cmd = LogCommand {
            filter: vec!["included".to_string()],
            exclude: vec!["not this".to_string()],
            severity: Severity::Error,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            LogData::SymbolizedTargetLog(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp_nanos: 0.into(),
                    component_url: Some(String::default()),
                    moniker: "included/moniker".to_string(),
                    severity: diagnostics_data::Severity::Error,
                })
                .set_message("not this")
                .build(),
                "included".to_string()
            )
        )));

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            LogData::SymbolizedTargetLog(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp_nanos: 0.into(),
                    component_url: Some(String::default()),
                    moniker: "included/moniker".to_string(),
                    severity: diagnostics_data::Severity::Error,
                })
                .set_message("some message")
                .build(),
                "some message".to_string()
            )
        )));

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            LogData::SymbolizedTargetLog(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp_nanos: 0.into(),
                    component_url: Some(String::default()),
                    moniker: "included/moniker".to_string(),
                    severity: diagnostics_data::Severity::Warn,
                })
                .set_message("not this")
                .build(),
                "included".to_string()
            )
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            LogData::SymbolizedTargetLog(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp_nanos: 0.into(),
                    component_url: Some(String::default()),
                    moniker: "included/moniker".to_string(),
                    severity: diagnostics_data::Severity::Error,
                })
                .set_message("included")
                .build(),
                "not this".to_string()
            )
        )));
    }

    #[fuchsia::test]
    async fn test_empty_criteria() {
        let cmd = empty_dump_command();
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_klog_only() {
        let cmd = LogCommand { kernel: true, ..empty_dump_command() };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "klog".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_multiple_matches() {
        let cmd = LogCommand {
            filter: vec!["included".to_string(), "also".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "moniker".to_string(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("also message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("not in there message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("different message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_multiple_excludes() {
        let cmd = LogCommand {
            exclude: vec![".cmx".to_string(), "also".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker.cmx:12345".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "also/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_moniker_filter() {
        let cmd = LogCommand {
            moniker: vec!["/core/network/netstack".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "bootstrap/archivist".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("excluded")
            .build()
            .into()
        )));

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(LogData::FfxEvent(
            EventType::LoggingStarted
        ),)));

        let allowed_cmd = LogCommand { ..empty_dump_command() };
        let allowed_criteria = LogFilterCriteria::try_from_cmd(&allowed_cmd, None).unwrap();
        assert!(allowed_criteria.matches_filters_to_log_entry(&make_log_entry(LogData::FfxEvent(
            EventType::LoggingStarted
        ),)));

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "core/network/netstack".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .build()
            .into()
        )));

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "core/network/dhcp".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_tag_filter() {
        let cmd = LogCommand {
            tags: vec!["tag1".to_string()],
            exclude_tags: vec!["tag3".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag1")
            .add_tag("tag2")
            .build()
            .into()
        )));

        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag2")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag1")
            .add_tag("tag3")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_matches_component_url() {
        let cmd = LogCommand {
            filter: vec!["fuchsia.com".to_string()],
            exclude: vec!["not-this-component.cmx".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        assert!(criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("fuchsia.com/this-component.cmx".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("fuchsia.com/not-this-component.cmx".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("message")
            .build()
            .into()
        )));
        assert!(!criteria.matches_filters_to_log_entry(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("some-other.com/component.cmx".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_matches_spam_filter() {
        let cmd = LogCommand { ..empty_dump_command() };
        let mut criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();

        // spam matches
        let spam_filter = FakeLogSpamFilter { is_spam_result: true };
        criteria.spam_filter = Some(Box::new(spam_filter));
        assert!(criteria.matches_spam(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("component".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .build()
            .into()
        )));

        // spam not matched
        let spam_filter = FakeLogSpamFilter { is_spam_result: false };
        criteria.spam_filter = Some(Box::new(spam_filter));
        assert!(!criteria.matches_spam(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("component".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .build()
            .into()
        )));

        // spam filter is None
        criteria.spam_filter = None;
        assert!(!criteria.matches_spam(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some("component".to_string()),
                moniker: "any/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_disable_spam_filter() {
        let cmd = LogCommand {
            spam_list_path: Some("dummy_path".to_string()),
            disable_spam_filter: true,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();
        assert!(criteria.spam_filter.is_none())
    }

    #[fuchsia::test]
    async fn test_criteria_spam_list_path_flag_set() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"logSpam": []}}"#).unwrap();
        let cmd = LogCommand {
            spam_list_path: tmp.path().to_str().map(|s| s.to_string()),
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, None).unwrap();
        assert!(criteria.spam_filter.is_some())
    }

    #[fuchsia::test]
    async fn test_criteria_spam_list_default_config_set() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"logSpam": []}}"#).unwrap();
        let cmd = LogCommand { ..empty_dump_command() };
        let default_config: Option<String> = Some(tmp.path().to_str().unwrap().into());
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, default_config).unwrap();
        assert!(criteria.spam_filter.is_some())
    }

    #[fuchsia::test]
    async fn test_criteria_prefer_spam_list_flag_override() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"logSpam": []}}"#).unwrap();
        let cmd = LogCommand {
            spam_list_path: tmp.path().to_str().map(|s| s.to_string()),
            ..empty_dump_command()
        };
        let default_config = Some("invalid_path".to_string());
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, default_config).unwrap();
        assert!(criteria.spam_filter.is_some())
    }

    #[fuchsia::test]
    async fn test_criteria_spam_list_default_config_unset() {
        let cmd = LogCommand { ..empty_dump_command() };
        let default_config = None;
        let criteria = LogFilterCriteria::try_from_cmd(&cmd, default_config).unwrap();
        assert!(criteria.spam_filter.is_none())
    }

    #[fuchsia::test]
    async fn test_spam_list_applies_highlighting_only_to_spam_line() {
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

        let options = LogFormatterOptions {
            display: DisplayOption::Text(Default::default()),
            raw: false,
            highlight_spam: true,
        };

        let mut filter = LogFilterCriteria::default();
        filter.spam_filter =
            Some(Box::new(AlternatingSpamFilter { last_message_was_spam: Cell::new(false) }));
        let mut output = vec![];
        {
            let mut formatter = DefaultLogFormatter::new(filter, &mut output, options.clone());
            formatter.push_log(ArchiveIteratorResult::Ok(log_entry())).await.unwrap();
            formatter.push_log(ArchiveIteratorResult::Ok(log_entry())).await.unwrap();
            formatter.push_log(ArchiveIteratorResult::Ok(log_entry())).await.unwrap();
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message
\u{1b}[38;5;11m[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\u{1b}[m
[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\n",
            "first message should be uncolored, second should be yellow, third should be uncolored"
        );
    }

    #[fuchsia::test]
    async fn test_from_time_passed_to_daemon() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand {
            since: Some(Local.timestamp(FAKE_START_TIMESTAMP, 0).into()),
            since_monotonic: None,
            until: None,
            until_monotonic: None,
            ..empty_dump_command()
        };
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            min_timestamp_nanos: Some(TimeBound::Absolute(
                Duration::from_secs(FAKE_START_TIMESTAMP as u64).as_nanos() as u64,
            )),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(vec![])),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }

    #[fuchsia::test]
    async fn test_since_monotonic_passed_to_daemon() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand {
            since: None,
            since_monotonic: Some(default_ts()),
            until: None,
            until_monotonic: None,
            ..empty_dump_command()
        };
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            min_timestamp_nanos: Some(TimeBound::Monotonic(default_ts().as_nanos() as u64)),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };

        let mut writer = Vec::new();
        log_cmd(
            setup_fake_daemon_server(params, Arc::new(vec![])),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }

    #[fuchsia::test]
    async fn test_multiple_from_time_args_fails() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand {
            since: Some(Local.timestamp(FAKE_START_TIMESTAMP, 0).into()),
            since_monotonic: Some(default_ts()),
            until: None,
            until_monotonic: None,
            ..empty_dump_command()
        };

        let mut writer = Vec::new();
        assert!(log_cmd(
            setup_fake_daemon_server(
                DaemonDiagnosticsStreamParameters::default(),
                Arc::new(vec![])
            ),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap_err()
        .ffx_error()
        .is_some());

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }

    #[fuchsia::test]
    async fn test_multiple_to_time_args_fails() {
        let mut formatter = FakeLogFormatter::new();
        let cmd = LogCommand {
            until: Some(Local.timestamp(FAKE_START_TIMESTAMP, 0).into()),
            until_monotonic: Some(default_ts()),
            since: None,
            since_monotonic: None,
            ..empty_dump_command()
        };

        let mut writer = Vec::new();
        assert!(log_cmd(
            setup_fake_daemon_server(
                DaemonDiagnosticsStreamParameters::default(),
                Arc::new(vec![])
            ),
            setup_fake_rcs(),
            &setup_fake_log_settings_proxy(vec![]),
            &mut formatter,
            cmd,
            &mut writer,
        )
        .await
        .unwrap_err()
        .ffx_error()
        .is_some());

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }

    #[fuchsia::test]
    async fn test_default_formatter() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions::default();
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options.clone());
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_with_json() {
        let mut output = vec![];
        let options =
            LogFormatterOptions { display: DisplayOption::Json, raw: false, highlight_spam: false };
        {
            let mut formatter = DefaultLogFormatter::new(
                LogFilterCriteria::default(),
                &mut output,
                options.clone(),
            );
            formatter.push_log(ArchiveIteratorResult::Ok(log_entry())).await.unwrap();
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            r#"{"data":{"TargetLog":{"data_source":"Logs","metadata":{"component_url":"component_url","timestamp":1615535969000000000,"severity":"WARN","tags":["tag1","tag2"],"pid":1,"tid":2},"moniker":"some/moniker","payload":{"root":{"message":{"value":"message"}}},"version":1}},"timestamp":0,"version":1}
"#
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_local_time() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { tz: Timezone::Utc, offset: 0 },
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options.clone());

        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options.clone(),
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message",
            "Before setting the boot timestamp, it should use monotonic time."
        );

        formatter.set_boot_timestamp(1);
        // NB: We can't assert on a specific time here because local timezones will vary between
        // devices used for this test.
        assert_ne!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][moniker] WARN: message",
            "Local time should not be equal to monotonic",
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_utc_time() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                time_format: LogTimeDisplayFormat::WallTime { tz: Timezone::Utc, offset: 0 },
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };
        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);

        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options.clone(),
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message"
        );

        formatter.set_boot_timestamp(1);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[2021-03-12 07:59:29.000][1][2][some/moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_colored_output() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                color: LogTextColor::BySeverity,
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "\u{1b}[38;5;3m[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message\u{1b}[m"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_hide_metadata() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                show_metadata: false,
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };

        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][some/moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_symbolized_log_message() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions::default();
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        let mut entry = log_entry();
        entry.data = match entry.data.clone() {
            LogData::TargetLog(d) => LogData::SymbolizedTargetLog(d, "symbolized".to_string()),
            _ => panic!(),
        };
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                entry,
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: symbolized"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_raw() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions { raw: true, ..LogFormatterOptions::default() };
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_show_tags() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                show_tags: true,
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_hides_tags_if_empty() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                show_tags: true,
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };

        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                LogEntry {
                    data: LogData::TargetLog(logs_data_builder().build()),
                    timestamp: 0.into(),
                    version: 0,
                },
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: <missing message>"
        );
    }

    #[fuchsia::test]
    async fn test_default_formatter_multiline_message() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(Default::default()),
            raw: false,
            highlight_spam: false,
        };
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                LogEntry {
                    data: LogData::TargetLog(
                        logs_data_builder().set_message("multi\nline\nmessage").build()
                    ),
                    timestamp: 0.into(),
                    version: 0
                },
                formatter.has_previous_log,
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: multi\nline\nmessage"
        );
    }

    #[fuchsia::test]
    fn display_for_structured_fields() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(Default::default()),
            raw: false,
            highlight_spam: false,
        };

        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                LogEntry {
                    data: LogData::TargetLog(
                        logs_data_builder()
                            .set_message("my message")
                            .add_key(LogsProperty::String(
                                LogsField::Other("bar".to_string()),
                                "baz".to_string()
                            ))
                            .add_key(LogsProperty::Int(LogsField::Other("foo".to_string()), 2i64))
                            .build()
                    ),
                    timestamp: 0.into(),
                    version: 0,
                },
                formatter.has_previous_log,
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: my message bar=baz foo=2"
        );
    }

    #[fuchsia::test]
    fn display_for_file_and_line() {
        let mut stdout = Unblock::new(std::io::stdout());
        let display_options = LogTextDisplayOptions { show_file: true, ..Default::default() };
        let options = LogFormatterOptions {
            display: DisplayOption::Text(display_options),
            raw: false,
            highlight_spam: false,
        };

        let mut formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        let message_with_file_and_line = logs_data_builder()
            .set_line(123u64)
            .set_file("path/to/file.cc".to_string())
            .set_message("my message")
            .build();
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options.clone(),
                LogEntry {
                    data: LogData::TargetLog(message_with_file_and_line.clone()),
                    timestamp: 0.into(),
                    version: 0
                },
                formatter.has_previous_log,
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: [path/to/file.cc(123)] my message"
        );

        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options.clone(),
                LogEntry {
                    data: LogData::TargetLog(
                        logs_data_builder()
                            .set_file("path/to/file.cc".to_string())
                            .set_message("my message")
                            .build()
                    ),
                    timestamp: 0.into(),
                    version: 0,
                },
                formatter.has_previous_log,
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: [path/to/file.cc] my message"
        );

        // Disable file printing for the last assertion.
        match &mut formatter.options.display {
            DisplayOption::Text(ref mut text_options) => {
                text_options.show_file = false;
            }
            _ => unreachable!(),
        }
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                LogEntry {
                    data: LogData::TargetLog(message_with_file_and_line),
                    timestamp: 0.into(),
                    version: 0
                },
                formatter.has_previous_log,
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: my message"
        );
    }

    #[fuchsia::test]
    fn display_short_moniker() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(LogTextDisplayOptions {
                show_full_moniker: false,
                ..Default::default()
            }),
            raw: false,
            highlight_spam: false,
        };
        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                log_entry(),
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][moniker][tag1,tag2] WARN: message"
        );
    }

    #[fuchsia::test]
    fn display_dropped_and_rolled_out_info() {
        let mut stdout = Unblock::new(std::io::stdout());
        let options = LogFormatterOptions {
            display: DisplayOption::Text(Default::default()),
            raw: false,
            highlight_spam: false,
        };

        let formatter =
            DefaultLogFormatter::new(LogFilterCriteria::default(), &mut stdout, options);
        let message_with_file_and_line =
            logs_data_builder().set_dropped(3).set_rolled_out(5).set_message("my message").build();

        assert_eq!(
            DefaultLogFormatter::format_text_log(
                formatter.options,
                LogEntry {
                    data: LogData::TargetLog(message_with_file_and_line),
                    timestamp: 0.into(),
                    version: 0
                },
                formatter.has_previous_log
            )
            .unwrap()
            .unwrap(),
            "[1615535969.000000][1][2][some/moniker] WARN: my message [dropped=3] [rolled=5]"
        );
    }
}
