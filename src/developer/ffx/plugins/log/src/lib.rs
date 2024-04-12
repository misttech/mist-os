// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use error::LogError;
use ffx_log_args::LogCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_ffx::{TargetCollectionProxy, TargetQuery};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::{LogSettingsMarker, LogSettingsProxy, StreamParameters};
use fidl_fuchsia_diagnostics_host::ArchiveAccessorMarker;
use log_command::{
    log_formatter::{
        dump_logs_from_socket, BootTimeAccessor, DefaultLogFormatter, LogEntry, LogFormatter,
        Symbolize, WriterContainer,
    },
    InstanceGetter, LogSubCommand, SymbolizeMode, WatchCommand,
};
use log_symbolizer::LogSymbolizer;
use std::{fmt::Debug, io::Write};
use symbolizer::NonTransactionalSymbolizer;
use tokio::io::{AsyncRead, AsyncWrite};
use transactional_symbolizer::{RealSymbolizerProcess, SymbolizerProcess, TransactionalSymbolizer};

// NOTE: This is required for the legacy ffx toolchain
// which automatically adds ffx_core even though we don't use it.
use ffx_core as _;

mod condition_variable;
mod error;
mod mutex;
mod symbolizer;
#[cfg(test)]
mod testing_utils;
mod transactional_symbolizer;

const ARCHIVIST_MONIKER: &str = "bootstrap/archivist";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(FfxTool)]
pub struct LogTool {
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
    rcs_proxy: RemoteControlProxy,
    #[command]
    cmd: LogCommand,
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
        log_impl(writer, self.rcs_proxy, self.target_collection, self.cmd).await?;
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
    let enable_transactional_symbolizer =
        ffx_config::get("log_cmd.pretty_backtraces").await.unwrap_or(false);
    let mut disable_prettification = false;
    let mut no_symbolize = cmd.no_symbolize;
    // TODO(b/299980894): Clean this up once no longer needed.
    if cmd.no_symbolize || cmd.raw {
        eprintln!(concat!(
            "WARNING: --no-symbolize and --raw have been deprecated",
            " and replaced with --symbolize."
        ));
        eprintln!("These legacy options will eventually be removed.");
        if cmd.raw {
            no_symbolize = true;
        }
    } else {
        match cmd.symbolize {
            SymbolizeMode::Off => {
                no_symbolize = true;
            }
            SymbolizeMode::Pretty => {}
            SymbolizeMode::Classic => {
                disable_prettification = true;
            }
        }
    }
    let instance_getter = rcs::root_realm_query(&rcs_proxy, TIMEOUT).await?;
    log_main(
        writer,
        rcs_proxy,
        target_collection_proxy,
        cmd,
        if no_symbolize {
            None
        } else {
            if enable_transactional_symbolizer {
                Some(EitherSymbolizer::Left(TransactionalSymbolizer::new(
                    RealSymbolizerProcess::new(!disable_prettification).await?,
                )?))
            } else {
                Some(EitherSymbolizer::Right(LogSymbolizer::new()))
            }
        },
        instance_getter,
    )
    .await
}

/// Temporary compatibility enum allowing for the usage of Symbolize
/// and Symbolizer, until the legacy Symbolizer trait is removed.
/// This is needed because we can't implement a foreign trait (Symbolize)
/// on a foreign type (Either), and we likely don't want to define
/// this in log_formatter as this is specific to ffx and not needed
/// on the device.
enum EitherSymbolizer<A, B> {
    Left(A),
    Right(B),
}

/// Temporary compatibility trait allowing for the usage of Symbolize
/// and Symbolizer, until the legacy Symbolizer trait is removed.
trait AsyncIntoSymbolize {
    async fn into_symbolize(self) -> Result<impl Symbolize, LogError>;
}

impl AsyncIntoSymbolize for LogSymbolizer {
    async fn into_symbolize(self) -> Result<impl Symbolize, LogError> {
        NonTransactionalSymbolizer::new(self).await
    }
}

impl<T: SymbolizerProcess + 'static> AsyncIntoSymbolize for TransactionalSymbolizer<T>
where
    T::Stdin: AsyncWrite + Unpin + Debug,
    T::Stdout: AsyncRead + Unpin + Debug,
{
    async fn into_symbolize(self) -> Result<impl Symbolize, LogError> {
        Ok(self)
    }
}

impl<A: AsyncIntoSymbolize, B: AsyncIntoSymbolize> AsyncIntoSymbolize for EitherSymbolizer<A, B> {
    async fn into_symbolize(self) -> Result<impl Symbolize, LogError> {
        Ok(match self {
            EitherSymbolizer::Left(result) => {
                EitherSymbolizer::Left(result.into_symbolize().await?)
            }
            EitherSymbolizer::Right(result) => {
                EitherSymbolizer::Right(result.into_symbolize().await?)
            }
        })
    }
}

#[async_trait(?Send)]
impl<A: Symbolize, B: Symbolize> Symbolize for EitherSymbolizer<A, B> {
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry> {
        match self {
            EitherSymbolizer::Left(value) => value.symbolize(entry).await,
            EitherSymbolizer::Right(value) => value.symbolize(entry).await,
        }
    }
}

// Main logging event loop.
async fn log_main<W>(
    writer: W,
    rcs_proxy: RemoteControlProxy,
    target_collection_proxy: TargetCollectionProxy,
    cmd: LogCommand,
    symbolizer: Option<impl AsyncIntoSymbolize>,
    instance_getter: impl InstanceGetter,
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write + 'static,
{
    let node_name = rcs_proxy.identify_host().await??.nodename;
    let target_query = TargetQuery { string_matcher: node_name, ..Default::default() };
    let formatter = DefaultLogFormatter::<W>::new_from_args(&cmd, writer);
    log_loop(target_collection_proxy, target_query, cmd, formatter, symbolizer, &instance_getter)
        .await?;
    Ok(())
}

struct DeviceConnection {
    boot_timestamp: u64,
    log_socket: fuchsia_async::Socket,
    log_settings_client: LogSettingsProxy,
}

// TODO(https://fxbug.dev/42080003): Remove this once Overnet
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

async fn connect_to_rcs(
    target_collection_proxy: &TargetCollectionProxy,
    query: &TargetQuery,
) -> Result<RemoteControlProxy, LogError> {
    let (client, server) = fidl::endpoints::create_proxy()?;
    target_collection_proxy.open_target(query, server).await??;
    let (rcs_client, rcs_server) = fidl::endpoints::create_proxy()?;
    client.open_remote_control(rcs_server).await??;
    Ok(rcs_client)
}

async fn log_loop<W>(
    target_collection_proxy: TargetCollectionProxy,
    target_query: TargetQuery,
    cmd: LogCommand,
    mut formatter: impl LogFormatter + BootTimeAccessor + WriterContainer<W>,
    symbolizer: Option<impl AsyncIntoSymbolize>,
    realm_query: &impl InstanceGetter,
) -> Result<(), LogError>
where
    W: ToolIO<OutputItem = LogEntry> + Write,
{
    let symbolizer_channel: Box<dyn Symbolize> = match symbolizer {
        Some(inner) => Box::new(inner.into_symbolize().await?),
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
            let maybe_connection = connect_to_target(
                &target_collection_proxy,
                &target_query,
                &mut stream_mode,
                prev_timestamp,
            )
            .await;
            if let Ok(connected) = maybe_connection {
                connection = connected;
                break;
            }
            backoff += 1;
            if backoff > 10 {
                backoff = 10;
            }
            let err = format!("{}", anyhow::Error::from(maybe_connection.err().unwrap()));
            if matches!(&last_error, Some(value) if *value == err) {
                eprintln!("Error connecting to device, retrying in {backoff} seconds.");
            } else {
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
        let maybe_err = dump_logs_from_socket(
            connection.log_socket,
            &mut formatter,
            symbolizer_channel.as_ref(),
        )
        .await;
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
        handle_rcs_connection, handle_target_collection_connection, Configuration, Manager,
        TestEvent,
    };
    use assert_matches::assert_matches;
    use async_trait::async_trait;
    use chrono::{Local, TimeZone};
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use ffx_writer::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::TargetCollectionMarker;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use fidl_fuchsia_diagnostics::StreamMode;
    use fuchsia_async::Task;
    use futures::StreamExt;
    use log_command::{
        log_formatter::{LogData, TIMESTAMP_FORMAT},
        parse_seconds_string_as_duration, parse_time, DumpCommand, TimeFormat,
    };
    use log_symbolizer::{FakeSymbolizerForTest, NoOpSymbolizer};
    use moniker::Moniker;
    use selectors::parse_log_interest_selector;
    use std::rc::Rc;

    const TEST_STR: &str = "[1980-01-01 00:00:03.000][ffx] INFO: Hello world 2!\u{1b}[m\n";

    #[derive(Default)]
    struct FakeInstanceGetter {
        output: Vec<Moniker>,
        expected_selector: Option<String>,
    }

    impl AsyncIntoSymbolize for NoOpSymbolizer {
        async fn into_symbolize(self) -> Result<impl Symbolize, LogError> {
            NonTransactionalSymbolizer::new(self).await
        }
    }

    impl AsyncIntoSymbolize for FakeSymbolizerForTest {
        async fn into_symbolize(self) -> Result<impl Symbolize, LogError> {
            NonTransactionalSymbolizer::new(self).await
        }
    }

    #[async_trait(?Send)]
    impl InstanceGetter for FakeInstanceGetter {
        async fn get_monikers_from_query(
            &self,
            query: &str,
        ) -> Result<Vec<Moniker>, log_command::LogError> {
            if let Some(expected) = &self.expected_selector {
                assert_eq!(expected, query);
            }
            Ok(self.output.clone())
        }
    }

    #[fuchsia::test]
    async fn symbolizer_replaces_markers_with_symbolized_logs() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = FakeSymbolizerForTest::new("prefix", vec![]);
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

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
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let manager = Manager::new();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(Some(Format::Json), &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");
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
    async fn logger_prints_error_if_ambiguous_selector() {
        let (rcs_proxy, rcs_server) =
            fidl::endpoints::create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy::<TargetCollectionMarker>().unwrap();

        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let manager = Manager::new();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![
            Moniker::try_from("core/some/ambiguous_selector:thing/test").unwrap(),
            Moniker::try_from("core/other/ambiguous_selector:thing/test").unwrap(),
        ];

        // Main should return an error
        let error = format!(
            "{}",
            log_main(
                MachineWriter::<LogEntry>::new_test(None, &test_buffers),
                rcs_proxy,
                target_collection_proxy,
                cmd,
                Some(symbolizer),
                getter,
            )
            .await
            .expect_err("log_main succeeded")
        );

        const EXPECTED_INTEREST_ERROR: &str = r#"WARN: One or more of your selectors appears to be ambiguous
and may not match any components on your system.

If this is unintentional you can explicitly match using the
following command:

ffx log \
	--select core/some/ambiguous_selector\\:thing/test#INFO \
	--select core/other/ambiguous_selector\\:thing/test#INFO

If this is intentional, you can disable this with
ffx log --force-select.
"#;
        assert_eq!(error, EXPECTED_INTEREST_ERROR);
    }

    #[fuchsia::test]
    async fn logger_translates_selector_if_one_match() {
        let (rcs_proxy, rcs_server) =
            fidl::endpoints::create_proxy::<RemoteControlMarker>().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy::<TargetCollectionMarker>().unwrap();

        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new();
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![Moniker::try_from("core/some/ambiguous_selector").unwrap()];
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            getter,
        )
        .await
        .expect("log_main failed");

        let severity =
            vec![parse_log_interest_selector("core/some/ambiguous_selector#INFO").unwrap()];
        assert_matches!(event_stream.next().await, Some(TestEvent::SeverityChanged(s)) if s == severity);
    }

    #[fuchsia::test]
    async fn logger_prints_error_if_both_dump_and_since_now_are_combined() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            since: Some(parse_time("now").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let manager = Manager::new();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();

        assert_matches!(
            log_main(
                MachineWriter::<LogEntry>::new_test(None, &test_buffers),
                rcs_proxy,
                target_collection_proxy,
                cmd,
                Some(symbolizer),
                FakeInstanceGetter::default(),
            )
            .await,
            Err(LogError::DumpWithSinceNow)
        );
    }

    #[fuchsia::test]
    async fn logger_prints_hello_world_message_and_exits_in_snapshot_mode() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new();
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_does_not_color_logs_if_disabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            no_color: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new();
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_metadata_if_enabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            show_metadata: true,
            no_color: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new();
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][1][2][ffx] INFO: Hello world!\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_utc_time_if_enabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            clock: TimeFormat::Utc,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1970-01-01 00:00:00.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_filtered_by_severity() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            clock: TimeFormat::Utc,
            severity: Severity::Error,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "\u{1b}[38;5;1m[1970-01-01 00:00:03.000][ffx] ERROR: Hello world!\u{1b}[m\n"
                .to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    async fn check_for_message(test_buffers: &fho::macro_deps::ffx_writer::TestBuffers, msg: &str) {
        while test_buffers.stdout.clone().into_string() != msg {
            test_buffers.stdout.wait_ready().await;
        }
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
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
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(
                    parse_time("1980-01-01T00:00:03").unwrap().time.naive_utc().timestamp_nanos(),
                ),
            })
            .set_pid(1)
            .set_tid(2)
            .set_message("Hello world 2!")
            .build()],
            send_mode_event: true,
            ..Default::default()
        }));
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = Task::local(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        ));

        // Run the stream until we get the expected message.
        check_for_message(&test_buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        environment.reboot_target(42);

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        check_for_message(&test_buffers, TEST_STR).await;

        // Second connection has a different timestamp so should be treated
        // as a reboot.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );

        environment.disconnect_target();

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_across_reboots_heuristic() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
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
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(
                    parse_time("1980-01-01T00:00:03").unwrap().time.naive_utc().timestamp_nanos(),
                ),
            })
            .set_pid(1)
            .set_tid(2)
            .set_message("Hello world 2!")
            .build()],
            send_mode_event: true,
            ..Default::default()
        }));
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();

        // Intentionally unused. When in streaming mode, this should never return a value.
        let _result = Task::local(log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        ));

        // Run the stream until we get the expected message.
        check_for_message(&test_buffers, TEST_STR).await;

        // First connection should have used Subscribe mode.
        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::Subscribe))
        );

        environment.disconnect_target();

        // Device is paused when we exit the loop because there's nothing
        // polling the future.
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        // We should reconnect and get another message.
        check_for_message(&test_buffers, TEST_STR).await;

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

        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));

        check_for_message(&test_buffers, TEST_STR).await;

        assert_matches!(
            event_stream.next().await,
            Some(TestEvent::Connected(StreamMode::SnapshotThenSubscribe))
        );
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            clock: TimeFormat::Local,
            since: Some(parse_time("1980-01-01T00:00:01").unwrap()),
            until: Some(parse_time("1980-01-01T00:00:05").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1980-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_logs_since_specific_timestamp_monotonic() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            clock: TimeFormat::Utc,
            since_monotonic: Some(parse_seconds_string_as_duration("1").unwrap()),
            until_monotonic: Some(parse_seconds_string_as_duration("5").unwrap()),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[1970-01-01 00:00:03.000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_local_time_if_enabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            clock: TimeFormat::Local,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

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
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_full_moniker_by_default() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_full_moniker_when_enabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            show_full_moniker: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][host/ffx][test tag] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_tag_when_hidden() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            hide_tags: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_sets_severity_appropriately_then_exits() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let severity = vec![parse_log_interest_selector("archivist.cm#TRACE").unwrap()];
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: severity.clone(),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new();
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx] INFO: Hello world!\u{1b}[m\n".to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::SeverityChanged(s)) if s == severity);
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_shows_file_names_by_default() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

        assert_eq!(
            test_buffers.stdout.into_string(),
            "[00000.000000][ffx][test tag] INFO: [test_filename.cc(42)] Hello world!\u{1b}[m\n"
                .to_string()
        );
        assert_matches!(event_stream.next().await, Some(TestEvent::LogSettingsConnectionClosed));
    }

    #[fuchsia::test]
    async fn logger_hides_filename_if_disabled() {
        let (rcs_proxy, rcs_server) = fidl::endpoints::create_proxy().unwrap();
        let (target_collection_proxy, target_collection_server) =
            fidl::endpoints::create_proxy().unwrap();
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            hide_file: true,
            ..LogCommand::default()
        };
        let symbolizer = NoOpSymbolizer::new();
        let mut manager = Manager::new_with_config(Rc::new(Configuration {
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
        let mut event_stream = manager.take_event_stream().unwrap();
        let environment = manager.get_environment();
        Task::local(handle_rcs_connection(rcs_server, environment.clone())).detach();
        Task::local(handle_target_collection_connection(
            target_collection_server,
            environment.clone(),
        ))
        .detach();
        let test_buffers = TestBuffers::default();
        log_main(
            MachineWriter::<LogEntry>::new_test(None, &test_buffers),
            rcs_proxy,
            target_collection_proxy,
            cmd,
            Some(symbolizer),
            FakeInstanceGetter::default(),
        )
        .await
        .expect("log_main failed");

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
