// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use perfetto_protos::perfetto::protos::get_async_command_response::{
    ClearIncrementalState, Cmd, Flush, SetupDataSource, SetupTracing, StartDataSource,
    StopDataSource,
};
use perfetto_protos::perfetto::protos::{
    DataSourceDescriptor, GetAsyncCommandResponse, InitializeConnectionRequest,
    RegisterDataSourceRequest, RegisterDataSourceResponse, TrackEventDescriptor,
};
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::socket::{
    new_socket_file, resolve_unix_socket_address, Socket, SocketDomain, SocketPeer, SocketProtocol,
    SocketType,
};
use starnix_core::vfs::FsString;
use starnix_logging::{log_error, log_info, track_stub};
use starnix_sync::{FileOpsCore, LockBefore, Locked};
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DataSourceError {
    #[error("Unimplemented behaviour: {0}")]
    Unimplemented(String),
    /// Errors forwarded from the underlying producer
    #[error(transparent)]
    Producer(#[from] perfetto::ProducerError),

    #[error("invalid response from connection: {0}")]
    InvalidResponse(String),
}

struct FuchsiaDataSource {
    connection: perfetto::Producer,
}

impl FuchsiaDataSource {
    fn new<L>(
        mut connection: perfetto::Producer,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<FuchsiaDataSource, DataSourceError>
    where
        L: LockBefore<FileOpsCore>,
    {
        // We've established a connection on the socket. Time to initialize.
        let init_req = InitializeConnectionRequest {
            // 256KiB buffer size with 4KiB pages is the default size which will work for us.
            shared_memory_page_size_hint_bytes: Some(4096),
            shared_memory_size_hint_bytes: Some(256 * 1024),
            producer_name: Some(String::from("fuchsia")),
            ..Default::default()
        };
        connection.initialize_connection(init_req, locked, current_task)?;

        let register_data_source_req = RegisterDataSourceRequest {
            data_source_descriptor: Some(DataSourceDescriptor {
                name: Some(String::from("fuchsia.tracing")),
                // We tell the perfetto service what our id is. It can be whatever we want it to
                // be, it just has to be unique for our connection. Since we're the only data
                // source, we claim the id `1`.
                id: Some(1),
                will_notify_on_start: Some(true),
                will_notify_on_stop: Some(true),
                handles_incremental_state_clear: Some(false),
                no_flush: Some(true),
                track_event_descriptor: Some(TrackEventDescriptor { available_categories: vec![] }),
                ..Default::default()
            }),
        };

        // Now, we need to tell perfetto about our data source.
        match connection.register_data_source(register_data_source_req, locked, current_task) {
            Ok(RegisterDataSourceResponse { error: Some(s) }) => {
                return Err(DataSourceError::InvalidResponse(format!(
                    "RegisterDatasource error: {}",
                    s
                )));
            }
            Ok(r) => r,
            Err(e) => {
                return Err(DataSourceError::Producer(e));
            }
        };

        Ok(FuchsiaDataSource { connection })
    }

    fn setup_tracing(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "setup_tracing");
        Err(DataSourceError::Unimplemented("setup_tracing".into()))
    }

    fn setup_data_source(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "setup_data_source");
        Err(DataSourceError::Unimplemented("setup_data_source".into()))
    }

    fn start_data_source(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "start_data_source");
        Err(DataSourceError::Unimplemented("start_data_source".into()))
    }

    fn stop_data_source(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "stop_data_source");
        Err(DataSourceError::Unimplemented("stop_data_source".into()))
    }

    fn flush(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "flush");
        Err(DataSourceError::Unimplemented("flush".into()))
    }

    fn clear_incremental_state(&mut self) -> Result<(), DataSourceError> {
        track_stub!(TODO("https://fxbug.dev/357665862"), "clear_incremental_state");
        Err(DataSourceError::Unimplemented("clear_incremental_state".into()))
    }

    fn handle_command<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), DataSourceError>
    where
        L: LockBefore<FileOpsCore>,
    {
        self.connection.get_command_request(locked, current_task)?;

        // The service may respond to a command request with multiple command responses, indicated
        // with the "has_more" field. We need to keep requesting commands until we handle them all
        // before we can send the next request for more commands.
        loop {
            let response = self.connection.get_command_response(locked, current_task)?;

            let (Some(GetAsyncCommandResponse { cmd: Some(cmd) }), has_more) = response else {
                return Err(DataSourceError::InvalidResponse(
                    "Received empty command response".into(),
                ));
            };
            match cmd {
                Cmd::SetupTracing(SetupTracing { .. }) => {
                    self.setup_tracing()?;
                }
                Cmd::SetupDataSource(SetupDataSource { .. }) => {
                    self.setup_data_source()?;
                }
                Cmd::StartDataSource(StartDataSource { .. }) => {
                    self.start_data_source()?;
                }
                Cmd::StopDataSource(StopDataSource { .. }) => {
                    self.stop_data_source()?;
                }
                Cmd::Flush(Flush { .. }) => {
                    self.flush()?;
                }
                Cmd::ClearIncrementalState(ClearIncrementalState { .. }) => {
                    self.clear_incremental_state()?;
                }
            }
            if !has_more {
                break;
            }
        }
        Ok(())
    }
}

// Wait for perfetto to start and the socket to appear
//
// We can connect to perfetto by connecting to perfetto's producer socket and sending some
// ipc messages. Usually this is located at /dev/socket/traced_producer, but it is
// configurable.
fn wait_for_perfetto_ready<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    socket_path: FsString,
) -> perfetto::Producer
where
    L: LockBefore<FileOpsCore>,
{
    loop {
        // This seems bad, is there a way to actually wait for the socket to appear and be ready?
        std::thread::sleep(std::time::Duration::from_secs(5));
        let Ok(conn_file) = new_socket_file(
            locked,
            current_task,
            SocketDomain::Unix,
            SocketType::Stream,
            OpenFlags::RDWR,
            SocketProtocol::from_raw(0),
        ) else {
            continue;
        };
        let Ok(conn_socket) = Socket::get_from_file(&conn_file) else {
            continue;
        };
        let Ok(peer_handle) =
            resolve_unix_socket_address(locked, current_task, socket_path.as_ref())
        else {
            continue;
        };
        let Ok(_) = conn_socket.connect(locked, current_task, SocketPeer::Handle(peer_handle))
        else {
            continue;
        };
        match perfetto::Producer::new(locked, current_task, conn_file.clone()) {
            Ok(conn) => return conn,
            Err(err) => {
                log_error!(
                    tag="perfetto-producer", err:%; "Failed to set up perfetto producer connection");
                continue;
            }
        };
    }
}

pub fn start_perfetto_producer_thread(kernel: &Arc<Kernel>, socket_path: FsString) -> () {
    kernel.kthreads.spawner().spawn(move |locked, current_task| {
        let conn = wait_for_perfetto_ready(locked, current_task, socket_path);
        // Register as a datasource with the perfetto daemon and begin servicing requests from
        // it.
        let mut data_source = match FuchsiaDataSource::new(conn, locked, current_task) {
            Ok(data_source) => data_source,
            Err(err) => {
                log_error!(tag="perfetto-producer", err:%; "Failed to set up data source");
                return;
            }
        };

        // Commands work on a "hanging-get" style protocol, we repeatedly ask for a command
        // from perfetto, block until we get one, then handle whatever we get.
        log_info!(tag="perfetto-producer"; "Initialized and ready to trace!");
        loop {
            if let Err(err) = data_source.handle_command(locked, current_task) {
                log_error!(tag="perfetto-producer", err:%; "Error serving connection");
                return;
            }
        }
    });
}
