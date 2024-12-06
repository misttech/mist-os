// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use perfetto_consumer_proto::perfetto::protos::trace_config::buffer_config::FillPolicy;
use perfetto_consumer_proto::perfetto::protos::trace_config::{BufferConfig, DataSource};
use perfetto_consumer_proto::perfetto::protos::{
    ipc_frame, DataSourceConfig, DisableTracingRequest, EnableTracingRequest, FreeBuffersRequest,
    FtraceConfig, IpcFrame, ReadBuffersRequest, ReadBuffersResponse, TraceConfig,
};
use prost::Message;
use starnix_core::task::{CurrentTask, EventHandler, Kernel, Waiter};
use starnix_core::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
use starnix_core::vfs::socket::{
    new_socket_file, resolve_unix_socket_address, Socket, SocketDomain, SocketPeer, SocketProtocol,
    SocketType,
};
use starnix_core::vfs::{FileHandle, FsStr, FsString};
use starnix_logging::{log_error, CATEGORY_ATRACE, NAME_PERFETTO_BLOB};
use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};

use fuchsia_trace::{category_enabled, trace_state, ProlongedContext, TraceState};

/// Sender for the trace state, which sends a message each time trace state is updated.
static TRACE_STATE_SENDER: OnceLock<Sender<TraceState>> = OnceLock::new();

const PERFETTO_BUFFER_SIZE_KB: u32 = 63488;

/// State for reading Perfetto IPC frames.
///
/// Each frame is composed of a 32 bit length in little endian, followed by
/// the proto-encoded message. This state handles reads that only include
/// partial messages.
struct FrameReader {
    /// File to read from.
    file: FileHandle,
    /// Buffer for passing to read() calls.
    ///
    /// This buffer does not store any data long-term, but is persisted to
    /// avoid reallocating the buffer repeatedly.
    read_buffer: VecOutputBuffer,
    /// Data that has been read but not processed.
    data: VecDeque<u8>,
    /// If we've received enough bytes to know the next message's size, those
    /// bytes are removed from [data] and the size is populated here.
    next_message_size: Option<usize>,
}

impl FrameReader {
    fn new(file: FileHandle) -> Self {
        Self {
            file,
            read_buffer: VecOutputBuffer::new(4096),
            data: VecDeque::with_capacity(4096),
            next_message_size: None,
        }
    }

    /// Repeatedly reads from the specified file until a full message is available.
    fn next_frame_blocking<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<IpcFrame, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        loop {
            if self.next_message_size.is_none() && self.data.len() >= 4 {
                let len_bytes: [u8; 4] = self
                    .data
                    .drain(..4)
                    .collect::<Vec<_>>()
                    .try_into()
                    .expect("self.data has at least 4 elements");
                self.next_message_size = Some(u32::from_le_bytes(len_bytes) as usize);
            }
            if let Some(message_size) = self.next_message_size {
                if self.data.len() >= message_size {
                    let message: Vec<u8> = self.data.drain(..message_size).collect();
                    self.next_message_size = None;
                    return Ok(IpcFrame::decode(message.as_slice())?);
                }
            }

            let waiter = Waiter::new();
            self.file.wait_async(
                locked,
                current_task,
                &waiter,
                FdEvents::POLLIN,
                EventHandler::None,
            );
            while self.file.query_events(locked, current_task)? & FdEvents::POLLIN
                != FdEvents::POLLIN
            {
                waiter.wait(locked, current_task)?;
            }
            self.file.read(locked, current_task, &mut self.read_buffer)?;
            self.data.extend(self.read_buffer.data());
            self.read_buffer.reset();
        }
    }
}

/// Bookkeeping information needed for IPC messages to and from Perfetto.
struct PerfettoConnection {
    /// File handle corresponding to the communication socket. Data is written to and read from
    /// this file.
    conn_file: FileHandle,
    /// State for combining read byte data into IPC frames.
    frame_reader: FrameReader,
    /// Reply from the BindService call that was made when the connection was opened.
    /// This call includes ids for the various IPCs that the Perfetto service supports.
    bind_service_reply: ipc_frame::BindServiceReply,
    /// Next unused request id. This is used for correlating repies to requests.
    request_id: u64,
}

impl PerfettoConnection {
    /// Opens a socket connection to the specified socket path and initializes the requisite
    /// bookkeeping information.
    fn new(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        socket_path: &FsStr,
    ) -> Result<Self, anyhow::Error> {
        let conn_file = new_socket_file(
            current_task,
            SocketDomain::Unix,
            SocketType::Stream,
            OpenFlags::RDWR,
            SocketProtocol::from_raw(0),
        )?;
        let conn_socket = Socket::get_from_file(&conn_file)?;
        let peer =
            SocketPeer::Handle(resolve_unix_socket_address(locked, current_task, socket_path)?);
        conn_socket.connect(current_task, peer)?;
        let mut frame_reader = FrameReader::new(conn_file.clone());
        let mut request_id = 1;

        let bind_service_message = IpcFrame {
            request_id: Some(request_id),
            data_for_testing: Vec::new(),
            msg: Some(ipc_frame::Msg::MsgBindService(ipc_frame::BindService {
                service_name: Some("ConsumerPort".to_string()),
            })),
        };
        request_id += 1;
        let mut bind_service_bytes =
            Vec::with_capacity(bind_service_message.encoded_len() + std::mem::size_of::<u32>());
        bind_service_bytes.extend_from_slice(
            &u32::try_from(bind_service_message.encoded_len()).unwrap().to_le_bytes(),
        );
        bind_service_message.encode(&mut bind_service_bytes)?;
        let mut bind_service_buffer: VecInputBuffer = bind_service_bytes.into();
        conn_file.write(locked, current_task, &mut bind_service_buffer)?;

        let reply_frame = frame_reader.next_frame_blocking(locked, current_task)?;

        let bind_service_reply = match reply_frame.msg {
            Some(ipc_frame::Msg::MsgBindServiceReply(reply)) => reply,
            m => return Err(anyhow::anyhow!("Got unexpected reply message: {:?}", m)),
        };

        Ok(Self { conn_file, frame_reader, bind_service_reply, request_id })
    }

    fn send_message<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        msg: ipc_frame::Msg,
    ) -> Result<u64, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        let request_id = self.request_id;
        let frame =
            IpcFrame { request_id: Some(request_id), data_for_testing: Vec::new(), msg: Some(msg) };

        self.request_id += 1;

        let mut frame_bytes = Vec::with_capacity(frame.encoded_len() + std::mem::size_of::<u32>());
        frame_bytes.extend_from_slice(&u32::try_from(frame.encoded_len())?.to_le_bytes());
        frame.encode(&mut frame_bytes)?;
        let mut buffer: VecInputBuffer = frame_bytes.into();
        self.conn_file.write(locked, current_task, &mut buffer)?;

        Ok(request_id)
    }

    fn method_id(&self, name: &str) -> Result<u32, anyhow::Error> {
        for method in &self.bind_service_reply.methods {
            if let Some(method_name) = method.name.as_ref() {
                if method_name == name {
                    if let Some(id) = method.id {
                        return Ok(id);
                    } else {
                        return Err(anyhow::anyhow!(
                            "Matched method name {} but found no id",
                            method_name
                        ));
                    }
                }
            }
        }
        Err(anyhow::anyhow!("Did not find method {}", name))
    }

    fn enable_tracing<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        req: EnableTracingRequest,
    ) -> Result<u64, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        let method_id = self.method_id("EnableTracing")?;
        let mut encoded_args: Vec<u8> = Vec::with_capacity(req.encoded_len());
        req.encode(&mut encoded_args)?;

        self.send_message(
            locked,
            current_task,
            ipc_frame::Msg::MsgInvokeMethod(ipc_frame::InvokeMethod {
                service_id: self.bind_service_reply.service_id,
                method_id: Some(method_id),
                args_proto: Some(encoded_args),
                drop_reply: None,
            }),
        )
    }

    fn disable_tracing<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        req: DisableTracingRequest,
    ) -> Result<u64, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        let method_id = self.method_id("DisableTracing")?;
        let mut encoded_args: Vec<u8> = Vec::with_capacity(req.encoded_len());
        req.encode(&mut encoded_args)?;

        self.send_message(
            locked,
            current_task,
            ipc_frame::Msg::MsgInvokeMethod(ipc_frame::InvokeMethod {
                service_id: self.bind_service_reply.service_id,
                method_id: Some(method_id),
                args_proto: Some(encoded_args),
                drop_reply: None,
            }),
        )
    }

    fn read_buffers<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        req: ReadBuffersRequest,
    ) -> Result<u64, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        let method_id = self.method_id("ReadBuffers")?;
        let mut encoded_args: Vec<u8> = Vec::with_capacity(req.encoded_len());
        req.encode(&mut encoded_args)?;

        self.send_message(
            locked,
            current_task,
            ipc_frame::Msg::MsgInvokeMethod(ipc_frame::InvokeMethod {
                service_id: self.bind_service_reply.service_id,
                method_id: Some(method_id),
                args_proto: Some(encoded_args),
                drop_reply: None,
            }),
        )
    }

    fn free_buffers<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        req: FreeBuffersRequest,
    ) -> Result<u64, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        let method_id = self.method_id("FreeBuffers")?;
        let mut encoded_args: Vec<u8> = Vec::with_capacity(req.encoded_len());
        req.encode(&mut encoded_args)?;

        self.send_message(
            locked,
            current_task,
            ipc_frame::Msg::MsgInvokeMethod(ipc_frame::InvokeMethod {
                service_id: self.bind_service_reply.service_id,
                method_id: Some(method_id),
                args_proto: Some(encoded_args),
                drop_reply: None,
            }),
        )
    }
}

/// State needed to act upon trace state changes.
struct CallbackState {
    /// The previously observed trace state.
    prev_state: TraceState,
    /// Path to the Perfetto consumer socket.
    socket_path: FsString,
    /// Connection to the consumer socket, if it has been initialized. This gets initialized the
    /// first time it is needed.
    connection: Option<PerfettoConnection>,
    /// Prolonged trace context to prevent the Fuchsia trace session from terminating while reading
    /// data from Perfetto.
    prolonged_context: Option<ProlongedContext>,
    /// Partial trace packet returned from Perfetto but not yet written to Fuchsia.
    packet_data: Vec<u8>,
}

impl CallbackState {
    fn connection(
        &mut self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
    ) -> Result<&mut PerfettoConnection, anyhow::Error> {
        match self.connection {
            None => {
                self.connection =
                    Some(PerfettoConnection::new(locked, current_task, self.socket_path.as_ref())?);
                Ok(self.connection.as_mut().unwrap())
            }
            Some(ref mut conn) => Ok(conn),
        }
    }

    fn on_state_change(
        &mut self,
        locked: &mut Locked<'_, Unlocked>,
        new_state: TraceState,
        current_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        match new_state {
            TraceState::Started => {
                self.prolonged_context = ProlongedContext::acquire();
                let connection = self.connection(locked, current_task)?;
                // A fixed set of data sources that may be of interest. As demand for other sources
                // is found, add them here, and it may become worthwhile to allow this set to be
                // configurable per trace session.
                let mut data_sources = vec![
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("track_event".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("android.surfaceflinger.frame".to_string()),
                            target_buffer: Some(0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("android.surfaceflinger.frametimeline".to_string()),
                            target_buffer: Some(0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ];
                if category_enabled(CATEGORY_ATRACE) {
                    data_sources.push(DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("linux.ftrace".to_string()),
                            ftrace_config: Some(FtraceConfig {
                                ftrace_events: vec!["ftrace/print".to_string()],
                                // Enable all supported atrace categories. This could be improved
                                // in the future to be a subset that is configurable by each trace
                                // session.
                                atrace_categories: vec![
                                    "am".to_string(),
                                    "adb".to_string(),
                                    "aidl".to_string(),
                                    "dalvik".to_string(),
                                    "audio".to_string(),
                                    "binder_lock".to_string(),
                                    "binder_driver".to_string(),
                                    "bionic".to_string(),
                                    "camera".to_string(),
                                    "database".to_string(),
                                    "gfx".to_string(),
                                    "hal".to_string(),
                                    "input".to_string(),
                                    "network".to_string(),
                                    "nnapi".to_string(),
                                    "pm".to_string(),
                                    "power".to_string(),
                                    "rs".to_string(),
                                    "res".to_string(),
                                    "rro".to_string(),
                                    "sched".to_string(),
                                    "sm".to_string(),
                                    "ss".to_string(),
                                    "vibrator".to_string(),
                                    "video".to_string(),
                                    "view".to_string(),
                                    "webview".to_string(),
                                    "wm".to_string(),
                                ],
                                atrace_apps: vec!["*".to_string()],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                connection.enable_tracing(
                    locked,
                    current_task,
                    EnableTracingRequest {
                        trace_config: Some(TraceConfig {
                            buffers: vec![BufferConfig {
                                size_kb: Some(PERFETTO_BUFFER_SIZE_KB),
                                fill_policy: Some(FillPolicy::Discard.into()),
                                ..Default::default()
                            }],
                            data_sources,
                            ..Default::default()
                        }),
                        attach_notification_only: None,
                    },
                )?;
            }
            TraceState::Stopping | TraceState::Stopped => {
                if self.prev_state == TraceState::Started {
                    let context = fuchsia_trace::Context::acquire();
                    // Now that we have acquired a context (or at least attempted to),
                    // we can drop the prolonged context. We want to do this early to
                    // avoid making the trace session hang if this function exits
                    // on an error path.
                    self.prolonged_context = None;

                    let disable_request;
                    let read_buffers_request;
                    let blob_name_ref;
                    {
                        let connection = self.connection(locked, current_task)?;
                        disable_request = connection.disable_tracing(
                            locked,
                            current_task,
                            DisableTracingRequest {},
                        )?;
                        loop {
                            let frame = connection
                                .frame_reader
                                .next_frame_blocking(locked, current_task)?;
                            if frame.request_id == Some(disable_request) {
                                break;
                            }
                        }

                        read_buffers_request =
                            connection.read_buffers(locked, current_task, ReadBuffersRequest {})?;
                        blob_name_ref = context
                            .as_ref()
                            .map(|context| context.register_string_literal(NAME_PERFETTO_BLOB));
                    }

                    // IPC responses may be spread across multiple frames, so loop until we get a
                    // message that indicates it is the last one. Additionally, if there are
                    // unrelated messages on the socket (e.g. leftover from a previous trace
                    // session), the loop will read past and ignore them.
                    loop {
                        let frame = self
                            .connection(locked, current_task)?
                            .frame_reader
                            .next_frame_blocking(locked, current_task)?;
                        if frame.request_id != Some(read_buffers_request) {
                            continue;
                        }
                        if let Some(ipc_frame::Msg::MsgInvokeMethodReply(reply)) = &frame.msg {
                            if let Ok(response) = ReadBuffersResponse::decode(
                                reply.reply_proto.as_deref().unwrap_or(&[]),
                            ) {
                                for slice in &response.slices {
                                    if let Some(data) = &slice.data {
                                        self.packet_data.extend(data);
                                    }
                                    if slice.last_slice_for_packet.unwrap_or(false) {
                                        let mut blob_data = Vec::new();
                                        // Packet field number = 1, length delimited type = 2.
                                        blob_data.push(1 << 3 | 2);
                                        // Push a varint encoded length.
                                        // See https://protobuf.dev/programming-guides/encoding/
                                        const HIGH_BIT: u8 = 0x80;
                                        const LOW_SEVEN_BITS: usize = 0x7F;
                                        let mut value = self.packet_data.len();
                                        while value >= HIGH_BIT as usize {
                                            blob_data
                                                .push((value & LOW_SEVEN_BITS) as u8 | HIGH_BIT);
                                            value >>= 7;
                                        }
                                        blob_data.push(value as u8);
                                        // `append` moves all data out of the passed Vec, so
                                        // s.packet_data will be empty after this call.
                                        blob_data.append(&mut self.packet_data);
                                        if let Some(context) = &context {
                                            context.write_blob_record(
                                                fuchsia_trace::TRACE_BLOB_TYPE_PERFETTO,
                                                blob_name_ref.as_ref().expect(
                                                    "blob_name_ref is Some whenever context is",
                                                ),
                                                blob_data.as_slice(),
                                            );
                                        }
                                    }
                                }
                            }
                            if reply.has_more != Some(true) {
                                break;
                            }
                        }
                    }
                    // The response to a free buffers request does not have anything meaningful,
                    // so we don't need to worry about tracking the request id to match to the
                    // response.
                    let _free_buffers_request_id =
                        self.connection(locked, current_task)?.free_buffers(
                            locked,
                            current_task,
                            FreeBuffersRequest { buffer_ids: vec![0] },
                        )?;
                }
            }
        }
        self.prev_state = new_state;
        Ok(())
    }
}

pub fn start_perfetto_consumer_thread(
    kernel: &Arc<Kernel>,
    socket_path: FsString,
) -> Result<(), Errno> {
    let (sender, receiver) = channel::<TraceState>();
    kernel.kthreads.spawner().spawn({
        move |locked, current_task| {
            let mut callback_state = CallbackState {
                prev_state: TraceState::Stopped,
                socket_path,
                connection: None,
                prolonged_context: None,
                packet_data: Vec::new(),
            };
            while let Ok(state) = receiver.recv() {
                callback_state.on_state_change(locked, state, &current_task).unwrap_or_else(|e| {
                    log_error!("perfetto_consumer callback error: {:?}", e);
                })
            }
        }
    });
    // Store the other end of the channel so that it can be called from a static callback function
    // when the trace is changed.
    TRACE_STATE_SENDER.set(sender).or_else(|e| {
        log_error!("Failed to set perfetto_consumer trace state sender: {:?}", e);
        Err(errno!(EINVAL))
    })?;
    fuchsia_trace_observer::start_trace_observer(c_callback);
    Ok(())
}

extern "C" fn c_callback() {
    let state = trace_state();
    if let Some(sender) = TRACE_STATE_SENDER.get() {
        sender.send(state).unwrap_or_else(|e| log_error!("perfetto_consumer send failed: {:?}", e));
    } else {
        log_error!("perfetto_consumer sender was not set when the callback was called");
    }
}
