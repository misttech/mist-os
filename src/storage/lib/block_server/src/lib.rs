// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Error};
use block_protocol::{BlockFifoRequest, BlockFifoResponse};
use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
use fuchsia_async::{self as fasync, FifoReadable, FifoWritable};
use futures::stream::FuturesUnordered;
use futures::{Future, FutureExt as _, StreamExt as _};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::sync::Arc;
use zx::HandleBased;
use {
    fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_partition as fpartition,
    fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_zircon as zx,
};

#[derive(Clone)]
pub struct PartitionInfo {
    pub block_count: u64,
    pub block_size: u32,
    pub type_guid: [u8; 16],
    pub instance_guid: [u8; 16],
    pub name: String,
}

pub trait Interface: Send + Sync + 'static {
    /// Returns information about the partition.
    fn info(&self) -> &PartitionInfo;

    /// Called for a request to read bytes.
    fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called for a request to write bytes.
    fn write(
        &self,
        device_block_offset: u64,
        length: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to flush the device.
    fn flush(&self) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to trim a region.
    fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
}

// Multiple Block I/O request may be sent as a group.
// Notes:
// - the group is identified by `group_id` in the request
// - if using groups, a response will not be sent unless `BlockIoFlag::GROUP_LAST`
//   flag is set.
// - when processing a request of a group fails, subsequent requests of that
//   group will not be processed.
//
// Refer to sdk/fidl/fuchsia.hardware.block.driver/block.fidl for details.
//
// FifoMessageGroup keeps track of the relevant BlockFifoResponse field for
// a group requests. Only `status` and `count` needs to be updated.
struct FifoMessageGroup {
    status: zx::Status,
    count: u32,
    req_id: Option<u32>,
}

impl FifoMessageGroup {
    // Initialise a FifoMessageGroup given the request group ID.
    // `count` is set to 0 as no requests has been processed yet.
    fn new() -> Self {
        Self { status: zx::Status::OK, count: 0, req_id: None }
    }
}

#[derive(Default)]
struct FifoMessageGroups(BTreeMap<u16, FifoMessageGroup>);

// Keeps track of all the group requests that are currently being processed
impl FifoMessageGroups {
    // Returns the current MessageGroup with this group ID
    fn get(&mut self, group_id: u16) -> &mut FifoMessageGroup {
        self.0.entry(group_id).or_insert_with(|| FifoMessageGroup::new())
    }

    /// Completes a request and returns a response to be sent if it's the last outstanding request
    /// for this group.
    fn complete(&mut self, group_id: u16, status: zx::Status) -> Option<BlockFifoResponse> {
        let Entry::Occupied(mut o) = self.0.entry(group_id) else { unreachable!() };
        let group = o.get_mut();
        if group.count == 1 {
            if let Some(reqid) = group.req_id {
                let status =
                    if group.status != zx::Status::OK { group.status } else { status }.into_raw();

                o.remove();

                return Some(BlockFifoResponse {
                    status,
                    reqid,
                    group: group_id,
                    ..Default::default()
                });
            }
        }

        group.count = group.count.checked_sub(1).unwrap();
        if status != zx::Status::OK && group.status == zx::Status::OK {
            group.status = status
        }
        None
    }
}

/// BlockServer is an implementation of fuchsia.hardware.block.partition.Partition.
pub struct BlockServer<I> {
    block_size: u32,
    interface: I,
}

impl<I: Interface> BlockServer<I> {
    pub fn new(interface: I) -> Arc<Self> {
        let block_size = interface.info().block_size;
        Arc::new(Self { block_size, interface })
    }

    /// Called to process requests for fuchsia.hardware.block.partition/Partition.
    pub async fn handle_requests(
        self: &Arc<Self>,
        requests: fvolume::VolumeRequestStream,
    ) -> Result<(), Error> {
        let mut requests = requests.fuse();
        let mut sessions = FuturesUnordered::new();
        loop {
            futures::select! {
                request = requests.next() => {
                    if let Some(request) = request {
                        if let Some(new_session) = self.handle_request(request?).await? {
                            sessions.push(new_session);
                        }
                    }
                }
                _ = sessions.select_next_some() => {}
                complete => return Ok(()),
            }
        }
    }

    /// Processes a partition request.
    async fn handle_request(
        self: &Arc<Self>,
        request: fvolume::VolumeRequest,
    ) -> Result<Option<impl Future<Output = ()>>, Error> {
        match request {
            fvolume::VolumeRequest::GetInfo { responder } => {
                responder.send(Ok(&fblock::BlockInfo {
                    block_count: self.interface.info().block_count,
                    block_size: self.interface.info().block_size,
                    max_transfer_size: fblock::MAX_TRANSFER_UNBOUNDED,
                    flags: fblock::Flag::empty(),
                }))?;
            }
            fvolume::VolumeRequest::GetStats { clear: _, responder } => {
                // TODO(https://fxbug.dev/348077960): Implement this
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fvolume::VolumeRequest::OpenSession { session, control_handle: _ } => {
                let stream = session.into_stream()?;
                let block_server = self.clone();
                let mut session = Session::new(block_server)?;
                return Ok(Some(async move {
                    let _ = session.run(stream).await;
                }));
            }
            fvolume::VolumeRequest::ReadBlocks {
                responder,
                vmo: _,
                length: _,
                dev_offset: _,
                vmo_offset: _,
            } => {
                // TODO(https://fxbug.dev/348077960): Implement or remove this
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fvolume::VolumeRequest::WriteBlocks {
                responder,
                vmo: _,
                length: _,
                dev_offset: _,
                vmo_offset: _,
            } => {
                // TODO(https://fxbug.dev/348077960): Implement or remove this
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fvolume::VolumeRequest::GetTypeGuid { responder } => {
                let mut guid = fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                guid.value.copy_from_slice(&self.interface.info().type_guid);
                responder.send(zx::sys::ZX_OK, Some(&guid))?;
            }
            fvolume::VolumeRequest::GetInstanceGuid { responder } => {
                let mut guid = fpartition::Guid { value: [0u8; fpartition::GUID_LENGTH as usize] };
                guid.value.copy_from_slice(&self.interface.info().instance_guid);
                responder.send(zx::sys::ZX_OK, Some(&guid))?;
            }
            fvolume::VolumeRequest::GetName { responder } => {
                responder.send(zx::sys::ZX_OK, Some(&self.interface.info().name))?;
            }
            fvolume::VolumeRequest::QuerySlices { responder, .. } => {
                responder.send(
                    zx::sys::ZX_ERR_NOT_SUPPORTED,
                    &[fvolume::VsliceRange { allocated: false, count: 0 }; 16],
                    0,
                )?;
            }
            fvolume::VolumeRequest::GetVolumeInfo { responder, .. } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None, None)?;
            }
            fvolume::VolumeRequest::Extend { responder, .. } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED)?;
            }
            fvolume::VolumeRequest::Shrink { responder, .. } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED)?;
            }
            fvolume::VolumeRequest::Destroy { responder, .. } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED)?;
            }
        }
        Ok(None)
    }

    /// Proesses a fifo request.
    async fn process_fifo_request(
        &self,
        op_code: Result<BlockOpcode, zx::Status>,
        request: &BlockFifoRequest,
        vmo: Option<Arc<zx::Vmo>>,
    ) -> Result<(), zx::Status> {
        match op_code? {
            BlockOpcode::Read => {
                self.interface
                    .read(
                        request.dev_offset,
                        request.length,
                        vmo.as_ref().ok_or(zx::Status::INVALID_ARGS)?,
                        request
                            .vmo_offset
                            .checked_mul(self.block_size as u64)
                            .ok_or(zx::Status::INVALID_ARGS)?,
                    )
                    .await
            }
            BlockOpcode::Write => {
                self.interface
                    .write(
                        request.dev_offset,
                        request.length,
                        vmo.as_ref().ok_or(zx::Status::INVALID_ARGS)?,
                        request
                            .vmo_offset
                            .checked_mul(self.block_size as u64)
                            .ok_or(zx::Status::INVALID_ARGS)?,
                    )
                    .await
            }
            BlockOpcode::Flush => self.interface.flush().await,
            BlockOpcode::Trim => self.interface.trim(request.dev_offset, request.length).await,
            BlockOpcode::CloseVmo => {
                // The caller removed the vmo if present so we just need to check we have a vmo.
                // This error code is the one that the C++ server used.
                vmo.ok_or(zx::Status::IO)?;
                Ok(())
            }
        }
    }
}

struct Session<I> {
    block_server: Arc<BlockServer<I>>,
    fifo: fasync::Fifo<BlockFifoRequest, BlockFifoResponse>,
    peer_fifo: zx::Fifo,
    message_groups: FifoMessageGroups,
    vmos: BTreeMap<u16, Arc<zx::Vmo>>,
}

impl<I: Interface> Session<I> {
    fn new(block_server: Arc<BlockServer<I>>) -> Result<Self, zx::Status> {
        let (peer_fifo, fifo) = zx::Fifo::create(16, std::mem::size_of::<BlockFifoRequest>())?;
        Ok(Self {
            block_server,
            fifo: fasync::Fifo::from_fifo(fifo),
            peer_fifo,
            message_groups: FifoMessageGroups::default(),
            vmos: BTreeMap::new(),
        })
    }

    async fn run(&mut self, stream: fblock::SessionRequestStream) -> Result<(), Error> {
        let mut stream = stream.fuse();
        let mut requests = FuturesUnordered::new();
        loop {
            futures::select! {
                req = stream.next() => {
                    let Some(req) = req else { return Ok(()) };
                    self.handle_request(req?)?;
                }
                req = self.fifo.read_entry().fuse() => {
                    if let Some(request) = self.handle_fifo_request(req?) {
                        requests.push(request);
                    }
                }
                result = requests.select_next_some() => {
                    let (group_or_request, status) = result;
                    match group_or_request {
                        GroupOrRequest::Group(group_id) => {
                            if let Some(response) = self.message_groups.complete(group_id, status) {
                                self.fifo.write_entries(&response).await?;
                            }
                        }
                        GroupOrRequest::Request(reqid) => {
                            self.fifo.write_entries(&BlockFifoResponse {
                                status: status.into_raw(),
                                reqid,
                                ..Default::default()
                            }).await?;
                        }
                    }
                }
            }
        }
    }

    fn handle_request(&mut self, request: fblock::SessionRequest) -> Result<(), Error> {
        match request {
            fblock::SessionRequest::GetFifo { responder } => {
                let rights = zx::Rights::TRANSFER
                    | zx::Rights::READ
                    | zx::Rights::WRITE
                    | zx::Rights::SIGNAL
                    | zx::Rights::WAIT;
                match self.peer_fifo.duplicate_handle(rights) {
                    Ok(fifo) => responder.send(Ok(fifo))?,
                    Err(s) => responder.send(Err(s.into_raw()))?,
                }
                Ok(())
            }
            fblock::SessionRequest::AttachVmo { vmo, responder } => {
                if self.vmos.len() == u16::MAX as usize {
                    responder.send(Err(zx::Status::NO_RESOURCES.into_raw()))?;
                } else {
                    let vmo_id = match self.vmos.last_entry() {
                        None => 1,
                        Some(o) => {
                            o.key().checked_add(1).unwrap_or_else(|| {
                                let mut vmo_id = 1;
                                // Find the first gap...
                                for (&id, _) in &self.vmos {
                                    if id > vmo_id {
                                        break;
                                    }
                                    vmo_id = id + 1;
                                }
                                vmo_id
                            })
                        }
                    };
                    self.vmos.insert(vmo_id, Arc::new(vmo));
                    responder.send(Ok(&fblock::VmoId { id: vmo_id }))?;
                }
                Ok(())
            }
            fblock::SessionRequest::Close { responder } => {
                responder.send(Ok(()))?;
                Err(anyhow!("Closed"))
            }
        }
    }

    fn handle_fifo_request(
        &mut self,
        request: BlockFifoRequest,
    ) -> Option<impl Future<Output = (GroupOrRequest, zx::Status)>> {
        let flags = BlockIoFlag::from_bits_truncate(request.command.flags);
        let is_group = flags.contains(BlockIoFlag::GROUP_ITEM);
        let last_in_group = flags.contains(BlockIoFlag::GROUP_LAST);
        let mut op_code =
            BlockOpcode::from_primitive(request.command.opcode).ok_or(zx::Status::INVALID_ARGS);

        let group_or_request = if is_group {
            let group = self.message_groups.get(request.group);
            if group.req_id.is_some() {
                // We have already received a request tagged as last.
                if group.status == zx::Status::OK {
                    group.status = zx::Status::INVALID_ARGS;
                }
                return None;
            }
            if last_in_group {
                group.req_id = Some(request.reqid);
                // If the group has had an error, there is no point trying to issue this request.
                if group.status != zx::Status::OK {
                    op_code = Err(group.status);
                }
            } else if group.status != zx::Status::OK {
                // The group has already encountered an error, so there is no point trying to issue
                // this request.
                return None;
            }
            group.count += 1;
            GroupOrRequest::Group(request.group)
        } else {
            GroupOrRequest::Request(request.reqid)
        };

        let vmo = match op_code {
            Ok(BlockOpcode::Read) | Ok(BlockOpcode::Write) => {
                self.vmos.get(&request.vmoid).cloned()
            }
            Ok(BlockOpcode::CloseVmo) => self.vmos.remove(&request.vmoid),
            _ => None,
        };

        let block_server = self.block_server.clone();
        Some(async move {
            (
                group_or_request,
                block_server.process_fifo_request(op_code, &request, vmo).await.into(),
            )
        })
    }
}

enum GroupOrRequest {
    Group(u16),
    Request(u32),
}

#[cfg(test)]
mod tests {
    use super::{BlockServer, PartitionInfo};
    use block_protocol::{BlockFifoCommand, BlockFifoRequest, BlockFifoResponse};
    use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
    use fuchsia_async::{FifoReadable as _, FifoWritable as _};
    use fuchsia_zircon::{AsHandleRef as _, HandleBased as _};
    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use futures::FutureExt as _;
    use std::future::poll_fn;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use {
        fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_async as fasync,
        fuchsia_zircon as zx,
    };

    struct MockInterface {
        partition_info: PartitionInfo,
        read_hook: Option<
            Box<
                dyn Fn(u64, u32, &Arc<zx::Vmo>, u64) -> BoxFuture<'static, Result<(), zx::Status>>
                    + Send
                    + Sync,
            >,
        >,
    }

    impl MockInterface {
        fn with_info(partition_info: PartitionInfo) -> Self {
            Self { partition_info, read_hook: None }
        }
    }

    impl super::Interface for MockInterface {
        fn info(&self) -> &PartitionInfo {
            &self.partition_info
        }

        async fn read(
            &self,
            device_block_offset: u64,
            block_count: u32,
            vmo: &Arc<zx::Vmo>,
            vmo_offset: u64,
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
        ) -> Result<(), zx::Status> {
            unreachable!();
        }

        async fn flush(&self) -> Result<(), zx::Status> {
            unreachable!();
        }

        async fn trim(
            &self,
            _device_block_offset: u64,
            _block_count: u32,
        ) -> Result<(), zx::Status> {
            unreachable!();
        }
    }

    const BLOCK_SIZE: u32 = 512;

    fn test_partition_info() -> PartitionInfo {
        PartitionInfo {
            block_count: 1234,
            block_size: BLOCK_SIZE,
            type_guid: [1; 16],
            instance_guid: [2; 16],
            name: "foo".to_string(),
        }
    }

    #[fuchsia::test]
    async fn test_info() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        futures::join!(
            async {
                let block_server =
                    BlockServer::new(MockInterface::with_info(test_partition_info()));
                block_server.handle_requests(stream).await.unwrap();
            },
            async {
                let partition_info = test_partition_info();

                let block_info = proxy.get_info().await.unwrap().unwrap();
                assert_eq!(block_info.block_count, partition_info.block_count);
                assert_eq!(block_info.block_size, partition_info.block_size);

                // TODO(https://fxbug.dev/348077960): Check max_transfer_size and flags

                let (status, type_guid) = proxy.get_type_guid().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(&type_guid.unwrap().value, &partition_info.type_guid);

                let (status, instance_guid) = proxy.get_instance_guid().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(&instance_guid.unwrap().value, &partition_info.instance_guid);

                let (status, name) = proxy.get_name().await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(&name.unwrap(), &partition_info.name);

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_attach_vmo() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
        let koid = vmo.get_koid().unwrap();

        futures::join!(
            async {
                let block_server = BlockServer::new(MockInterface {
                    read_hook: Some(Box::new(move |_, _, vmo, _| {
                        assert_eq!(vmo.get_koid().unwrap(), koid);
                        Box::pin(async { Ok(()) })
                    })),
                    ..MockInterface::with_info(test_partition_info())
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                assert_ne!(vmo_id.id, 0);

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

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
                        fifo.write_entries(&BlockFifoRequest {
                            command: BlockFifoCommand {
                                opcode: BlockOpcode::Read.into_primitive(),
                                ..Default::default()
                            },
                            vmoid: vmo_id.id,
                            ..Default::default()
                        })
                        .await
                        .unwrap();

                        let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                        assert_eq!(response.status, zx::sys::ZX_OK);
                    }

                    count += 1;
                }

                assert_eq!(count, u16::MAX as u64);

                // Detach the original VMO, and make sure we can then attach another one.
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::CloseVmo.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
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
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        let mut server = std::pin::pin!(async {
            let block_server = BlockServer::new(MockInterface::with_info(PartitionInfo {
                block_count: 1234,
                block_size: 512,
                type_guid: [1; 16],
                instance_guid: [2; 16],
                name: "foo".to_string(),
            }));
            block_server.handle_requests(stream).await.unwrap();
        }
        .fuse());

        let mut client = std::pin::pin!(async {
            let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

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

    struct IoMockInterface {
        partition_info: PartitionInfo,
        counter: AtomicU64,
        do_checks: bool,
        return_errors: bool,
    }

    impl super::Interface for IoMockInterface {
        fn info(&self) -> &PartitionInfo {
            &self.partition_info
        }

        async fn read(
            &self,
            device_block_offset: u64,
            block_count: u32,
            _vmo: &Arc<zx::Vmo>,
            vmo_offset: u64,
        ) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::INTERNAL)
            } else {
                if self.do_checks {
                    assert_eq!(self.counter.fetch_add(1, Ordering::Relaxed), 0);
                    assert_eq!(device_block_offset, 1);
                    assert_eq!(block_count, 2);
                    assert_eq!(vmo_offset, 3 * BLOCK_SIZE as u64);
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
        ) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NOT_SUPPORTED)
            } else {
                if self.do_checks {
                    assert_eq!(self.counter.fetch_add(1, Ordering::Relaxed), 1);
                    assert_eq!(device_block_offset, 4);
                    assert_eq!(block_count, 5);
                    assert_eq!(vmo_offset, 6 * BLOCK_SIZE as u64);
                }
                Ok(())
            }
        }

        async fn flush(&self) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NO_RESOURCES)
            } else {
                if self.do_checks {
                    assert_eq!(self.counter.fetch_add(1, Ordering::Relaxed), 2);
                }
                Ok(())
            }
        }

        async fn trim(&self, device_block_offset: u64, block_count: u32) -> Result<(), zx::Status> {
            if self.return_errors {
                Err(zx::Status::NO_MEMORY)
            } else {
                if self.do_checks {
                    assert_eq!(self.counter.fetch_add(1, Ordering::Relaxed), 3);
                    assert_eq!(device_block_offset, 7);
                    assert_eq!(block_count, 8);
                }
                Ok(())
            }
        }
    }

    #[fuchsia::test]
    async fn test_io() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        futures::join!(
            async {
                let block_server = BlockServer::new(IoMockInterface {
                    partition_info: test_partition_info(),
                    counter: AtomicU64::new(0),
                    return_errors: false,
                    do_checks: true,
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                // READ
                fifo.write_entries(&BlockFifoRequest {
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

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // WRITE
                fifo.write_entries(&BlockFifoRequest {
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

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // FLUSH
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Flush.into_primitive(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                // TRIM
                fifo.write_entries(&BlockFifoRequest {
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

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_io_errors() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        futures::join!(
            async {
                let block_server = BlockServer::new(IoMockInterface {
                    partition_info: test_partition_info(),
                    counter: AtomicU64::new(0),
                    return_errors: true,
                    do_checks: false,
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                // READ
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_INTERNAL);

                // WRITE
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Write.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NOT_SUPPORTED);

                // FLUSH
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Flush.into_primitive(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NO_RESOURCES);

                // TRIM
                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Trim.into_primitive(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_NO_MEMORY);

                std::mem::drop(proxy);
            }
        );
    }

    #[fuchsia::test]
    async fn test_invalid_args() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        futures::join!(
            async {
                let block_server = BlockServer::new(IoMockInterface {
                    partition_info: test_partition_info(),
                    counter: AtomicU64::new(0),
                    return_errors: false,
                    do_checks: false,
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                async fn test(
                    fifo: &fasync::Fifo<BlockFifoResponse, BlockFifoRequest>,
                    request: BlockFifoRequest,
                ) -> Result<(), zx::Status> {
                    fifo.write_entries(&request).await.unwrap();
                    let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                    zx::Status::ok(response.status)
                }

                // READ

                let good_read_request = || BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    ..Default::default()
                };

                assert_eq!(
                    test(&fifo, BlockFifoRequest { vmoid: vmo_id.id + 1, ..good_read_request() })
                        .await,
                    Err(zx::Status::INVALID_ARGS)
                );

                assert_eq!(
                    test(
                        &fifo,
                        BlockFifoRequest {
                            vmo_offset: 0xffff_ffff_ffff_ffff,
                            ..good_read_request()
                        }
                    )
                    .await,
                    Err(zx::Status::INVALID_ARGS)
                );

                // WRITE

                let good_write_request = || BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Write.into_primitive(),
                        ..Default::default()
                    },
                    vmoid: vmo_id.id,
                    ..Default::default()
                };

                assert_eq!(
                    test(&fifo, BlockFifoRequest { vmoid: vmo_id.id + 1, ..good_write_request() })
                        .await,
                    Err(zx::Status::INVALID_ARGS)
                );

                assert_eq!(
                    test(
                        &fifo,
                        BlockFifoRequest {
                            vmo_offset: 0xffff_ffff_ffff_ffff,
                            ..good_write_request()
                        }
                    )
                    .await,
                    Err(zx::Status::INVALID_ARGS)
                );

                // CLOSE VMO

                assert_eq!(
                    test(
                        &fifo,
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
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        let waiting_readers = Arc::new(Mutex::new(Vec::new()));
        let waiting_readers_clone = waiting_readers.clone();

        futures::join!(
            async move {
                let block_server = BlockServer::new(MockInterface {
                    partition_info: test_partition_info(),
                    read_hook: Some(Box::new(move |dev_block_offset, _, _, _| {
                        let (tx, rx) = oneshot::channel();
                        waiting_readers_clone.lock().unwrap().push((dev_block_offset as u32, tx));
                        Box::pin(async move {
                            let _ = rx.await;
                            Ok(())
                        })
                    })),
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    reqid: 1,
                    dev_offset: 1, // Intentionally use the same as `reqid`.
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        ..Default::default()
                    },
                    reqid: 2,
                    dev_offset: 2,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                // Wait till both those entries are pending.
                poll_fn(|cx: &mut Context<'_>| {
                    if waiting_readers.lock().unwrap().len() == 2 {
                        Poll::Ready(())
                    } else {
                        // Yield to the executor.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;

                assert!(futures::poll!(fifo.read_entry()).is_pending());

                let (id, tx) = waiting_readers.lock().unwrap().pop().unwrap();
                tx.send(()).unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, id);

                assert!(futures::poll!(fifo.read_entry()).is_pending());

                let (id, tx) = waiting_readers.lock().unwrap().pop().unwrap();
                tx.send(()).unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, id);
            }
        );
    }

    #[fuchsia::test]
    async fn test_groups() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        futures::join!(
            async move {
                let block_server = BlockServer::new(MockInterface {
                    partition_info: test_partition_info(),
                    read_hook: Some(Box::new(move |_, _, _, _| Box::pin(async { Ok(()) }))),
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: BlockIoFlag::GROUP_ITEM.bits(),
                        ..Default::default()
                    },
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                        ..Default::default()
                    },
                    reqid: 2,
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, 2);
                assert_eq!(response.group, 1);
            }
        );
    }

    #[fuchsia::test]
    async fn test_group_error() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        futures::join!(
            async move {
                let block_server = BlockServer::new(MockInterface {
                    partition_info: test_partition_info(),
                    read_hook: Some(Box::new(move |_, _, _, _| {
                        counter_clone.fetch_add(1, Ordering::Relaxed);
                        Box::pin(async { Err(zx::Status::BAD_STATE) })
                    })),
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: BlockIoFlag::GROUP_ITEM.bits(),
                        ..Default::default()
                    },
                    group: 1,
                    vmoid: vmo_id.id,
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

                assert!(futures::poll!(fifo.read_entry()).is_pending());

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: BlockIoFlag::GROUP_ITEM.bits(),
                        ..Default::default()
                    },
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                        ..Default::default()
                    },
                    reqid: 2,
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_BAD_STATE);
                assert_eq!(response.reqid, 2);
                assert_eq!(response.group, 1);

                assert!(futures::poll!(fifo.read_entry()).is_pending());

                // Only the first request should have been processed.
                assert_eq!(counter.load(Ordering::Relaxed), 1);
            }
        );
    }

    #[fuchsia::test]
    async fn test_group_with_two_lasts() {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>().unwrap();

        let (tx, rx) = oneshot::channel();

        futures::join!(
            async move {
                let rx = Mutex::new(Some(rx));
                let block_server = BlockServer::new(MockInterface {
                    partition_info: test_partition_info(),
                    read_hook: Some(Box::new(move |_, _, _, _| {
                        let rx = rx.lock().unwrap().take().unwrap();
                        Box::pin(async {
                            let _ = rx.await;
                            Ok(())
                        })
                    })),
                });
                block_server.handle_requests(stream).await.unwrap();
            },
            async move {
                let (session_proxy, server) = fidl::endpoints::create_proxy().unwrap();

                proxy.open_session(server).unwrap();

                let vmo = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
                let vmo_id = session_proxy
                    .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
                    .await
                    .unwrap()
                    .unwrap();

                let fifo =
                    fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                        ..Default::default()
                    },
                    reqid: 1,
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                fifo.write_entries(&BlockFifoRequest {
                    command: BlockFifoCommand {
                        opcode: BlockOpcode::Read.into_primitive(),
                        flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                        ..Default::default()
                    },
                    reqid: 2,
                    group: 1,
                    vmoid: vmo_id.id,
                    ..Default::default()
                })
                .await
                .unwrap();

                // Send an independent request to flush through the fifo.
                fifo.write_entries(&BlockFifoRequest {
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
                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_OK);
                assert_eq!(response.reqid, 3);

                // Now release the original request.
                tx.send(()).unwrap();

                // The response should be for the first message tagged as last, and it should be
                // an error because we sent two messages with the LAST marker.
                let response: BlockFifoResponse = fifo.read_entry().await.unwrap();
                assert_eq!(response.status, zx::sys::ZX_ERR_INVALID_ARGS);
                assert_eq!(response.reqid, 1);
                assert_eq!(response.group, 1);
            }
        );
    }
}
