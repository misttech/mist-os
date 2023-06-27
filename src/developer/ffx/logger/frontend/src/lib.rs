// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use async_channel::{unbounded, Receiver, Sender};
use async_trait::async_trait;
use chrono::{Local, TimeZone};
use diagnostics_data::{LogsData, Timestamp};
use errors::ffx_error;
use ffx_daemon_target::logger::{
    streamer::GenericDiagnosticsStreamer, write_logs_to_file, SymbolizerConfig,
};
use ffx_log_data::{EventType, LogData, LogEntry};
use ffx_log_utils::{run_logging_pipeline, OrderedBatchPipeline};
use fidl::{
    endpoints::{create_proxy, DiscoverableProtocolMarker as _, ServerEnd},
    AsyncSocket,
};
use fidl_fuchsia_developer_ffx::{
    DaemonDiagnosticsStreamParameters, DiagnosticsProxy, DiagnosticsStreamDiagnosticsResult,
    DiagnosticsStreamError, LogSession, OpenTargetError, SessionSpec, StreamMode,
    TargetCollectionProxy, TargetConnectionError, TargetQuery, TimeBound,
};
use fidl_fuchsia_developer_remotecontrol::{
    ArchiveIteratorEntry, ArchiveIteratorError, ArchiveIteratorGetNextResponder,
    ArchiveIteratorMarker, ArchiveIteratorProxy, ArchiveIteratorRequest,
    ArchiveIteratorRequestStream, ConnectCapabilityError, DiagnosticsData, IdentifyHostError,
    InlineData,
};
use fidl_fuchsia_diagnostics::StreamParameters;
use fidl_fuchsia_diagnostics_host::{ArchiveAccessorMarker, ArchiveAccessorProxy};
use fidl_fuchsia_io as fio;
use fuchsia_async::Timer;
use futures::{lock::Mutex, AsyncReadExt, AsyncWriteExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    iter::Iterator,
    sync::Arc,
    time::{Duration, SystemTime},
};
use thiserror::Error;
use timeout::timeout;

type ArchiveIteratorResult = Result<LogEntry, ArchiveIteratorError>;
const PIPELINE_SIZE: usize = 1;
const NO_STREAM_ERROR: &str = "\
The proactive logger isn't connected to this target.

Verify that the target is up with `ffx target list` and retry \
in a few seconds. If the issue persists, run the following command:

$ ffx doctor --restart-daemon";
const NANOS_IN_SECOND: i64 = 1_000_000_000;
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S.%3f";
const RETRY_TIMEOUT_MILLIS: u64 = 1000;
const READ_BUFFER_SIZE: usize = 1000 * 1000 * 5;
const ARCHIVIST_MONIKER: &str = "/bootstrap/archivist";

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

#[async_trait(?Send)]
pub trait LogFormatter {
    async fn push_log(&mut self, log_entry: ArchiveIteratorResult) -> Result<()>;
    fn set_boot_timestamp(&mut self, boot_ts_nanos: i64);
}

#[derive(Clone, Debug)]
pub struct LogCommandParameters {
    pub target_identifier: String,
    pub session: Option<SessionSpec>,
    pub from_bound: Option<TimeBound>,
    pub to_bound: Option<TimeBound>,
    pub stream_mode: StreamMode,
}

impl Default for LogCommandParameters {
    fn default() -> Self {
        Self {
            target_identifier: String::default(),
            session: Some(SessionSpec::Relative(0)),
            from_bound: None,
            to_bound: None,
            stream_mode: StreamMode::SnapshotAll,
        }
    }
}

#[async_trait(?Send)]
pub trait StreamDiagnostics {
    async fn r#stream_diagnostics(
        &mut self,
        target: Option<&str>,
        parameters: DaemonDiagnosticsStreamParameters,
        iterator: ServerEnd<ArchiveIteratorMarker>,
    ) -> Result<DiagnosticsStreamDiagnosticsResult, anyhow::Error>;
}

#[async_trait(?Send)]
impl StreamDiagnostics for DiagnosticsProxy {
    async fn stream_diagnostics(
        &mut self,
        target: Option<&str>,
        parameters: DaemonDiagnosticsStreamParameters,
        iterator: fidl::endpoints::ServerEnd<ArchiveIteratorMarker>,
    ) -> Result<fidl_fuchsia_developer_ffx::DiagnosticsStreamDiagnosticsResult, anyhow::Error> {
        return Ok((self as &DiagnosticsProxy)
            .stream_diagnostics(target, &parameters, iterator)
            .await?);
    }
}

struct ConnectionState {
    // Connection to the Overnet daemon, used
    // for connecting to devices.
    target_collection_proxy: TargetCollectionProxy,
    // Current boot timestamp
    boot_timestamp: u64,
    // Channel to send reconnects over (when the target connection is lost)
    running_daemon_channel: Option<Sender<(ServerEnd<ArchiveIteratorMarker>, AsyncSocket)>>,
    // Current connection handler
    current_task: Option<fuchsia_async::Task<Result<(), LogError>>>,
    // Streaming mode
    mode: Option<StreamMode>,
    // True if timestamp filtering should be disabled
    disable_timestamps: bool,
    // Previous boot timestamp, or 0 if not yet connected.
    last_boot_timestamp: u64,
}

pub struct RemoteDiagnosticsBridgeProxyWrapper {
    node_name: String,
    connection_state: Mutex<ConnectionState>,
}

#[derive(Debug, Error)]
enum LogError {
    #[error("failed to open the target: {:?}", error)]
    OpenTargetError { error: OpenTargetError },
    #[error("failed to connect to the target: {:?}", error)]
    TargetConnectionError { error: TargetConnectionError },
    #[error("failed to connect: {:?}", error)]
    ConnectCapabilityError { error: ConnectCapabilityError },
    #[error("FIDL error: {}", error)]
    FidlError {
        #[from]
        error: fidl::Error,
    },
    #[error("Unable to identify host: {:?}", error)]
    IdentifyHostError { error: IdentifyHostError },
    #[error("No boot timestamp was available")]
    NoBootTimestamp,
    #[error("Unknown error: {}", error)]
    UnknownError {
        #[from]
        error: anyhow::Error,
    },
    #[error("End of stream")]
    EndOfStream,
    #[error("IO error {}", error)]
    IOError {
        #[from]
        error: std::io::Error,
    },
    #[error("JSON error {}", error)]
    JsonError {
        #[from]
        error: serde_json::Error,
    },
    #[error("Peer closed")]
    PeerClosed,
}

impl From<OpenTargetError> for LogError {
    fn from(error: OpenTargetError) -> Self {
        Self::OpenTargetError { error }
    }
}

impl From<TargetConnectionError> for LogError {
    fn from(error: TargetConnectionError) -> Self {
        Self::TargetConnectionError { error }
    }
}

impl From<IdentifyHostError> for LogError {
    fn from(error: IdentifyHostError) -> Self {
        Self::IdentifyHostError { error }
    }
}

impl From<ConnectCapabilityError> for LogError {
    fn from(error: ConnectCapabilityError) -> Self {
        Self::ConnectCapabilityError { error }
    }
}

#[derive(Clone)]
struct LocalStreamParameters {
    daemon_parameters: DaemonDiagnosticsStreamParameters,
    boot_ts: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

struct JsonDecoder {
    socket: fuchsia_async::Socket,
    buffer: Vec<u8>,
    state: JsonDecoderState,
}

#[derive(PartialEq)]
enum JsonDecoderState {
    Init,
    ReadingMessage(Vec<u8>),
}

impl JsonDecoderState {
    fn take(&mut self) -> JsonDecoderState {
        let mut ret = JsonDecoderState::Init;
        std::mem::swap(self, &mut ret);
        ret
    }
}

impl JsonDecoder {
    fn new(socket: fuchsia_async::Socket) -> Self {
        let mut buffer = vec![];
        buffer.resize(READ_BUFFER_SIZE, 0);
        Self { socket, buffer, state: JsonDecoderState::Init }
    }

    async fn decode(&mut self) -> Result<Vec<LogsData>, LogError> {
        let mut out_vec = vec![];
        loop {
            let mut next_state = JsonDecoderState::Init;
            match self.state.take() {
                JsonDecoderState::Init => {
                    let bytes_read = self.socket.read(&mut self.buffer).await?;
                    if bytes_read == 0 {
                        return Err(LogError::PeerClosed);
                    }
                    // Try to decode message
                    let des = serde_json::Deserializer::from_slice(&self.buffer[..bytes_read]);
                    let mut stream = des.into_iter::<OneOrMany<LogsData>>();
                    while let Some(Ok(value)) = stream.next() {
                        handle_value(value, &mut out_vec);
                    }
                    // byte_offset is the last deserialized object, if any
                    if stream.byte_offset() != bytes_read {
                        next_state = JsonDecoderState::ReadingMessage(
                            self.buffer[stream.byte_offset()..bytes_read].to_vec(),
                        );
                    }
                }
                JsonDecoderState::ReadingMessage(mut msg) => {
                    let res = self.socket.read(&mut self.buffer).await?;
                    if res == 0 {
                        return Err(LogError::PeerClosed);
                    }
                    // append to msg
                    msg.append(&mut self.buffer[..res].to_vec());
                    let des = serde_json::Deserializer::from_slice(&msg);
                    let mut stream = des.into_iter::<OneOrMany<LogsData>>();
                    while let Some(Ok(value)) = stream.next() {
                        handle_value(value, &mut out_vec);
                    }
                    // byte_offset is the last deserialized object, if any
                    if stream.byte_offset() != msg.len() {
                        next_state =
                            JsonDecoderState::ReadingMessage(msg[stream.byte_offset()..].to_vec());
                    }
                }
            }
            self.state = next_state;
            if self.state == JsonDecoderState::Init {
                return Ok(out_vec);
            }
        }
    }
}

fn handle_value(
    value: OneOrMany<diagnostics_data::Data<diagnostics_data::Logs>>,
    out_vec: &mut Vec<diagnostics_data::Data<diagnostics_data::Logs>>,
) {
    match value {
        OneOrMany::One(one) => {
            out_vec.push(one);
        }
        OneOrMany::Many(mut many) => {
            out_vec.append(&mut many);
        }
    }
}

impl RemoteDiagnosticsBridgeProxyWrapper {
    pub fn new(target_collection_proxy: TargetCollectionProxy, node_name: String) -> Self {
        Self {
            node_name,
            connection_state: Mutex::new(ConnectionState {
                target_collection_proxy,
                boot_timestamp: 0,
                running_daemon_channel: None,
                current_task: None,
                mode: None,
                disable_timestamps: false,
                last_boot_timestamp: 0,
            }),
        }
    }

    async fn translate_logs(
        self: Arc<Self>,
        mut iterator: ServerEnd<ArchiveIteratorMarker>,
        mut proxy: AsyncSocket,
        mut new_connection_receiver: Receiver<(ServerEnd<ArchiveIteratorMarker>, AsyncSocket)>,
        parameters: LocalStreamParameters,
    ) -> Result<(), LogError> {
        let mut pending_request = None;
        loop {
            let symbolizer_config = SymbolizerConfig::new().await?;
            let (sender, receiver) = unbounded();
            let streamer = Arc::new(DirectStreamer::new(sender, symbolizer_config));
            let (daemon_iterator, translate_task) =
                write_logs_to_file(streamer.clone(), Some(&streamer.config))?;
            let this = self.clone();
            let this_2 = self.clone();
            let parameters = parameters.clone();
            let frontend = fuchsia_async::Task::local(async move {
                this_2
                    .frontend_to_daemon(
                        iterator.into_stream()?,
                        receiver,
                        pending_request.take(),
                        parameters,
                    )
                    .await
            });

            let _target_connection = fuchsia_async::Task::local(async move {
                this.daemon_to_target(daemon_iterator, proxy).await
            });

            // Run the translator until it exits
            let _ = translate_task.await;

            // Close the streaming channel, which should cause both the frontend and backend
            // task to exit.
            drop(streamer);
            // Upon reconnect, the daemon sends the first message
            // after reboot on the old channel, followed by subsequent messages on the
            // new channel.
            pending_request = Some(frontend.await?);
            {
                let state = self.connection_state.lock().await;
                if matches!(state.mode, Some(StreamMode::SnapshotAll)) {
                    // End of stream, terminate connection.
                    return Ok(());
                }
            }
            let (new_connection, new_device_proxy) =
                new_connection_receiver.next().await.ok_or(LogError::EndOfStream)?;
            iterator = new_connection;
            proxy = new_device_proxy;
        }
    }

    async fn frontend_to_daemon(
        self: Arc<Self>,
        mut server: ArchiveIteratorRequestStream,
        mut receiver: Receiver<LogEntry>,
        mut old_request: Option<ArchiveIteratorGetNextResponder>,
        parameters: LocalStreamParameters,
    ) -> Result<ArchiveIteratorGetNextResponder, LogError> {
        // Handle the old request if present.
        if let Some(responder) = old_request.take() {
            if let Some(responder) =
                self.handle_frontend_request(&mut receiver, responder, &parameters).await?
            {
                return Ok(responder);
            }
        }
        while let Some(Ok(ArchiveIteratorRequest::GetNext { responder })) = server.next().await {
            if let Some(responder) =
                self.handle_frontend_request(&mut receiver, responder, &parameters).await?
            {
                return Ok(responder);
            }
        }
        Err(LogError::EndOfStream)
    }

    async fn daemon_to_target(
        self: &Arc<Self>,
        daemon_iterator: ServerEnd<ArchiveIteratorMarker>,
        socket: AsyncSocket,
    ) -> Result<(), LogError> {
        let mut daemon_server = daemon_iterator.into_stream()?;
        let mut decoder = JsonDecoder::new(socket);
        while let Some(Ok(ArchiveIteratorRequest::GetNext { responder })) =
            daemon_server.next().await
        {
            let mode = self.connection_state.lock().await.mode.clone();
            let out_vec = match (decoder.decode().await, mode) {
                (Err(error), Some(StreamMode::SnapshotAll)) => {
                    responder.send(Ok(vec![ArchiveIteratorEntry {
                        end_of_stream: Some(true),
                        ..Default::default()
                    }]))?;
                    return Err(error);
                }
                (Err(error), _) => {
                    return Err(error);
                }
                (Ok(value), _) => value,
            };
            // Encode value into a new emulated socket
            let (local, remote) = fuchsia_async::emulated_handle::Socket::create_stream();
            let mut local = fuchsia_async::Socket::from_socket(local)?;
            let results = vec![ArchiveIteratorEntry {
                data: None,
                diagnostics_data: Some(DiagnosticsData::Socket(remote)),
                truncated_chars: None,
                ..Default::default()
            }];
            responder.send(Ok(results))?;
            local.write_all(serde_json::to_string(&out_vec)?.as_bytes()).await?;
        }
        Ok(())
    }

    async fn handle_frontend_request(
        self: &Arc<Self>,
        receiver: &mut Receiver<LogEntry>,
        responder: ArchiveIteratorGetNextResponder,
        parameters: &LocalStreamParameters,
    ) -> Result<Option<ArchiveIteratorGetNextResponder>, LogError> {
        loop {
            let Some(value) = receiver.next().await else {
                return Ok(Some(responder));
            };
            let disable_timestamps = self.connection_state.lock().await.disable_timestamps;
            // Skip if timestamp is not within specified range
            match (
                &parameters.daemon_parameters.min_timestamp_nanos,
                &value.data,
                disable_timestamps,
            ) {
                (Some(TimeBound::Absolute(utc)), LogData::TargetLog(target), false)
                    if (target.metadata.timestamp + parameters.boot_ts) <= (*utc) as i64 =>
                {
                    continue;
                }
                (Some(TimeBound::Monotonic(device_ts)), LogData::TargetLog(target), false)
                    if target.metadata.timestamp <= (*device_ts) as i64 =>
                {
                    continue;
                }
                _ => {}
            }
            let translated = ArchiveIteratorEntry {
                data: None,
                truncated_chars: None,
                diagnostics_data: Some(DiagnosticsData::Inline(InlineData {
                    data: serde_json::to_string(&value).unwrap(),
                    truncated_chars: 0,
                })),
                ..Default::default()
            };
            responder.send(Ok(vec![translated]))?;
            return Ok(None);
        }
    }

    async fn connect(&self) -> Result<ArchiveAccessorProxy, LogError> {
        let mut state = self.connection_state.lock().await;
        let (client, server) = create_proxy()?;
        state
            .target_collection_proxy
            .open_target(
                &TargetQuery { string_matcher: Some(self.node_name.clone()), ..Default::default() },
                server,
            )
            .await??;
        let (rcs_client, rcs_server) = create_proxy()?;
        client.open_remote_control(rcs_server).await??;
        state.boot_timestamp = rcs_client
            .identify_host()
            .await??
            .boot_timestamp_nanos
            .ok_or(LogError::NoBootTimestamp)?;
        let (diagnostics_client, diagnostics_server) = create_proxy::<ArchiveAccessorMarker>()?;
        let channel = diagnostics_server.into_channel();
        rcs_client
            .connect_capability(
                ARCHIVIST_MONIKER,
                ArchiveAccessorMarker::PROTOCOL_NAME,
                channel,
                fio::OpenFlags::empty(),
            )
            .await??;
        Ok(diagnostics_client)
    }
}

struct DirectStreamer<'a> {
    log_entry_sender: Sender<LogEntry>,
    config: SymbolizerConfig<'a>,
}

impl<'a> DirectStreamer<'a> {
    fn new(log_entry_sender: Sender<LogEntry>, config: SymbolizerConfig<'a>) -> DirectStreamer<'a> {
        DirectStreamer { log_entry_sender, config }
    }
}

#[async_trait(?Send)]
impl<'a> GenericDiagnosticsStreamer for DirectStreamer<'a> {
    async fn setup_stream(
        &self,
        _target_nodename: String,
        _session_timestamp_nanos: i64,
    ) -> Result<()> {
        Ok(())
    }

    async fn append_logs(&self, entries: Vec<LogEntry>) -> Result<()> {
        for entry in entries {
            self.log_entry_sender.send(entry).await?;
        }
        Ok(())
    }

    async fn read_most_recent_target_timestamp(&self) -> Result<Option<Timestamp>> {
        Ok(None)
    }

    async fn read_most_recent_entry_timestamp(&self) -> Result<Option<Timestamp>> {
        Ok(None)
    }

    async fn clean_sessions_for_target(&self) -> Result<()> {
        Ok(())
    }

    async fn stream_entries(
        &self,
        _stream_mode: StreamMode,
        _min_target_timestamp: Option<Timestamp>,
    ) -> Result<ffx_daemon_target::logger::streamer::SessionStream> {
        panic!("unexpected stream_entries call");
    }
}

#[async_trait(?Send)]
impl StreamDiagnostics for Arc<RemoteDiagnosticsBridgeProxyWrapper> {
    async fn stream_diagnostics(
        &mut self,
        _target: Option<&str>,
        mut parameters: DaemonDiagnosticsStreamParameters,
        iterator: fidl::endpoints::ServerEnd<ArchiveIteratorMarker>,
    ) -> Result<fidl_fuchsia_developer_ffx::DiagnosticsStreamDiagnosticsResult, anyhow::Error> {
        let (socket, server) = fuchsia_async::emulated_handle::Socket::create_stream();
        let connection = self.connect().await?;
        {
            let mut state = self.connection_state.lock().await;
            if state.last_boot_timestamp == 0 {
                state.last_boot_timestamp = state.boot_timestamp;
            } else {
                if state.last_boot_timestamp != state.boot_timestamp {
                    parameters.stream_mode = Some(StreamMode::SnapshotAllThenSubscribe);
                    state.disable_timestamps = true;
                }
            }
        }
        let _ = connection.stream_diagnostics(
            &StreamParameters {
                data_type: Some(fidl_fuchsia_diagnostics::DataType::Logs),
                stream_mode: parameters.stream_mode.as_ref().map(|mode| match mode {
                    StreamMode::SnapshotAll => fidl_fuchsia_diagnostics::StreamMode::Snapshot,
                    StreamMode::SnapshotRecentThenSubscribe => {
                        fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe
                    }
                    StreamMode::SnapshotAllThenSubscribe => {
                        fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe
                    }
                    StreamMode::Subscribe => fidl_fuchsia_diagnostics::StreamMode::Subscribe,
                }),
                format: Some(fidl_fuchsia_diagnostics::Format::Json),
                client_selector_configuration: Some(
                    fidl_fuchsia_diagnostics::ClientSelectorConfiguration::SelectAll(true),
                ),
                ..Default::default()
            },
            server,
        );
        let socket = fuchsia_async::Socket::from_socket(socket)?;

        let this = self.clone();

        let mut state = self.connection_state.lock().await;
        state.mode = parameters.stream_mode;
        if let Some(daemon) = &state.running_daemon_channel {
            daemon.send((iterator, socket)).await?;
        } else {
            let boot_ts = state.boot_timestamp;
            // root task
            let (sender, receiver) = unbounded();
            state.current_task = Some(fuchsia_async::Task::local(async move {
                this.translate_logs(
                    iterator,
                    socket,
                    receiver,
                    LocalStreamParameters {
                        daemon_parameters: parameters,
                        boot_ts: boot_ts as i64,
                    },
                )
                .await
            }));
            state.running_daemon_channel = Some(sender);
        }
        Ok(Ok(LogSession {
            session_timestamp_nanos: Some(state.boot_timestamp),
            ..Default::default()
        }))
    }
}

async fn setup_diagnostics_stream(
    diagnostics_proxy: &mut impl StreamDiagnostics,
    target_str: &str,
    server: ServerEnd<ArchiveIteratorMarker>,
    stream_mode: StreamMode,
    from_bound: Option<TimeBound>,
    session: Option<SessionSpec>,
) -> Result<Result<LogSession, DiagnosticsStreamError>> {
    let params = DaemonDiagnosticsStreamParameters {
        stream_mode: Some(stream_mode),
        min_timestamp_nanos: from_bound,
        session,
        ..Default::default()
    };
    diagnostics_proxy
        .stream_diagnostics(Some(&target_str), params, server)
        .await
        .context("connecting to daemon")
}

pub async fn exec_log_cmd<W: std::io::Write>(
    params: LogCommandParameters,
    mut diagnostics_proxy: impl StreamDiagnostics,
    log_formatter: &mut impl LogFormatter,
    writer: &mut W,
) -> Result<()> {
    let (mut proxy, server) =
        create_proxy::<ArchiveIteratorMarker>().context("failed to create endpoints")?;

    let session = setup_diagnostics_stream(
        &mut diagnostics_proxy,
        &params.target_identifier,
        server,
        params.stream_mode,
        params.from_bound.clone(),
        params.session.clone(),
    )
    .await?
    .map_err(|e| match e {
        DiagnosticsStreamError::NoStreamForTarget => anyhow!(ffx_error!("{}", NO_STREAM_ERROR)),
        _ => anyhow!("failure setting up diagnostics stream: {:?}", e),
    })?;

    let session_timestamp_nanos =
        session.session_timestamp_nanos.as_ref().context("missing session timestamp")?;
    log_formatter.set_boot_timestamp(*session_timestamp_nanos as i64);

    let to_bound_monotonic = params.to_bound.as_ref().map(|bound| match bound {
        TimeBound::Monotonic(ts) => Duration::from_nanos(*ts),
        TimeBound::Absolute(ts) => Duration::from_nanos(ts - session_timestamp_nanos),
        _ => panic!("unexpected TimeBound value"),
    });

    let mut requests = OrderedBatchPipeline::new(PIPELINE_SIZE);
    // This variable is set to true iff the most recent log we received was a disconnect event.
    let mut got_disconnect = false;
    loop {
        // If our last log entry was a disconnect event, we add a timeout to the logging pipeline. If no logs come through
        // before the timeout, we assume the disconnect event is still relevant and retry connecting to the target.
        let (get_next_results, terminal_err) = if got_disconnect {
            match timeout(Duration::from_secs(5), run_logging_pipeline(&mut requests, &proxy)).await
            {
                Ok(tup) => tup,
                Err(_) => match retry_loop(&mut diagnostics_proxy, params.clone(), writer).await {
                    Ok(p) => {
                        proxy = p;
                        continue;
                    }
                    Err(e) => {
                        writeln!(writer, "Retry failed - trying again. Error was: {}", e)?;
                        continue;
                    }
                },
            }
        } else {
            run_logging_pipeline(&mut requests, &proxy).await
        };

        for result in get_next_results.into_iter() {
            got_disconnect = false;
            if let Err(e) = result {
                tracing::warn!("got an error from the daemon {:?}", e);
                log_formatter.push_log(Err(e)).await?;
                continue;
            }

            // The real data should always be in the diagnostics_data field so we throw away all
            // entries that have a None for that field and leave us an iterable of DiagnosticsData
            // types.
            let entries = result.unwrap().into_iter().filter_map(|e| e.diagnostics_data);

            for entry in entries {
                // There are two types of logs: small ones that fit inline in a message and long
                // ones that must be transported via a socket.
                // We deserialize the log entry directly from the inline variant or we fetch the
                // data by reading from the socket and then deserializing.
                let parsed = match entry {
                    DiagnosticsData::Inline(inline) => {
                        serde_json::from_str::<LogEntry>(&inline.data)?
                    }
                    DiagnosticsData::Socket(socket) => log_entry_from_socket(socket).await?,
                };
                got_disconnect = false;

                match (&parsed.data, to_bound_monotonic) {
                    (LogData::TargetLog(log_data), Some(t)) => {
                        let ts: i64 = log_data.metadata.timestamp.into();
                        if ts as u128 > t.as_nanos() {
                            return Ok(());
                        }
                    }
                    (LogData::FfxEvent(EventType::TargetDisconnected), _) => {
                        log_formatter.push_log(Ok(parsed)).await?;
                        // Rather than immediately attempt a retry here, we continue the loop. If neither of the
                        // outer loops have a log entry following this disconnect event, we will retry after attempting fetch
                        // subsequent logs from the backend.
                        got_disconnect = true;
                        continue;
                    }
                    _ => {}
                }

                log_formatter.push_log(Ok(parsed)).await?;
            }
        }

        if let Some(err) = terminal_err {
            tracing::info!("log command got a terminal error: {}", err);
            return Ok(());
        }
    }
}

async fn retry_loop<W: std::io::Write>(
    diagnostics_proxy: &mut impl StreamDiagnostics,
    params: LogCommandParameters,
    writer: &mut W,
) -> Result<ArchiveIteratorProxy> {
    let mut fail_count = 0;
    loop {
        let (new_proxy, server) =
            create_proxy::<ArchiveIteratorMarker>().context("failed to create endpoints")?;
        match setup_diagnostics_stream(
            diagnostics_proxy,
            &params.target_identifier,
            server,
            params.stream_mode,
            params.from_bound.clone(),
            params.session.clone(),
        )
        .await?
        {
            Ok(_) => return Ok(new_proxy),
            Err(e) => {
                match e {
                    DiagnosticsStreamError::NoMatchingTargets => {
                        fail_count += 1;
                        write!(
                            writer,
                            "{}",
                            format_ffx_event(
                                &format!("{} isn't up. Retrying...", &params.target_identifier),
                                None
                            )
                        )?;
                    }
                    DiagnosticsStreamError::NoStreamForTarget => {
                        fail_count += 1;
                        write!(
                            writer,
                            "{}",
                            format_ffx_event(
                                &format!(
                                    "{} is up, but the logger hasn't started yet. Retrying...",
                                    &params.target_identifier
                                ),
                                None
                            )
                        )?;
                    }
                    _ => {
                        fail_count += 1;
                        write!(
                            writer,
                            "{}",
                            format_ffx_event(&format!("Retry failed: ({:?}).", e), None)
                        )?;
                    }
                }
                if fail_count > 5 {
                    writeln!(writer, "If this persists, consider restarting the ffx daemon with `ffx doctor --restart-daemon`.")?;
                } else {
                    writeln!(writer, "")?;
                }

                Timer::new(Duration::from_millis(RETRY_TIMEOUT_MILLIS)).await;
                continue;
            }
        }
    }
}

// This function drains the data from the passed in [socket] assuming it contains the bytes of a
// LogEntry object serialized to JSON.
async fn log_entry_from_socket(socket: fidl::Socket) -> Result<LogEntry> {
    let mut socket = fidl::AsyncSocket::from_socket(socket)
        .map_err(|e| anyhow!("failure to create async socket: {:?}", e))?;
    let mut result = Vec::new();
    let _ = socket
        .read_to_end(&mut result)
        .await
        .map_err(|e| anyhow!("failure to read log from socket: {:?}", e))?;
    let entry: LogEntry = serde_json::from_slice(&result)?;
    Ok(entry)
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use ffx_config::test_init;
    use ffx_log_test_utils::{
        setup_fake_archive_iterator, ArchiveIteratorParameters, FakeArchiveIteratorResponse,
    };
    use fidl_fuchsia_developer_ffx::{
        DiagnosticsMarker, DiagnosticsRequest, TargetCollectionMarker, TargetCollectionRequest,
        TargetRequest,
    };
    use fidl_fuchsia_developer_remotecontrol::{
        ArchiveIteratorError, IdentifyHostResponse, RemoteControlConnectCapabilityResponder,
        RemoteControlMarker, RemoteControlRequest,
    };
    use fidl_fuchsia_diagnostics_host::ArchiveAccessorRequest;
    use futures::TryStreamExt;
    use std::{ops::ControlFlow, sync::Arc};

    const DEFAULT_TS_NANOS: u64 = 1615535969000000000;
    const BOOT_TS: u64 = 98765432000000000;
    const START_TIMESTAMP_FOR_DAEMON: u64 = 1515903706000000000;
    const TARGET_NAME: &str = "test-node";

    fn default_ts() -> Duration {
        Duration::from_nanos(DEFAULT_TS_NANOS)
    }

    impl From<DaemonDiagnosticsStreamParameters> for LogCommandParameters {
        fn from(params: DaemonDiagnosticsStreamParameters) -> Self {
            Self {
                session: params.session,
                from_bound: params.min_timestamp_nanos,
                stream_mode: params.stream_mode.unwrap(),
                ..Self::default()
            }
        }
    }
    struct FakeLogFormatter {
        pushed_logs: Vec<ArchiveIteratorResult>,
        sender: Option<Sender<ArchiveIteratorResult>>,
    }

    #[async_trait(?Send)]
    impl LogFormatter for FakeLogFormatter {
        async fn push_log(&mut self, log_entry: ArchiveIteratorResult) -> Result<()> {
            if let Some(sender) = &self.sender {
                sender.send(log_entry).await?;
            } else {
                self.pushed_logs.push(log_entry);
            }
            Ok(())
        }

        fn set_boot_timestamp(&mut self, boot_ts_nanos: i64) {
            assert_eq!(boot_ts_nanos, BOOT_TS as i64)
        }
    }

    impl FakeLogFormatter {
        fn new() -> Self {
            Self { pushed_logs: vec![], sender: None }
        }

        fn new_with_stream(sender: Sender<ArchiveIteratorResult>) -> Self {
            Self { pushed_logs: vec![], sender: Some(sender) }
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

    fn setup_fake_diagnostics_server(
        expected_parameters: DaemonDiagnosticsStreamParameters,
        expected_responses: Arc<Vec<FakeArchiveIteratorResponse>>,
        use_socket: bool,
    ) -> DiagnosticsProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<DiagnosticsMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    DiagnosticsRequest::StreamDiagnostics {
                        target,
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
                                use_socket,
                            },
                        )
                        .unwrap();
                        responder
                            .send(Ok(&LogSession {
                                target_identifier: target,
                                session_timestamp_nanos: Some(BOOT_TS),
                                ..Default::default()
                            }))
                            .context("error sending response")
                            .expect("should send")
                    }
                }
            }
        })
        .detach();
        proxy
    }

    struct FakeDiagnosticsServerArgs {
        expected_responses: Arc<Vec<FakeArchiveIteratorResponse>>,
        use_socket: bool,
        advance_timestamps_on_reboot: bool,
    }

    fn setup_fake_diagnostics_server_direct(
        args: FakeDiagnosticsServerArgs,
    ) -> Arc<RemoteDiagnosticsBridgeProxyWrapper> {
        let (target_collection_proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<TargetCollectionMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let mut time_offset = 0;
            while let Some(Ok(TargetCollectionRequest::OpenTarget {
                query,
                target_handle,
                responder,
            })) = stream.next().await
            {
                if let ControlFlow::Break(_) = handle_open_target(
                    query,
                    responder,
                    target_handle,
                    &args.expected_responses,
                    args.use_socket,
                    time_offset,
                )
                .await
                {
                    return;
                }
                if args.advance_timestamps_on_reboot {
                    time_offset += 1;
                }
            }
        })
        .detach();
        let bridge = Arc::new(RemoteDiagnosticsBridgeProxyWrapper::new(
            target_collection_proxy,
            TARGET_NAME.to_string(),
        ));
        bridge
    }

    async fn handle_open_target(
        query: TargetQuery,
        responder: fidl_fuchsia_developer_ffx::TargetCollectionOpenTargetResponder,
        target_handle: ServerEnd<fidl_fuchsia_developer_ffx::TargetMarker>,
        expected_responses: &Arc<Vec<FakeArchiveIteratorResponse>>,
        use_socket: bool,
        time_offset: u64,
    ) -> ControlFlow<()> {
        assert_matches!(query.string_matcher, Some(value) if value == TARGET_NAME);
        responder.send(Ok(())).unwrap();
        let mut target_stream = target_handle.into_stream().unwrap();
        while let Some(Ok(TargetRequest::OpenRemoteControl { remote_control, responder })) =
            target_stream.next().await
        {
            if let Some(value) = handle_open_remote_control(
                responder,
                remote_control,
                expected_responses,
                use_socket,
                time_offset,
            )
            .await
            {
                return value;
            }
        }
        ControlFlow::Continue(())
    }

    async fn handle_open_remote_control(
        responder: fidl_fuchsia_developer_ffx::TargetOpenRemoteControlResponder,
        remote_control: ServerEnd<RemoteControlMarker>,
        expected_responses: &Arc<Vec<FakeArchiveIteratorResponse>>,
        use_socket: bool,
        time_offset: u64,
    ) -> Option<ControlFlow<()>> {
        responder.send(Ok(())).unwrap();
        let mut remote_control_stream = remote_control.into_stream().unwrap();
        if let Some(Ok(RemoteControlRequest::IdentifyHost { responder })) =
            remote_control_stream.next().await
        {
            responder
                .send(Ok(&IdentifyHostResponse {
                    nodename: Some(TARGET_NAME.to_string()),
                    boot_timestamp_nanos: Some(BOOT_TS + time_offset),
                    ..Default::default()
                }))
                .unwrap();
        }
        // Client should send an IdentifyHost request to get the timestamp

        // Client is then expected to connect to the remote diagnostics service
        if let Some(Ok(RemoteControlRequest::ConnectCapability {
            moniker,
            capability_name,
            server_chan,
            flags: _,
            responder,
        })) = remote_control_stream.next().await
        {
            if let Some(value) = handle_connect_capability(
                &moniker,
                &capability_name,
                responder,
                server_chan,
                expected_responses,
                use_socket,
            )
            .await
            {
                return value;
            }
        }
        None
    }

    async fn handle_connect_capability(
        moniker: &str,
        capability_name: &str,
        responder: RemoteControlConnectCapabilityResponder,
        service_chan: fidl::Channel,
        expected_responses: &Arc<Vec<FakeArchiveIteratorResponse>>,
        use_socket: bool,
    ) -> Option<Option<ControlFlow<()>>> {
        assert_eq!(moniker, ARCHIVIST_MONIKER);
        assert_eq!(capability_name, ArchiveAccessorMarker::PROTOCOL_NAME);
        responder.send(Ok(())).unwrap();
        let server_end = ServerEnd::<ArchiveAccessorMarker>::new(service_chan);
        let mut diagnostics_stream = server_end.into_stream().unwrap();
        if let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
            stream,
            parameters,
            responder,
        })) = diagnostics_stream.next().await
        {
            let (client, server) = fidl::endpoints::create_endpoints::<ArchiveIteratorMarker>();
            fuchsia_async::Task::local(async move {
                let mut stream = fuchsia_async::Socket::from_socket(stream).unwrap();
                let proxy = client.into_proxy().unwrap();
                loop {
                    let Ok(Ok(batch)) = proxy.get_next().await else {
                        break;
                    };
                    for value in batch {
                        let data = value.diagnostics_data.unwrap();
                        match data {
                            DiagnosticsData::Inline(data) => {
                                stream.write_all(data.data.as_bytes()).await.unwrap();
                            }
                            DiagnosticsData::Socket(socket) => {
                                let mut socket =
                                    fuchsia_async::Socket::from_socket(socket).unwrap();
                                let mut data = vec![];
                                socket.read_to_end(&mut data).await.unwrap();
                                stream.write_all(&data).await.unwrap();
                            }
                        }
                    }
                }
            })
            .detach();
            setup_fake_archive_iterator(
                server,
                ArchiveIteratorParameters {
                    legacy_format: false,
                    responses: expected_responses.clone(),
                    use_socket,
                },
            )
            .unwrap();
            responder.send().unwrap();
            if parameters.stream_mode == Some(fidl_fuchsia_diagnostics::StreamMode::Snapshot) {
                return Some(Some(ControlFlow::Break(())));
            }
        }
        None
    }

    fn make_log_entry(log_data: LogData) -> LogEntry {
        LogEntry {
            version: 1,
            timestamp: Timestamp::from(default_ts().as_nanos() as i64),
            data: log_data,
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_error_conversions() {
        let src = OpenTargetError::TargetNotFound;
        assert_matches!(
            LogError::from(src.clone()),
            LogError::OpenTargetError { error }
            if error == src
        );

        let src = TargetConnectionError::ConnectionRefused;
        assert_matches!(
            LogError::from(src.clone()),
            LogError::TargetConnectionError { error }
            if error == src
        );

        let src = IdentifyHostError::GetDeviceNameFailed;
        assert_matches!(
            LogError::from(src.clone()),
            LogError::IdentifyHostError { error }
            if error == src
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_generic_streamer_direct() {
        let env = test_init().await.unwrap();
        env.load().await;
        let (sender, _receiver) = unbounded();

        let symbolizer_config = SymbolizerConfig::new().await.unwrap();
        let direct_streamer = DirectStreamer::new(sender, symbolizer_config);

        // setup_stream should not crash
        direct_streamer.setup_stream(String::default(), BOOT_TS as i64).await.unwrap();
        direct_streamer.clean_sessions_for_target().await.unwrap();
        assert_eq!(direct_streamer.read_most_recent_entry_timestamp().await.unwrap(), None);
        assert_eq!(direct_streamer.read_most_recent_target_timestamp().await.unwrap(), None);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_json_decoder() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_async::emulated_handle::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = vec![LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build()];
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        let part_a = &serialized_bytes[..15];
        let part_b = &serialized_bytes[15..20];
        let part_c = &serialized_bytes[20..];
        local.write(part_a).unwrap();
        local.write(part_b).unwrap();
        local.write(part_c).unwrap();
        let decoded_log = &decoder.decode().await.unwrap();
        assert_eq!(decoded_log, &test_log);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_json_decoder_regular_message() {
        // This is intentionally a datagram socket so we can
        // send the entire message as one "packet".
        let (local, remote) = fuchsia_async::emulated_handle::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = vec![LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build()];
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        local.write(serialized_bytes).unwrap();
        let decoded_log = decoder.decode().await.unwrap();
        assert_eq!(decoded_log, test_log);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_json_decoder_truncated_message() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_async::emulated_handle::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build();
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        let part_a = &serialized_bytes[..15];
        let part_b = &serialized_bytes[15..20];
        local.write(part_a).unwrap();
        local.write(part_b).unwrap();
        drop(local);
        assert_matches!(decoder.decode().await, Err(LogError::PeerClosed));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_logs_direct() {
        let stream_modes = vec![
            StreamMode::SnapshotAll,
            StreamMode::SnapshotAllThenSubscribe,
            StreamMode::Subscribe,
            StreamMode::SnapshotRecentThenSubscribe,
        ];

        let env = test_init().await.unwrap();
        env.load().await;

        for mode in stream_modes {
            let (sender, mut receiver) = unbounded();
            let params =
                DaemonDiagnosticsStreamParameters { stream_mode: Some(mode), ..Default::default() };

            let log = LogsDataBuilder::new(BuilderArgs {
                component_url: None,
                moniker: "ffx".to_string(),
                timestamp_nanos: Timestamp::from(BOOT_TS as i64),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("Test message")
            .build();

            let expected_responses =
                vec![FakeArchiveIteratorResponse::new_with_values(vec![serde_json::to_string(
                    &log,
                )
                .unwrap()])];
            let _log_response = make_log_entry(LogData::TargetLog(log.clone()));

            let mut formatter = FakeLogFormatter::new_with_stream(sender);
            let _logger_task = fuchsia_async::Task::local(async move {
                let mut writer = Vec::new();
                exec_log_cmd(
                    LogCommandParameters::from(params.clone()),
                    setup_fake_diagnostics_server_direct(FakeDiagnosticsServerArgs {
                        expected_responses: Arc::new(expected_responses),
                        use_socket: false,
                        advance_timestamps_on_reboot: false,
                    }),
                    &mut formatter,
                    &mut writer,
                )
                .await
                .unwrap();
            });

            assert_matches!(
                receiver.next().await.unwrap().unwrap().data,
                LogData::FfxEvent(EventType::LoggingStarted)
            );
            assert_matches!(receiver.next().await.unwrap().unwrap().data, LogData::TargetLog(value) if value == log);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_logs_with_monotonic_timestamp_filter() {
        let env = test_init().await.unwrap();
        env.load().await;
        let (sender, mut receiver) = unbounded();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
            min_timestamp_nanos: Some(TimeBound::Monotonic(BOOT_TS)),
            ..Default::default()
        };

        let log_0 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Discarded message")
        .build();

        let log_1 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from((BOOT_TS + 1) as i64),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Received message")
        .build();

        let expected_responses = vec![FakeArchiveIteratorResponse::new_with_values(vec![
            serde_json::to_string(&log_0).unwrap(),
            serde_json::to_string(&log_1).unwrap(),
        ])];

        let mut formatter = FakeLogFormatter::new_with_stream(sender);
        let _logger_task = fuchsia_async::Task::local(async move {
            let mut writer = Vec::new();
            exec_log_cmd(
                LogCommandParameters::from(params.clone()),
                setup_fake_diagnostics_server_direct(FakeDiagnosticsServerArgs {
                    expected_responses: Arc::new(expected_responses),
                    use_socket: false,
                    advance_timestamps_on_reboot: false,
                }),
                &mut formatter,
                &mut writer,
            )
            .await
            .unwrap();
        });

        // First message should be skipped
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log_1
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_logs_with_utc_timestamp_filter() {
        let env = test_init().await.unwrap();
        env.load().await;
        let (sender, mut receiver) = unbounded();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
            min_timestamp_nanos: Some(TimeBound::Absolute(BOOT_TS)),
            ..Default::default()
        };

        let log_0 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(0),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Discarded message")
        .build();

        let log_1 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(1),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Received message")
        .build();

        let expected_responses = vec![FakeArchiveIteratorResponse::new_with_values(vec![
            serde_json::to_string(&log_0).unwrap(),
            serde_json::to_string(&log_1).unwrap(),
        ])];

        let mut formatter = FakeLogFormatter::new_with_stream(sender);
        let _logger_task = fuchsia_async::Task::local(async move {
            let mut writer = Vec::new();
            exec_log_cmd(
                LogCommandParameters::from(params.clone()),
                setup_fake_diagnostics_server_direct(FakeDiagnosticsServerArgs {
                    expected_responses: Arc::new(expected_responses),
                    use_socket: false,
                    advance_timestamps_on_reboot: false,
                }),
                &mut formatter,
                &mut writer,
            )
            .await
            .unwrap();
        });

        // First message should be skipped
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log_1
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_logs_with_now_timestamp_filter() {
        let env = test_init().await.unwrap();
        env.load().await;
        let (sender, mut receiver) = unbounded();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
            min_timestamp_nanos: Some(TimeBound::Absolute(BOOT_TS)),
            ..Default::default()
        };

        let log_0 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(0),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Discarded message")
        .build();

        let log_1 = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(1),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Received message")
        .build();

        let expected_responses = vec![FakeArchiveIteratorResponse::new_with_values(vec![
            serde_json::to_string(&log_0).unwrap(),
            serde_json::to_string(&log_1).unwrap(),
        ])];

        let mut formatter = FakeLogFormatter::new_with_stream(sender);
        let _logger_task = fuchsia_async::Task::local(async move {
            let mut writer = Vec::new();
            exec_log_cmd(
                LogCommandParameters::from(params.clone()),
                setup_fake_diagnostics_server_direct(FakeDiagnosticsServerArgs {
                    expected_responses: Arc::new(expected_responses),
                    use_socket: false,
                    advance_timestamps_on_reboot: true,
                }),
                &mut formatter,
                &mut writer,
            )
            .await
            .unwrap();
        });

        // First message should be skipped
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log_1
        );

        // Second time through we should get both messages
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::TargetDisconnected)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log_0
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log_1
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_logs_direct_retry() {
        let env = test_init().await.unwrap();
        env.load().await;
        let (sender, mut receiver) = unbounded();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
            ..Default::default()
        };

        let log = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
            severity: diagnostics_data::Severity::Info,
        })
        .set_message("Test message")
        .build();

        let expected_responses = vec![FakeArchiveIteratorResponse::new_with_values(vec![
            serde_json::to_string(&log).unwrap(),
        ])];
        let _log_response = make_log_entry(LogData::TargetLog(log.clone()));

        let mut formatter = FakeLogFormatter::new_with_stream(sender);
        let _logger_task = fuchsia_async::Task::local(async move {
            let mut writer = Vec::new();
            exec_log_cmd(
                LogCommandParameters::from(params.clone()),
                setup_fake_diagnostics_server_direct(FakeDiagnosticsServerArgs {
                    expected_responses: Arc::new(expected_responses),
                    use_socket: false,
                    advance_timestamps_on_reboot: false,
                }),
                &mut formatter,
                &mut writer,
            )
            .await
            .unwrap();
        });

        // First connection
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log
        );

        // Second connection
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::TargetDisconnected)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log
        );

        // Third connection
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::TargetDisconnected)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::FfxEvent(EventType::LoggingStarted)
        );
        assert_matches!(
            receiver.next().await.unwrap().unwrap().data,
            LogData::TargetLog(value)
            if value == log
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dump_empty() {
        let mut formatter = FakeLogFormatter::new();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            ..Default::default()
        };
        let expected_responses = vec![];

        let mut writer = Vec::new();
        exec_log_cmd(
            LogCommandParameters::from(params.clone()),
            setup_fake_diagnostics_server(params, Arc::new(expected_responses), false),
            &mut formatter,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());

        formatter.assert_same_logs(vec![])
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_watch() {
        let mut formatter = FakeLogFormatter::new();
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
        exec_log_cmd(
            LogCommandParameters::from(params.clone()),
            setup_fake_diagnostics_server(params, Arc::new(expected_responses), false),
            &mut formatter,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
        formatter.assert_same_logs(vec![Ok(log1), Ok(log2), Ok(log3)])
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_watch_no_dump_with_error() {
        let mut formatter = FakeLogFormatter::new();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::Subscribe),
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
        exec_log_cmd(
            LogCommandParameters::from(params.clone()),
            setup_fake_diagnostics_server(params, Arc::new(expected_responses), false),
            &mut formatter,
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
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_from_time_passed_to_daemon() {
        let mut formatter = FakeLogFormatter::new();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            min_timestamp_nanos: Some(TimeBound::Absolute(START_TIMESTAMP_FOR_DAEMON)),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };

        let mut writer = Vec::new();
        exec_log_cmd(
            LogCommandParameters::from(params.clone()),
            setup_fake_diagnostics_server(params, Arc::new(vec![]), false),
            &mut formatter,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_from_monotonic_passed_to_daemon() {
        let mut formatter = FakeLogFormatter::new();
        let params = DaemonDiagnosticsStreamParameters {
            stream_mode: Some(StreamMode::SnapshotAll),
            min_timestamp_nanos: Some(TimeBound::Monotonic(default_ts().as_nanos() as u64)),
            session: Some(SessionSpec::Relative(0)),
            ..Default::default()
        };

        let mut writer = Vec::new();
        exec_log_cmd(
            LogCommandParameters::from(params.clone()),
            setup_fake_diagnostics_server(params, Arc::new(vec![]), false),
            &mut formatter,
            &mut writer,
        )
        .await
        .unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert!(output.is_empty());
    }
}
