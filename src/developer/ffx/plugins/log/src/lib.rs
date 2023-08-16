// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{LogTextColor, LogTextDisplayOptions, LogTimeDisplayFormat, Timezone};
use error::LogError;
use ffx_log_args::FfxLogCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_developer_ffx::{TargetCollectionProxy, TargetQuery};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::{LogSettingsMarker, LogSettingsProxy, StreamParameters};
use fidl_fuchsia_diagnostics_host::ArchiveAccessorMarker;
use log_command::{
    filter::LogFilterCriteria,
    log_formatter::{
        dump_logs_from_socket, BootTimeAccessor, DefaultLogFormatter, DeviceOrLocalTimestamp,
        LogEntry, LogFormatter, LogFormatterOptions, WriterContainer,
    },
    LogCommand, LogSubCommand, TimeFormat, WatchCommand,
};
use log_symbolizer::{LogSymbolizer, Symbolizer};
use std::io::Write;
use symbolizer::SymbolizerChannel;

// NOTE: This is required for the legacy ffx toolchain
// which automatically adds ffx_core even though we don't use it.
use ffx_core as _;

mod error;
mod symbolizer;
#[cfg(test)]
mod testing_utils;

const ARCHIVIST_MONIKER: &str = "/bootstrap/archivist";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(FfxTool)]
pub struct LogTool {
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
    rcs_proxy: RemoteControlProxy,
    #[command]
    cmd: FfxLogCommand,
}

fho::embedded_plugin!(LogTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for LogTool {
    type Writer = MachineWriter<LogEntry>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        log_main(
            writer,
            self.rcs_proxy,
            self.target_collection,
            self.cmd.cmd,
            LogSymbolizer::new(),
        )
        .await?;
        Ok(())
    }
}

// Main entrypoint called from other plugins
pub async fn log_impl(
    writer: impl ToolIO<OutputItem = LogEntry> + Write + 'static,
    rcs_proxy: RemoteControlProxy,
    target_collection_proxy: TargetCollectionProxy,
    cmd: LogCommand,
) -> Result<(), LogError> {
    log_main(writer, rcs_proxy, target_collection_proxy, cmd, LogSymbolizer::new()).await
}

// Main logging event loop.
async fn log_main(
    writer: impl ToolIO<OutputItem = LogEntry> + Write + 'static,
    rcs_proxy: RemoteControlProxy,
    target_collection_proxy: TargetCollectionProxy,
    cmd: LogCommand,
    symbolizer: impl Symbolizer,
) -> Result<(), LogError> {
    let is_json = writer.is_machine();
    let node_name = rcs_proxy.identify_host().await??.nodename;
    let target_query = TargetQuery { string_matcher: node_name, ..Default::default() };
    let formatter = DefaultLogFormatter::new(
        LogFilterCriteria::try_from(&cmd)?,
        writer,
        LogFormatterOptions {
            // TODO(https://fxbug.dev/121413): Add support for log options.
            display: if is_json {
                None
            } else {
                Some(LogTextDisplayOptions {
                    show_tags: !cmd.hide_tags,
                    color: if cmd.no_color { LogTextColor::None } else { LogTextColor::BySeverity },
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
            raw: cmd.raw,
            // TODO(https://fxbug.dev/132262): Remove this when possible.
            highlight_spam: false,
            since: DeviceOrLocalTimestamp::new(cmd.since.as_ref(), cmd.since_monotonic.as_ref()),
            until: DeviceOrLocalTimestamp::new(cmd.until.as_ref(), cmd.until_monotonic.as_ref()),
        },
    );
    log_loop(target_collection_proxy, target_query, cmd, formatter, symbolizer).await?;
    Ok(())
}

struct DeviceConnection {
    boot_timestamp: u64,
    log_socket: fuchsia_async::Socket,
    log_settings_client: LogSettingsProxy,
}

// TODO(https://fxbug.dev/129624): Remove this once Overnet
// has support for reconnect handling.
async fn connect_to_target(
    target_collection_proxy: &TargetCollectionProxy,
    query: &TargetQuery,
    stream_mode: &mut fidl_fuchsia_diagnostics::StreamMode,
    prev_timestamp: Option<u64>,
) -> Result<DeviceConnection, LogError> {
    // Connect to device
    let rcs_client = connect_to_rcs(target_collection_proxy, query).await?;
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
    let (diagnostics_client, diagnostics_server) = create_proxy::<ArchiveAccessorMarker>()?;
    rcs::connect_with_timeout::<ArchiveAccessorMarker>(
        TIMEOUT,
        ARCHIVIST_MONIKER,
        &rcs_client,
        diagnostics_server.into_channel(),
    )
    .await?;
    // Connect to LogSettings
    let (log_settings_client, log_settings_server) = create_proxy::<LogSettingsMarker>()?;
    rcs::connect_with_timeout::<LogSettingsMarker>(
        TIMEOUT,
        ARCHIVIST_MONIKER,
        &rcs_client,
        log_settings_server.into_channel(),
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
        log_socket: fuchsia_async::Socket::from_socket(local)?,
        log_settings_client,
    })
}

async fn connect_to_rcs(
    target_collection_proxy: &TargetCollectionProxy,
    query: &TargetQuery,
) -> Result<RemoteControlProxy, LogError> {
    let (client, server) = create_proxy()?;
    target_collection_proxy.open_target(query, server).await??;
    let (rcs_client, rcs_server) = create_proxy()?;
    client.open_remote_control(rcs_server).await??;
    Ok(rcs_client)
}

async fn log_loop<W>(
    target_collection_proxy: TargetCollectionProxy,
    target_query: TargetQuery,
    cmd: LogCommand,
    mut formatter: impl LogFormatter + BootTimeAccessor + WriterContainer<W>,
    symbolizer: impl Symbolizer,
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write,
{
    let symbolizer_channel = SymbolizerChannel::new(symbolizer).await?;
    let mut stream_mode = get_stream_mode(cmd.clone())?;
    // TODO(https://fxbug.dev/129624): Add support for reconnect handling to Overnet.
    // This plugin needs special logic to handle reconnects as logging should tolerate
    // a device rebooting and remaining in a consistent state (automatically) after the reboot.
    // Eventually we should have direct support for this in Overnet, but for now we have to
    // handle reconnects manually.
    let mut prev_timestamp = None;
    loop {
        let connection = connect_to_target(
            &target_collection_proxy,
            &target_query,
            &mut stream_mode,
            prev_timestamp,
        )
        .await?;
        prev_timestamp = Some(connection.boot_timestamp);
        if !cmd.select.is_empty() {
            connection.log_settings_client.set_interest(&cmd.select).await?;
        }
        formatter.set_boot_timestamp(connection.boot_timestamp as i64);
        let maybe_err =
            dump_logs_from_socket(connection.log_socket, &mut formatter, &symbolizer_channel).await;
        if stream_mode == fidl_fuchsia_diagnostics::StreamMode::Snapshot {
            break;
        }
        if let Err(value) = maybe_err {
            writeln!(formatter.writer().stderr(), "{value}")?;
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
    use crate::testing_utils::{
        handle_rcs_connection, handle_target_collection_connection, Configuration, TaskManager,
        TestEvent,
    };
    use assert_matches::assert_matches;
    use chrono::{Local, TimeZone};
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use ffx_writer::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::TargetCollectionMarker;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use fidl_fuchsia_diagnostics::StreamMode;
    use futures::{future::poll_fn, Future, StreamExt};
    use log_command::{
        log_formatter::{LogData, TIMESTAMP_FORMAT},
        parse_seconds_string_as_duration, parse_time, DumpCommand,
    };
    use log_symbolizer::{FakeSymbolizerForTest, NoOpSymbolizer};
    use selectors::parse_log_interest_selector;
    use std::{
        pin::{pin, Pin},
        rc::Rc,
        task::Poll,
    };

    const TEST_STR: &str = "[1980-01-01 00:00:03.000][ffx] INFO: Hello world 2!\u{1b}[m\n";

    #[fuchsia::test]
    async fn symbolizer_replaces_markers_with_symbolized_logs() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = FakeSymbolizerForTest::new("prefix", vec![]);
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("{{{reset}}}")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("{{{mmap:something}}")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("not_real")
                .build(),
            ],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string().split('\n').collect::<Vec<_>>(),
            vec![
                "[00000.000000][ffx] INFO: prefix{{{reset}}}\u{1b}[m",
                "[00000.000000][ffx] INFO: prefix{{{mmap:something}}\u{1b}[m",
                "[00000.000000][ffx] INFO: not_real\u{1b}[m",
                ""
            ]
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn json_logger_test() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let task_manager = TaskManager::new();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(Some(Format::Json), &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<LogEntry>(&test_buffers.stdout.into_string()).unwrap(),
            LogEntry {
                timestamp: 1.into(),
                data: LogData::TargetLog(
                    LogsDataBuilder::new(BuilderArgs {
                        component_url: Some("ffx".into()),
                        moniker: "ffx".into(),
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
    async fn logger_prints_error_if_both_dump_and_since_now_are_combined() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            since: Some(parse_time("now").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let task_manager = TaskManager::new();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));

        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        assert_matches!(main_result.next().await.unwrap(), Err(LogError::DumpWithSinceNow));
    }

    #[fuchsia::test]
    async fn logger_prints_hello_world_message_and_exits_in_snapshot_mode() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_does_not_color_logs_if_disabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            no_color: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_metadata_if_enabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            show_metadata: true,
            no_color: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][1][2][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_utc_time_if_enabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            clock: TimeFormat::Utc,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(1),
            })
            .set_pid(1)
            .set_tid(2)
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1970-01-01 00:00:00.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_filtered_by_severity() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            clock: TimeFormat::Utc,
            severity: Severity::Error,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Error,
                    timestamp_nanos: Timestamp::from(3000000000i64),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(6000000000i64),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "\u{1b}[38;5;1m[1970-01-01 00:00:03.000][ffx] ERROR: Hello world!\u{1b}[m\n"
                .to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    async fn check_for_message(
        runner: &mut Pin<&mut impl Future<Output = ()>>,
        test_buffers: &fho::macro_deps::ffx_writer::TestBuffers,
        msg: &str,
    ) {
        poll_fn(|mut context| {
            // When in streaming mode, we should never exit.
            assert_eq!(runner.as_mut().poll(&mut context), Poll::Pending);

            // check messages until we get the Hello World message.
            if test_buffers.stdout.clone().into_string() == msg {
                return Poll::Ready(());
            }
            Poll::Pending
        })
        .await;
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Watch(WatchCommand {})),
            clock: TimeFormat::Local,
            since: Some(log_command::DetailedDateTime {
                is_now: true,
                ..parse_time("1980-01-01T00:00:01").unwrap()
            }),
            until: None,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:00")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:03")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world 2!")
                .build(),
            ],
            send_mode_event: true,
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));

        // Run the stream until we get the expected message.
        let mut runner = pin!(task_manager.run());
        check_for_message(&mut runner, &test_buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        scheduler.config.boot_timestamp.set(42);
        check_for_message(&mut runner, &test_buffers, TEST_STR).await;
        // Second connection has a different timestamp so should be treated
        // as a reboot.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots_heuristic() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Watch(WatchCommand {})),
            clock: TimeFormat::Local,
            since: Some(log_command::DetailedDateTime {
                is_now: true,
                ..parse_time("1980-01-01T00:00:01").unwrap()
            }),
            until: None,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:00")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:03")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world 2!")
                .build(),
            ],
            send_mode_event: true,
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));

        // Run the stream until we get the expected message.
        let mut runner = pin!(task_manager.run());
        check_for_message(&mut runner, &test_buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        check_for_message(&mut runner, &test_buffers, TEST_STR).await;

        // Second connection has a matching timestamp to the first one, so we should
        // Subscribe to not repeat messages.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        // For the third connection, we should get a
        // SnapshotThenSubscribe request because the timestamp
        // changed and it's clear it's actually a separate boot not a disconnect/reconnect
        scheduler.config.boot_timestamp.set(42);
        check_for_message(&mut runner, &test_buffers, TEST_STR).await;
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            clock: TimeFormat::Local,
            since: Some(parse_time("1980-01-01T00:00:01").unwrap()),
            until: Some(parse_time("1980-01-01T00:00:05").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:00")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:03")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(
                        parse_time("1980-01-01T00:00:06")
                            .unwrap()
                            .time
                            .naive_utc()
                            .timestamp_nanos(),
                    ),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1980-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_monotonic() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            clock: TimeFormat::Utc,
            since_monotonic: Some(parse_seconds_string_as_duration("1").unwrap()),
            until_monotonic: Some(parse_seconds_string_as_duration("5").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(3000000000i64),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".into()),
                    moniker: "ffx".into(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(6000000000i64),
                })
                .set_pid(1)
                .set_tid(2)
                .set_message("Hello world!")
                .build(),
            ],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1970-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_local_time_if_enabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            clock: TimeFormat::Local,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(1),
            })
            .set_pid(1)
            .set_tid(2)
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            format!(
                "[{}][ffx] INFO: Hello world!\u{1b}[m\n",
                Local.timestamp(0, 1).format(TIMESTAMP_FORMAT)
            )
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_tags_by_default() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_full_moniker_by_default() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "host/ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_full_moniker_when_enabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            show_full_moniker: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "host/ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][host/ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_tag_when_hidden() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            hide_tags: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_sets_severity_appropriately_then_exits() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let severity = vec![parse_log_interest_selector("archivist.cm#TRACE").unwrap()];
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            select: severity.clone(),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::SeverityChanged(s)) if s == severity);
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_file_names_by_default() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .set_file("test_filename.cc")
            .set_line(42)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: [test_filename.cc(42)] Hello world!\u{1b}[m\n"
                .to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_filename_if_disabled() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: log_command::SessionSpec::Relative(0),
            })),
            hide_file: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut task_manager = TaskManager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .set_file("test_filename.cc")
            .set_line(42)
            .add_tag("test tag")
            .set_message("Hello world!")
            .build()],
            ..Default::default()
        }));
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            symbolizer,
        ));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
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
