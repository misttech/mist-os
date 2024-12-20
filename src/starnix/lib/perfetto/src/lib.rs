// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use perfetto_consumer_proto::perfetto::protos::{
    ipc_frame, DisableTracingRequest, EnableTracingRequest, FreeBuffersRequest, IpcFrame,
    ReadBuffersRequest,
};
use prost::Message;
use starnix_core::task::{CurrentTask, EventHandler, Waiter};
use starnix_core::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
use starnix_core::vfs::socket::{
    new_socket_file, resolve_unix_socket_address, Socket, SocketDomain, SocketPeer, SocketProtocol,
    SocketType,
};
use starnix_core::vfs::{FileHandle, FsStr};
use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use std::collections::VecDeque;

/// State for reading Perfetto IPC frames.
///
/// Each frame is composed of a 32 bit length in little endian, followed by
/// the proto-encoded message. This state handles reads that only include
/// partial messages.
pub struct FrameReader {
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
    pub fn new(file: FileHandle) -> Self {
        Self {
            file,
            read_buffer: VecOutputBuffer::new(4096),
            data: VecDeque::with_capacity(4096),
            next_message_size: None,
        }
    }

    /// Repeatedly reads from the specified file until a full message is available.
    pub fn next_frame_blocking<L>(
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
pub struct Consumer {
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

impl Consumer {
    /// Opens a socket connection to the specified socket path and initializes the requisite
    /// bookkeeping information.
    pub fn new(
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

    pub fn enable_tracing<L>(
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

    pub fn disable_tracing<L>(
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

    pub fn read_buffers<L>(
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

    pub fn free_buffers<L>(
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

    pub fn next_frame_blocking<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<IpcFrame, anyhow::Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        self.frame_reader.next_frame_blocking(locked, current_task)
    }
}
