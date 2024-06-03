// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        file::FxFile,
        fxblob::{blob::FxBlob, BlobDirectory},
        node::FxNode,
        pager::PagerBacked,
        volume::FxVolume,
    },
    anyhow::{anyhow, Error},
    arrayref::{array_refs, mut_array_refs},
    async_trait::async_trait,
    event_listener::EventListener,
    fuchsia_async as fasync,
    fuchsia_hash::Hash,
    fuchsia_zircon as zx,
    fxfs::{
        drop_event::DropEvent,
        errors::FxfsError,
        log::*,
        object_handle::{ReadObjectHandle, WriteObjectHandle},
        object_store::ObjectDescriptor,
    },
    linked_hash_map::LinkedHashMap,
    std::{
        cmp::{Eq, PartialEq},
        collections::btree_map::{BTreeMap, Entry},
        marker::PhantomData,
        mem::size_of,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    },
    storage_device::buffer::{Buffer, BufferRef},
};

const FILE_OPEN_MARKER: u64 = u64::MAX;
const REPLAY_THREADS: usize = 2;
// The number of messages to buffer before sending to record. They are chunked up to reduce the
// number of allocations in the serving threads.
const MESSAGE_CHUNK_SIZE: usize = 64;
const IO_SIZE: usize = 1 << 17; // 128KiB. Needs to be a power of 2 and >= block size.

/// A helper trait for WriteObjectHandle to be used in dynamic dispatch.
#[async_trait]
pub trait UnsizedWriteObjectHandle: Send {
    /// Append the contents of `buf` to the handle.
    async fn append(&self, buf: BufferRef<'_>) -> Result<u64, Error>;

    /// The underlying block size increments for the handle.
    fn block_size(&self) -> u64;

    /// Allocate an I/O buffer that can be used to append to the handle.
    async fn allocate_buffer(&self, size: usize) -> Buffer<'_>;
}

#[async_trait]
impl<T: WriteObjectHandle> UnsizedWriteObjectHandle for T {
    async fn append(&self, buf: BufferRef<'_>) -> Result<u64, Error> {
        self.write_or_append(None, buf).await
    }

    fn block_size(&self) -> u64 {
        self.block_size()
    }

    async fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.allocate_buffer(size).await
    }
}

trait RecordedVolume: Send + Sync + Sized {
    type IdType: std::fmt::Display + Ord + Send + Sized;
    type NodeType: PagerBacked;
    type MessageType: Message<IdType = Self::IdType>;
    type OpenerType: Opener<Self> + Send + Sized + Sync;

    fn new_opener(
        volume: Arc<FxVolume>,
    ) -> impl std::future::Future<Output = Result<Self::OpenerType, Error>> + Send;
}

/// Creates an object opener for the volume. Allows opening either `FxFile`s or `FxBlob`s through a
/// generic interface.
trait Opener<T: RecordedVolume> {
    fn open(
        &self,
        id: T::IdType,
    ) -> impl std::future::Future<Output = Result<Arc<T::NodeType>, Error>> + Send;
}

trait Message: Eq + PartialEq + Sized + Send + Sync + std::hash::Hash + 'static {
    type IdType: std::fmt::Display + Ord + Send + Sized;

    fn id(&self) -> Self::IdType;
    fn offset(&self) -> u64;
    fn encode_to(&self, dest: &mut [u8]);
    fn decode_from(src: &[u8]) -> Self;
    fn is_zeroes(&self) -> bool;
    fn from_node_request(node: Arc<dyn FxNode>, offset: u64) -> Result<Self, Error>;
    fn open_marker_for_id(&self) -> Self;
    fn is_open_marker(&self) -> bool;
}

struct BlobVolume;
impl RecordedVolume for BlobVolume {
    type IdType = Hash;
    type NodeType = FxBlob;
    type MessageType = BlobMessage;
    type OpenerType = BlobOpener;

    async fn new_opener(volume: Arc<FxVolume>) -> Result<Self::OpenerType, Error> {
        Ok(Self::OpenerType {
            dir: volume
                .get_or_load_node(
                    volume.store().root_directory_object_id(),
                    ObjectDescriptor::Directory,
                    None,
                )
                .await?
                .into_any()
                .downcast::<BlobDirectory>()
                .map_err(|_| FxfsError::Inconsistent)?,
        })
    }
}

struct FileVolume;
impl RecordedVolume for FileVolume {
    type IdType = u64;
    type NodeType = FxFile;
    type MessageType = FileMessage;
    type OpenerType = FileOpener;

    async fn new_opener(volume: Arc<FxVolume>) -> Result<Self::OpenerType, Error> {
        Ok(Self::OpenerType { volume })
    }
}

#[derive(Debug, Eq, std::hash::Hash, PartialEq)]
struct BlobMessage {
    id: Hash,
    // Don't bother with offset+length. The kernel is going split up and align it one way and then
    // we're going to change it all with read-ahead/read-around.
    offset: u64,
}

impl BlobMessage {
    fn encode_to_impl(&self, dest: &mut [u8; size_of::<Self>()]) {
        let (first, second) = mut_array_refs![dest, size_of::<Hash>(), size_of::<u64>()];
        *first = self.id.into();
        *second = self.offset.to_le_bytes();
    }

    fn decode_from_impl(src: &[u8; size_of::<Self>()]) -> Self {
        let (first, second) = array_refs!(src, size_of::<Hash>(), size_of::<u64>());
        Self { id: Hash::from_array(*first), offset: u64::from_le_bytes(*second) }
    }
}

impl Message for BlobMessage {
    type IdType = Hash;

    fn id(&self) -> Self::IdType {
        self.id
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn encode_to(&self, dest: &mut [u8]) {
        self.encode_to_impl(dest.try_into().unwrap());
    }

    fn decode_from(src: &[u8]) -> Self {
        Self::decode_from_impl(src.try_into().unwrap())
    }

    fn is_zeroes(&self) -> bool {
        self.id == Hash::from_array([0u8; size_of::<Hash>()]) && self.offset == 0
    }

    fn from_node_request(node: Arc<dyn FxNode>, offset: u64) -> Result<Self, Error> {
        match node.into_any().downcast::<FxBlob>() {
            Ok(blob) => Ok(Self { id: blob.root(), offset }),
            Err(_) => Err(anyhow!("Cannot record non-blob entry.")),
        }
    }

    fn open_marker_for_id(&self) -> Self {
        Self { id: self.id, offset: FILE_OPEN_MARKER }
    }

    fn is_open_marker(&self) -> bool {
        self.offset == FILE_OPEN_MARKER
    }
}

struct BlobOpener {
    dir: Arc<BlobDirectory>,
}

impl Opener<BlobVolume> for BlobOpener {
    async fn open(&self, id: Hash) -> Result<Arc<FxBlob>, Error> {
        self.dir.lookup_blob(id).await
    }
}

#[derive(Debug, Eq, std::hash::Hash, PartialEq)]
struct FileMessage {
    id: u64,
    // Don't bother with offset+length. The kernel is going split up and align it one way and then
    // we're going to change it all with read-ahead/read-around.
    offset: u64,
}

impl FileMessage {
    fn encode_to_impl(&self, dest: &mut [u8; size_of::<Self>()]) {
        let (first, second) = mut_array_refs![dest, size_of::<u64>(), size_of::<u64>()];
        *first = self.id.to_le_bytes();
        *second = self.offset.to_le_bytes();
    }

    fn decode_from_impl(src: &[u8; size_of::<Self>()]) -> Self {
        let (first, second) = array_refs!(src, size_of::<u64>(), size_of::<u64>());
        Self { id: u64::from_le_bytes(*first), offset: u64::from_le_bytes(*second) }
    }
}

impl Message for FileMessage {
    type IdType = u64;

    fn id(&self) -> Self::IdType {
        self.id
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn encode_to(&self, dest: &mut [u8]) {
        self.encode_to_impl(dest.try_into().unwrap())
    }

    fn decode_from(src: &[u8]) -> Self {
        Self::decode_from_impl(src.try_into().unwrap())
    }

    fn is_zeroes(&self) -> bool {
        self.id == 0 && self.offset == 0
    }

    fn from_node_request(node: Arc<dyn FxNode>, offset: u64) -> Result<Self, Error> {
        match node.into_any().downcast::<FxFile>() {
            Ok(file) => Ok(Self { id: file.object_id(), offset }),
            Err(_) => Err(anyhow!("Cannot record non-file entry")),
        }
    }

    fn open_marker_for_id(&self) -> Self {
        Self { id: self.id, offset: FILE_OPEN_MARKER }
    }

    fn is_open_marker(&self) -> bool {
        self.offset == FILE_OPEN_MARKER
    }
}

struct FileOpener {
    volume: Arc<FxVolume>,
}

impl Opener<FileVolume> for FileOpener {
    async fn open(&self, id: u64) -> Result<Arc<FxFile>, Error> {
        self.volume
            .get_or_load_node(id, ObjectDescriptor::File, None)
            .await?
            .into_any()
            .downcast::<FxFile>()
            .map_err(|_| anyhow!("Non-file opened"))
    }
}

/// Paired with a RecordingState, takes messages to be written into the current profile. This
/// should be dropped before the recording is stopped to ensure that all messages have been
/// flushed to the writer thread.
pub trait Recorder: Send + Sync {
    /// Record a page in request, for the given identifier and offset.
    fn record(&mut self, node: Arc<dyn FxNode>, offset: u64) -> Result<(), Error>;

    /// Record file opens to gather what files were actually used during the recording.
    fn record_open(&mut self, node: Arc<dyn FxNode>) -> Result<(), Error>;
}

struct RecorderImpl<T: Message> {
    sender: async_channel::Sender<Vec<T>>,
    buffer: Vec<T>,
}

impl<T: Message> RecorderImpl<T> {
    fn new(sender: async_channel::Sender<Vec<T>>) -> Self {
        Self { sender, buffer: Vec::with_capacity(MESSAGE_CHUNK_SIZE) }
    }
}

impl<T: Message> Recorder for RecorderImpl<T> {
    fn record(&mut self, node: Arc<dyn FxNode>, offset: u64) -> Result<(), Error> {
        self.buffer.push(T::from_node_request(node, offset)?);
        if self.buffer.len() >= MESSAGE_CHUNK_SIZE {
            // try_send to avoid async await, we use an unbounded channel anyways so any failure
            // here should only be if the channel is closed, which is permanent anyways.
            self.sender.try_send(std::mem::replace(
                &mut self.buffer,
                Vec::with_capacity(MESSAGE_CHUNK_SIZE),
            ))?;
        }
        Ok(())
    }

    fn record_open(&mut self, node: Arc<dyn FxNode>) -> Result<(), Error> {
        self.record(node, FILE_OPEN_MARKER)
    }
}

impl<T: Message> Drop for RecorderImpl<T> {
    fn drop(&mut self) {
        // If the channel is closed then we've already shut down the receiver.
        debug_assert!(!self.sender.is_closed());
        // Best effort sending what messages have already been queued.
        if self.buffer.len() > 0 {
            let buffer = std::mem::take(&mut self.buffer);
            let _ = self.sender.try_send(buffer);
        }
    }
}

struct RecordingState<T: RecordedVolume> {
    stopped_listener: EventListener,
    sender: async_channel::Sender<Vec<T::MessageType>>,
}

impl<T: RecordedVolume> RecordingState<T> {
    fn new(handle: Box<dyn UnsizedWriteObjectHandle>) -> Self {
        let finished_event = DropEvent::new();
        let (sender, receiver) = async_channel::unbounded::<Vec<T::MessageType>>();
        let stopped_listener = (*finished_event).listen();
        fasync::Task::spawn(async move {
            // DropEvent should fire when this task is done.
            let _finished_event = finished_event;
            let mut recording = LinkedHashMap::<T::MessageType, ()>::new();
            while let Ok(buffer) = receiver.recv().await {
                for message in buffer {
                    recording.insert(message, ());
                }
            }

            let block_size = handle.block_size() as usize;
            let mut offset = 0;
            let mut io_buf = handle.allocate_buffer(IO_SIZE).await;
            let mut next_block = block_size;
            for (message, _) in &recording {
                // If this is a file opening marker or a file opening was never recorded, drop the
                // message.
                if message.is_open_marker()
                    || !recording.contains_key(&message.open_marker_for_id())
                {
                    continue;
                }

                let mut next_offset = offset + size_of::<T::MessageType>();
                if next_offset > next_block {
                    // Zero the remainder of the block. Stopping on block boundaries allows us to
                    // resize the I/O without supporting reading/writing half messages to a buffer.
                    io_buf.as_mut_slice()[offset..next_block].fill(0);
                    if next_block >= IO_SIZE {
                        // The buffer is full.  Write it out.
                        if let Err(e) = handle.append(io_buf.as_ref()).await {
                            error!("Failed to write profile block: {:?}", e);
                            return;
                        }
                        offset = 0;
                        next_offset = size_of::<T::MessageType>();
                        next_block = block_size;
                    } else {
                        offset = next_block;
                        next_offset = offset + size_of::<T::MessageType>();
                        next_block += block_size;
                    }
                }
                message.encode_to(&mut io_buf.as_mut_slice()[offset..next_offset]);
                offset = next_offset;
            }
            if offset > 0 {
                io_buf.as_mut_slice()[offset..next_block].fill(0);
                if let Err(e) = handle.append(io_buf.subslice(0..next_block)).await {
                    error!("Failed to write profile block: {:?}", e);
                }
            }
        })
        .detach();

        Self { stopped_listener, sender }
    }

    fn close(self) {
        // This closes the channel for *all* senders, which starts shutting down.
        self.sender.close();
        self.stopped_listener.wait();
    }
}

struct Request<P: PagerBacked> {
    file: Arc<P>,
    offset: u64,
}

struct ReplayState<T: RecordedVolume> {
    // Using a DropEvent to trigger task shutdown if this goes out of scope.
    stop_on_drop: DropEvent,
    stopped_listener: EventListener,
    _phantom: PhantomData<T>,
}

impl<T: RecordedVolume> ReplayState<T> {
    fn new(handle: Box<dyn ReadObjectHandle>, volume: Arc<FxVolume>) -> Self {
        let stop_on_drop = DropEvent::new();
        let (sender, receiver) = async_channel::unbounded::<Request<T::NodeType>>();
        let shutting_down = Arc::new(AtomicBool::new(false));

        // Create async_channel. An async thread reads and populates the channel, then N threads
        // consume it and touch pages.
        let mut replay_threads = Vec::with_capacity(REPLAY_THREADS);
        for _ in 0..REPLAY_THREADS {
            let stop_processing = shutting_down.clone();
            let queue = receiver.clone();
            replay_threads.push(std::thread::spawn(move || {
                Self::page_in_thread(queue, stop_processing);
            }));
        }

        let stopped_event = DropEvent::new();
        let stopped_listener = stopped_event.listen();
        let start_shutdown = stop_on_drop.listen();
        fasync::Task::spawn(async move {
            // Move the drop event in
            let _stopped_event = stopped_event;
            // Hold the items in cache until replay is stopped.
            let mut local_cache: BTreeMap<T::IdType, Arc<T::NodeType>> = BTreeMap::new();
            if let Err(e) = Self::read_and_queue(handle, volume, &sender, &mut local_cache).await {
                error!("Failed to read back profile: {:?}", e);
            }

            sender.close();
            start_shutdown.await;

            shutting_down.store(true, Ordering::Relaxed);
            for thread in replay_threads {
                let _ = thread.join();
            }
        })
        .detach();

        Self { stop_on_drop, stopped_listener, _phantom: PhantomData }
    }

    fn page_in_thread(
        queue: async_channel::Receiver<Request<T::NodeType>>,
        stop_processing: Arc<AtomicBool>,
    ) {
        while let Ok(request) = queue.recv_blocking() {
            let _ = request.file.vmo().op_range(
                zx::VmoOp::COMMIT,
                request.offset,
                zx::system_get_page_size() as u64,
            );
            if stop_processing.load(Ordering::Relaxed) {
                break;
            }
        }
    }

    async fn read_and_queue(
        handle: Box<dyn ReadObjectHandle>,
        volume: Arc<FxVolume>,
        sender: &async_channel::Sender<Request<T::NodeType>>,
        local_cache: &mut BTreeMap<T::IdType, Arc<T::NodeType>>,
    ) -> Result<(), Error> {
        let opener = T::new_opener(volume).await?;
        let mut io_buf = handle.allocate_buffer(IO_SIZE).await;
        let block_size = handle.block_size() as usize;
        let file_size = handle.get_size() as usize;
        let mut offset = 0;
        while offset < file_size {
            let actual = handle
                .read(offset as u64, io_buf.as_mut())
                .await
                .map_err(|e| e.context(format!("Failed to read at offset: {}", offset)))?;
            offset += actual;
            let mut local_offset = 0;
            let mut next_block = block_size;
            let mut next_offset = size_of::<T::MessageType>();
            while next_offset <= actual {
                let msg =
                    T::MessageType::decode_from(&io_buf.as_slice()[local_offset..next_offset]);

                local_offset = next_offset;
                next_offset = local_offset + size_of::<T::MessageType>();
                // Messages don't overlap block boundaries.
                if next_offset > next_block {
                    local_offset = next_block;
                    next_offset = local_offset + size_of::<T::MessageType>();
                    next_block += block_size;
                }

                // Ignore trailing zeroes. This is technically a valid entry but extremely unlikely
                // and will only break an optimization.
                if msg.is_zeroes() {
                    break;
                }

                let file = match local_cache.entry(msg.id()) {
                    Entry::Occupied(entry) => entry.get().clone(),
                    Entry::Vacant(entry) => match opener.open(msg.id()).await {
                        Err(e) => {
                            warn!("Failed to open object {} from profile: {:?}", msg.id(), e);
                            continue;
                        }
                        Ok(file) => {
                            entry.insert(file.clone());
                            file
                        }
                    },
                };

                sender.send(Request { file, offset: msg.offset() }).await.unwrap();
            }
        }
        Ok(())
    }

    fn close(self) {
        let ReplayState { stop_on_drop, stopped_listener, .. } = self;
        std::mem::drop(stop_on_drop);
        stopped_listener.wait();
    }
}

/// Holds the current profile recording and/or replay state, and provides methods for state
/// transitions.
pub trait ProfileState: Send + Sync {
    /// Creates a new recording and returns the `Recorder` object to record to. The recording starts
    /// shutdown when the associated `Recorder` is dropped, and shutdown is completed when
    /// `stop_profiler()` is called. Stops any recording currently in progress.
    fn record_new(&mut self, handle: Box<dyn UnsizedWriteObjectHandle>) -> Box<dyn Recorder>;

    /// Stop in-flight recording and replaying. This method will block until the resources have been
    /// cleaned up.
    fn stop_profiler(&mut self);

    /// Reads given handle to parse a profile and replay it by requesting pages via
    /// ZX_VMO_OP_COMMIT in blocking background threads. Stops any replay currently in progress.
    fn replay_profile(&mut self, handle: Box<dyn ReadObjectHandle>, volume: Arc<FxVolume>);
}

pub fn new_profile_state(is_blob: bool) -> Box<dyn ProfileState> {
    if is_blob {
        Box::new(ProfileStateImpl::<BlobVolume>::new())
    } else {
        Box::new(ProfileStateImpl::<FileVolume>::new())
    }
}

struct ProfileStateImpl<T: RecordedVolume> {
    recording: Option<RecordingState<T>>,
    replay: Option<ReplayState<T>>,
}

impl<T: RecordedVolume> ProfileStateImpl<T> {
    fn new() -> Self {
        Self { recording: None, replay: None }
    }
}

impl<T: RecordedVolume> ProfileState for ProfileStateImpl<T> {
    fn record_new(&mut self, handle: Box<dyn UnsizedWriteObjectHandle>) -> Box<dyn Recorder> {
        if let Some(recording) = std::mem::take(&mut self.recording) {
            recording.close();
        }
        let state = RecordingState::new(handle);
        let recorder = Box::new(RecorderImpl::new(state.sender.clone()));
        self.recording = Some(state);
        recorder
    }

    fn stop_profiler(&mut self) {
        // Stop replay first to let the recording likely capture it all if necessary.
        if let Some(replay) = std::mem::take(&mut self.replay) {
            replay.close();
        }
        if let Some(recording) = std::mem::take(&mut self.recording) {
            recording.close();
        }
    }

    fn replay_profile(&mut self, handle: Box<dyn ReadObjectHandle>, volume: Arc<FxVolume>) {
        if let Some(replay) = std::mem::take(&mut self.replay) {
            replay.close();
        }
        self.replay = Some(ReplayState::new(handle, volume));
    }
}

impl<T: RecordedVolume> Drop for ProfileStateImpl<T> {
    fn drop(&mut self) {
        self.stop_profiler();
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            new_profile_state, BlobMessage, BlobVolume, FileMessage, FileVolume, Message,
            ReplayState, Request, IO_SIZE,
        },
        crate::fuchsia::{
            file::FxFile,
            fxblob::{
                blob::FxBlob,
                testing::{new_blob_fixture, open_blob_fixture, BlobFixture},
                BlobDirectory,
            },
            node::FxNode,
            pager::PagerBacked,
            testing::{open_file_checked, TestFixture, TestFixtureOptions},
        },
        anyhow::Error,
        async_trait::async_trait,
        delivery_blob::CompressionMode,
        event_listener::{Event, EventListener},
        fidl_fuchsia_io::OpenFlags,
        fuchsia_async as fasync,
        fuchsia_hash::Hash,
        fxfs::{
            object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
            object_store::{
                transaction::{lock_keys, LockKey, Options},
                ObjectDescriptor,
            },
        },
        std::{
            collections::BTreeMap,
            mem::size_of,
            sync::{Arc, Mutex},
            time::Duration,
        },
        storage_device::{
            buffer::{BufferRef, MutableBufferRef},
            buffer_allocator::{BufferAllocator, BufferFuture, BufferSource},
        },
    };

    struct FakeReaderWriterInner {
        data: Vec<u8>,
        delays: Vec<EventListener>,
    }

    struct FakeReaderWriter {
        allocator: BufferAllocator,
        inner: Arc<Mutex<FakeReaderWriterInner>>,
    }

    const BLOCK_SIZE: usize = 4096;

    impl FakeReaderWriter {
        fn new() -> Self {
            Self {
                allocator: BufferAllocator::new(BLOCK_SIZE, BufferSource::new(IO_SIZE * 2)),
                inner: Arc::new(Mutex::new(FakeReaderWriterInner {
                    data: Vec::new(),
                    delays: Vec::new(),
                })),
            }
        }

        fn push_delay(&self, delay: EventListener) {
            self.inner.lock().unwrap().delays.insert(0, delay);
        }
    }

    impl ObjectHandle for FakeReaderWriter {
        fn object_id(&self) -> u64 {
            0
        }

        fn block_size(&self) -> u64 {
            self.allocator.block_size() as u64
        }

        fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
            self.allocator.allocate_buffer(size)
        }
    }

    impl WriteObjectHandle for FakeReaderWriter {
        async fn write_or_append(
            &self,
            offset: Option<u64>,
            buf: BufferRef<'_>,
        ) -> Result<u64, Error> {
            // We only append for now.
            assert!(offset.is_none());
            let delay = self.inner.lock().unwrap().delays.pop();
            if let Some(delay) = delay {
                delay.await;
            }
            // This relocking has a TOCTOU flavour, but it shouldn't matter for this application.
            self.inner.lock().unwrap().data.extend_from_slice(buf.as_slice());
            Ok(buf.len() as u64)
        }

        async fn truncate(&self, _size: u64) -> Result<(), Error> {
            unreachable!();
        }

        async fn flush(&self) -> Result<(), Error> {
            unreachable!();
        }
    }

    async fn write_file(fixture: &TestFixture, name: &str, data: &[u8]) -> u64 {
        let root_dir = fixture.volume().root_dir();
        let mut transaction = fixture
            .volume()
            .volume()
            .store()
            .filesystem()
            .new_transaction(
                lock_keys![LockKey::object(
                    fixture.volume().volume().store().store_object_id(),
                    root_dir.object_id()
                )],
                Options::default(),
            )
            .await
            .expect("Creating transaction for new file");
        let id = root_dir
            .directory()
            .create_child_file(&mut transaction, name, None)
            .await
            .expect("Creating new_file")
            .object_id();
        transaction.commit().await.unwrap();
        let file = open_file_checked(
            fixture.root(),
            OpenFlags::RIGHT_WRITABLE | OpenFlags::RIGHT_READABLE | OpenFlags::NOT_DIRECTORY,
            name,
        )
        .await;
        file.write(data).await.unwrap().expect("Writing file");
        id
    }

    #[async_trait]
    impl ReadObjectHandle for FakeReaderWriter {
        async fn read(&self, offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
            let delay = self.inner.lock().unwrap().delays.pop();
            if let Some(delay) = delay {
                delay.await;
            }
            // This relocking has a TOCTOU flavour, but it shouldn't matter for this application.
            let inner = self.inner.lock().unwrap();
            assert!(offset as usize <= inner.data.len());
            let offset_end = std::cmp::min(offset as usize + buf.len(), inner.data.len());
            let size = offset_end - offset as usize;
            buf.as_mut_slice()[..size].clone_from_slice(&inner.data[offset as usize..offset_end]);
            Ok(size)
        }

        fn get_size(&self) -> u64 {
            self.inner.lock().unwrap().data.len() as u64
        }
    }

    #[fuchsia::test(threads = 10)]
    async fn test_encode_decode_blob() {
        let mut buf = [0u8; size_of::<BlobMessage>()];
        let m = BlobMessage { id: [88u8; 32].into(), offset: 77 };
        m.encode_to(&mut buf.as_mut_slice());
        let m2 = BlobMessage::decode_from(&buf);
        assert_eq!(m, m2);
    }

    #[fuchsia::test(threads = 10)]
    async fn test_encode_decode_file() {
        let mut buf = [0u8; size_of::<FileMessage>()];
        let m = FileMessage { id: 88, offset: 77 };
        m.encode_to(&mut buf.as_mut_slice());
        let m2 = FileMessage::decode_from(&buf);
        assert!(!m2.is_zeroes());
        assert_eq!(m, m2);
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_basic_blob() {
        let fixture = new_blob_fixture().await;
        {
            let hash = fixture.write_blob(&[88u8], CompressionMode::Never).await;
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");

            let mut state = new_profile_state(true);
            let handle = Box::new(FakeReaderWriter::new());
            let inner = handle.inner.clone();
            assert_eq!(inner.lock().unwrap().data.len(), 0);
            {
                // Drop recorder when finished writing to flush data.
                let mut recorder = state.record_new(handle);
                recorder.record(blob.clone(), 0).unwrap();
                recorder.record_open(blob).unwrap();
            }
            state.stop_profiler();
            assert_eq!(inner.lock().unwrap().data.len(), BLOCK_SIZE);
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_basic_file() {
        let fixture = TestFixture::new().await;
        {
            let id = write_file(&fixture, "foo", &[88u8]).await;
            let node = fixture
                .volume()
                .volume()
                .get_or_load_node(id, ObjectDescriptor::File, Some(fixture.volume().root_dir()))
                .await
                .unwrap();

            let mut state = new_profile_state(false);
            let handle = Box::new(FakeReaderWriter::new());
            let inner = handle.inner.clone();
            assert_eq!(inner.lock().unwrap().data.len(), 0);
            {
                // Drop recorder when finished writing to flush data.
                let mut recorder = state.record_new(handle);
                recorder.record(node.clone(), 0).unwrap();
                recorder.record_open(node).unwrap();
            }
            state.stop_profiler();
            assert_eq!(inner.lock().unwrap().data.len(), BLOCK_SIZE);
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_filtered_without_open() {
        let fixture = new_blob_fixture().await;
        {
            let hash = fixture.write_blob(&[88u8], CompressionMode::Never).await;
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");

            let mut state = new_profile_state(true);
            let handle = Box::new(FakeReaderWriter::new());
            let inner = handle.inner.clone();
            assert_eq!(inner.lock().unwrap().data.len(), 0);
            {
                // Drop recorder when finished writing to flush data.
                let mut recorder = state.record_new(handle);
                recorder.record(blob.clone(), 0).unwrap();
            }
            state.stop_profiler();
            assert_eq!(inner.lock().unwrap().data.len(), 0);
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_blob_more_than_block() {
        let mut state = new_profile_state(true);

        let handle = Box::new(FakeReaderWriter::new());
        let inner = handle.inner.clone();
        let fixture = new_blob_fixture().await;
        assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
        let message_count = (fixture.fs().block_size() as usize / size_of::<BlobMessage>()) + 1;
        assert_eq!(inner.lock().unwrap().data.len(), 0);
        let hash;
        {
            hash = fixture.write_blob(&[88u8], CompressionMode::Never).await;
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
            // Drop recorder when finished writing to flush data.
            let mut recorder = state.record_new(handle);
            recorder.record_open(blob.clone()).unwrap();
            for i in 0..message_count {
                recorder.record(blob.clone(), 4096 * i as u64).unwrap();
            }
        }
        state.stop_profiler();
        {
            let data = &inner.lock().unwrap().data;
            assert_eq!(data.len(), BLOCK_SIZE * 2);
        }

        let mut result = Box::new(FakeReaderWriter::new());
        result.inner = inner.clone();
        let mut local_cache: BTreeMap<Hash, Arc<FxBlob>> = BTreeMap::new();
        let (sender, receiver) = async_channel::unbounded::<Request<FxBlob>>();

        let volume = fixture.volume().volume().clone();
        let task = fasync::Task::spawn(async move {
            ReplayState::<BlobVolume>::read_and_queue(result, volume, &sender, &mut local_cache)
                .await
                .unwrap();
        });

        let mut recv_count = 0;
        while let Ok(msg) = receiver.recv().await {
            assert_eq!(msg.file.root(), hash);
            assert_eq!(msg.offset, 4096 * recv_count);
            recv_count += 1;
        }
        task.await;
        assert_eq!(recv_count, message_count as u64);

        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_file_more_than_block() {
        let mut state = new_profile_state(false);

        let handle = Box::new(FakeReaderWriter::new());
        let inner = handle.inner.clone();
        let fixture = TestFixture::new().await;
        assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
        let message_count = (fixture.fs().block_size() as usize / size_of::<FileMessage>()) + 1;
        assert_eq!(inner.lock().unwrap().data.len(), 0);
        let id;
        {
            id = write_file(&fixture, "foo", &[88u8]).await;
            let node = fixture
                .volume()
                .volume()
                .get_or_load_node(id, ObjectDescriptor::File, Some(fixture.volume().root_dir()))
                .await
                .unwrap();
            // Drop recorder when finished writing to flush data.
            let mut recorder = state.record_new(handle);
            recorder.record_open(node.clone()).unwrap();
            for i in 0..message_count {
                recorder.record(node.clone(), 4096 * i as u64).unwrap();
            }
        }
        state.stop_profiler();
        {
            let data = &inner.lock().unwrap().data;
            assert_eq!(data.len(), BLOCK_SIZE * 2);
        }

        let mut result = Box::new(FakeReaderWriter::new());
        result.inner = inner.clone();
        let mut local_cache: BTreeMap<u64, Arc<FxFile>> = BTreeMap::new();
        let (sender, receiver) = async_channel::unbounded::<Request<FxFile>>();

        let volume = fixture.volume().volume().clone();
        let task = fasync::Task::spawn(async move {
            ReplayState::<FileVolume>::read_and_queue(result, volume, &sender, &mut local_cache)
                .await
                .unwrap();
        });

        let mut recv_count = 0;
        while let Ok(msg) = receiver.recv().await {
            assert_eq!(msg.file.object_id(), id);
            assert_eq!(msg.offset, 4096 * recv_count);
            recv_count += 1;
        }
        task.await;
        assert_eq!(recv_count, message_count as u64);

        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_more_than_io_size() {
        let mut state = new_profile_state(true);

        let handle = Box::new(FakeReaderWriter::new());
        let inner = handle.inner.clone();
        let fixture = new_blob_fixture().await;
        let message_count = (IO_SIZE as usize / size_of::<BlobMessage>()) + 1;
        assert_eq!(inner.lock().unwrap().data.len(), 0);
        let hash;
        {
            hash = fixture.write_blob(&[88u8], CompressionMode::Never).await;
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
            // Drop recorder when finished writing to flush data.
            let mut recorder = state.record_new(handle);
            recorder.record_open(blob.clone()).unwrap();
            for i in 0..message_count {
                recorder.record(blob.clone(), 4096 * i as u64).unwrap();
            }
        }
        state.stop_profiler();
        {
            let data = &inner.lock().unwrap().data;
            assert_eq!(data.len(), IO_SIZE + BLOCK_SIZE);
        }

        let mut result = Box::new(FakeReaderWriter::new());
        result.inner = inner.clone();
        let mut local_cache: BTreeMap<Hash, Arc<FxBlob>> = BTreeMap::new();
        let (sender, receiver) = async_channel::unbounded::<Request<FxBlob>>();

        let volume = fixture.volume().volume().clone();
        let task = fasync::Task::spawn(async move {
            ReplayState::<BlobVolume>::read_and_queue(result, volume, &sender, &mut local_cache)
                .await
                .unwrap();
        });

        let mut recv_count = 0;
        while let Ok(msg) = receiver.recv().await {
            assert_eq!(msg.file.root(), hash);
            assert_eq!(msg.offset, 4096 * recv_count);
            recv_count += 1;
        }
        task.await;
        assert_eq!(recv_count, message_count as u64);

        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_replay_profile_blob() {
        // Create all the files that we need first, then restart the filesystem to clear cache.
        let mut state = new_profile_state(true);
        let handle = Box::new(FakeReaderWriter::new());
        let data_original = handle.inner.clone();

        let mut hashes = Vec::new();

        let fixture = new_blob_fixture().await;
        {
            assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
            let message_count = (fixture.fs().block_size() as usize / size_of::<BlobMessage>()) + 1;

            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let mut recorder = state.record_new(handle);
            // Page in the zero offsets only to avoid readahead strangeness.
            for i in 0..message_count {
                let hash =
                    fixture.write_blob(i.to_le_bytes().as_slice(), CompressionMode::Never).await;
                let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
                recorder.record_open(blob.clone()).unwrap();
                hashes.push(hash);
                recorder.record(blob.clone(), 0).unwrap();
            }
        };
        let device = fixture.close().await;
        device.ensure_unique();
        state.stop_profiler();

        device.reopen(false);
        let fixture = open_blob_fixture(device).await;
        {
            // Need to get the root vmo to check committed bytes.
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            // Ensure that nothing is paged in right now.
            for hash in &hashes {
                let blob = dir.lookup_blob(*hash).await.expect("Opening blob");
                assert_eq!(blob.vmo().info().unwrap().committed_bytes, 0);
            }

            let mut handle = Box::new(FakeReaderWriter::new());
            // New handle with the recorded data gets replayed on the volume.
            handle.inner = data_original;
            state.replay_profile(handle, fixture.volume().volume().clone());

            // Await all data being played back by checking that things have paged in.
            for hash in &hashes {
                let blob = dir.lookup_blob(*hash).await.expect("Opening blob");
                while blob.vmo().info().unwrap().committed_bytes == 0 {
                    fasync::Timer::new(Duration::from_millis(25)).await;
                }
                // The replay task shouldn't increment the blob's open count. This ensures that we
                // can still delete blobs while we are replaying a profile.
                assert!(blob.mark_to_be_purged());
            }
            state.stop_profiler();
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_replay_profile_file() {
        // Create all the files that we need first, then restart the filesystem to clear cache.
        let mut state = new_profile_state(false);
        let handle = Box::new(FakeReaderWriter::new());
        let data_original = handle.inner.clone();

        let mut ids = Vec::new();

        let fixture = TestFixture::new().await;
        {
            assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
            let message_count = (fixture.fs().block_size() as usize / size_of::<FileMessage>()) + 1;

            let mut recorder = state.record_new(handle);
            // Page in the zero offsets only to avoid readahead strangeness.
            for i in 0..message_count {
                let id = write_file(&fixture, &i.to_string(), &[88u8]).await;
                let node = fixture
                    .volume()
                    .volume()
                    .get_or_load_node(id, ObjectDescriptor::File, Some(fixture.volume().root_dir()))
                    .await
                    .unwrap();
                recorder.record_open(node.clone()).unwrap();
                ids.push(id);
                recorder.record(node.clone(), 0).unwrap();
            }
        };
        let device = fixture.close().await;
        device.ensure_unique();
        state.stop_profiler();

        device.reopen(false);
        let fixture = TestFixture::open(
            device,
            TestFixtureOptions {
                encrypted: true,
                as_blob: false,
                format: false,
                serve_volume: false,
            },
        )
        .await;
        {
            // Ensure that nothing is paged in right now.
            for id in &ids {
                let file = fixture
                    .volume()
                    .volume()
                    .get_or_load_node(
                        *id,
                        ObjectDescriptor::File,
                        Some(fixture.volume().root_dir()),
                    )
                    .await
                    .unwrap()
                    .into_any()
                    .downcast::<FxFile>()
                    .unwrap();
                assert_eq!(file.vmo().info().unwrap().committed_bytes, 0);
            }

            let mut handle = Box::new(FakeReaderWriter::new());
            // New handle with the recorded data gets replayed on the volume.
            handle.inner = data_original;
            state.replay_profile(handle, fixture.volume().volume().clone());

            // Await all data being played back by checking that things have paged in.
            for id in &ids {
                let file = fixture
                    .volume()
                    .volume()
                    .get_or_load_node(
                        *id,
                        ObjectDescriptor::File,
                        Some(fixture.volume().root_dir()),
                    )
                    .await
                    .unwrap()
                    .into_any()
                    .downcast::<FxFile>()
                    .unwrap();
                while file.vmo().info().unwrap().committed_bytes == 0 {
                    fasync::Timer::new(Duration::from_millis(25)).await;
                }
                // The replay task shouldn't increment the file's open count. This ensures that we
                // can still delete blobs while we are replaying a profile.
                assert!(file.mark_to_be_purged());
            }
            state.stop_profiler();
        }
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_during_replay() {
        let mut state = new_profile_state(true);
        let handle = Box::new(FakeReaderWriter::new());
        let data_original = handle.inner.clone();

        let hash;
        let fixture = new_blob_fixture().await;
        // First make a simple recording.
        {
            let mut recorder = state.record_new(handle);
            hash = fixture.write_blob(&[0, 1, 2, 3], CompressionMode::Never).await;
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
            recorder.record_open(blob.clone()).unwrap();
            recorder.record(blob.clone(), 0).unwrap();
        };
        let device = fixture.close().await;
        device.ensure_unique();
        state.stop_profiler();
        assert_ne!(data_original.lock().unwrap().data.len(), 0);

        let recording_handle = Box::new(FakeReaderWriter::new());
        let data_second = recording_handle.inner.clone();
        device.reopen(false);
        let fixture = open_blob_fixture(device).await;
        {
            // Need to get the root vmo to check committed bytes.
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            // Ensure that nothing is paged in right now.
            let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
            assert_eq!(blob.vmo().info().unwrap().committed_bytes, 0);

            // Start recording
            let mut recorder = state.record_new(recording_handle);
            recorder.record(blob.clone(), 4096).unwrap();

            // Replay the original recording.
            let mut replaying_handle = Box::new(FakeReaderWriter::new());
            replaying_handle.inner = data_original.clone();
            state.replay_profile(replaying_handle, fixture.volume().volume().clone());

            // Await all data being played back by checking that things have paged in.
            {
                let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
                while blob.vmo().info().unwrap().committed_bytes == 0 {
                    fasync::Timer::new(Duration::from_millis(25)).await;
                }
            }

            // Record the open after the replay. Needs both the before and after action to
            // capture anything ensuring that the two procedures overlapped.
            recorder.record_open(blob.clone()).unwrap();
        }
        state.stop_profiler();

        assert_ne!(data_original.lock().unwrap().data.len(), 0);
        assert_ne!(data_second.lock().unwrap().data.len(), 0);
        assert_ne!(data_second.lock().unwrap().data, data_original.lock().unwrap().data);
        fixture.close().await;
    }

    // Doesn't ensure that anything reads back properly, just that everything shuts down when
    // stopped early.
    #[fuchsia::test(threads = 10)]
    async fn test_replay_profile_stop_early() {
        // Create all the files that we need first.
        let mut state = new_profile_state(true);
        let fixture = new_blob_fixture().await;
        {
            let recording_handle = Box::new(FakeReaderWriter::new());
            let inner = recording_handle.inner.clone();
            let dir = fixture
                .volume()
                .root()
                .clone()
                .into_any()
                .downcast::<BlobDirectory>()
                .expect("Root should be BlobDirectory");
            // Queue up more than 2 pages. Doesn't matter that they're all the same.
            {
                let hash = fixture.write_blob(&[0, 1, 2, 3], CompressionMode::Never).await;
                let blob = dir.lookup_blob((*hash).into()).await.expect("Opening blob");
                let mut recorder = state.record_new(recording_handle);
                recorder.record_open(blob.clone()).unwrap();
                assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
                let message_count =
                    (fixture.fs().block_size() as usize / size_of::<BlobMessage>()) * 2 + 1;
                for _ in 0..message_count {
                    recorder.record(blob.clone(), 0).unwrap();
                }
            }
            state.stop_profiler();

            let mut replay_handle = Box::new(FakeReaderWriter::new());
            replay_handle.inner = inner.clone();
            let delay1 = Event::new();
            replay_handle.push_delay(delay1.listen());
            let delay2 = Event::new();
            replay_handle.push_delay(delay2.listen());

            // Don't let delay1 hold us up. Only wait on the second read.
            delay1.notify(usize::MAX);

            state.replay_profile(replay_handle, fixture.volume().volume().clone());

            // Let it make some small amount of progress.
            fasync::Timer::new(Duration::from_millis(5)).await;

            fasync::Task::spawn(async move {
                // Let the profiler wait on this a little.
                fasync::Timer::new(Duration::from_millis(5)).await;
                delay2.notify(usize::MAX);
            })
            .detach();

            // Should block on the second read for a bit, then cleanly shutdown.
            state.stop_profiler();
        }

        fixture.close().await;
    }
}
