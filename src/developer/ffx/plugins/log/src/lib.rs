// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use error::LogError;
use ffx_log_args::LogCommand;
use fho::{Connector, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::{LogSettingsMarker, LogSettingsProxy, StreamParameters};
use fidl_fuchsia_diagnostics_host::ArchiveAccessorMarker;
use log_command::log_formatter::{
    dump_logs_from_socket, BootTimeAccessor, DefaultLogFormatter, LogEntry, LogFormatter,
    Symbolize, WriterContainer,
};
use log_command::{InstanceGetter, LogProcessingResult, LogSubCommand, WatchCommand};
use std::io::Write;
use transactional_symbolizer::{RealSymbolizerProcess, TransactionalSymbolizer};

// NOTE: This is required for the legacy ffx toolchain
// which automatically adds ffx_core even though we don't use it.
use ffx_core::{self as _};

mod condition_variable;
mod error;
mod mutex;
mod transactional_symbolizer;

#[cfg(test)]
mod testing_utils;

const ARCHIVIST_MONIKER: &str = "bootstrap/archivist";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(FfxTool)]
pub struct LogTool {
    #[command]
    cmd: LogCommand,
    rcs_connector: Connector<RemoteControlProxy>,
}

struct NoOpSymoblizer;

#[async_trait(?Send)]
impl Symbolize for NoOpSymoblizer {
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry> {
        Some(entry)
    }
}

fho::embedded_plugin!(LogTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for LogTool {
    type Writer = MachineWriter<LogEntry>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        log_impl(writer, self.cmd, self.rcs_connector).await?;
        Ok(())
    }
}

// Main entrypoint called from other plugins
pub async fn log_impl(
    writer: impl ToolIO<OutputItem = LogEntry> + Write + 'static,
    cmd: LogCommand,
    rcs_connector: Connector<RemoteControlProxy>,
) -> Result<(), LogError> {
    let rcs_proxy = connect_to_rcs(&rcs_connector).await?;
    let instance_getter = rcs::root_realm_query(&rcs_proxy, TIMEOUT).await?;
    // TODO(b/333908164): We have 3 different flags that all do the same thing.
    // Remove them when possible.
    let symbolize_disabled = cmd.symbolize.is_symbolize_disabled() || cmd.no_symbolize || cmd.raw;
    let prettification_disabled = cmd.symbolize.is_prettification_disabled();
    log_main(
        writer,
        cmd,
        if symbolize_disabled {
            None
        } else {
            Some(TransactionalSymbolizer::new(
                RealSymbolizerProcess::new(!prettification_disabled).await?,
            )?)
        },
        instance_getter,
        rcs_connector,
    )
    .await
}

// Main logging event loop.
async fn log_main<W>(
    writer: W,
    cmd: LogCommand,
    symbolizer: Option<impl Symbolize>,
    instance_getter: impl InstanceGetter,
    rcs_connector: Connector<RemoteControlProxy>,
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write + 'static,
{
    let formatter = DefaultLogFormatter::<W>::new_from_args(&cmd, writer);
    log_loop(cmd, formatter, symbolizer, &instance_getter, rcs_connector).await?;
    Ok(())
}

struct DeviceConnection {
    boot_timestamp: u64,
    log_socket: fuchsia_async::Socket,
    log_settings_client: LogSettingsProxy,
}

async fn connect_to_rcs(
    rcs_connector: &Connector<RemoteControlProxy>,
) -> fho::Result<RemoteControlProxy> {
    rcs_connector.try_connect(|_target| Ok(())).await
}

// TODO(https://fxbug.dev/42080003): Remove this once Overnet
// has support for reconnect handling.
async fn connect_to_target(
    stream_mode: &mut fidl_fuchsia_diagnostics::StreamMode,
    prev_timestamp: Option<u64>,
    rcs_connector: &Connector<RemoteControlProxy>,
) -> Result<DeviceConnection, LogError> {
    // Connect to device
    let rcs_client = connect_to_rcs(rcs_connector).await?;
    let boot_timestamp =
        rcs_client.identify_host().await??.boot_timestamp_nanos.ok_or(LogError::NoBootTimestamp)?;
    // If we detect a reboot we want to SnapshotThenSubscribe so
    // we get all of the logs from the reboot. If not, we use Snapshot
    // to avoid getting duplicate logs.
    match prev_timestamp {
        Some(timestamp) if timestamp != boot_timestamp => {
            *stream_mode = fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe;
        }
        _ => {}
    }
    // Connect to ArchiveAccessor
    let diagnostics_client = rcs::toolbox::connect_with_timeout::<ArchiveAccessorMarker>(
        &rcs_client,
        Some(ARCHIVIST_MONIKER),
        TIMEOUT,
    )
    .await?;
    // Connect to LogSettings
    let log_settings_client = rcs::toolbox::connect_with_timeout::<LogSettingsMarker>(
        &rcs_client,
        Some(ARCHIVIST_MONIKER),
        TIMEOUT,
    )
    .await?;
    // Setup stream
    let (local, remote) = fuchsia_async::emulated_handle::Socket::create_stream();
    diagnostics_client
        .stream_diagnostics(
            &StreamParameters {
                data_type: Some(fidl_fuchsia_diagnostics::DataType::Logs),
                stream_mode: Some(*stream_mode),
                format: Some(fidl_fuchsia_diagnostics::Format::Json),
                client_selector_configuration: Some(
                    fidl_fuchsia_diagnostics::ClientSelectorConfiguration::SelectAll(true),
                ),
                ..Default::default()
            },
            remote,
        )
        .await?;
    Ok(DeviceConnection {
        boot_timestamp,
        log_socket: fuchsia_async::Socket::from_socket(local),
        log_settings_client,
    })
}

async fn log_loop<W>(
    cmd: LogCommand,
    mut formatter: impl LogFormatter + BootTimeAccessor + WriterContainer<W>,
    symbolizer: Option<impl Symbolize>,
    realm_query: &impl InstanceGetter,
    rcs_connector: Connector<RemoteControlProxy>,
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write,
{
    let symbolizer_channel: Box<dyn Symbolize> = match symbolizer {
        Some(inner) => Box::new(inner),
        None => Box::new(NoOpSymoblizer {}),
    };
    let mut stream_mode = get_stream_mode(cmd.clone())?;
    // TODO(https://fxbug.dev/42080003): Add support for reconnect handling to Overnet.
    // This plugin needs special logic to handle reconnects as logging should tolerate
    // a device rebooting and remaining in a consistent state (automatically) after the reboot.
    // Eventually we should have direct support for this in Overnet, but for now we have to
    // handle reconnects manually.
    let mut prev_timestamp = None;
    loop {
        let connection;
        // Linear backoff up to 10 seconds.
        let mut backoff = 0;
        let mut last_error = None;
        loop {
            let maybe_connection =
                connect_to_target(&mut stream_mode, prev_timestamp, &rcs_connector).await;
            if let Ok(connected) = maybe_connection {
                connection = connected;
                break;
            }
            backoff += 1;
            if backoff > 10 {
                backoff = 10;
            }
            let err = maybe_connection.err().unwrap();
            if matches!(err, LogError::FidlError(fidl::Error::ClientChannelClosed { .. })) {
                continue;
            }
            let err = format!("{:?}", err);
            if matches!(&last_error, Some(value) if *value == err) {
                eprintln!("Error connecting to device, retrying in {backoff} seconds.");
            } else {
                if err.contains("FFX Daemon was told not to autostart and no existing Daemon instance was found") {
                    return Err(LogError::DaemonRetriesDisabled);
                }
                eprintln!(
                    "Error connecting to device, retrying in {backoff} seconds. Error: {err}",
                );
                last_error = Some(err);
            }
            fuchsia_async::Timer::new(std::time::Duration::from_secs(backoff)).await;
        }
        prev_timestamp = Some(connection.boot_timestamp);
        cmd.maybe_set_interest(
            &connection.log_settings_client,
            realm_query,
            formatter.writer().is_machine(),
        )
        .await?;
        formatter.set_boot_timestamp(connection.boot_timestamp as i64);
        let result = dump_logs_from_socket(
            connection.log_socket,
            &mut formatter,
            symbolizer_channel.as_ref(),
        )
        .await;
        if stream_mode == fidl_fuchsia_diagnostics::StreamMode::Snapshot {
            break;
        }
        match result {
            Ok(LogProcessingResult::Exit) => {
                break;
            }
            Ok(LogProcessingResult::Continue) => {}
            Err(value) => {
                writeln!(formatter.writer().stderr(), "{value}")?;
            }
        }
    }
    Ok(())
}

fn get_stream_mode(cmd: LogCommand) -> Result<fidl_fuchsia_diagnostics::StreamMode, LogError> {
    let sub_command = cmd.sub_command.unwrap_or(LogSubCommand::Watch(WatchCommand {}));
    let stream_mode = if matches!(sub_command, LogSubCommand::Dump(..)) {
        if cmd.since.map(|value| value.is_now).unwrap_or(false) {
            return Err(LogError::DumpWithSinceNow);
        }
        fidl_fuchsia_diagnostics::StreamMode::Snapshot
    } else {
        cmd.since
            .as_ref()
            .map(|value| {
                if value.is_now {
                    fidl_fuchsia_diagnostics::StreamMode::Subscribe
                } else {
                    fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe
                }
            })
            .unwrap_or(fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe)
    };
    Ok(stream_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing_utils::{TestEnvironment, TestEnvironmentConfig, TestEvent};
    use assert_matches::assert_matches;
    use chrono::{Local, TimeZone};
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use ffx_writer::{Format, TestBuffers};
    use fidl_fuchsia_diagnostics::StreamMode;
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use log_command::log_formatter::{LogData, TIMESTAMP_FORMAT};
    use log_command::{
        parse_seconds_string_as_duration, parse_time, DumpCommand, SymbolizeMode, TimeFormat,
    };
    use moniker::Moniker;
    use selectors::parse_log_interest_selector;

    const TEST_STR: &str = "[1980-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n";

    async fn check_for_message(buffers: &TestBuffers, msg: &str) {
        while buffers.stdout.clone().into_string() != msg {
            buffers.stdout.wait_ready().await;
        }
    }

    #[fuchsia::test]
    async fn json_logger_test() {
        let environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let rcs_connector = environment.rcs_connector().await;
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(Some(Format::Json), &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        let output = buffers.into_stdout_str();

        assert_eq!(
            serde_json::from_str::<LogEntry>(&output).unwrap(),
            LogEntry {
                timestamp: 1.into(),
                data: LogData::TargetLog(
                    LogsDataBuilder::new(BuilderArgs {
                        component_url: Some("ffx".into()),
                        moniker: "host/ffx".try_into().unwrap(),
                        severity: Severity::Info,
                        timestamp_nanos: Timestamp::from(0),
                    })
                    .set_pid(1)
                    .set_tid(2)
                    .set_message("Hello world!")
                    .build(),
                ),
            }
        );
    }

    #[fuchsia::test]
    async fn logger_prints_error_if_ambiguous_selector() {
        let environment = TestEnvironment::new(TestEnvironmentConfig {
            instances: vec![
                Moniker::try_from("core/some/ambiguous_selector:thing/test").unwrap(),
                Moniker::try_from("core/other/ambiguous_selector:thing/test").unwrap(),
            ],
            ..Default::default()
        });
        let rcs_connector = environment.rcs_connector().await;
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };

        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        let error = format!("{}", tool.main(writer).await.unwrap_err());

        const EXPECTED_INTEREST_ERROR: &str = r#"WARN: One or more of your selectors appears to be ambiguous
and may not match any components on your system.

If this is unintentional you can explicitly match using the
following command:

ffx log \
	--select core/other/ambiguous_selector\\:thing/test#INFO \
	--select core/some/ambiguous_selector\\:thing/test#INFO

If this is intentional, you can disable this with
ffx log --force-select.
"#;
        assert_eq!(error, EXPECTED_INTEREST_ERROR);
    }

    #[fuchsia::test]
    async fn logger_sets_interest_if_one_match() {
        let selectors = vec![parse_log_interest_selector("core/foo#INFO").unwrap()];
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            instances: vec![Moniker::try_from("core/foo").unwrap()],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: selectors.clone(),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut event_stream = environment.take_event_stream().unwrap();

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(event_stream.next().await, Some(TestEvent::SetInterest(selectors)));
    }

    #[fuchsia::test]
    async fn logger_prints_error_if_both_dump_and_since_now_are_combined() {
        let environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            since: Some(parse_time("now").unwrap()),
            ..LogCommand::default()
        };
        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        let result = tool.main(writer).await;
        assert_matches!(result, Err(fho::Error::User(err)) => {
            assert_matches!(err.downcast_ref::<LogError>(), Some(LogError::DumpWithSinceNow));
        });
    }

    #[fuchsia::test]
    async fn logger_prints_current_logs_and_exits_on_dump() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut event_stream = environment.take_event_stream().unwrap();

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(buffers.into_stdout_str(), "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n",);
        // ffx log keeps this connection always open. If it exits, it means that ffx log exits.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_does_not_color_logs_if_disabled() {
        let environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            no_color: true,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(buffers.into_stdout_str(), "[00000.000000][ffx] INFO: Hello world!\n",);
    }

    #[fuchsia::test]
    async fn logger_shows_metadata_if_enabled() {
        let environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            no_color: true,
            show_metadata: true,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(buffers.into_stdout_str(), "[00000.000000][1][2][ffx] INFO: Hello world!\n",);
    }

    #[fuchsia::test]
    async fn logger_shows_utc_time_if_enabled() {
        let environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Utc,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[1970-01-01 00:00:00.000][ffx] INFO: Hello world!\u{1b}[m\n",
        );
    }

    #[fuchsia::test]
    async fn logger_shows_logs_filtered_by_severity() {
        let environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![
                testing_utils::test_log_with_severity(Timestamp::from(0), Severity::Info),
                testing_utils::test_log_with_severity(
                    Timestamp::from(3000000000i64),
                    Severity::Error,
                ),
                testing_utils::test_log_with_severity(
                    Timestamp::from(6000000000i64),
                    Severity::Info,
                ),
            ],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Utc,
            severity: Severity::Error,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "\u{1b}[38;5;1m[1970-01-01 00:00:03.000][ffx] ERROR: Hello world!\u{1b}[m\n",
        );
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log(Timestamp::from(
                parse_time("1980-01-01T00:00:03")
                    .unwrap()
                    .time
                    .naive_utc()
                    .timestamp_nanos_opt()
                    .unwrap(),
            ))],
            send_connected_event: true,
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Watch(WatchCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Local,
            since: Some(log_command::DetailedDateTime {
                is_now: true,
                ..parse_time("1980-01-01T00:00:01").unwrap()
            }),
            until: None,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut event_stream = environment.take_event_stream().unwrap();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = fasync::Task::local(tool.main(writer));

        // Run the stream until we get the expected message.
        check_for_message(&buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        environment.reboot_target(42);

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));

        check_for_message(&buffers, TEST_STR).await;

        // Second connection has a different timestamp so should be treated
        // as a reboot.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );

        environment.disconnect_target();

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots_heuristic() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log(Timestamp::from(
                parse_time("1980-01-01T00:00:03")
                    .unwrap()
                    .time
                    .naive_utc()
                    .timestamp_nanos_opt()
                    .unwrap(),
            ))],
            send_connected_event: true,
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Watch(WatchCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Local,
            since: Some(log_command::DetailedDateTime {
                is_now: true,
                ..parse_time("1980-01-01T00:00:01").unwrap()
            }),
            until: None,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut event_stream = environment.take_event_stream().unwrap();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = fasync::Task::local(tool.main(writer));

        // Run the stream until we get the expected message.
        check_for_message(&buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        environment.disconnect_target();

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));

        // We should reconnect and get another message.
        check_for_message(&buffers, TEST_STR).await;

        // Second connection has a matching timestamp to the first one, so we should
        // Subscribe to not repeat messages.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        // For the third connection, we should get a
        // SnapshotThenSubscribe request because the timestamp
        // changed and it's clear it's actually a separate boot not a disconnect/reconnect
        environment.reboot_target(42);

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));

        check_for_message(&buffers, TEST_STR).await;

        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![
                testing_utils::test_log(Timestamp::from(
                    parse_time("1980-01-01T00:00:00")
                        .unwrap()
                        .time
                        .naive_utc()
                        .timestamp_nanos_opt()
                        .unwrap(),
                )),
                testing_utils::test_log(Timestamp::from(
                    parse_time("1980-01-01T00:00:03")
                        .unwrap()
                        .time
                        .naive_utc()
                        .timestamp_nanos_opt()
                        .unwrap(),
                )),
                testing_utils::test_log(Timestamp::from(
                    parse_time("1980-01-01T00:00:06")
                        .unwrap()
                        .time
                        .naive_utc()
                        .timestamp_nanos_opt()
                        .unwrap(),
                )),
            ],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            since: Some(parse_time("1980-01-01T00:00:01").unwrap()),
            until: Some(parse_time("1980-01-01T00:00:05").unwrap()),
            clock: TimeFormat::Utc,
            ..LogCommand::default()
        };

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);
        let mut event_stream = environment.take_event_stream().unwrap();

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[1980-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n",
        );

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_monotonic() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![
                testing_utils::test_log(Timestamp::from(0)),
                testing_utils::test_log(Timestamp::from(3000000000i64)),
                testing_utils::test_log(Timestamp::from(6000000000i64)),
            ],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Utc,
            since_monotonic: Some(parse_seconds_string_as_duration("1").unwrap()),
            until_monotonic: Some(parse_seconds_string_as_duration("5").unwrap()),
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[1970-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n",
        );

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_local_time_if_enabled() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig::default());
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            clock: TimeFormat::Local,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            format!(
                "[{}][ffx] INFO: Hello world!\u{1b}[m\n",
                Local.timestamp_opt(0, 1).unwrap().format(TIMESTAMP_FORMAT)
            )
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_tags_by_default() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_tag(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));

        assert_eq!(
            buffers.into_stdout_str(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n",
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_full_moniker_by_default() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_tag(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n"
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_full_moniker_when_enabled() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_tag(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            show_full_moniker: true,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[00000.000000][host/ffx][test tag] INFO: Hello world!\u{1b}[m\n"
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_tag_when_instructed() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_tag(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            hide_tags: true,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(buffers.into_stdout_str(), "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n",);
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_sets_severity_appropriately_then_exits() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log(Timestamp::from(0))],
            ..Default::default()
        });
        let selector = vec![parse_log_interest_selector("archivist.cm#TRACE").unwrap()];
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            select: selector.clone(),
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(buffers.into_stdout_str(), "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n");
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::SetInterest(s)) if s == selector
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_file_names_by_default() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_file(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[00000.000000][ffx][test tag] INFO: [test_filename.cc(42)] Hello world!\u{1b}[m\n",
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_filename_if_disabled() {
        let mut environment = TestEnvironment::new(TestEnvironmentConfig {
            messages: vec![testing_utils::test_log_with_file(Timestamp::from(0))],
            ..Default::default()
        });
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            symbolize: SymbolizeMode::Off,
            hide_file: true,
            ..LogCommand::default()
        };
        let mut event_stream = environment.take_event_stream().unwrap();

        let rcs_connector = environment.rcs_connector().await;
        let tool = LogTool { cmd, rcs_connector };
        let buffers = TestBuffers::default();
        let writer = MachineWriter::<LogEntry>::new_test(None, &buffers);

        assert_matches!(tool.main(writer).await, Ok(()));
        assert_eq!(
            buffers.into_stdout_str(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n",
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsClosed));
    }

    #[fuchsia::test]
    async fn get_stream_mode_tests() {
        assert_matches!(
            get_stream_mode(LogCommand { ..LogCommand::default() }),
            Ok(fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe)
        );
        assert_matches!(
            get_stream_mode(LogCommand {
                since: Some(parse_time("now").unwrap()),
                ..LogCommand::default()
            }),
            Ok(fidl_fuchsia_diagnostics::StreamMode::Subscribe)
        );
        assert_matches!(
            get_stream_mode(LogCommand {
                since: Some(parse_time("09/04/1998").unwrap()),
                ..LogCommand::default()
            }),
            Ok(fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe)
        );
    }
}
