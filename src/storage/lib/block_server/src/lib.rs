// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::bin::Bin;
use anyhow::{anyhow, Error};
use block_protocol::{BlockFifoRequest, BlockFifoResponse};
use fidl_fuchsia_hardware_block::MAX_TRANSFER_UNBOUNDED;
use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
use fuchsia_sync::Mutex;
use futures::{Future, FutureExt as _, TryStreamExt as _};
use slab::Slab;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZero;
use std::ops::Range;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use zx::HandleBased;
use {
    fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_partition as fpartition,
    fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_async as fasync,
};

pub mod async_interface;
mod bin;
pub mod c_interface;

pub(crate) const FIFO_MAX_REQUESTS: usize = 64;

type TraceFlowId = Option<NonZero<u64>>;

#[derive(Clone)]
pub enum DeviceInfo {
    Block(BlockInfo),
    Partition(PartitionInfo),
}

impl DeviceInfo {
    fn block_count(&self) -> Option<u64> {
        match self {
            Self::Block(BlockInfo { block_count, .. }) => Some(*block_count),
            Self::Partition(PartitionInfo { block_range, .. }) => {
                block_range.as_ref().map(|range| range.end - range.start)
            }
        }
    }

    fn max_transfer_blocks(&self) -> Option<NonZero<u32>> {
        match self {
            Self::Block(BlockInfo { max_transfer_blocks, .. }) => max_transfer_blocks.clone(),
            Self::Partition(PartitionInfo { max_transfer_blocks, .. }) => {
                max_transfer_blocks.clone()
            }
        }
    }

    fn max_transfer_size(&self, block_size: u32) -> u32 {
        if let Some(max_blocks) = self.max_transfer_blocks() {
            max_blocks.get() * block_size
        } else {
            MAX_TRANSFER_UNBOUNDED
        }
    }
}

/// Information associated with non-partition block devices.
#[derive(Clone)]
pub struct BlockInfo {
    pub device_flags: fblock::Flag,
    pub block_count: u64,
    pub max_transfer_blocks: Option<NonZero<u32>>,
}

/// Information associated with a block device that is also a partition.
#[derive(Clone)]
pub struct PartitionInfo {
    /// The device flags reported by the underlying device.
    pub device_flags: fblock::Flag,
    pub max_transfer_blocks: Option<NonZero<u32>>,
    /// If `block_range` is None, the partition is a volume and may not be contiguous.
    /// In this case, the server will use the `get_volume_info` method to get the count of assigned
    /// slices and use that (along with the slice and block sizes) to determine the block count.
    pub block_range: Option<Range<u64>>,
    pub type_guid: [u8; 16],
    pub instance_guid: [u8; 16],
    pub name: String,
    pub flags: u64,
}

/// We internally keep track of active requests, so that when the server is torn down, we can
/// deallocate all of the resources for pending requests.
struct ActiveRequest<S> {
    session: S,
    group_or_request: GroupOrRequest,
    trace_flow_id: TraceFlowId,
    vmo_bin_key: Option<usize>,
    status: zx::Status,
    count: u32,
    req_id: Option<u32>,
}

pub struct ActiveRequests<S>(Mutex<ActiveRequestsInner<S>>);

impl<S> Default for ActiveRequests<S> {
    fn default() -> Self {
        Self(Mutex::new(ActiveRequestsInner { requests: Slab::default(), vmo_bin: Bin::new() }))
    }
}

impl<S> ActiveRequests<S> {
    fn complete_and_take_response(
        &self,
        request_id: RequestId,
        status: zx::Status,
    ) -> Option<(S, BlockFifoResponse)> {
        self.0.lock().complete_and_take_response(request_id, status)
    }
}

struct ActiveRequestsInner<S> {
    requests: Slab<ActiveRequest<S>>,
    vmo_bin: Bin<Arc<zx::Vmo>>,
}

// Keeps track of all the requests that are currently being processed
impl<S> ActiveRequestsInner<S> {
    /// Completes a request.
    fn complete(&mut self, request_id: RequestId, status: zx::Status) {
        let group = &mut self.requests[request_id.0];

        group.count = group.count.checked_sub(1).unwrap();
        if status != zx::Status::OK && group.status == zx::Status::OK {
            group.status = status
        }

        fuchsia_trace::duration!(
            c"storage",
            c"block_server::finish_transaction",
            "request_id" => request_id.0,
            "group_completed" => group.count == 0,
            "status" => status.into_raw());
        if let Some(trace_flow_id) = group.trace_flow_id {
            fuchsia_trace::flow_step!(
                c"storage",
                c"block_server::finish_request",
                trace_flow_id.get().into()
            );
        }
    }

    /// Takes the response if all requests are finished.
    fn take_response(&mut self, request_id: RequestId) -> Option<(S, BlockFifoResponse)> {
        let group = &self.requests[request_id.0];
        match group.req_id {
            Some(reqid) if group.count == 0 => {
                let group = self.requests.remove(request_id.0);
                if let Some(vmo_bin_key) = group.vmo_bin_key {
                    self.vmo_bin.release(vmo_bin_key);
                }
                Some((
                    group.session,
                    BlockFifoResponse {
                        status: group.status.into_raw(),
                        reqid,
                        group: group.group_or_request.group_id().unwrap_or(0),
                        ..Default::default()
                    },
                ))
            }
            _ => None,
        }
    }

    /// Competes the request and returns a response if the request group is finished.
    fn complete_and_take_response(
        &mut self,
        request_id: RequestId,
        status: zx::Status,
    ) -> Option<(S, BlockFifoResponse)> {
        self.complete(request_id, status);
        self.take_response(request_id)
    }
}

/// BlockServer is an implementation of fuchsia.hardware.block.partition.Partition.
/// cbindgen:no-export
pub struct BlockServer<SM> {
    block_size: u32,
    session_manager: Arc<SM>,
}

/// A single entry in `[OffsetMap]`.
#[derive(Debug)]
pub struct BlockOffsetMapping {
    source_block_offset: u64,
    target_block_offset: u64,
    length: u64,
}

impl BlockOffsetMapping {
    fn are_blocks_within_source_range(&self, blocks: (u64, u32)) -> bool {
        blocks.0 >= self.source_block_offset
            && blocks.0 + blocks.1 as u64 - self.source_block_offset <= self.length
    }
}

impl std::convert::TryFrom<fblock::BlockOffsetMapping> for BlockOffsetMapping {
    type Error = zx::Status;

    fn try_from(wire: fblock::BlockOffsetMapping) -> Result<Self, Self::Error> {
        wire.source_block_offset.checked_add(wire.length).ok_or(zx::Status::INVALID_ARGS)?;
        wire.target_block_offset.checked_add(wire.length).ok_or(zx::Status::INVALID_ARGS)?;
        Ok(Self {
            source_block_offset: wire.source_block_offset,
            target_block_offset: wire.target_block_offset,
            length: wire.length,
        })
    }
}

/// Remaps the offset of block requests based on an internal map, and truncates long requests.
/// TODO(https://fxbug.dev/402515764): For now, this just supports a single entry in the map, which
/// is all that is required for GPT partitions.  If we want to support this for FVM, we will need
/// to support multiple entries, which requires changing the block server to support request
/// splitting.
pub struct OffsetMap {
    mapping: Option<BlockOffsetMapping>,
    max_transfer_blocks: Option<NonZero<u32>>,
}

impl OffsetMap {
    /// An OffsetMap that remaps requests.
    pub fn new(mapping: BlockOffsetMapping, max_transfer_blocks: Option<NonZero<u32>>) -> Self {
        Self { mapping: Some(mapping), max_transfer_blocks }
    }

    /// An OffsetMap that just enforces maximum request sizes.
    pub fn empty(max_transfer_blocks: Option<NonZero<u32>>) -> Self {
        Self { mapping: None, max_transfer_blocks }
    }

    pub fn is_empty(&self) -> bool {
        self.mapping.is_some()
    }

    fn mapping(&self) -> Option<&BlockOffsetMapping> {
        self.mapping.as_ref()
    }

    fn max_transfer_blocks(&self) -> Option<NonZero<u32>> {
        self.max_transfer_blocks
    }
}

// Methods take Arc<Self> rather than &self because of
// https://github.com/rust-lang/rust/issues/42940.
pub trait SessionManager: 'static {
    type Session;

    fn on_attach_vmo(
        self: Arc<Self>,
        vmo: &Arc<zx::Vmo>,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Creates a new session to handle `stream`.
    /// The returned future should run until the session completes, for example when the client end
    /// closes.
    /// `offset_map`, will be used to adjust the block offset/length of FIFO requests.
    fn open_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Called to get block/partition information for Block::GetInfo, Partition::GetTypeGuid, etc.
    fn get_info(&self) -> impl Future<Output = Result<Cow<'_, DeviceInfo>, zx::Status>> + Send;

    /// Called to handle the GetVolumeInfo FIDL call.
    fn get_volume_info(
        &self,
    ) -> impl Future<Output = Result<(fvolume::VolumeManagerInfo, fvolume::VolumeInfo), zx::Status>> + Send
    {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the QuerySlices FIDL call.
    fn query_slices(
        &self,
        _start_slices: &[u64],
    ) -> impl Future<Output = Result<Vec<fvolume::VsliceRange>, zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the Shrink FIDL call.
    fn extend(
        &self,
        _start_slice: u64,
        _slice_count: u64,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the Shrink FIDL call.
    fn shrink(
        &self,
        _start_slice: u64,
        _slice_count: u64,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Returns the active requests.
    fn active_requests(&self) -> &ActiveRequests<Self::Session>;
}

pub trait IntoSessionManager {
    type SM: SessionManager;

    fn into_session_manager(self) -> Arc<Self::SM>;
}

impl<SM: SessionManager> BlockServer<SM> {
    pub fn new(block_size: u32, session_manager: impl IntoSessionManager<SM = SM>) -> Self {
        Self { block_size, session_manager: session_manager.into_session_manager() }
    }

    /// Called to process requests for fuchsia.hardware.block.volume/Volume.
    pub async fn handle_requests(
        &self,
        mut requests: fvolume::VolumeRequestStream,
    ) -> Result<(), Error> {
        let scope = fasync::Scope::new();
        while let Some(request) = requests.try_next().await.unwrap() {
            if let Some(session) = self.handle_request(request).await? {
                scope.spawn(session.map(|_| ()));
            }
        }
        scope.await;
        Ok(())
    }

    /// Processes a partition request.  If a new session task is created in response to the request,
    /// it is returned.
    async fn handle_request(
        &self,
        request: fvolume::VolumeRequest,
    ) -> Result<Option<impl Future<Output = Result<(), Error>> + Send>, Error> {
        match request {
            fvolume::VolumeRequest::GetInfo { responder } => match self.device_info().await {
                Ok(info) => {
                    let max_transfer_size = info.max_transfer_size(self.block_size);
                    let (block_count, flags) = match info.as_ref() {
                        DeviceInfo::Block(BlockInfo { block_count, device_flags, .. }) => {
                            (*block_count, *device_flags)
                        }
                        DeviceInfo::Partition(partition_info) => {
                            let block_count = if let Some(range) =
                                partition_info.block_range.as_ref()
                            {
                                range.end - range.start
                            } else {
                                let volume_info = self.session_manager.get_volume_info().await?;
                                volume_info.0.slice_size * volume_info.1.partition_slice_count
                                    / self.block_size as u64
                            };
                            (block_count, partition_info.device_flags)
                        }
                    };
                    responder.send(Ok(&fblock::BlockInfo {
                        block_count,
                        block_size: self.block_size,
                        max_transfer_size,
                        flags,
                    }))?;
                }
                Err(status) => responder.send(Err(status.into_raw()))?,
            },
            fvolume::VolumeRequest::GetStats { clear: _, responder } => {
                // TODO(https://fxbug.dev/348077960): Implement this
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fvolume::VolumeRequest::OpenSession { session, control_handle: _ } => {
                match self.device_info().await {
                    Ok(info) => {
                        return Ok(Some(self.session_manager.clone().open_session(
                            session.into_stream(),
                            OffsetMap::empty(info.max_transfer_blocks()),
                            self.block_size,
                        )));
                    }
                    Err(status) => session.close_with_epitaph(status)?,
                }
            }
            fvolume::VolumeRequest::OpenSessionWithOffsetMap {
                session,
                offset_map,
                initial_mappings,
                control_handle: _,
            } => match self.device_info().await {
                Ok(info) => {
                    if offset_map.is_some()
                        || initial_mappings.as_ref().is_none_or(|m| m.len() != 1)
                    {
                        // TODO(https://fxbug.dev/402515764): Support multiple mappings and
                        // dynamic querying for FVM as needed.  A single static map is
                        // sufficient for GPT.
                        session.close_with_epitaph(zx::Status::NOT_SUPPORTED)?;
                        return Ok(None);
                    }
                    let initial_mapping: BlockOffsetMapping =
                        match initial_mappings.unwrap().pop().unwrap().try_into() {
                            Ok(m) => m,
                            Err(status) => {
                                session.close_with_epitaph(status)?;
                                return Ok(None);
                            }
                        };
                    if let Some(max) = info.block_count() {
                        if initial_mapping.target_block_offset + initial_mapping.length > max {
                            session.close_with_epitaph(zx::Status::INVALID_ARGS)?;
                            return Ok(None);
                        }
                    }
                    return Ok(Some(self.session_manager.clone().open_session(
                        session.into_stream(),
                        OffsetMap::new(initial_mapping, info.max_transfer_blocks()),
                        self.block_size,
                    )));
                }
                Err(status) => session.close_with_epitaph(status)?,
            },
            fvolume::VolumeRequest::GetTypeGuid { responder } => match self.device_info().await {
                Ok(info) => {
                    if let DeviceInfo::Partition(partition_info) = info.as_ref() {
                        let mut guid =
                            fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                        guid.value.copy_from_slice(&partition_info.type_guid);
                        responder.send(zx::sys::ZX_OK, Some(&guid))?;
                    } else {
                        responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
                    }
                }
                Err(status) => {
                    responder.send(status.into_raw(), None)?;
                }
            },
            fvolume::VolumeRequest::GetInstanceGuid { responder } => {
                match self.device_info().await {
                    Ok(info) => {
                        if let DeviceInfo::Partition(partition_info) = info.as_ref() {
                            let mut guid =
                                fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                            guid.value.copy_from_slice(&partition_info.instance_guid);
                            responder.send(zx::sys::ZX_OK, Some(&guid))?;
                        } else {
                            responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
                        }
                    }
                    Err(status) => {
                        responder.send(status.into_raw(), None)?;
                    }
                }
            }
            fvolume::VolumeRequest::GetName { responder } => match self.device_info().await {
                Ok(info) => {
                    if let DeviceInfo::Partition(partition_info) = info.as_ref() {
                        responder.send(zx::sys::ZX_OK, Some(&partition_info.name))?;
                    } else {
                        responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
                    }
                }
                Err(status) => {
                    responder.send(status.into_raw(), None)?;
                }
            },
            fvolume::VolumeRequest::GetMetadata { responder } => match self.device_info().await {
                Ok(info) => {
                    if let DeviceInfo::Partition(info) = info.as_ref() {
                        let mut type_guid =
                            fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                        type_guid.value.copy_from_slice(&info.type_guid);
                        let mut instance_guid =
                            fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                        instance_guid.value.copy_from_slice(&info.instance_guid);
                        responder.send(Ok(&fpartition::PartitionGetMetadataResponse {
                            name: Some(info.name.clone()),
                            type_guid: Some(type_guid),
                            instance_guid: Some(instance_guid),
                            start_block_offset: info.block_range.as_ref().map(|range| range.start),
                            num_blocks: info
                                .block_range
                                .as_ref()
                                .map(|range| range.end - range.start),
                            flags: Some(info.flags),
                            ..Default::default()
                        }))?;
                    }
                }
                Err(status) => responder.send(Err(status.into_raw()))?,
            },
            fvolume::VolumeRequest::QuerySlices { responder, start_slices } => {
                match self.session_manager.query_slices(&start_slices).await {
                    Ok(mut results) => {
                        let results_len = results.len();
                        assert!(results_len <= 16);
                        results.resize(16, fvolume::VsliceRange { allocated: false, count: 0 });
                        responder.send(
                            zx::sys::ZX_OK,
                            &results.try_into().unwrap(),
                            results_len as u64,
                        )?;
                    }
                    Err(s) => {
                        responder.send(
                            s.into_raw(),
                            &[fvolume::VsliceRange { allocated: false, count: 0 }; 16],
                            0,
                        )?;
                    }
                }
            }
            fvolume::VolumeRequest::GetVolumeInfo { responder, .. } => {
                match self.session_manager.get_volume_info().await {
                    Ok((manager_info, volume_info)) => {
                        responder.send(zx::sys::ZX_OK, Some(&manager_info), Some(&volume_info))?
                    }
                    Err(s) => responder.send(s.into_raw(), None, None)?,
                }
            }
            fvolume::VolumeRequest::Extend { responder, start_slice, slice_count } => {
                responder.send(
                    zx::Status::from(self.session_manager.extend(start_slice, slice_count).await)
                        .into_raw(),
                )?;
            }
            fvolume::VolumeRequest::Shrink { responder, start_slice, slice_count } => {
                responder.send(
                    zx::Status::from(self.session_manager.shrink(start_slice, slice_count).await)
                        .into_raw(),
                )?;
            }
            fvolume::VolumeRequest::Destroy { responder, .. } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED)?;
            }
        }
        Ok(None)
    }

    async fn device_info(&self) -> Result<Cow<'_, DeviceInfo>, zx::Status> {
        self.session_manager.get_info().await
    }
}

struct SessionHelper<SM: SessionManager> {
    session_manager: Arc<SM>,
    offset_map: OffsetMap,
    block_size: u32,
    peer_fifo: zx::Fifo<BlockFifoResponse, BlockFifoRequest>,
    vmos: Mutex<BTreeMap<u16, Arc<zx::Vmo>>>,
}

impl<SM: SessionManager> SessionHelper<SM> {
    fn new(
        session_manager: Arc<SM>,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> Result<(Self, zx::Fifo<BlockFifoRequest, BlockFifoResponse>), zx::Status> {
        let (peer_fifo, fifo) = zx::Fifo::create(16)?;
        Ok((
            Self { session_manager, offset_map, block_size, peer_fifo, vmos: Mutex::default() },
            fifo,
        ))
    }

    async fn handle_request(&self, request: fblock::SessionRequest) -> Result<(), Error> {
        match request {
            fblock::SessionRequest::GetFifo { responder } => {
                let rights = zx::Rights::TRANSFER
                    | zx::Rights::READ
                    | zx::Rights::WRITE
                    | zx::Rights::SIGNAL
                    | zx::Rights::WAIT;
                match self.peer_fifo.duplicate_handle(rights) {
                    Ok(fifo) => responder.send(Ok(fifo.downcast()))?,
                    Err(s) => responder.send(Err(s.into_raw()))?,
                }
                Ok(())
            }
            fblock::SessionRequest::AttachVmo { vmo, responder } => {
                let vmo = Arc::new(vmo);
                let vmo_id = {
                    let mut vmos = self.vmos.lock();
                    if vmos.len() == u16::MAX as usize {
                        responder.send(Err(zx::Status::NO_RESOURCES.into_raw()))?;
                        return Ok(());
                    } else {
                        let vmo_id = match vmos.last_entry() {
                            None => 1,
                            Some(o) => {
                                o.key().checked_add(1).unwrap_or_else(|| {
                                    let mut vmo_id = 1;
                                    // Find the first gap...
                                    for (&id, _) in &*vmos {
                                        if id > vmo_id {
                                            break;
                                        }
                                        vmo_id = id + 1;
                                    }
                                    vmo_id
                                })
                            }
                        };
                        vmos.insert(vmo_id, vmo.clone());
                        vmo_id
                    }
                };
                self.session_manager.clone().on_attach_vmo(&vmo).await?;
                responder.send(Ok(&fblock::VmoId { id: vmo_id }))?;
                Ok(())
            }
            fblock::SessionRequest::Close { responder } => {
                responder.send(Ok(()))?;
                Err(anyhow!("Closed"))
            }
        }
    }

    /// Decodes `request`.
    fn decode_fifo_request(
        &self,
        session: SM::Session,
        request: &BlockFifoRequest,
    ) -> Result<DecodedRequest, Option<BlockFifoResponse>> {
        let flags = BlockIoFlag::from_bits_truncate(request.command.flags);
        let mut operation = BlockOpcode::from_primitive(request.command.opcode)
            .ok_or(zx::Status::INVALID_ARGS)
            .and_then(|code| {
                if matches!(code, BlockOpcode::Read | BlockOpcode::Write | BlockOpcode::Trim) {
                    if request.length == 0 {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    // Make sure the end offsets won't wrap.
                    if request.dev_offset.checked_add(request.length as u64).is_none()
                        || (code != BlockOpcode::Trim
                            && (request.length as u64 * self.block_size as u64)
                                .checked_add(request.vmo_offset)
                                .is_none())
                    {
                        return Err(zx::Status::OUT_OF_RANGE);
                    }
                }
                Ok(match code {
                    BlockOpcode::Read => Operation::Read {
                        device_block_offset: request.dev_offset,
                        block_count: request.length,
                        _unused: 0,
                        vmo_offset: request
                            .vmo_offset
                            .checked_mul(self.block_size as u64)
                            .ok_or(zx::Status::OUT_OF_RANGE)?,
                    },
                    BlockOpcode::Write => {
                        let mut options = WriteOptions::empty();
                        if flags.contains(BlockIoFlag::FORCE_ACCESS) {
                            options |= WriteOptions::FORCE_ACCESS;
                        }
                        if flags.contains(BlockIoFlag::PRE_BARRIER) {
                            options |= WriteOptions::PRE_BARRIER;
                        }
                        Operation::Write {
                            device_block_offset: request.dev_offset,
                            block_count: request.length,
                            options: options,
                            vmo_offset: request
                                .vmo_offset
                                .checked_mul(self.block_size as u64)
                                .ok_or(zx::Status::OUT_OF_RANGE)?,
                        }
                    }
                    BlockOpcode::Flush => Operation::Flush,
                    BlockOpcode::Trim => Operation::Trim {
                        device_block_offset: request.dev_offset,
                        block_count: request.length,
                    },
                    BlockOpcode::CloseVmo => Operation::CloseVmo,
                })
            });

        let group_or_request = if flags.contains(BlockIoFlag::GROUP_ITEM) {
            GroupOrRequest::Group(request.group)
        } else {
            GroupOrRequest::Request(request.reqid)
        };

        let mut active_requests = self.session_manager.active_requests().0.lock();
        let mut request_id = None;

        // Multiple Block I/O request may be sent as a group.
        // Notes:
        // - the group is identified by the group id in the request
        // - if using groups, a response will not be sent unless `BlockIoFlag::GROUP_LAST`
        //   flag is set.
        // - when processing a request of a group fails, subsequent requests of that
        //   group will not be processed.
        //
        // Refer to sdk/fidl/fuchsia.hardware.block.driver/block.fidl for details.
        if group_or_request.is_group() {
            // Search for an existing entry that matches this group.  NOTE: This is a potentially
            // expensive way to find a group (it's iterating over all slots in the active-requests
            // slab).  This can be optimised easily should we need to.
            for (key, group) in &mut active_requests.requests {
                if group.group_or_request == group_or_request {
                    if group.req_id.is_some() {
                        // We have already received a request tagged as last.
                        if group.status == zx::Status::OK {
                            group.status = zx::Status::INVALID_ARGS;
                        }
                        // Ignore this request.
                        return Err(None);
                    }
                    if flags.contains(BlockIoFlag::GROUP_LAST) {
                        group.req_id = Some(request.reqid);
                        // If the group has had an error, there is no point trying to issue this
                        // request.
                        if group.status != zx::Status::OK {
                            operation = Err(group.status);
                        }
                    } else if group.status != zx::Status::OK {
                        // The group has already encountered an error, so there is no point trying
                        // to issue this request.
                        return Err(None);
                    }
                    request_id = Some(RequestId(key));
                    group.count += 1;
                    break;
                }
            }
        }

        let mut retain_vmo = false;
        let vmo = match &operation {
            Ok(Operation::Read { .. } | Operation::Write { .. }) => {
                self.vmos.lock().get(&request.vmoid).cloned().map_or(Err(zx::Status::IO), |vmo| {
                    retain_vmo = true;
                    Ok(Some(vmo))
                })
            }
            Ok(Operation::CloseVmo) => {
                self.vmos.lock().remove(&request.vmoid).map_or(Err(zx::Status::IO), |vmo| {
                    active_requests.vmo_bin.add(vmo.clone());
                    Ok(Some(vmo))
                })
            }
            _ => Ok(None),
        }
        .unwrap_or_else(|e| {
            operation = Err(e);
            None
        });

        let trace_flow_id = NonZero::new(request.trace_flow_id);
        let request_id = request_id.unwrap_or_else(|| {
            let vmo_bin_key =
                if retain_vmo { Some(active_requests.vmo_bin.retain()) } else { None };
            RequestId(active_requests.requests.insert(ActiveRequest {
                session,
                group_or_request,
                trace_flow_id,
                vmo_bin_key,
                status: zx::Status::OK,
                count: 1,
                req_id: if !flags.contains(BlockIoFlag::GROUP_ITEM)
                    || flags.contains(BlockIoFlag::GROUP_LAST)
                {
                    Some(request.reqid)
                } else {
                    None
                },
            }))
        });

        Ok(DecodedRequest {
            request_id,
            trace_flow_id,
            operation: operation.map_err(|status| {
                active_requests.complete_and_take_response(request_id, status).map(|(_, r)| r)
            })?,
            vmo,
        })
    }

    fn take_vmos(&self) -> BTreeMap<u16, Arc<zx::Vmo>> {
        std::mem::take(&mut *self.vmos.lock())
    }

    /// Maps the request and returns the mapped request with an optional remainder.
    fn map_request(
        &self,
        mut request: DecodedRequest,
    ) -> Result<(DecodedRequest, Option<DecodedRequest>), Option<BlockFifoResponse>> {
        let mut active_requests = self.session_manager.active_requests().0.lock();
        let active_request = &mut active_requests.requests[request.request_id.0];
        if active_request.status != zx::Status::OK {
            return Err(active_requests
                .complete_and_take_response(request.request_id, zx::Status::BAD_STATE)
                .map(|(_, r)| r));
        }
        let mapping = self.offset_map.mapping();
        match (mapping, request.operation.blocks()) {
            (Some(mapping), Some(blocks)) if !mapping.are_blocks_within_source_range(blocks) => {
                return Err(active_requests
                    .complete_and_take_response(request.request_id, zx::Status::OUT_OF_RANGE)
                    .map(|(_, r)| r));
            }
            _ => {}
        }
        let remainder = request.operation.map(
            self.offset_map.mapping(),
            self.offset_map.max_transfer_blocks(),
            self.block_size,
        );
        if remainder.is_some() {
            active_request.count += 1;
        }
        static CACHE: AtomicU64 = AtomicU64::new(0);
        if let Some(context) =
            fuchsia_trace::TraceCategoryContext::acquire_cached(c"storage", &CACHE)
        {
            use fuchsia_trace::ArgValue;
            let trace_args = [
                ArgValue::of("request_id", request.request_id.0),
                ArgValue::of("opcode", request.operation.trace_label()),
            ];
            let _scope = fuchsia_trace::duration(
                c"storage",
                c"block_server::start_transaction",
                &trace_args,
            );
            if let Some(trace_flow_id) = active_request.trace_flow_id {
                fuchsia_trace::flow_step(
                    &context,
                    c"block_server::start_transaction",
                    trace_flow_id.get().into(),
                    &[],
                );
            }
        }
        let remainder = remainder.map(|operation| DecodedRequest { operation, ..request.clone() });
        Ok((request, remainder))
    }

    fn drop_active_requests(&self, pred: impl Fn(&SM::Session) -> bool) {
        self.session_manager.active_requests().0.lock().requests.retain(|_, r| !pred(&r.session));
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct RequestId(usize);

#[derive(Clone, Debug)]
struct DecodedRequest {
    request_id: RequestId,
    trace_flow_id: TraceFlowId,
    operation: Operation,
    vmo: Option<Arc<zx::Vmo>>,
}

/// cbindgen:no-export
pub type WriteOptions = block_protocol::WriteOptions;

#[repr(C)]
#[derive(Clone, Debug)]
pub enum Operation {
    // NOTE: On the C++ side, this ends up as a union and, for efficiency reasons, there is code
    // that assumes that some fields for reads and writes (and possibly trim) line-up (e.g. common
    // code can read `device_block_offset` from the read variant and then assume it's valid for the
    // write variant).
    Read {
        device_block_offset: u64,
        block_count: u32,
        _unused: u32,
        vmo_offset: u64,
    },
    Write {
        device_block_offset: u64,
        block_count: u32,
        options: WriteOptions,
        vmo_offset: u64,
    },
    Flush,
    Trim {
        device_block_offset: u64,
        block_count: u32,
    },
    /// This will never be seen by the C interface.
    CloseVmo,
}

impl Operation {
    fn trace_label(&self) -> &'static str {
        match self {
            Operation::Read { .. } => "read",
            Operation::Write { .. } => "write",
            Operation::Flush { .. } => "flush",
            Operation::Trim { .. } => "trim",
            Operation::CloseVmo { .. } => "close_vmo",
        }
    }

    /// Returns (offset, length).
    fn blocks(&self) -> Option<(u64, u32)> {
        match self {
            Operation::Read { device_block_offset, block_count, .. }
            | Operation::Write { device_block_offset, block_count, .. }
            | Operation::Trim { device_block_offset, block_count, .. } => {
                Some((*device_block_offset, *block_count))
            }
            _ => None,
        }
    }

    /// Returns mutable references to (offset, length).
    fn blocks_mut(&mut self) -> Option<(&mut u64, &mut u32)> {
        match self {
            Operation::Read { device_block_offset, block_count, .. }
            | Operation::Write { device_block_offset, block_count, .. }
            | Operation::Trim { device_block_offset, block_count, .. } => {
                Some((device_block_offset, block_count))
            }
            _ => None,
        }
    }

    /// Maps the operation using `mapping` and returns the remainder.  `mapping` *must* overlap the
    /// start of the operation.
    fn map(
        &mut self,
        mapping: Option<&BlockOffsetMapping>,
        max_blocks: Option<NonZero<u32>>,
        block_size: u32,
    ) -> Option<Self> {
        let mut max = match self {
            Operation::Read { .. } | Operation::Write { .. } => max_blocks.map(|m| m.get() as u64),
            _ => None,
        };
        let (offset, length) = self.blocks_mut()?;
        if let Some(mapping) = mapping {
            let delta = *offset - mapping.source_block_offset;
            debug_assert!(*offset - mapping.source_block_offset < mapping.length);
            *offset = mapping.target_block_offset + delta;
            let mapping_max = mapping.target_block_offset + mapping.length - *offset;
            max = match max {
                None => Some(mapping_max),
                Some(m) => Some(std::cmp::min(m, mapping_max)),
            };
        };
        if let Some(max) = max {
            if *length as u64 > max {
                let rem = (*length as u64 - max) as u32;
                *length = max as u32;
                return Some(match self {
                    Operation::Read {
                        device_block_offset,
                        block_count: _,
                        vmo_offset,
                        _unused,
                    } => Operation::Read {
                        device_block_offset: *device_block_offset + max,
                        block_count: rem,
                        vmo_offset: *vmo_offset + max * block_size as u64,
                        _unused: *_unused,
                    },
                    Operation::Write {
                        device_block_offset,
                        block_count: _,
                        vmo_offset,
                        options,
                    } => {
                        // Only send the barrier flag once per write request.
                        let options = *options & !WriteOptions::PRE_BARRIER;
                        Operation::Write {
                            device_block_offset: *device_block_offset + max,
                            block_count: rem,
                            vmo_offset: *vmo_offset + max * block_size as u64,
                            options,
                        }
                    }
                    Operation::Trim { device_block_offset, block_count: _ } => Operation::Trim {
                        device_block_offset: *device_block_offset,
                        block_count: rem,
                    },
                    _ => unreachable!(),
                });
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum GroupOrRequest {
    Group(u16),
    Request(u32),
}

impl GroupOrRequest {
    fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }

    fn group_id(&self) -> Option<u16> {
        match self {
            Self::Group(id) => Some(*id),
            Self::Request(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockServer, DeviceInfo, PartitionInfo, TraceFlowId, FIFO_MAX_REQUESTS};
    use assert_matches::assert_matches;
    use block_protocol::{BlockFifoCommand, BlockFifoRequest, BlockFifoResponse, WriteOptions};
    use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
    use fuchsia_sync::Mutex;
    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use futures::FutureExt as _;
    use std::borrow::Cow;
    use std::future::poll_fn;
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use zx::{AsHandleRef as _, HandleBased as _};
    use {
        fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume,
        fuchsia_async as fasync,
    };

    #[derive(Default)]
    struct MockInterface {
        read_hook: Option<
            Box<
                dyn Fn(u64, u32, &Arc<zx::Vmo>, u64) -> BoxFuture<'static, Result<(), zx::Status>>
                    + Send
                    + Sync,
            >,
        >,
    }

    impl super::async_interface::Interface for MockInterface {
        async fn on_attach_vmo(&self, _vmo: &zx::Vmo) -> Result<(), zx::Status> {
            Ok(())
        }

        async fn get_info(&self) -> Result<Cow<'_, DeviceInfo>, zx::Status> {
            Ok(Cow::Owned(test_device_info()))
        }

        async fn read(
            &self,
            device_block_offset: u64,
            block_count: u32,
            vmo: &Arc<zx::Vmo>,
            vmo_offset: u64,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            if let Some(read_hook) = &self.read_hook {
                read_hook(device_block_offset, block_count, vmo, vmo_offset).await
            } else {
                unimplemented!();
            }
        }

        async fn write(
            &self,
            _device_block_offset: u64,
            _block_count: u32,
            _vmo: &Arc<zx::Vmo>,
            _vmo_offset: u64,
            _opts: WriteOptions,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            unreachable!();
        }

        async fn flush(&self, _trace_flow_id: TraceFlowId) -> Result<(), zx::Status> {
            unreachable!();
        }

        async fn trim(
            &self,
            _device_block_offset: u64,
            _block_count: u32,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            unreachable!();
        }

        async fn get_volume_info(
            &self,
        ) -> Result<(fvolume::VolumeManagerInfo, fvolume::VolumeInfo), zx::Status> {
            // Hang forever for the test_requests_dont_block_sessions test.
            let () = std::future::pending().await;
            unreachable!();
        }
    }

    const BLOCK_SIZE: u32 = 512;

    fn test_device_info() -> DeviceInfo {
        DeviceInfo::Partition(PartitionInfo {
            device_flags: fblock::Flag::READONLY,
            max_transfer_blocks: None,
            block_range: Some(0..100),
            type_guid: [1; 16],
            instance_guid: [2; 16],
            name: "foo".to_string(),
            flags: 0xabcd,
        })
    }

    #[fuchsia::test]
    async fn test_info() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        futures::join!(
            async {
                let block_server = BlockServer::new(BLOCK_SIZE, Arc::new(MockInterface::default()));
                block_server.handle_requests(stream).await.unwrap();
            },
            async {
                let expected_info = test_device_info();
                let partition_info = if let DeviceInfo::Partition(info) = &expected_info {
                    info
                } else {
                    unreachable!()
                };

                let block_info = proxy.get_info().await.unwrap().unwrap();
                assert_eq!(
                    block_info.block_count,
                    partition_info.block_range.as_ref().unwrap().end
                        - partition_info.block_range.as_ref().unwrap().start
                );
                assert_eq!(block_info.flags, fblock::Flag::READONLY);

                // TODO(https://fxbug.dev/348077960): Check max_transfer_size

                let (status, type_guid) = proxy.get_type_guid().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(&type_guid.as_ref().unwrap().value, &partition_info.type_guid);

                let (status, instance_guid) = proxy.get_instance_guid().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(&instance_guid.as_ref().unwrap().value, &partition_info.instance_guid);

                let (status, name) = proxy.get_name().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(name.as_ref(), Some(&partition_info.name));

                let metadata = proxy.get_metadata().await.unwrap().expect("get_flags failed");
                assert_eq!(metadata.name, name);
                assert_eq!(metadata.type_guid.as_ref(), type_guid.as_deref());
                assert_eq!(metadata.instance_guid.as_ref(), instance_guid.as_deref());
                assert_eq!(
                    metadata.start_block_offset,
                    Some(partition_info.block_range.as_ref().unwrap().start)
                );
                assert_eq!(metadata.num_blocks, Some(block_info.block_count));
                assert_eq!(metadata.flags, Some(partition_info.flags));

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_attach_vmo() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
        let koid = vmo.get_koid().unwrap();

        futures::join!(
            async {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |_, _, vmo, _| {
                            assert_eq!(vmo.get_koid().unwrap(), koid);
                            Box::pin(async { Ok(()) })
                        })),
                        ..MockInterface::default()
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                assert_ne!(vmo_id.id, 0);

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                // Keep attaching VMOs until we eventually hit the maximum.
                let mut count = 1;
                loop {
                    match session_proxy
                        .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                        .await
                        .unwrap()
                    {
                        Ok(vmo_id) => assert_ne!(vmo_id.id, 0),
                        Err(e) => {
                            assert_eq!(e, zx::sys::ZX_ERR_NO_RESOURCES);
                            break;
                        }
                    }

                    // Only test every 10 to keep test time down.
                    if count % 10 == 0 {
                        writer
                            .write_entries(&BlockFifoRequest {
                                command: BlockFifoCommand {
                                    opcode: BlockOpcode::Read.into_primitive(),
                                    ..Default::default()
                                },
                                vmoid: vmo_id.id,
                                length: 1,
                                ..Default::default()
                            })
                            .await
                            .unwrap();

                        let mut response = BlockFifoResponse::default();
                        reader.read_entries(&mut response).await.unwrap();
                        assert_eq!(response.status, zx::sys::ZX_OK);
                    }

                    count += 1;
                }

                assert_eq!(count, u16::MAX as u64);

                // Detach the original VMO, and make sure we can then attach another one.
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::CloseVmo.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                let new_vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                // It should reuse the same ID.
                assert_eq!(new_vmo_id.id, vmo_id.id);

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_close() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let mut server = std::pin::pin!(async {
            let block_server = BlockServer::new(BLOCK_SIZE, Arc::new(MockInterface::default()));
            block_server.handle_requests(stream).await.unwrap();
        }
        .fuse());

        let mut client = std::pin::pin!(async {
            let (session_proxy, server) = fidl::endpoints::create_proxy();

            proxy.open_session(server).unwrap();

            // Dropping the proxy should not cause the session to terminate because the session is
            // still live.
            std::mem::drop(proxy);

            session_proxy.close().await.unwrap().unwrap();

            // Keep the session alive.  Calling `close` should cause the server to terminate.
            let _: () = std::future::pending().await;
        }
        .fuse());

        futures::select!(
            _ = server => {}
            _ = client => unreachable!(),
        );
    }

    #[derive(Default)]
    struct IoMockInterface {
        do_checks: bool,
        expected_op: Arc<Mutex<Option<ExpectedOp>>>,
        return_errors: bool,
    }

    #[derive(Debug)]
    enum ExpectedOp {
        Read(u64, u32, u64),
        Write(u64, u32, u64),
        Trim(u64, u32),
        Flush,
    }

    impl super::async_interface::Interface for IoMockInterface {
        async fn on_attach_vmo(&self, _vmo: &zx::Vmo) -> Result<(), zx::Status> {
            Ok(())
        }

        async fn get_info(&self) -> Result<Cow<'_, DeviceInfo>, zx::Status> {
            Ok(Cow::Owned(test_device_info()))
        }

        async fn read(
            &self,
            device_block_offset: u64,
            block_count: u32,
            _vmo: &Arc<zx::Vmo>,
            vmo_offset: u64,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::INTERNAL)
            } else {
                if self.do_checks {
                    assert_matches!(
                        self.expected_op.lock().take(),
                        Some(ExpectedOp::Read(a, b, c)) if device_block_offset == a &&
                            block_count == b && vmo_offset / BLOCK_SIZE as u64 == c,
                        "Read {device_block_offset} {block_count} {vmo_offset}"
                    );
                }
                Ok(())
            }
        }

        async fn write(
            &self,
            device_block_offset: u64,
            block_count: u32,
            _vmo: &Arc<zx::Vmo>,
            vmo_offset: u64,
            _opts: WriteOptions,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NOT_SUPPORTED)
            } else {
                if self.do_checks {
                    assert_matches!(
                        self.expected_op.lock().take(),
                        Some(ExpectedOp::Write(a, b, c)) if device_block_offset == a &&
                            block_count == b && vmo_offset / BLOCK_SIZE as u64 == c,
                        "Write {device_block_offset} {block_count} {vmo_offset}"
                    );
                }
                Ok(())
            }
        }

        async fn flush(&self, _trace_flow_id: TraceFlowId) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NO_RESOURCES)
            } else {
                if self.do_checks {
                    assert_matches!(self.expected_op.lock().take(), Some(ExpectedOp::Flush));
                }
                Ok(())
            }
        }

        async fn trim(
            &self,
            device_block_offset: u64,
            block_count: u32,
            _trace_flow_id: TraceFlowId,
        ) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NO_MEMORY)
            } else {
                if self.do_checks {
                    assert_matches!(
                        self.expected_op.lock().take(),
                        Some(ExpectedOp::Trim(a, b)) if device_block_offset == a &&
                            block_count == b,
                        "Trim {device_block_offset} {block_count}"
                    );
                }
                Ok(())
            }
        }
    }

    #[fuchsia::test]
    async fn test_io() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let expected_op = Arc::new(Mutex::new(None));
        let expected_op_clone = expected_op.clone();

        let server = async {
            let block_server = BlockServer::new(
                BLOCK_SIZE,
                Arc::new(IoMockInterface {
                    return_errors: false,
                    do_checks: true,
                    expected_op: expected_op_clone,
                }),
            );
            block_server.handle_requests(stream).await.unwrap();
        };

        let client = async move {
            let (session_proxy, server) = fidl::endpoints::create_proxy();

            proxy.open_session(server).unwrap();

            let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
            let vmo_id = session_proxy
                .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                .await
                .unwrap()
                .unwrap();

            let mut fifo =
                fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
            let (mut reader, mut writer) = fifo.async_io();

            // READ
            *expected_op.lock() = Some(ExpectedOp::Read(1, 2, 3));
            writer
                .write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    dev_offset: 1,
                    length: 2,
                    vmo_offset: 3,
                    ..Default::default()
                })
                .await
                .unwrap();

            let mut response = BlockFifoResponse::default();
            reader.read_entries(&mut response).await.unwrap();
            assert_eq!(response.status, zx::sys::ZX_OK);

            // WRITE
            *expected_op.lock() = Some(ExpectedOp::Write(4, 5, 6));
            writer
                .write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Write.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    dev_offset: 4,
                    length: 5,
                    vmo_offset: 6,
                    ..Default::default()
                })
                .await
                .unwrap();

            let mut response = BlockFifoResponse::default();
            reader.read_entries(&mut response).await.unwrap();
            assert_eq!(response.status, zx::sys::ZX_OK);

            // FLUSH
            *expected_op.lock() = Some(ExpectedOp::Flush);
            writer
                .write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Flush.into_primitive(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .unwrap();

            reader.read_entries(&mut response).await.unwrap();
            assert_eq!(response.status, zx::sys::ZX_OK);

            // TRIM
            *expected_op.lock() = Some(ExpectedOp::Trim(7, 8));
            writer
                .write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Trim.into_primitive(),
                        ..Default::default()
                    },
                    dev_offset: 7,
                    length: 8,
                    ..Default::default()
                })
                .await
                .unwrap();

            reader.read_entries(&mut response).await.unwrap();
            assert_eq!(response.status, zx::sys::ZX_OK);

            std::mem::drop(proxy);
        };

        futures::join!(server, client);
    }

    #[fuchsia::test]
    async fn test_io_errors() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        futures::join!(
            async {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(IoMockInterface {
                        return_errors: true,
                        do_checks: false,
                        expected_op: Arc::new(Mutex::new(None)),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                // READ
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        length: 1,
                        reqid: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_INTERNAL);

                // WRITE
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Write.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        length: 1,
                        reqid: 2,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NOT_SUPPORTED);

                // FLUSH
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Flush.into_primitive(),
                            ..Default::default()
                        },
                        reqid: 3,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NO_RESOURCES);

                // TRIM
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Trim.into_primitive(),
                            ..Default::default()
                        },
                        reqid: 4,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NO_MEMORY);

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_invalid_args() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        futures::join!(
            async {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(IoMockInterface {
                        return_errors: false,
                        do_checks: false,
                        expected_op: Arc::new(Mutex::new(None)),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                async fn test(
                    fifo: &mut fasync::Fifo<BlockFifoResponse, BlockFifoRequest>,
                    request: BlockFifoRequest,
                ) -> Result<(), zx::Status> {
                    let (mut reader, mut writer) = fifo.async_io();
                    writer.write_entries(&request).await.unwrap();
                    let mut response = BlockFifoResponse::default();
                    reader.read_entries(&mut response).await.unwrap();
                    zx::Status::ok(response.status)
                }

                // READ

                let good_read_request = || BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    length: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                };

                assert_eq!(
                    test(
                        &mut fifo,
                        BlockFifoRequest { vmoid: vmo_id.id + 1, ..good_read_request() }
                    )
                    .await,
                    Err(zx::Status::IO)
                );

                assert_eq!(
                    test(
                        &mut fifo,
                        BlockFifoRequest {
                            vmo_offset: 0xffff_ffff_ffff_ffff,
                            ..good_read_request()
                        }
                    )
                    .await,
                    Err(zx::Status::OUT_OF_RANGE)
                );

                assert_eq!(
                    test(&mut fifo, BlockFifoRequest { length: 0, ..good_read_request() }).await,
                    Err(zx::Status::INVALID_ARGS)
                );

                // WRITE

                let good_write_request = || BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Write.into_primitive(),
                        ..Default::default()
                    },
                    length: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                };

                assert_eq!(
                    test(
                        &mut fifo,
                        BlockFifoRequest { vmoid: vmo_id.id + 1, ..good_write_request() }
                    )
                    .await,
                    Err(zx::Status::IO)
                );

                assert_eq!(
                    test(
                        &mut fifo,
                        BlockFifoRequest {
                            vmo_offset: 0xffff_ffff_ffff_ffff,
                            ..good_write_request()
                        }
                    )
                    .await,
                    Err(zx::Status::OUT_OF_RANGE)
                );

                assert_eq!(
                    test(&mut fifo, BlockFifoRequest { length: 0, ..good_write_request() }).await,
                    Err(zx::Status::INVALID_ARGS)
                );

                // CLOSE VMO

                assert_eq!(
                    test(
                        &mut fifo,
                        BlockFifoRequest {
                            command: BlockFifoCommand {
                                opcode: BlockOpcode::CloseVmo.into_primitive(),
                                ..Default::default()
                            },
                            vmoid: vmo_id.id + 1,
                            ..Default::default()
                        }
                    )
                    .await,
                    Err(zx::Status::IO)
                );

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_concurrent_requests() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let waiting_readers = Arc::new(Mutex::new(Vec::new()));
        let waiting_readers_clone = waiting_readers.clone();

        futures::join!(
            async move {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |dev_block_offset, _, _, _| {
                            let (tx, rx) = oneshot::channel();
                            waiting_readers_clone.lock().push((dev_block_offset as u32, tx));
                            Box::pin(async move {
                                let _ = rx.await;
                                Ok(())
                            })
                        })),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            ..Default::default()
                        },
                        reqid: 1,
                        dev_offset: 1, // Intentionally use the same as `reqid`.
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            ..Default::default()
                        },
                        reqid: 2,
                        dev_offset: 2,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                // Wait till both those entries are pending.
                poll_fn(|cx: &mut Context<'_>| {
                    if waiting_readers.lock().len() == 2 {
                        Poll::Ready(())
                    } else {
                        // Yield to the executor.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;

                let mut response = BlockFifoResponse::default();
                assert!(futures::poll!(pin!(reader.read_entries(&mut response))).is_pending());

                let (id, tx) = waiting_readers.lock().pop().unwrap();
                tx.send(()).unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, id);

                assert!(futures::poll!(pin!(reader.read_entries(&mut response))).is_pending());

                let (id, tx) = waiting_readers.lock().pop().unwrap();
                tx.send(()).unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, id);
            }
        );
    }

    #[fuchsia::test]
    async fn test_groups() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        futures::join!(
            async move {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |_, _, _, _| Box::pin(async { Ok(()) }))),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: BlockIoFlag::GROUP_ITEM.bits(),
                            ..Default::default()
                        },
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                            ..Default::default()
                        },
                        reqid: 2,
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, 2);
                assert_eq!(response.group, 1);
            }
        );
    }

    #[fuchsia::test]
    async fn test_group_error() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        futures::join!(
            async move {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |_, _, _, _| {
                            counter_clone.fetch_add(1, Ordering::Relaxed);
                            Box::pin(async { Err(zx::Status::BAD_STATE) })
                        })),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: BlockIoFlag::GROUP_ITEM.bits(),
                            ..Default::default()
                        },
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                // Wait until processed.
                poll_fn(|cx: &mut Context<'_>| {
                    if counter.load(Ordering::Relaxed) == 1 {
                        Poll::Ready(())
                    } else {
                        // Yield to the executor.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;

                let mut response = BlockFifoResponse::default();
                assert!(futures::poll!(pin!(reader.read_entries(&mut response))).is_pending());

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: BlockIoFlag::GROUP_ITEM.bits(),
                            ..Default::default()
                        },
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                            ..Default::default()
                        },
                        reqid: 2,
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_BAD_STATE);
                assert_eq!(response.reqid, 2);
                assert_eq!(response.group, 1);

                assert!(futures::poll!(pin!(reader.read_entries(&mut response))).is_pending());

                // Only the first request should have been processed.
                assert_eq!(counter.load(Ordering::Relaxed), 1);
            }
        );
    }

    #[fuchsia::test]
    async fn test_group_with_two_lasts() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let (tx, rx) = oneshot::channel();

        futures::join!(
            async move {
                let rx = Mutex::new(Some(rx));
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |_, _, _, _| {
                            let rx = rx.lock().take().unwrap();
                            Box::pin(async {
                                let _ = rx.await;
                                Ok(())
                            })
                        })),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                            ..Default::default()
                        },
                        reqid: 1,
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                            ..Default::default()
                        },
                        reqid: 2,
                        group: 1,
                        vmoid: vmo_id.id,
                        length: 1,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                // Send an independent request to flush through the fifo.
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::CloseVmo.into_primitive(),
                            ..Default::default()
                        },
                        reqid: 3,
                        vmoid: vmo_id.id,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                // It should succeed.
                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, 3);

                // Now release the original request.
                tx.send(()).unwrap();

                // The response should be for the first message tagged as last, and it should be
                // an error because we sent two messages with the LAST marker.
                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_INVALID_ARGS);
                assert_eq!(response.reqid, 1);
                assert_eq!(response.group, 1);
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_requests_dont_block_sessions() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let (tx, rx) = oneshot::channel();

        fasync::Task::local(async move {
            let rx = Mutex::new(Some(rx));
            let block_server = BlockServer::new(
                BLOCK_SIZE,
                Arc::new(MockInterface {
                    read_hook: Some(Box::new(move |_, _, _, _| {
                        let rx = rx.lock().take().unwrap();
                        Box::pin(async {
                            let _ = rx.await;
                            Ok(())
                        })
                    })),
                }),
            );
            block_server.handle_requests(stream).await.unwrap();
        })
        .detach();

        let mut fut = pin!(async {
            let (session_proxy, server) = fidl::endpoints::create_proxy();

            proxy.open_session(server).unwrap();

            let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
            let vmo_id = session_proxy
                .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                .await
                .unwrap()
                .unwrap();

            let mut fifo =
                fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
            let (mut reader, mut writer) = fifo.async_io();

            writer
                .write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                        ..Default::default()
                    },
                    reqid: 1,
                    group: 1,
                    vmoid: vmo_id.id,
                    length: 1,
                    ..Default::default()
                })
                .await
                .unwrap();

            let mut response = BlockFifoResponse::default();
            reader.read_entries(&mut response).await.unwrap();
            assert_eq!(response.status, zx::sys::ZX_OK);
        });

        // The response won't come back until we send on `tx`.
        assert!(fasync::TestExecutor::poll_until_stalled(&mut fut).await.is_pending());

        let mut fut2 = pin!(proxy.get_volume_info());

        // get_volume_info is set up to stall forever.
        assert!(fasync::TestExecutor::poll_until_stalled(&mut fut2).await.is_pending());

        // If we now free up the first future, it should resolve; the stalled call to
        // get_volume_info should not block the fifo response.
        let _ = tx.send(());

        assert!(fasync::TestExecutor::poll_until_stalled(&mut fut).await.is_ready());
    }

    #[fuchsia::test]
    async fn test_request_flow_control() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        // The client will ensure that MAX_REQUESTS are queued up before firing `event`, and the
        // server will block until that happens.
        const MAX_REQUESTS: u64 = FIFO_MAX_REQUESTS as u64;
        let event = Arc::new((event_listener::Event::new(), AtomicBool::new(false)));
        let event_clone = event.clone();
        futures::join!(
            async move {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(MockInterface {
                        read_hook: Some(Box::new(move |_, _, _, _| {
                            let event_clone = event_clone.clone();
                            Box::pin(async move {
                                if !event_clone.1.load(Ordering::SeqCst) {
                                    event_clone.0.listen().await;
                                }
                                Ok(())
                            })
                        })),
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                for i in 0..MAX_REQUESTS {
                    writer
                        .write_entries(&BlockFifoRequest {
                            command: BlockFifoCommand {
                                opcode: BlockOpcode::Read.into_primitive(),
                                ..Default::default()
                            },
                            reqid: (i + 1) as u32,
                            dev_offset: i,
                            vmoid: vmo_id.id,
                            length: 1,
                            ..Default::default()
                        })
                        .await
                        .unwrap();
                }
                assert!(futures::poll!(pin!(writer.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    reqid: u32::MAX,
                    dev_offset: MAX_REQUESTS,
                    vmoid: vmo_id.id,
                    length: 1,
                    ..Default::default()
                })))
                .is_pending());
                // OK, let the server start to process.
                event.1.store(true, Ordering::SeqCst);
                event.0.notify(usize::MAX);
                // For each entry we read, make sure we can write a new one in.
                let mut finished_reqids = vec![];
                for i in MAX_REQUESTS..2 * MAX_REQUESTS {
                    let mut response = BlockFifoResponse::default();
                    reader.read_entries(&mut response).await.unwrap();
                    assert_eq!(response.status, zx::sys::ZX_OK);
                    finished_reqids.push(response.reqid);
                    writer
                        .write_entries(&BlockFifoRequest {
                            command: BlockFifoCommand {
                                opcode: BlockOpcode::Read.into_primitive(),
                                ..Default::default()
                            },
                            reqid: (i + 1) as u32,
                            dev_offset: i,
                            vmoid: vmo_id.id,
                            length: 1,
                            ..Default::default()
                        })
                        .await
                        .unwrap();
                }
                let mut response = BlockFifoResponse::default();
                for _ in 0..MAX_REQUESTS {
                    reader.read_entries(&mut response).await.unwrap();
                    assert_eq!(response.status, zx::sys::ZX_OK);
                    finished_reqids.push(response.reqid);
                }
                // Verify that we got a response for each request.  Note that we can't assume FIFO
                // ordering.
                finished_reqids.sort();
                assert_eq!(finished_reqids.len(), 2 * MAX_REQUESTS as usize);
                let mut i = 1;
                for reqid in finished_reqids {
                    assert_eq!(reqid, i);
                    i += 1;
                }
            }
        );
    }

    #[fuchsia::test]
    async fn test_passthrough_io_with_fixed_map() {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

        let expected_op = Arc::new(Mutex::new(None));
        let expected_op_clone = expected_op.clone();
        futures::join!(
            async {
                let block_server = BlockServer::new(
                    BLOCK_SIZE,
                    Arc::new(IoMockInterface {
                        return_errors: false,
                        do_checks: true,
                        expected_op: expected_op_clone,
                    }),
                );
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy();

                let mappings = [fblock::BlockOffsetMapping {
                    source_block_offset: 0,
                    target_block_offset: 10,
                    length: 20,
                }];
                proxy.open_session_with_offset_map(server, None, Some(&mappings[..])).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let mut fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
                let (mut reader, mut writer) = fifo.async_io();

                // READ
                *expected_op.lock() = Some(ExpectedOp::Read(11, 2, 3));
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        dev_offset: 1,
                        length: 2,
                        vmo_offset: 3,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                let mut response = BlockFifoResponse::default();
                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // WRITE
                *expected_op.lock() = Some(ExpectedOp::Write(14, 5, 6));
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Write.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        dev_offset: 4,
                        length: 5,
                        vmo_offset: 6,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // FLUSH
                *expected_op.lock() = Some(ExpectedOp::Flush);
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Flush.into_primitive(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // TRIM
                *expected_op.lock() = Some(ExpectedOp::Trim(17, 3));
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Trim.into_primitive(),
                            ..Default::default()
                        },
                        dev_offset: 7,
                        length: 3,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // READ past window
                *expected_op.lock() = None;
                writer
                    .write_entries(&BlockFifoRequest {
                        command: BlockFifoCommand {
                            opcode: BlockOpcode::Read.into_primitive(),
                            ..Default::default()
                        },
                        vmoid: vmo_id.id,
                        dev_offset: 19,
                        length: 2,
                        vmo_offset: 3,
                        ..Default::default()
                    })
                    .await
                    .unwrap();

                reader.read_entries(&mut response).await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_OUT_OF_RANGE);

                std::mem::drop(proxy);
            }
        );
    }
}
