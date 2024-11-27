// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_hardware_block::BlockProxy;
use fidl_fuchsia_hardware_block_partition::PartitionProxy;
use fidl_fuchsia_hardware_block_volume::VolumeProxy;
use fuchsia_async::{self as fasync, FifoReadable as _, FifoWritable as _};
use futures::channel::oneshot;
use futures::executor::block_on;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::ops::{DerefMut, Range};
use std::pin::Pin;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use storage_trace::{self as trace, TraceFutureExt};
use zx::sys::zx_handle_t;
use zx::{self as zx, HandleBased as _};
use {fidl_fuchsia_hardware_block as block, fidl_fuchsia_hardware_block_driver as block_driver};

pub use cache::Cache;

pub use block::Flag as BlockFlags;

pub use block_protocol::*;

pub mod cache;

const TEMP_VMO_SIZE: usize = 65536;

pub use block_driver::{BlockIoFlag, BlockOpcode};

fn fidl_to_status(error: fidl::Error) -> zx::Status {
    match error {
        fidl::Error::ClientChannelClosed { status, .. } => status,
        _ => zx::Status::INTERNAL,
    }
}

fn opcode_str(opcode: u8) -> &'static str {
    match BlockOpcode::from_primitive(opcode) {
        Some(BlockOpcode::Read) => "read",
        Some(BlockOpcode::Write) => "write",
        Some(BlockOpcode::Flush) => "flush",
        Some(BlockOpcode::Trim) => "trim",
        Some(BlockOpcode::CloseVmo) => "close_vmo",
        None => "unknown",
    }
}

// Generates a trace ID that will be unique across the system (as long as |request_id| isn't
// reused within this process).
fn generate_trace_flow_id(request_id: u32) -> u64 {
    lazy_static! {
        static ref SELF_HANDLE: zx_handle_t = fuchsia_runtime::process_self().raw_handle();
    };
    *SELF_HANDLE as u64 + (request_id as u64) << 32
}

pub enum BufferSlice<'a> {
    VmoId { vmo_id: &'a VmoId, offset: u64, length: u64 },
    Memory(&'a [u8]),
}

impl<'a> BufferSlice<'a> {
    pub fn new_with_vmo_id(vmo_id: &'a VmoId, offset: u64, length: u64) -> Self {
        BufferSlice::VmoId { vmo_id, offset, length }
    }
}

impl<'a> From<&'a [u8]> for BufferSlice<'a> {
    fn from(buf: &'a [u8]) -> Self {
        BufferSlice::Memory(buf)
    }
}

pub enum MutableBufferSlice<'a> {
    VmoId { vmo_id: &'a VmoId, offset: u64, length: u64 },
    Memory(&'a mut [u8]),
}

impl<'a> MutableBufferSlice<'a> {
    pub fn new_with_vmo_id(vmo_id: &'a VmoId, offset: u64, length: u64) -> Self {
        MutableBufferSlice::VmoId { vmo_id, offset, length }
    }
}

impl<'a> From<&'a mut [u8]> for MutableBufferSlice<'a> {
    fn from(buf: &'a mut [u8]) -> Self {
        MutableBufferSlice::Memory(buf)
    }
}

#[derive(Default)]
struct RequestState {
    result: Option<zx::Status>,
    waker: Option<Waker>,
}

#[derive(Default)]
struct FifoState {
    // The fifo.
    fifo: Option<fasync::Fifo<BlockFifoResponse, BlockFifoRequest>>,

    // The next request ID to be used.
    next_request_id: u32,

    // A queue of messages to be sent on the fifo.
    queue: std::collections::VecDeque<BlockFifoRequest>,

    // Map from request ID to RequestState.
    map: HashMap<u32, RequestState>,

    // The waker for the FifoPoller.
    poller_waker: Option<Waker>,
}

impl FifoState {
    fn terminate(&mut self) {
        self.fifo.take();
        for (_, request_state) in self.map.iter_mut() {
            request_state.result.get_or_insert(zx::Status::CANCELED);
            if let Some(waker) = request_state.waker.take() {
                waker.wake();
            }
        }
        if let Some(waker) = self.poller_waker.take() {
            waker.wake();
        }
    }

    // Returns true if polling should be terminated.
    fn poll_send_requests(&mut self, context: &mut Context<'_>) -> bool {
        let fifo = if let Some(fifo) = self.fifo.as_ref() {
            fifo
        } else {
            return true;
        };

        loop {
            let slice = self.queue.as_slices().0;
            if slice.is_empty() {
                return false;
            }
            match fifo.write(context, slice) {
                Poll::Ready(Ok(sent)) => {
                    self.queue.drain(0..sent);
                }
                Poll::Ready(Err(_)) => {
                    self.terminate();
                    return true;
                }
                Poll::Pending => {
                    return false;
                }
            }
        }
    }
}

type FifoStateRef = Arc<Mutex<FifoState>>;

// A future used for fifo responses.
struct ResponseFuture {
    request_id: u32,
    fifo_state: FifoStateRef,
}

impl ResponseFuture {
    fn new(fifo_state: FifoStateRef, request_id: u32) -> Self {
        ResponseFuture { request_id, fifo_state }
    }
}

impl Future for ResponseFuture {
    type Output = Result<(), zx::Status>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.fifo_state.lock().unwrap();
        let request_state = state.map.get_mut(&self.request_id).unwrap();
        if let Some(result) = request_state.result {
            Poll::Ready(result.into())
        } else {
            request_state.waker.replace(context.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for ResponseFuture {
    fn drop(&mut self) {
        self.fifo_state.lock().unwrap().map.remove(&self.request_id).unwrap();
    }
}

/// Wraps a vmo-id. Will panic if you forget to detach.
#[derive(Debug)]
#[must_use]
pub struct VmoId(AtomicU16);

impl VmoId {
    /// VmoIds will normally be vended by attach_vmo, but this might be used in some tests
    pub fn new(id: u16) -> Self {
        Self(AtomicU16::new(id))
    }

    /// Invalidates self and returns a new VmoId with the same underlying ID.
    pub fn take(&self) -> Self {
        Self(AtomicU16::new(self.0.swap(block_driver::BLOCK_VMOID_INVALID, Ordering::Relaxed)))
    }

    pub fn is_valid(&self) -> bool {
        self.id() != block_driver::BLOCK_VMOID_INVALID
    }

    /// Takes the ID.  The caller assumes responsibility for detaching.
    #[must_use]
    pub fn into_id(self) -> u16 {
        self.0.swap(block_driver::BLOCK_VMOID_INVALID, Ordering::Relaxed)
    }

    pub fn id(&self) -> u16 {
        self.0.load(Ordering::Relaxed)
    }
}

impl PartialEq for VmoId {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for VmoId {}

impl Drop for VmoId {
    fn drop(&mut self) {
        assert_eq!(
            self.0.load(Ordering::Relaxed),
            block_driver::BLOCK_VMOID_INVALID,
            "Did you forget to detach?"
        );
    }
}

impl Hash for VmoId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id().hash(state);
    }
}

/// Represents a client connection to a block device. This is a simplified version of the block.fidl
/// interface.
/// Most users will use the RemoteBlockClient instantiation of this trait.
#[async_trait]
pub trait BlockClient: Send + Sync {
    /// Wraps AttachVmo from fuchsia.hardware.block::Block.
    async fn attach_vmo(&self, vmo: &zx::Vmo) -> Result<VmoId, zx::Status>;

    /// Detaches the given vmo-id from the device.
    async fn detach_vmo(&self, vmo_id: VmoId) -> Result<(), zx::Status>;

    /// Reads from the device at |device_offset| into the given buffer slice.
    async fn read_at(
        &self,
        buffer_slice: MutableBufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status>;

    async fn write_at_with_opts(
        &self,
        buffer_slice: BufferSlice<'_>,
        device_offset: u64,
        opts: WriteOptions,
    ) -> Result<(), zx::Status>;

    /// Writes the data in |buffer_slice| to the device.
    async fn write_at(
        &self,
        buffer_slice: BufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status> {
        self.write_at_with_opts(buffer_slice, device_offset, WriteOptions::empty()).await
    }

    /// Trims the given range on the block device.
    async fn trim(&self, device_range: Range<u64>) -> Result<(), zx::Status>;

    /// Sends a flush request to the underlying block device.
    async fn flush(&self) -> Result<(), zx::Status>;

    /// Closes the fifo.
    async fn close(&self) -> Result<(), zx::Status>;

    /// Returns the block size of the device.
    fn block_size(&self) -> u32;

    /// Returns the size, in blocks, of the device.
    fn block_count(&self) -> u64;

    /// Returns the block flags reported by the device.
    fn block_flags(&self) -> BlockFlags;

    /// Returns true if the remote fifo is still connected.
    fn is_connected(&self) -> bool;
}

struct Common {
    block_size: u32,
    block_count: u64,
    block_flags: BlockFlags,
    fifo_state: FifoStateRef,
    temp_vmo: futures::lock::Mutex<zx::Vmo>,
    temp_vmo_id: VmoId,
}

impl Common {
    fn new(
        fifo: fasync::Fifo<BlockFifoResponse, BlockFifoRequest>,
        info: &block::BlockInfo,
        temp_vmo: zx::Vmo,
        temp_vmo_id: VmoId,
    ) -> Self {
        let fifo_state = Arc::new(Mutex::new(FifoState { fifo: Some(fifo), ..Default::default() }));
        fasync::Task::spawn(FifoPoller { fifo_state: fifo_state.clone() }).detach();
        Self {
            block_size: info.block_size,
            block_count: info.block_count,
            block_flags: info.flags,
            fifo_state,
            temp_vmo: futures::lock::Mutex::new(temp_vmo),
            temp_vmo_id,
        }
    }

    fn to_blocks(&self, bytes: u64) -> Result<u64, zx::Status> {
        if bytes % self.block_size as u64 != 0 {
            Err(zx::Status::INVALID_ARGS)
        } else {
            Ok(bytes / self.block_size as u64)
        }
    }

    // Sends the request and waits for the response.
    async fn send(&self, mut request: BlockFifoRequest) -> Result<(), zx::Status> {
        let trace_args = storage_trace::trace_future_args!(
            c"storage",
            c"BlockOp",
            "op" => opcode_str(request.command.opcode)
        );
        async move {
            let (request_id, trace_flow_id) = {
                let mut state = self.fifo_state.lock().unwrap();
                if state.fifo.is_none() {
                    // Fifo has been closed.
                    return Err(zx::Status::CANCELED);
                }
                trace::duration!(c"storage", c"BlockOp::start");
                let request_id = state.next_request_id;
                let trace_flow_id = generate_trace_flow_id(request_id);
                state.next_request_id = state.next_request_id.overflowing_add(1).0;
                assert!(
                    state.map.insert(request_id, RequestState::default()).is_none(),
                    "request id in use!"
                );
                request.reqid = request_id;
                request.trace_flow_id = generate_trace_flow_id(request_id);
                trace::flow_begin!(c"storage", c"BlockOp", trace_flow_id);
                state.queue.push_back(request);
                if let Some(waker) = state.poller_waker.clone() {
                    state.poll_send_requests(&mut Context::from_waker(&waker));
                }
                (request_id, trace_flow_id)
            };
            ResponseFuture::new(self.fifo_state.clone(), request_id).await?;
            trace::duration!(c"storage", c"BlockOp::end");
            trace::flow_end!(c"storage", c"BlockOp", trace_flow_id);
            Ok(())
        }
        .trace(trace_args)
        .await
    }

    async fn detach_vmo(&self, vmo_id: VmoId) -> Result<(), zx::Status> {
        self.send(BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::CloseVmo.into_primitive(),
                flags: 0,
                ..Default::default()
            },
            vmoid: vmo_id.into_id(),
            ..Default::default()
        })
        .await
    }

    async fn read_at(
        &self,
        buffer_slice: MutableBufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status> {
        match buffer_slice {
            MutableBufferSlice::VmoId { vmo_id, offset, length } => {
                self.send(BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: 0,
                        ..Default::default()
                    },
                    vmoid: vmo_id.id(),
                    length: self
                        .to_blocks(length)?
                        .try_into()
                        .map_err(|_| zx::Status::INVALID_ARGS)?,
                    vmo_offset: self.to_blocks(offset)?,
                    dev_offset: self.to_blocks(device_offset)?,
                    ..Default::default()
                })
                .await?
            }
            MutableBufferSlice::Memory(mut slice) => {
                let temp_vmo = self.temp_vmo.lock().await;
                let mut device_block = self.to_blocks(device_offset)?;
                loop {
                    let to_do = std::cmp::min(TEMP_VMO_SIZE, slice.len());
                    let block_count = self.to_blocks(to_do as u64)? as u32;
                    self.send(BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: 0,
                            ..Default::default()
                        },
                        vmoid: self.temp_vmo_id.id(),
                        length: block_count,
                        vmo_offset: 0,
                        dev_offset: device_block,
                        ..Default::default()
                    })
                    .await?;
                    temp_vmo.read(&mut slice[..to_do], 0)?;
                    if to_do == slice.len() {
                        break;
                    }
                    device_block += block_count as u64;
                    slice = &mut slice[to_do..];
                }
            }
        }
        Ok(())
    }

    async fn write_at(
        &self,
        buffer_slice: BufferSlice<'_>,
        device_offset: u64,
        opts: WriteOptions,
    ) -> Result<(), zx::Status> {
        let flags = if opts.contains(WriteOptions::FORCE_ACCESS) {
            BlockIoFlag::FORCE_ACCESS.bits()
        } else {
            0
        };
        match buffer_slice {
            BufferSlice::VmoId { vmo_id, offset, length } => {
                self.send(BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Write.into_primitive(),
                        flags,
                        ..Default::default()
                    },
                    vmoid: vmo_id.id(),
                    length: self
                        .to_blocks(length)?
                        .try_into()
                        .map_err(|_| zx::Status::INVALID_ARGS)?,
                    vmo_offset: self.to_blocks(offset)?,
                    dev_offset: self.to_blocks(device_offset)?,
                    ..Default::default()
                })
                .await?;
            }
            BufferSlice::Memory(mut slice) => {
                let temp_vmo = self.temp_vmo.lock().await;
                let mut device_block = self.to_blocks(device_offset)?;
                loop {
                    let to_do = std::cmp::min(TEMP_VMO_SIZE, slice.len());
                    let block_count = self.to_blocks(to_do as u64)? as u32;
                    temp_vmo.write(&slice[..to_do], 0)?;
                    self.send(BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Write.into_primitive(),
                            flags,
                            ..Default::default()
                        },
                        vmoid: self.temp_vmo_id.id(),
                        length: block_count,
                        vmo_offset: 0,
                        dev_offset: device_block,
                        ..Default::default()
                    })
                    .await?;
                    if to_do == slice.len() {
                        break;
                    }
                    device_block += block_count as u64;
                    slice = &slice[to_do..];
                }
            }
        }
        Ok(())
    }

    async fn trim(&self, device_range: Range<u64>) -> Result<(), zx::Status> {
        let length = self.to_blocks(device_range.end - device_range.start)? as u32;
        let dev_offset = self.to_blocks(device_range.start)?;
        self.send(BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Trim.into_primitive(),
                flags: 0,
                ..Default::default()
            },
            vmoid: block_driver::BLOCK_VMOID_INVALID,
            length,
            dev_offset,
            ..Default::default()
        })
        .await
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        self.send(BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Flush.into_primitive(),
                flags: 0,
                ..Default::default()
            },
            vmoid: block_driver::BLOCK_VMOID_INVALID,
            ..Default::default()
        })
        .await
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn block_flags(&self) -> BlockFlags {
        self.block_flags
    }

    fn is_connected(&self) -> bool {
        self.fifo_state.lock().unwrap().fifo.is_some()
    }
}

impl Drop for Common {
    fn drop(&mut self) {
        // It's OK to leak the VMO id because the server will dump all VMOs when the fifo is torn
        // down.
        let _ = self.temp_vmo_id.take().into_id();
        self.fifo_state.lock().unwrap().terminate();
    }
}

/// RemoteBlockClient is a BlockClient that communicates with a real block device over FIDL.
pub struct RemoteBlockClient {
    session: block::SessionProxy,
    common: Common,
}

pub trait AsBlockProxy {
    fn get_info(&self) -> impl Future<Output = Result<block::BlockGetInfoResult, fidl::Error>>;

    fn open_session(&self, session: ServerEnd<block::SessionMarker>) -> Result<(), fidl::Error>;
}

impl<T: AsBlockProxy> AsBlockProxy for &T {
    fn get_info(&self) -> impl Future<Output = Result<block::BlockGetInfoResult, fidl::Error>> {
        AsBlockProxy::get_info(*self)
    }
    fn open_session(&self, session: ServerEnd<block::SessionMarker>) -> Result<(), fidl::Error> {
        AsBlockProxy::open_session(*self, session)
    }
}

macro_rules! impl_as_block_proxy {
    ($name:ident) => {
        impl AsBlockProxy for $name {
            async fn get_info(&self) -> Result<block::BlockGetInfoResult, fidl::Error> {
                $name::get_info(self).await
            }

            fn open_session(
                &self,
                session: ServerEnd<block::SessionMarker>,
            ) -> Result<(), fidl::Error> {
                $name::open_session(self, session)
            }
        }
    };
}

impl_as_block_proxy!(BlockProxy);
impl_as_block_proxy!(PartitionProxy);
impl_as_block_proxy!(VolumeProxy);

impl RemoteBlockClient {
    /// Returns a connection to a remote block device via the given channel.
    pub async fn new(remote: impl AsBlockProxy) -> Result<Self, zx::Status> {
        let info =
            remote.get_info().await.map_err(fidl_to_status)?.map_err(zx::Status::from_raw)?;
        let (session, server) = fidl::endpoints::create_proxy().map_err(fidl_to_status)?;
        let () = remote.open_session(server).map_err(fidl_to_status)?;
        let fifo =
            session.get_fifo().await.map_err(fidl_to_status)?.map_err(zx::Status::from_raw)?;
        let fifo = fasync::Fifo::from_fifo(fifo);
        let temp_vmo = zx::Vmo::create(TEMP_VMO_SIZE as u64)?;
        let dup = temp_vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
        let vmo_id =
            session.attach_vmo(dup).await.map_err(fidl_to_status)?.map_err(zx::Status::from_raw)?;
        let vmo_id = VmoId::new(vmo_id.id);
        Ok(RemoteBlockClient { session, common: Common::new(fifo, &info, temp_vmo, vmo_id) })
    }
}

#[async_trait]
impl BlockClient for RemoteBlockClient {
    async fn attach_vmo(&self, vmo: &zx::Vmo) -> Result<VmoId, zx::Status> {
        let dup = vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
        let vmo_id = self
            .session
            .attach_vmo(dup)
            .await
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        Ok(VmoId::new(vmo_id.id))
    }

    async fn detach_vmo(&self, vmo_id: VmoId) -> Result<(), zx::Status> {
        self.common.detach_vmo(vmo_id).await
    }

    async fn read_at(
        &self,
        buffer_slice: MutableBufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status> {
        self.common.read_at(buffer_slice, device_offset).await
    }

    async fn write_at_with_opts(
        &self,
        buffer_slice: BufferSlice<'_>,
        device_offset: u64,
        opts: WriteOptions,
    ) -> Result<(), zx::Status> {
        self.common.write_at(buffer_slice, device_offset, opts).await
    }

    async fn trim(&self, range: Range<u64>) -> Result<(), zx::Status> {
        self.common.trim(range).await
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        self.common.flush().await
    }

    async fn close(&self) -> Result<(), zx::Status> {
        let () =
            self.session.close().await.map_err(fidl_to_status)?.map_err(zx::Status::from_raw)?;
        Ok(())
    }

    fn block_size(&self) -> u32 {
        self.common.block_size()
    }

    fn block_count(&self) -> u64 {
        self.common.block_count()
    }

    fn block_flags(&self) -> BlockFlags {
        self.common.block_flags()
    }

    fn is_connected(&self) -> bool {
        self.common.is_connected()
    }
}

pub struct RemoteBlockClientSync {
    session: block::SessionSynchronousProxy,
    common: Common,
}

impl RemoteBlockClientSync {
    /// Returns a connection to a remote block device via the given channel, but spawns a separate
    /// thread for polling the fifo which makes it work in cases where no executor is configured for
    /// the calling thread.
    pub fn new(
        client_end: fidl::endpoints::ClientEnd<block::BlockMarker>,
    ) -> Result<Self, zx::Status> {
        let remote = block::BlockSynchronousProxy::new(client_end.into_channel());
        let info = remote
            .get_info(zx::MonotonicInstant::INFINITE)
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        let (client, server) = fidl::endpoints::create_endpoints();
        let () = remote.open_session(server).map_err(fidl_to_status)?;
        let session = block::SessionSynchronousProxy::new(client.into_channel());
        let fifo = session
            .get_fifo(zx::MonotonicInstant::INFINITE)
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        let temp_vmo = zx::Vmo::create(TEMP_VMO_SIZE as u64)?;
        let dup = temp_vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
        let vmo_id = session
            .attach_vmo(dup, zx::MonotonicInstant::INFINITE)
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        let vmo_id = VmoId::new(vmo_id.id);

        // The fifo needs to be instantiated from the thread that has the executor as that's where
        // the fifo registers for notifications to be delivered.
        let (sender, receiver) = oneshot::channel::<Result<Self, zx::Status>>();
        std::thread::spawn(move || {
            let mut executor = fasync::LocalExecutor::new();
            let fifo = fasync::Fifo::from_fifo(fifo);
            let common = Common::new(fifo, &info, temp_vmo, vmo_id);
            let fifo_state = common.fifo_state.clone();
            let _ = sender.send(Ok(RemoteBlockClientSync { session, common }));
            executor.run_singlethreaded(FifoPoller { fifo_state });
        });
        block_on(receiver).map_err(|_| zx::Status::CANCELED)?
    }

    pub fn attach_vmo(&self, vmo: &zx::Vmo) -> Result<VmoId, zx::Status> {
        let dup = vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
        let vmo_id = self
            .session
            .attach_vmo(dup, zx::MonotonicInstant::INFINITE)
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        Ok(VmoId::new(vmo_id.id))
    }

    pub fn detach_vmo(&self, vmo_id: VmoId) -> Result<(), zx::Status> {
        block_on(self.common.detach_vmo(vmo_id))
    }

    pub fn read_at(
        &self,
        buffer_slice: MutableBufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status> {
        block_on(self.common.read_at(buffer_slice, device_offset))
    }

    pub fn write_at(
        &self,
        buffer_slice: BufferSlice<'_>,
        device_offset: u64,
    ) -> Result<(), zx::Status> {
        block_on(self.common.write_at(buffer_slice, device_offset, WriteOptions::empty()))
    }

    pub fn flush(&self) -> Result<(), zx::Status> {
        block_on(self.common.flush())
    }

    pub fn close(&self) -> Result<(), zx::Status> {
        let () = self
            .session
            .close(zx::MonotonicInstant::INFINITE)
            .map_err(fidl_to_status)?
            .map_err(zx::Status::from_raw)?;
        Ok(())
    }

    pub fn block_size(&self) -> u32 {
        self.common.block_size()
    }

    pub fn block_count(&self) -> u64 {
        self.common.block_count()
    }

    pub fn is_connected(&self) -> bool {
        self.common.is_connected()
    }
}

impl Drop for RemoteBlockClientSync {
    fn drop(&mut self) {
        // Ignore errors here as there is not much we can do about it.
        let _ = self.close();
    }
}

// FifoPoller is a future responsible for sending and receiving from the fifo.
struct FifoPoller {
    fifo_state: FifoStateRef,
}

impl Future for FifoPoller {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state_lock = self.fifo_state.lock().unwrap();
        let state = state_lock.deref_mut(); // So that we can split the borrow.

        // Send requests.
        if state.poll_send_requests(context) {
            return Poll::Ready(());
        }

        // Receive responses.
        let fifo = state.fifo.as_ref().unwrap(); // Safe because poll_send_requests checks.
        while let Poll::Ready(result) = fifo.read_one(context) {
            match result {
                Ok(response) => {
                    let request_id = response.reqid;
                    // If the request isn't in the map, assume that it's a cancelled read.
                    if let Some(request_state) = state.map.get_mut(&request_id) {
                        request_state.result.replace(zx::Status::from_raw(response.status));
                        if let Some(waker) = request_state.waker.take() {
                            waker.wake();
                        }
                    }
                }
                Err(_) => {
                    state.terminate();
                    return Poll::Ready(());
                }
            }
        }

        state.poller_waker = Some(context.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockClient, BlockFifoRequest, BlockFifoResponse, BufferSlice, MutableBufferSlice,
        RemoteBlockClient, RemoteBlockClientSync, WriteOptions,
    };
    use block_server::{BlockServer, PartitionInfo};
    use fidl::endpoints::RequestStream as _;
    use fidl_fuchsia_hardware_block as block;
    use fuchsia_async::{self as fasync, FifoReadable as _, FifoWritable as _};
    use futures::future::{AbortHandle, Abortable, TryFutureExt as _};
    use futures::join;
    use futures::stream::futures_unordered::FuturesUnordered;
    use futures::stream::StreamExt as _;
    use ramdevice_client::RamdiskClient;
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const RAMDISK_BLOCK_SIZE: u64 = 1024;
    const RAMDISK_BLOCK_COUNT: u64 = 1024;

    pub async fn make_ramdisk() -> (RamdiskClient, block::BlockProxy, RemoteBlockClient) {
        let ramdisk = RamdiskClient::create(RAMDISK_BLOCK_SIZE, RAMDISK_BLOCK_COUNT)
            .await
            .expect("RamdiskClient::create failed");
        let client_end = ramdisk.open().expect("ramdisk.open failed");
        let proxy = client_end.into_proxy();
        let block_client = RemoteBlockClient::new(proxy).await.expect("new failed");
        assert_eq!(block_client.block_size(), 1024);
        let client_end = ramdisk.open().expect("ramdisk.open failed");
        let proxy = client_end.into_proxy();
        (ramdisk, proxy, block_client)
    }

    #[fuchsia::test]
    async fn test_against_ram_disk() {
        let (_ramdisk, block_proxy, block_client) = make_ramdisk().await;

        let stats_before = block_proxy
            .get_stats(false)
            .await
            .expect("get_stats failed")
            .map_err(zx::Status::from_raw)
            .expect("get_stats error");

        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        vmo.write(b"hello", 5).expect("vmo.write failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        block_client
            .write_at(BufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0)
            .await
            .expect("write_at failed");
        block_client
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 1024, 2048), 0)
            .await
            .expect("read_at failed");
        let mut buf: [u8; 5] = Default::default();
        vmo.read(&mut buf, 1029).expect("vmo.read failed");
        assert_eq!(&buf, b"hello");
        block_client.detach_vmo(vmo_id).await.expect("detach_vmo failed");

        // check that the stats are what we expect them to be
        let stats_after = block_proxy
            .get_stats(false)
            .await
            .expect("get_stats failed")
            .map_err(zx::Status::from_raw)
            .expect("get_stats error");
        // write stats
        assert_eq!(
            stats_before.write.success.total_calls + 1,
            stats_after.write.success.total_calls
        );
        assert_eq!(
            stats_before.write.success.bytes_transferred + 1024,
            stats_after.write.success.bytes_transferred
        );
        assert_eq!(stats_before.write.failure.total_calls, stats_after.write.failure.total_calls);
        // read stats
        assert_eq!(stats_before.read.success.total_calls + 1, stats_after.read.success.total_calls);
        assert_eq!(
            stats_before.read.success.bytes_transferred + 2048,
            stats_after.read.success.bytes_transferred
        );
        assert_eq!(stats_before.read.failure.total_calls, stats_after.read.failure.total_calls);
    }

    #[fuchsia::test]
    async fn test_against_ram_disk_with_flush() {
        let (_ramdisk, block_proxy, block_client) = make_ramdisk().await;

        let stats_before = block_proxy
            .get_stats(false)
            .await
            .expect("get_stats failed")
            .map_err(zx::Status::from_raw)
            .expect("get_stats error");

        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        vmo.write(b"hello", 5).expect("vmo.write failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        block_client
            .write_at(BufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0)
            .await
            .expect("write_at failed");
        block_client.flush().await.expect("flush failed");
        block_client
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 1024, 2048), 0)
            .await
            .expect("read_at failed");
        let mut buf: [u8; 5] = Default::default();
        vmo.read(&mut buf, 1029).expect("vmo.read failed");
        assert_eq!(&buf, b"hello");
        block_client.detach_vmo(vmo_id).await.expect("detach_vmo failed");

        // check that the stats are what we expect them to be
        let stats_after = block_proxy
            .get_stats(false)
            .await
            .expect("get_stats failed")
            .map_err(zx::Status::from_raw)
            .expect("get_stats error");
        // write stats
        assert_eq!(
            stats_before.write.success.total_calls + 1,
            stats_after.write.success.total_calls
        );
        assert_eq!(
            stats_before.write.success.bytes_transferred + 1024,
            stats_after.write.success.bytes_transferred
        );
        assert_eq!(stats_before.write.failure.total_calls, stats_after.write.failure.total_calls);
        // flush stats
        assert_eq!(
            stats_before.flush.success.total_calls + 1,
            stats_after.flush.success.total_calls
        );
        assert_eq!(stats_before.flush.failure.total_calls, stats_after.flush.failure.total_calls);
        // read stats
        assert_eq!(stats_before.read.success.total_calls + 1, stats_after.read.success.total_calls);
        assert_eq!(
            stats_before.read.success.bytes_transferred + 2048,
            stats_after.read.success.bytes_transferred
        );
        assert_eq!(stats_before.read.failure.total_calls, stats_after.read.failure.total_calls);
    }

    #[fuchsia::test]
    async fn test_alignment() {
        let (_ramdisk, _block_proxy, block_client) = make_ramdisk().await;
        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        block_client
            .write_at(BufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 1)
            .await
            .expect_err("expected failure due to bad alignment");
        block_client.detach_vmo(vmo_id).await.expect("detach_vmo failed");
    }

    #[fuchsia::test]
    async fn test_parallel_io() {
        let (_ramdisk, _block_proxy, block_client) = make_ramdisk().await;
        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        let mut reads = Vec::new();
        for _ in 0..1024 {
            reads.push(
                block_client
                    .read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0)
                    .inspect_err(|e| panic!("read should have succeeded: {}", e)),
            );
        }
        futures::future::join_all(reads).await;
        block_client.detach_vmo(vmo_id).await.expect("detach_vmo failed");
    }

    #[fuchsia::test]
    async fn test_closed_device() {
        let (ramdisk, _block_proxy, block_client) = make_ramdisk().await;
        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        let mut reads = Vec::new();
        for _ in 0..1024 {
            reads.push(
                block_client.read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0),
            );
        }
        assert!(block_client.is_connected());
        let _ = futures::join!(futures::future::join_all(reads), async {
            ramdisk.destroy().await.expect("ramdisk.destroy failed")
        });
        // Destroying the ramdisk is asynchronous. Keep issuing reads until they start failing.
        while block_client
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0)
            .await
            .is_ok()
        {}

        // Sometimes the FIFO will start rejecting requests before FIFO is actually closed, so we
        // get false-positives from is_connected.
        while block_client.is_connected() {
            // Sleep for a bit to minimise lock contention.
            fasync::Timer::new(fasync::MonotonicInstant::after(
                zx::MonotonicDuration::from_millis(500),
            ))
            .await;
        }

        // But once is_connected goes negative, it should stay negative.
        assert_eq!(block_client.is_connected(), false);
        let _ = block_client.detach_vmo(vmo_id).await;
    }

    #[fuchsia::test]
    async fn test_cancelled_reads() {
        let (_ramdisk, _block_proxy, block_client) = make_ramdisk().await;
        let vmo = zx::Vmo::create(131072).expect("Vmo::create failed");
        let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
        {
            let mut reads = FuturesUnordered::new();
            for _ in 0..1024 {
                reads.push(
                    block_client.read_at(MutableBufferSlice::new_with_vmo_id(&vmo_id, 0, 1024), 0),
                );
            }
            // Read the first 500 results and then dump the rest.
            for _ in 0..500 {
                reads.next().await;
            }
        }
        block_client.detach_vmo(vmo_id).await.expect("detach_vmo failed");
    }

    #[fuchsia::test]
    async fn test_parallel_large_read_and_write_with_memory_succeds() {
        let (_ramdisk, _block_proxy, block_client) = make_ramdisk().await;
        let block_client_ref = &block_client;
        let test_one = |offset, len, fill| async move {
            let buf = vec![fill; len];
            block_client_ref.write_at(buf[..].into(), offset).await.expect("write_at failed");
            // Read back an extra block either side.
            let mut read_buf = vec![0u8; len + 2 * RAMDISK_BLOCK_SIZE as usize];
            block_client_ref
                .read_at(read_buf.as_mut_slice().into(), offset - RAMDISK_BLOCK_SIZE)
                .await
                .expect("read_at failed");
            assert_eq!(
                &read_buf[0..RAMDISK_BLOCK_SIZE as usize],
                &[0; RAMDISK_BLOCK_SIZE as usize][..]
            );
            assert_eq!(
                &read_buf[RAMDISK_BLOCK_SIZE as usize..RAMDISK_BLOCK_SIZE as usize + len],
                &buf[..]
            );
            assert_eq!(
                &read_buf[RAMDISK_BLOCK_SIZE as usize + len..],
                &[0; RAMDISK_BLOCK_SIZE as usize][..]
            );
        };
        const WRITE_LEN: usize = super::TEMP_VMO_SIZE * 3 + RAMDISK_BLOCK_SIZE as usize;
        join!(
            test_one(RAMDISK_BLOCK_SIZE, WRITE_LEN, 0xa3u8),
            test_one(2 * RAMDISK_BLOCK_SIZE + WRITE_LEN as u64, WRITE_LEN, 0x7fu8)
        );
    }

    // Implements dummy server which can be used by test cases to verify whether
    // channel messages and fifo operations are being received - by using set_channel_handler or
    // set_fifo_hander respectively
    struct FakeBlockServer<'a> {
        server_channel: Option<fidl::endpoints::ServerEnd<block::BlockMarker>>,
        channel_handler: Box<dyn Fn(&block::SessionRequest) -> bool + 'a>,
        fifo_handler: Box<dyn Fn(BlockFifoRequest) -> BlockFifoResponse + 'a>,
    }

    impl<'a> FakeBlockServer<'a> {
        // Creates a new FakeBlockServer given a channel to listen on.
        //
        // 'channel_handler' and 'fifo_handler' closures allow for customizing the way how the server
        // handles requests received from channel or the fifo respectfully.
        //
        // 'channel_handler' receives a message before it is handled by the default implementation
        // and can return 'true' to indicate all processing is done and no further processing of
        // that message is required
        //
        // 'fifo_handler' takes as input a BlockFifoRequest and produces a response which the
        // FakeBlockServer will send over the fifo.
        fn new(
            server_channel: fidl::endpoints::ServerEnd<block::BlockMarker>,
            channel_handler: impl Fn(&block::SessionRequest) -> bool + 'a,
            fifo_handler: impl Fn(BlockFifoRequest) -> BlockFifoResponse + 'a,
        ) -> FakeBlockServer<'a> {
            FakeBlockServer {
                server_channel: Some(server_channel),
                channel_handler: Box::new(channel_handler),
                fifo_handler: Box::new(fifo_handler),
            }
        }

        // Runs the server
        async fn run(&mut self) {
            let server = self.server_channel.take().unwrap();

            // Set up a mock server.
            let (server_fifo, client_fifo) =
                zx::Fifo::<BlockFifoRequest, BlockFifoResponse>::create(16)
                    .expect("Fifo::create failed");
            let maybe_server_fifo = std::sync::Mutex::new(Some(client_fifo));

            let (fifo_future_abort, fifo_future_abort_registration) = AbortHandle::new_pair();
            let fifo_future = Abortable::new(
                async {
                    let fifo = fasync::Fifo::from_fifo(server_fifo);
                    loop {
                        let request = match fifo.read_entry().await {
                            Ok(r) => r,
                            Err(zx::Status::PEER_CLOSED) => break,
                            Err(e) => panic!("read_entry failed {:?}", e),
                        };

                        let response = self.fifo_handler.as_ref()(request);
                        fifo.write_entries(std::slice::from_ref(&response))
                            .await
                            .expect("write_entries failed");
                    }
                },
                fifo_future_abort_registration,
            );

            let channel_future = async {
                server
                    .into_stream()
                    .expect("into_stream failed")
                    .for_each_concurrent(None, |request| async {
                        let request = request.expect("unexpected fidl error");

                        match request {
                            block::BlockRequest::GetInfo { responder } => {
                                responder
                                    .send(Ok(&block::BlockInfo {
                                        block_count: 1024,
                                        block_size: 512,
                                        max_transfer_size: 1024 * 1024,
                                        flags: block::Flag::empty(),
                                    }))
                                    .expect("send failed");
                            }
                            block::BlockRequest::OpenSession { session, control_handle: _ } => {
                                let stream = session.into_stream().expect("into_stream failed");
                                stream
                                    .for_each(|request| async {
                                        let request = request.expect("unexpected fidl error");
                                        // Give a chance for the test to register and potentially
                                        // handle the event
                                        if self.channel_handler.as_ref()(&request) {
                                            return;
                                        }
                                        match request {
                                            block::SessionRequest::GetFifo { responder } => {
                                                match maybe_server_fifo.lock().unwrap().take() {
                                                    Some(fifo) => {
                                                        responder.send(Ok(fifo.downcast()))
                                                    }
                                                    None => responder.send(Err(
                                                        zx::Status::NO_RESOURCES.into_raw(),
                                                    )),
                                                }
                                                .expect("send failed")
                                            }
                                            block::SessionRequest::AttachVmo {
                                                vmo: _,
                                                responder,
                                            } => responder
                                                .send(Ok(&block::VmoId { id: 1 }))
                                                .expect("send failed"),
                                            block::SessionRequest::Close { responder } => {
                                                fifo_future_abort.abort();
                                                responder.send(Ok(())).expect("send failed")
                                            }
                                        }
                                    })
                                    .await
                            }
                            _ => panic!("Unexpected message"),
                        }
                    })
                    .await;
            };

            let _result = join!(fifo_future, channel_future);
            //_result can be Err(Aborted) since FifoClose calls .abort but that's expected
        }
    }

    #[fuchsia::test]
    async fn test_block_close_is_called() {
        let close_called = std::sync::Mutex::new(false);
        let (client_end, server) = fidl::endpoints::create_endpoints::<block::BlockMarker>();

        std::thread::spawn(move || {
            let _block_client =
                RemoteBlockClientSync::new(client_end).expect("RemoteBlockClientSync::new failed");
            // The drop here should cause Close to be sent.
        });

        let channel_handler = |request: &block::SessionRequest| -> bool {
            if let block::SessionRequest::Close { .. } = request {
                *close_called.lock().unwrap() = true;
            }
            false
        };
        FakeBlockServer::new(server, channel_handler, |_| unreachable!()).run().await;

        // After the server has finished running, we can check to see that close was called.
        assert!(*close_called.lock().unwrap());
    }

    #[fuchsia::test]
    async fn test_block_flush_is_called() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<block::BlockMarker>()
            .expect("create_proxy failed");

        struct Interface {
            flush_called: Arc<AtomicBool>,
        }
        impl block_server::async_interface::Interface for Interface {
            async fn get_info(&self) -> Result<Cow<'_, PartitionInfo>, zx::Status> {
                Ok(Cow::Owned(PartitionInfo {
                    block_range: Some(0..1000),
                    type_guid: [0; 16],
                    instance_guid: [0; 16],
                    name: Some("foo".to_string()),
                    flags: 0,
                }))
            }

            async fn read(
                &self,
                _device_block_offset: u64,
                _block_count: u32,
                _vmo: &Arc<zx::Vmo>,
                _vmo_offset: u64,
            ) -> Result<(), zx::Status> {
                unreachable!();
            }

            async fn write(
                &self,
                _device_block_offset: u64,
                _block_count: u32,
                _vmo: &Arc<zx::Vmo>,
                _vmo_offset: u64,
                _opts: WriteOptions,
            ) -> Result<(), zx::Status> {
                unreachable!();
            }

            async fn flush(&self) -> Result<(), zx::Status> {
                self.flush_called.store(true, Ordering::Relaxed);
                Ok(())
            }

            async fn trim(
                &self,
                _device_block_offset: u64,
                _block_count: u32,
            ) -> Result<(), zx::Status> {
                unreachable!();
            }
        }

        let flush_called = Arc::new(AtomicBool::new(false));

        futures::join!(
            async {
                let block_client = RemoteBlockClient::new(proxy).await.expect("new failed");

                block_client.flush().await.expect("flush failed");
            },
            async {
                let block_server = BlockServer::new(
                    512,
                    Arc::new(Interface { flush_called: flush_called.clone() }),
                );
                block_server.handle_requests(stream.cast_stream()).await.unwrap();
            }
        );

        assert!(flush_called.load(Ordering::Relaxed));
    }

    #[fuchsia::test]
    async fn test_trace_flow_ids_set() {
        let (proxy, server) = fidl::endpoints::create_proxy().expect("create_proxy failed");

        futures::join!(
            async {
                let block_client = RemoteBlockClient::new(proxy).await.expect("new failed");
                block_client.flush().await.expect("flush failed");
            },
            async {
                let flow_id: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);
                let fifo_handler = |request: BlockFifoRequest| -> BlockFifoResponse {
                    if request.trace_flow_id > 0 {
                        *flow_id.lock().unwrap() = Some(request.trace_flow_id);
                    }
                    BlockFifoResponse {
                        status: zx::Status::OK.into_raw(),
                        reqid: request.reqid,
                        ..Default::default()
                    }
                };
                FakeBlockServer::new(server, |_| false, fifo_handler).run().await;
                // After the server has finished running, verify the trace flow ID was set to some value.
                assert!(flow_id.lock().unwrap().is_some());
            }
        );
    }
}
