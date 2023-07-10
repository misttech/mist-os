// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Write;

use diagnostics_data::LogTextDisplayOptions;
use error::LogError;
use fho::{daemon_protocol, AvailabilityFlag, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_developer_ffx::{TargetCollectionProxy, TargetQuery};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::{LogSettingsMarker, LogSettingsProxy, StreamParameters};
use fidl_fuchsia_diagnostics_host::ArchiveAccessorMarker;
use log_command::{
    filter::LogFilterCriteria,
    log_formatter::{
        dump_logs_from_socket, BootTimeAccessor, DefaultLogFormatter, LogEntry, LogFormatter,
        LogFormatterOptions, NoOpSymbolizer, WriterContainer,
    },
    LogCommand, LogSubCommand, WatchCommand,
};

mod error;
pub mod spam_filter;
#[cfg(test)]
mod testing_utils;

const ARCHIVIST_MONIKER: &str = "/bootstrap/archivist";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_log_ng.enabled"))]
pub struct LogTool {
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
    rcs_proxy: RemoteControlProxy,
    #[command]
    cmd: LogCommand,
}

#[async_trait::async_trait(?Send)]
impl FfxMain for LogTool {
    type Writer = MachineWriter<LogEntry>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        log_main(writer, self.rcs_proxy, self.target_collection, self.cmd).await?;
        Ok(())
    }
}

// Main logging event loop.
async fn log_main(
    writer: impl ToolIO<OutputItem = LogEntry> + Write + 'static,
    rcs_proxy: RemoteControlProxy,
    target_collection_proxy: TargetCollectionProxy,
    cmd: LogCommand,
) -> Result<(), LogError> {
    let is_json = writer.is_machine();
    let node_name = rcs_proxy.identify_host().await??.nodename;
    let target_query = TargetQuery { string_matcher: node_name, ..Default::default() };
    log_loop(
        target_collection_proxy,
        target_query,
        cmd,
        DefaultLogFormatter::new(
            LogFilterCriteria::default(),
            writer,
            LogFormatterOptions {
                // TODO(https://fxbug.dev/121413): Add support for log options.
                display: if is_json { None } else { Some(LogTextDisplayOptions::default()) },
                ..Default::default()
            },
        ),
    )
    .await?;
    Ok(())
}

struct DeviceConnection {
    log_socket: fuchsia_async::Socket,
    log_settings_client: LogSettingsProxy,
}

// TODO(https://fxbug.dev/129624): Remove this once Overnet
// has support for reconnect handling.
async fn connect_to_target(
    target_collection_proxy: &TargetCollectionProxy,
    query: &TargetQuery,
    stream_mode: fidl_fuchsia_diagnostics::StreamMode,
) -> Result<DeviceConnection, LogError> {
    // Connect to device
    let rcs_client = connect_to_rcs(target_collection_proxy, query).await?;
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
                stream_mode: Some(stream_mode),
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
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write,
{
    let stream_mode = get_stream_mode(cmd.clone())?;
    // TODO(https://fxbug.dev/129624): Add support for reconnect handling to Overnet.
    // This plugin needs special logic to handle reconnects as logging should tolerate
    // a device rebooting and remaining in a consistent state (automatically) after the reboot.
    // Eventually we should have direct support for this in Overnet, but for now we have to
    // handle reconnects manually.
    loop {
        let connection =
            connect_to_target(&target_collection_proxy, &target_query, stream_mode).await?;
        if !cmd.select.is_empty() {
            connection.log_settings_client.set_interest(&cmd.select).await?;
        }
        let maybe_err =
            dump_logs_from_socket(connection.log_socket, &mut formatter, &NoOpSymbolizer {}).await;
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
        handle_rcs_connection, handle_target_collection_connection, TaskManager, TestEvent,
    };
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use ffx_writer::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::TargetCollectionMarker;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use futures::StreamExt;
    use log_command::{log_formatter::LogData, parse_time, DumpCommand};
    use selectors::parse_log_interest_selector;

    #[fuchsia::test]
    async fn json_logger_test() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let tool = LogTool {
            cmd: LogCommand {
                sub_command: Some(LogSubCommand::Dump(DumpCommand {
                    session: log_command::SessionSpec::Relative(0),
                })),
                ..LogCommand::default()
            },
            rcs_proxy: rcs_proxy,
            target_collection: target_collection_proxy,
        };
        let task_manager = TaskManager::new();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager.spawn_result(
            tool.main(MachineWriter::<LogEntry>::new_test(Some(Format::Json), &test_buffers)),
        );
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<LogEntry>(&test_buffers.stdout.into_string()).unwrap(),
            LogEntry {
                timestamp: 0.into(),
                data: LogData::TargetLog(
                    LogsDataBuilder::new(BuilderArgs {
                        component_url: Some("ffx".into()),
                        moniker: "ffx".into(),
                        severity: Severity::Info,
                        timestamp_nanos: Timestamp::from(0),
                    })
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
        let tool = LogTool {
            cmd: LogCommand {
                sub_command: Some(LogSubCommand::Dump(DumpCommand {
                    session: log_command::SessionSpec::Relative(0),
                })),
                since: Some(parse_time("now").unwrap()),
                ..LogCommand::default()
            },
            rcs_proxy: rcs_proxy,
            target_collection: target_collection_proxy,
        };
        let task_manager = TaskManager::new();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager
            .spawn_result(tool.main(MachineWriter::<LogEntry>::new_test(None, &test_buffers)));

        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        assert_matches!(
            main_result.next().await.unwrap().map_err(|value| {
                assert_matches!(value, fho::Error::User(value)=>value)
                    .downcast::<LogError>()
                    .unwrap()
            }),
            Err(LogError::DumpWithSinceNow)
        );
    }

    #[fuchsia::test]
    async fn logger_prints_hello_world_message_and_exits_in_snapshot_mode() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let tool = LogTool {
            cmd: LogCommand {
                sub_command: Some(LogSubCommand::Dump(DumpCommand {
                    session: log_command::SessionSpec::Relative(0),
                })),
                ..LogCommand::default()
            },
            rcs_proxy: rcs_proxy,
            target_collection: target_collection_proxy,
        };
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager
            .spawn_result(tool.main(MachineWriter::<LogEntry>::new_test(None, &test_buffers)));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][][][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_sets_severity_appropriately_then_exits() {
        let (rcs_proxy, rcs_server) = create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            create_proxy::<TargetCollectionMarker>().unwrap();
        let severity = vec![parse_log_interest_selector("archivist.cm#TRACE").unwrap()];
        let tool = LogTool {
            cmd: LogCommand {
                sub_command: Some(LogSubCommand::Dump(DumpCommand {
                    session: log_command::SessionSpec::Relative(0),
                })),
                select: severity.clone(),
                ..LogCommand::default()
            },
            rcs_proxy: rcs_proxy,
            target_collection: target_collection_proxy,
        };
        let mut task_manager = TaskManager::new();
        let mut event_stream = task_manager.take_event_stream().unwrap();
        let scheduler = task_manager.get_scheduler();
        task_manager.spawn(handle_rcs_connection(rcs_server, scheduler.clone()));
        task_manager.spawn(handle_target_collection_connection(
            target_collection_server,
            scheduler.clone(),
        ));
        let test_buffers = TestBuffers::default();
        let mut main_result = task_manager
            .spawn_result(tool.main(MachineWriter::<LogEntry>::new_test(None, &test_buffers)));
        // Run all tasks until exit.
        task_manager.run().await;
        // Ensure that main exited successfully.
        main_result.next().await.unwrap().unwrap();

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][][][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::SeverityChanged(s)) if s == severity);
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
