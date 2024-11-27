// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::file::FxFile;
use crate::fuchsia::node::OpenedNode;
use anyhow::Error;
use block_client::{BlockFifoRequest, BlockFifoResponse};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_hardware_block_volume::{self as volume, VolumeMarker, VolumeRequest};
use fuchsia_async::{self as fasync, FifoReadable, FifoWritable};
use futures::stream::TryStreamExt;
use futures::try_join;
use fxfs::errors::FxfsError;
use fxfs::round::{round_down, round_up};
use rustc_hash::FxHashMap as HashMap;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use vfs::file::File;
use vfs::node::Node;
use {fidl_fuchsia_hardware_block as block, fidl_fuchsia_io as fio};

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
    group_id: u16,
    status: zx::sys::zx_status_t,
    count: u32,
}

impl Hash for FifoMessageGroup {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.group_id.hash(state);
    }
}

impl FifoMessageGroup {
    // Initialise a FifoMessageGroup given the request group ID.
    // `count` is set to 0 as no requests has been processed yet.
    fn new(group_id: u16) -> Self {
        Self { group_id, status: zx::sys::ZX_OK, count: 0 }
    }

    // Takes the FifoMessageGroup and converts it to a BlockFifoResponse.
    // Note that this doesn't return the request ID, it needs to be set
    // after extracting the BlockFifoResponse before sending it
    fn into_response(self) -> BlockFifoResponse {
        return BlockFifoResponse {
            status: self.status,
            group: self.group_id,
            count: self.count,
            ..Default::default()
        };
    }

    fn increment_count(&mut self) {
        self.count += 1;
    }

    fn set_status(&mut self, status: zx::sys::zx_status_t) {
        self.status = status;
    }

    fn is_err(&self) -> bool {
        self.status != zx::sys::ZX_OK
    }
}

struct FifoMessageGroups(HashMap<u16, FifoMessageGroup>);

// Keeps track of all the group requests that are currently being processed
impl FifoMessageGroups {
    fn new() -> Self {
        Self(HashMap::default())
    }

    // Returns the current MessageGroup with this group ID
    fn get(&mut self, group_id: u16) -> &mut FifoMessageGroup {
        self.0.entry(group_id).or_insert_with(|| FifoMessageGroup::new(group_id))
    }

    // Remove a group when `BlockIoFlag::GROUP_LAST` flag is set.
    fn remove(&mut self, group_id: u16) -> FifoMessageGroup {
        match self.0.remove(&group_id) {
            Some(group) => group,
            // `remove(group_id)` can be called when the group has not yet been
            // added to this FifoMessageGroups. In which case, return a default
            // MessageGroup.
            None => FifoMessageGroup::new(group_id),
        }
    }
}

// This is the default slice size used for Volumes in devices
const DEVICE_VOLUME_SLICE_SIZE: u64 = 32 * 1024;

/// Implements server to handle Block requests
pub struct BlockServer {
    file: OpenedNode<FxFile>,
    server_channel: Option<zx::Channel>,
    maybe_server_fifo: Mutex<Option<zx::Fifo<BlockFifoResponse, BlockFifoRequest>>>,
    message_groups: Mutex<FifoMessageGroups>,
    vmos: Mutex<BTreeMap<u16, zx::Vmo>>,
}

impl BlockServer {
    /// Creates a new BlockServer given a server channel to listen on.
    pub fn new(file: OpenedNode<FxFile>, server_channel: zx::Channel) -> BlockServer {
        BlockServer {
            file,
            server_channel: Some(server_channel),
            maybe_server_fifo: Mutex::new(None),
            message_groups: Mutex::new(FifoMessageGroups::new()),
            vmos: Mutex::new(BTreeMap::new()),
        }
    }

    // Returns a VMO id that is currently not being used
    fn get_vmo_id(&self, vmo: zx::Vmo) -> Option<u16> {
        let mut vmos = self.vmos.lock().unwrap();
        let mut prev_id = 0;
        for &id in vmos.keys() {
            if id != prev_id + 1 {
                let vmo_id = prev_id + 1;
                vmos.insert(vmo_id, vmo);
                return Some(vmo_id);
            }
            prev_id = id;
        }
        if prev_id < std::u16::MAX {
            let vmo_id = prev_id + 1;
            vmos.insert(vmo_id, vmo);
            Some(vmo_id)
        } else {
            None
        }
    }

    async fn handle_blockio_write(&self, request: &BlockFifoRequest) -> Result<(), Error> {
        let block_size = self.file.get_block_size();

        let data = {
            let vmos = self.vmos.lock().unwrap();
            let vmo = vmos.get(&request.vmoid).ok_or(FxfsError::NotFound)?;
            let mut buffer = vec![0u8; (request.length as u64 * block_size) as usize];
            vmo.read(&mut buffer[..], request.vmo_offset * block_size)?;
            buffer
        };

        self.file.write_at_uncached(request.dev_offset * block_size as u64, &data[..]).await?;

        Ok(())
    }

    async fn handle_blockio_read(&self, request: &BlockFifoRequest) -> Result<(), Error> {
        let block_size = self.file.get_block_size();

        let mut buffer = vec![0u8; (request.length as u64 * block_size) as usize];
        let bytes_read = self
            .file
            .read_at_uncached(request.dev_offset * (block_size as u64), &mut buffer[..])
            .await?;

        // Fill in the rest of the buffer if bytes_read is less than the requested amount
        buffer[bytes_read as usize..].fill(0);

        let vmos = self.vmos.lock().unwrap();
        let vmo = vmos.get(&request.vmoid).ok_or(FxfsError::NotFound)?;
        vmo.write(&buffer[..], request.vmo_offset * block_size)?;

        Ok(())
    }

    async fn process_fifo_request(&self, request: &BlockFifoRequest) -> zx::sys::zx_status_t {
        fn into_raw_status(result: Result<(), Error>) -> zx::sys::zx_status_t {
            let status: zx::Status = result.map_err(|e| map_to_status(e)).into();
            status.into_raw()
        }

        match block_client::BlockOpcode::from_primitive(request.command.opcode) {
            Some(block_client::BlockOpcode::CloseVmo) => {
                let mut vmos = self.vmos.lock().unwrap();
                match vmos.remove(&request.vmoid) {
                    Some(_vmo) => zx::sys::ZX_OK,
                    None => zx::sys::ZX_ERR_NOT_FOUND,
                }
            }
            Some(block_client::BlockOpcode::Write) => {
                into_raw_status(self.handle_blockio_write(&request).await)
            }
            Some(block_client::BlockOpcode::Read) => {
                into_raw_status(self.handle_blockio_read(&request).await)
            }
            // TODO(https://fxbug.dev/42171261): simply returning ZX_OK since we're
            // writing to device and no need to flush cache, but need to
            // check that flush goes down the stack
            Some(block_client::BlockOpcode::Flush) => zx::sys::ZX_OK,
            // TODO(https://fxbug.dev/42171261)
            Some(block_client::BlockOpcode::Trim) => zx::sys::ZX_OK,
            None => panic!("Unexpected message, request {:?}", request.command.opcode),
        }
    }

    async fn handle_fifo_request(&self, request: BlockFifoRequest) -> Option<BlockFifoResponse> {
        let flags = block_client::BlockIoFlag::from_bits_truncate(request.command.flags);
        let is_group = flags.contains(block_client::BlockIoFlag::GROUP_ITEM);
        let wants_reply = flags.contains(block_client::BlockIoFlag::GROUP_LAST);

        // Set up the BlockFifoResponse for this request, but do no process request yet
        let mut maybe_reply = {
            if is_group {
                let mut groups = self.message_groups.lock().unwrap();
                if wants_reply {
                    let mut group = groups.remove(request.group);
                    group.increment_count();

                    // This occurs when a previous request in this group has failed
                    if group.is_err() {
                        let mut reply = group.into_response();
                        reply.reqid = request.reqid;
                        // No need to process this request
                        return Some(reply);
                    }

                    let mut response = group.into_response();
                    response.reqid = request.reqid;
                    Some(response)
                } else {
                    let group = groups.get(request.group);
                    group.increment_count();

                    if group.is_err() {
                        // No need to process this request
                        return None;
                    }
                    None
                }
            } else {
                Some(BlockFifoResponse { reqid: request.reqid, count: 1, ..Default::default() })
            }
        };
        let status = self.process_fifo_request(&request).await;
        // Status only needs to be updated in the reply if it's not OK
        if status != zx::sys::ZX_OK {
            match &mut maybe_reply {
                None => {
                    // maybe_reply will only be None if it's part of a group request
                    self.message_groups.lock().unwrap().get(request.group).set_status(status);
                }
                Some(reply) => {
                    reply.status = status;
                }
            }
        }
        maybe_reply
    }

    async fn handle_request(&self, request: VolumeRequest) -> Result<(), Error> {
        match request {
            VolumeRequest::GetInfo { responder } => {
                let block_size = self.file.get_block_size();
                let block_count =
                    (self.file.get_size().await.unwrap() + block_size - 1) / block_size;
                responder.send(Ok(&block::BlockInfo {
                    block_count,
                    block_size: block_size as u32,
                    max_transfer_size: 1024 * 1024,
                    flags: block::Flag::empty(),
                }))?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::GetStats { clear: _, responder } => {
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            VolumeRequest::OpenSession { session, control_handle: _ } => {
                let stream = session.into_stream();
                let () = stream
                    .try_for_each(|request| async {
                        let () = match request {
                            block::SessionRequest::GetFifo { responder } => {
                                match self.maybe_server_fifo.lock().unwrap().take() {
                                    Some(fifo) => responder.send(Ok(fifo.downcast()))?,
                                    None => {
                                        responder.send(Err(zx::Status::NO_RESOURCES.into_raw()))?
                                    }
                                }
                            }
                            block::SessionRequest::AttachVmo { vmo, responder } => {
                                match self.get_vmo_id(vmo) {
                                    Some(vmo_id) => {
                                        responder.send(Ok(&block::VmoId { id: vmo_id }))?
                                    }
                                    None => {
                                        responder.send(Err(zx::Status::NO_RESOURCES.into_raw()))?
                                    }
                                }
                            }
                            // TODO(https://fxbug.dev/42171261): close fifo
                            block::SessionRequest::Close { responder } => responder.send(Ok(()))?,
                        };
                        Ok(())
                    })
                    .await?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::GetTypeGuid { responder } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::GetInstanceGuid { responder } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::GetName { responder } => {
                responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED, None)?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::GetMetadata { responder } => {
                responder.send(Err(zx::sys::ZX_ERR_NOT_SUPPORTED))?;
            }
            VolumeRequest::QuerySlices { start_slices, responder } => {
                // Initialise slices with default value.
                let default = volume::VsliceRange { allocated: false, count: 0 };
                let mut slices = [default; volume::MAX_SLICE_REQUESTS as usize];

                let mut status = zx::sys::ZX_OK;
                let mut response_count = 0;
                for (slice, start_slice) in slices.iter_mut().zip(start_slices.into_iter()) {
                    match self.file.is_allocated(start_slice * DEVICE_VOLUME_SLICE_SIZE).await {
                        Ok((allocated, bytes)) => {
                            slice.count = round_up(bytes, DEVICE_VOLUME_SLICE_SIZE).unwrap();
                            slice.allocated = allocated;
                            response_count += 1;
                        }
                        Err(e) => {
                            status = e.into_raw();
                            break;
                        }
                    }
                }
                responder.send(status, &slices, response_count)?;
            }
            // TODO(https://fxbug.dev/42171261): need to check if this returns the right information.
            VolumeRequest::GetVolumeInfo { responder } => {
                match self.file.get_attributes(fio::NodeAttributesQuery::STORAGE_SIZE).await {
                    Ok(attr) => {
                        debug_assert!(attr.immutable_attributes.storage_size.is_some());
                        let allocated_bytes = attr.immutable_attributes.storage_size.unwrap();
                        let unallocated_bytes =
                            self.file.get_size_uncached().await - allocated_bytes;
                        let allocated_slices =
                            round_up(allocated_bytes, DEVICE_VOLUME_SLICE_SIZE).unwrap();
                        let unallocated_bytes =
                            round_down(unallocated_bytes, DEVICE_VOLUME_SLICE_SIZE);
                        let manager = volume::VolumeManagerInfo {
                            slice_size: DEVICE_VOLUME_SLICE_SIZE,
                            slice_count: allocated_slices + unallocated_bytes,
                            assigned_slice_count: allocated_slices,
                            maximum_slice_count: allocated_slices + unallocated_bytes,
                            max_virtual_slice: allocated_slices + unallocated_bytes,
                        };
                        let volume_info = volume::VolumeInfo {
                            partition_slice_count: allocated_slices,
                            slice_limit: 0,
                        };
                        responder.send(zx::sys::ZX_OK, Some(&manager), Some(&volume_info))?;
                    }
                    Err(e) => {
                        responder.send(e.into_raw(), None, None)?;
                    }
                }
            }
            VolumeRequest::Extend { start_slice, slice_count, responder } => {
                // TODO(https://fxbug.dev/42171261): this is a hack. When extend is called, the extent is
                // expected to be set as allocated. The easiest way to do this is to just
                // write an extent of zeroed data. Another issue here is the size. The memory
                // allocated here should be bounded to what's available.
                let data = vec![0u8; (slice_count * DEVICE_VOLUME_SLICE_SIZE) as usize];
                match self
                    .file
                    .write_at_uncached(start_slice * DEVICE_VOLUME_SLICE_SIZE, data[..].into())
                    .await
                {
                    Ok(_) => responder.send(zx::sys::ZX_OK)?,
                    Err(status) => responder.send(status.into_raw())?,
                };
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::Shrink { start_slice: _, slice_count: _, responder } => {
                responder.send(zx::sys::ZX_OK)?;
            }
            // TODO(https://fxbug.dev/42171261)
            VolumeRequest::Destroy { responder } => {
                responder.send(zx::sys::ZX_OK)?;
            }
        }
        Ok(())
    }

    async fn handle_requests(
        &self,
        server: fidl::endpoints::ServerEnd<VolumeMarker>,
    ) -> Result<(), Error> {
        server
            .into_stream()
            .map_err(|e| e.into())
            .try_for_each_concurrent(None, |request| self.handle_request(request))
            .await?;
        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let server = ServerEnd::<VolumeMarker>::new(self.server_channel.take().unwrap());

        // Create a fifo pair
        let (server_fifo, client_fifo) =
            zx::Fifo::<BlockFifoRequest, BlockFifoResponse>::create(16)?;
        self.maybe_server_fifo = Mutex::new(Some(client_fifo));

        // Handling requests from fifo
        let fifo_future = async {
            let fifo = fasync::Fifo::from_fifo(server_fifo);
            loop {
                match fifo.read_entry().await {
                    Ok(request) => {
                        if let Some(response) = self.handle_fifo_request(request).await {
                            fifo.write_entries(std::slice::from_ref(&response)).await?;
                        }
                        // if `self.handle_fifo_request(..)` returns None, then
                        // there's no reply for this request. This occurs for
                        // requests part of a group request where
                        // `BlockIoFlag::GROUP_LAST` flag is not set.
                    }
                    Err(zx::Status::PEER_CLOSED) => break Result::<_, Error>::Ok(()),
                    Err(e) => break Err(e.into()),
                }
            }
        };

        // Handling requests from fidl
        let channel_future = async {
            self.handle_requests(server).await?;
            // This is temporary for when client doesn't call for fifo
            self.maybe_server_fifo.lock().unwrap().take();
            Ok(())
        };

        try_join!(fifo_future, channel_future)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::fuchsia::testing::{open_file_checked, TestFixture};
    use block_client::{BlockClient, RemoteBlockClient, VmoId};
    use fidl::endpoints::{ClientEnd, ServerEnd};
    use fidl_fuchsia_hardware_block::BlockMarker;
    use fidl_fuchsia_hardware_block_volume::VolumeMarker;
    use fidl_fuchsia_io as fio;
    use fs_management::filesystem::Filesystem;
    use fs_management::Blobfs;
    use futures::join;
    use rustc_hash::FxHashSet as HashSet;

    struct BlockConnector(fio::DirectoryProxy, &'static str);

    impl fs_management::filesystem::BlockConnector for BlockConnector {
        fn connect_volume(&self) -> Result<ClientEnd<VolumeMarker>, anyhow::Error> {
            let (client, server) = fidl::endpoints::create_endpoints::<VolumeMarker>();
            self.0
                .open(
                    fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    self.1,
                    server.into_channel().into(),
                )
                .expect("open failed");
            Ok(client)
        }
    }

    #[fuchsia::test(threads = 10)]
    async fn test_block_server() {
        let fixture = TestFixture::new().await;
        let connector = {
            let root = fixture.root();
            let file = open_file_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "block_device",
            )
            .await;
            file.resize(2 * 1024 * 1024).await.expect("FIDL error").expect("resize error");
            let () = file.close().await.expect("FIDL error").expect("close error");
            let (client, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            root.clone2(server.into_channel().into()).expect("clone error");
            BlockConnector(client, "block_device")
        };

        {
            let mut blobfs = Filesystem::new(connector, Blobfs::default());
            blobfs.format().await.expect("format blobfs failed");
            blobfs.fsck().await.expect("fsck failed");
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_attach_vmo() {
        let fixture = TestFixture::new().await;
        let (client_channel, server_channel) = zx::Channel::create();
        join!(
            async {
                let block_client = RemoteBlockClient::new(
                    ClientEnd::<BlockMarker>::new(client_channel).into_proxy(),
                )
                .await
                .expect("RemoteBlockClient::new failed");
                let mut vmo_set = HashSet::default();
                let vmo = zx::Vmo::create(1).expect("Vmo::create failed");
                for _ in 1..5 {
                    match block_client.attach_vmo(&vmo).await {
                        Ok(vmo_id) => {
                            // TODO(https://fxbug.dev/42171261): need to detach vmoid. into_id() is a
                            // temporary solution. Remove this after detaching vmo has been
                            // implemented
                            // Make sure that vmo_id is unique
                            assert_eq!(vmo_set.insert(vmo_id.into_id()), true);
                        }
                        Err(e) => panic!("unexpected error {:?}", e),
                    }
                }
            },
            async {
                let root = fixture.root();
                root.open(
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    "foo",
                    ServerEnd::new(server_channel),
                )
                .expect("open failed");
            }
        );
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_detach_vmo() {
        let fixture = TestFixture::new().await;
        let (client_channel, server_channel) = zx::Channel::create();
        join!(
            async {
                let block_client = RemoteBlockClient::new(
                    ClientEnd::<BlockMarker>::new(client_channel).into_proxy(),
                )
                .await
                .expect("RemoteBlockClient::new failed");
                let vmo = zx::Vmo::create(1).expect("Vmo::create failed");
                let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");
                let vmo_id_copy = VmoId::new(vmo_id.id());
                block_client.detach_vmo(vmo_id).await.expect("detach failed");
                block_client.detach_vmo(vmo_id_copy).await.expect_err("detach succeeded");
            },
            async {
                let root = fixture.root();
                root.open(
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    "foo",
                    ServerEnd::new(server_channel),
                )
                .expect("open failed");
            }
        );
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_read_write_files() {
        let fixture = TestFixture::new().await;
        let (client_channel, server_channel) = zx::Channel::create();
        join!(
            async {
                let block_client = RemoteBlockClient::new(
                    ClientEnd::<BlockMarker>::new(client_channel).into_proxy(),
                )
                .await
                .expect("RemoteBlockClient::new failed");
                let vmo = zx::Vmo::create(131072).expect("create vmo failed");
                let vmo_id = block_client.attach_vmo(&vmo).await.expect("attach_vmo failed");

                // Must write with length as a multiple of the block_size
                let offset = block_client.block_size() as usize;
                let len = block_client.block_size() as usize;
                let write_buf = vec![0xa3u8; len];
                block_client
                    .write_at(write_buf[..].into(), offset as u64)
                    .await
                    .expect("write_at failed");

                // Read back an extra block either side
                let mut read_buf = vec![0u8; len + 2 * block_client.block_size() as usize];
                block_client
                    .read_at(read_buf.as_mut_slice().into(), 0)
                    .await
                    .expect("read_at failed");

                // We expect the extra block on the LHS of the read_buf to be 0
                assert_eq!(&read_buf[..offset], &vec![0; offset][..]);

                assert_eq!(&read_buf[offset..offset + len], &write_buf);

                // We expect the extra block on the RHS of the read_buf to be 0
                assert_eq!(
                    &read_buf[offset + len..],
                    &vec![0; block_client.block_size() as usize][..]
                );

                block_client.detach_vmo(vmo_id).await.expect("detach failed");
            },
            async {
                let root = fixture.root();
                root.open(
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    "foo",
                    ServerEnd::new(server_channel),
                )
                .expect("open failed");
            }
        );
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_flush_is_called() {
        let fixture = TestFixture::new().await;
        let (client_channel, server_channel) = zx::Channel::create();
        join!(
            async {
                let block_client = RemoteBlockClient::new(
                    ClientEnd::<BlockMarker>::new(client_channel).into_proxy(),
                )
                .await
                .expect("RemoteBlockClient::new failed");
                block_client.flush().await.expect("flush failed");
            },
            async {
                let root = fixture.root();
                root.open(
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    "foo",
                    ServerEnd::new(server_channel),
                )
                .expect("open failed");
            }
        );
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_get_info() {
        let fixture = TestFixture::new().await;
        let (client_channel, server_channel) = zx::Channel::create();
        let file_size = 2 * 1024 * 1024;
        join!(
            async {
                let original_block_device =
                    ClientEnd::<VolumeMarker>::new(client_channel).into_proxy();
                let info = original_block_device
                    .get_info()
                    .await
                    .expect("get_info failed")
                    .map_err(zx::Status::from_raw)
                    .expect("block get_info failed");
                assert_eq!(info.block_count * u64::from(info.block_size), file_size);
            },
            async {
                let root = fixture.root();
                let file = open_file_checked(
                    &root,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::NOT_DIRECTORY,
                    "block_device",
                )
                .await;
                let () = file
                    .resize(file_size)
                    .await
                    .expect("resize failed")
                    .map_err(zx::Status::from_raw)
                    .expect("resize error");
                let () = file
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(zx::Status::from_raw)
                    .expect("close error");

                root.open(
                    fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::BLOCK_DEVICE,
                    fio::ModeType::empty(),
                    "block_device",
                    ServerEnd::new(server_channel),
                )
                .expect("open failed");
            }
        );
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_blobfs() {
        let fixture = TestFixture::new().await;
        let connector = {
            let root = fixture.root();
            let file = open_file_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "block_device",
            )
            .await;
            file.resize(5 * 1024 * 1024).await.expect("FIDL error").expect("resize error");
            let () = file.close().await.expect("FIDL error").expect("close error");
            let (client, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            root.clone2(server.into_channel().into()).expect("clone error");
            BlockConnector(client, "block_device")
        };

        {
            let mut blobfs = Filesystem::new(connector, Blobfs::default());
            blobfs.format().await.expect("format blobfs failed");
            blobfs.fsck().await.expect("fsck failed");
            // Mount blobfs
            let serving = blobfs.serve().await.expect("serve blobfs failed");

            let content = String::from("Hello world!").into_bytes();
            let merkle_root_hash = fuchsia_merkle::from_slice(&content).root().to_string();
            {
                let file = fuchsia_fs::directory::open_file(
                    serving.root(),
                    &merkle_root_hash,
                    fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_WRITABLE,
                )
                .await
                .expect("open file failed");
                let () = file
                    .resize(content.len() as u64)
                    .await
                    .expect("resize failed")
                    .map_err(zx::Status::from_raw)
                    .expect("resize error");
                let _: u64 = file
                    .write(&content)
                    .await
                    .expect("write to file failed")
                    .map_err(zx::Status::from_raw)
                    .expect("write to file error");
            }
            // Check that blobfs can be successfully unmounted
            serving.shutdown().await.expect("shutdown blobfs failed");

            let serving = blobfs.serve().await.expect("serve blobfs failed");
            {
                let file = fuchsia_fs::directory::open_file(
                    serving.root(),
                    &merkle_root_hash,
                    fio::PERM_READABLE,
                )
                .await
                .expect("open file failed");
                let read_content =
                    fuchsia_fs::file::read(&file).await.expect("read from file failed");
                assert_eq!(content, read_content);
            }

            serving.shutdown().await.expect("shutdown blobfs failed");
        }
        fixture.close().await;
    }
}
