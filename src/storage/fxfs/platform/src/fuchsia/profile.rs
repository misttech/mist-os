// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        fxblob::{blob::FxBlob, BlobDirectory},
        pager::PagerBacked,
        volume::FxVolume,
    },
    anyhow::Error,
    arrayref::array_refs,
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
    std::{
        collections::btree_map::{BTreeMap, Entry},
        mem::size_of,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    },
};

const REPLAY_THREADS: usize = 2;

/// Paired with a ProfileState that is currently recording, takes messages to be written into the
/// current profile. This should be dropped before the recording is stopped to ensure that all
/// messages have been flushed to the writer.
pub struct Recorder {
    sender: async_channel::Sender<Vec<u8>>,
    buffer: Vec<u8>,
    offset: usize,
}

struct Message {
    hash: Hash,
    // Don't bother with offset+length. The kernel is going split up and align it one way and then
    // we're going to change it all with read-ahead/read-around.
    offset: u64,
}

impl Message {
    fn encode_to(hash: &Hash, offset: u64, dest: &mut [u8]) {
        dest[..size_of::<Hash>()].clone_from_slice(hash.as_bytes());
        dest[size_of::<Hash>()..].clone_from_slice(offset.to_le_bytes().as_ref());
    }

    fn decode_from(src: &[u8; size_of::<Message>()]) -> Self {
        let (first, second) = array_refs!(src, size_of::<Hash>(), size_of::<u64>());
        Self { hash: Hash::from_array(*first), offset: u64::from_le_bytes(*second) }
    }

    fn is_zeroes(&self) -> bool {
        self.hash == Hash::from_array([0u8; size_of::<Hash>()]) && self.offset == 0
    }
}

impl Recorder {
    /// Record a blob page in request, for the given hash and offset.
    pub fn record(&mut self, hash: Hash, offset: u64) -> Result<(), Error> {
        // Encode `Message` into the buffer.
        let offset_next = self.offset + size_of::<Message>();
        Message::encode_to(
            &hash,
            offset,
            &mut self.buffer.as_mut_slice()[self.offset..offset_next],
        );
        self.offset = offset_next;

        if self.offset + size_of::<Message>() > self.buffer.len() {
            // try_send to avoid async await, we use an unbounded channel anyways.
            let len = self.buffer.len();
            self.sender.try_send(std::mem::replace(&mut self.buffer, vec![0u8; len]))?;
            self.offset = 0;
        }
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Recorder should be dropped to flush entries first, if the channel is closed then we've
        // already shut down the receiver.
        debug_assert!(!self.sender.is_closed());
        // Best effort sending what messages have already been queued.
        if self.offset > 0 {
            let buffer = std::mem::take(&mut self.buffer);
            let _ = self.sender.try_send(buffer);
        }
    }
}

struct Request {
    file: Arc<FxBlob>,
    offset: u64,
}

struct RecordingState {
    stopped_listener: EventListener,
    sender_clone: async_channel::Sender<Vec<u8>>,
}

struct ReplayState {
    // Using a DropEvent to trigger Take shutdown if this goes out of scope.
    stop_on_drop: DropEvent,
    stopped_listener: EventListener,
}

enum State {
    None,
    Recording(RecordingState),
    Replaying(ReplayState),
}

/// Holds the current profile recording or replay state, and provides methods for state transitions.
pub struct ProfileState(State);

impl ProfileState {
    pub fn new() -> Self {
        Self(State::None)
    }

    /// Returns true if currently recording or replaying.
    pub fn busy(&self) -> bool {
        !matches!(self.0, State::None)
    }

    /// Creates a new recording and returns the `Recorder` object to record to. The recording starts
    /// shutdown when the associated `Recorder` is dropped, and shutdown is completed when
    /// `stop_profiler()` is called.
    pub fn record_new<T: WriteObjectHandle>(&mut self, handle: T) -> Recorder {
        self.stop_profiler();
        let finished_event = DropEvent::new();
        let (sender, receiver) = async_channel::unbounded::<Vec<u8>>();
        let sender_clone = sender.clone();
        let stopped_listener = (*finished_event).listen();
        let buffer = vec![0u8; handle.block_size() as usize];
        fasync::Task::spawn(async move {
            // DropEvent should fire when this task is done.
            let _finished_event = finished_event;
            while let Ok(buffer) = receiver.recv().await {
                let mut io_buf = handle.allocate_buffer(buffer.len()).await;
                // While I'd prefer to avoid this memcpy, making the io Buffer `Send` was going to
                // very messy.
                io_buf.as_mut_slice().clone_from_slice(buffer.as_slice());
                if let Err(e) = handle.write_or_append(None, io_buf.as_ref()).await {
                    error!("Failed to write profile block: {:?}", e);
                    break;
                }
            }
        })
        .detach();

        self.0 = State::Recording(RecordingState { stopped_listener, sender_clone });

        Recorder { sender, buffer, offset: 0 }
    }

    /// Stop in-flight recording and replaying. This method will block until the resources have been
    /// cleaned up.
    pub fn stop_profiler(&mut self) {
        let old = std::mem::replace(&mut self.0, State::None);
        match old {
            State::Recording(recording) => {
                // This closes the channel for *all* senders, which starts shutting down.
                recording.sender_clone.close();
                recording.stopped_listener.wait();
            }
            State::Replaying(replaying) => {
                let ReplayState { stop_on_drop, stopped_listener } = replaying;
                std::mem::drop(stop_on_drop);
                stopped_listener.wait();
            }
            State::None => {}
        }
    }

    fn page_in_thread(queue: async_channel::Receiver<Request>, stop_processing: Arc<AtomicBool>) {
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

    async fn read_and_queue<T: ReadObjectHandle>(
        handle: T,
        volume: Arc<FxVolume>,
        sender: &async_channel::Sender<Request>,
        local_cache: &mut BTreeMap<Hash, Arc<FxBlob>>,
    ) -> Result<(), Error> {
        let root_dir = volume
            .get_or_load_node(
                volume.store().root_directory_object_id(),
                ObjectDescriptor::Directory,
                None,
            )
            .await?
            .into_any()
            .downcast::<BlobDirectory>()
            .map_err(|_| FxfsError::Inconsistent)?;
        let mut io_buf = handle.allocate_buffer(handle.block_size() as usize).await;
        let file_size = handle.get_size() as usize;
        let mut offset = 0;
        while offset < file_size {
            let actual = handle
                .read(offset as u64, io_buf.as_mut())
                .await
                .map_err(|e| e.context(format!("Failed to read at offset: {}", offset)))?;
            if actual != io_buf.len() {
                // This is unexpected due to how it's written, but recoverable.
                warn!("Partial profile read at {} of length {}", offset, actual);
            }
            offset += actual;
            let mut local_offset = 0;
            for next_offset in std::ops::RangeInclusive::new(size_of::<Message>(), actual)
                .step_by(size_of::<Message>())
            {
                let msg = Message::decode_from(
                    io_buf.as_slice()[local_offset..next_offset].try_into().unwrap(),
                );
                local_offset = next_offset;

                // Ignore trailing zeroes. This is technically a valid entry but extremely unlikely
                // and will only break an optimization.
                if msg.is_zeroes() {
                    break;
                }

                let file = match local_cache.entry(msg.hash) {
                    Entry::Occupied(entry) => entry.get().clone(),
                    Entry::Vacant(entry) => match root_dir.lookup_blob(msg.hash).await {
                        Err(e) => {
                            warn!("Failed to open object {} from profile: {:?}", msg.hash, e);
                            continue;
                        }
                        Ok(file) => {
                            entry.insert(file.clone());
                            file
                        }
                    },
                };

                sender.send(Request { file, offset: msg.offset }).await.unwrap();
            }
        }
        Ok(())
    }

    /// Reads given handle to parse a profile and replay it by requesting pages via
    /// ZX_VMO_OP_COMMIT in blocking background threads.
    pub fn replay_profile<T: ReadObjectHandle>(&mut self, handle: T, volume: Arc<FxVolume>) {
        self.stop_profiler();
        let stop_on_drop = DropEvent::new();
        let (sender, receiver) = async_channel::unbounded::<Request>();
        let shutting_down = Arc::new(AtomicBool::new(false));

        // Create (bounded) async_channel, async thread reads and populates the channel, then N threads
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
            let mut local_cache: BTreeMap<Hash, Arc<FxBlob>> = BTreeMap::new();
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

        self.0 = State::Replaying(ReplayState { stop_on_drop, stopped_listener });
    }
}

impl Drop for ProfileState {
    fn drop(&mut self) {
        self.stop_profiler();
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Message, ProfileState, Request},
        crate::fuchsia::{
            fxblob::{
                blob::FxBlob,
                testing::{self as testing, BlobFixture},
                BlobDirectory,
            },
            pager::PagerBacked,
        },
        anyhow::Error,
        async_trait::async_trait,
        delivery_blob::CompressionMode,
        event_listener::{Event, EventListener},
        fuchsia_async as fasync,
        fuchsia_hash::Hash,
        fxfs::object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
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
                allocator: BufferAllocator::new(BLOCK_SIZE, BufferSource::new(BLOCK_SIZE * 8)),
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
    async fn test_recording_basic() {
        let mut state = ProfileState::new();

        let handle = FakeReaderWriter::new();
        let inner = handle.inner.clone();
        assert_eq!(inner.lock().unwrap().data.len(), 0);
        {
            // Drop recorder when finished writing to flush data.
            let mut recorder = state.record_new(handle);
            recorder.record(Hash::from_array([88u8; size_of::<Hash>()]), 0).unwrap();
        }
        state.stop_profiler();
        assert_eq!(inner.lock().unwrap().data.len(), BLOCK_SIZE);
    }

    #[fuchsia::test(threads = 10)]
    async fn test_recording_more_than_block() {
        let mut state = ProfileState::new();

        let handle = FakeReaderWriter::new();
        let inner = handle.inner.clone();
        let fixture = testing::new_blob_fixture().await;
        assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
        let message_count = (fixture.fs().block_size() as usize / size_of::<Message>()) + 1;
        assert_eq!(inner.lock().unwrap().data.len(), 0);
        let hash;
        {
            hash = fixture.write_blob(&[88u8], CompressionMode::Never).await;
            // Drop recorder when finished writing to flush data.
            let mut recorder = state.record_new(handle);
            for _ in 0..message_count {
                recorder.record(hash, 4096).unwrap();
            }
        }
        state.stop_profiler();
        {
            let data = &inner.lock().unwrap().data;
            assert_eq!(data.len(), BLOCK_SIZE * 2);
        }

        let mut result = FakeReaderWriter::new();
        result.inner = inner.clone();
        let mut local_cache: BTreeMap<Hash, Arc<FxBlob>> = BTreeMap::new();
        let (sender, receiver) = async_channel::unbounded::<Request>();

        let volume = fixture.volume().volume().clone();
        let task = fasync::Task::spawn(async move {
            ProfileState::read_and_queue(result, volume, &sender, &mut local_cache).await.unwrap();
        });

        let mut recv_count = 0;
        while let Ok(msg) = receiver.recv().await {
            recv_count += 1;
            assert_eq!(msg.file.root(), hash);
            assert_eq!(msg.offset, 4096);
        }
        task.await;
        assert_eq!(recv_count, message_count);

        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_replay_profile() {
        // Create all the files that we need first, then restart the filesystem to clear cache.
        let mut state = ProfileState::new();
        let handle = FakeReaderWriter::new();
        let data_original = handle.inner.clone();

        let mut hashes = Vec::new();

        let device = {
            let fixture = testing::new_blob_fixture().await;
            assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
            let message_count = (fixture.fs().block_size() as usize / size_of::<Message>()) + 1;

            let mut recorder = state.record_new(handle);
            // Page in the zero offsets only to avoid readahead strangeness.
            for i in 0..message_count {
                let hash =
                    fixture.write_blob(i.to_le_bytes().as_slice(), CompressionMode::Never).await;
                hashes.push(hash);
                recorder.record(hash, 0).unwrap();
            }
            fixture.close().await
        };
        device.ensure_unique();
        state.stop_profiler();

        device.reopen(false);
        let fixture = testing::open_blob_fixture(device).await;
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

            let mut handle = FakeReaderWriter::new();
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

    // Doesn't ensure that anything reads back properly, just that everything shuts down when
    // stopped early.
    #[fuchsia::test(threads = 10)]
    async fn test_replay_profile_stop_early() {
        // Create all the files that we need first, then restart the filesystem to clear cache.
        let mut state = ProfileState::new();

        let fixture = testing::new_blob_fixture().await;
        let recording_handle = FakeReaderWriter::new();
        let inner = recording_handle.inner.clone();
        // Queue up more than 2 pages. Doesn't matter that they're all the same.
        {
            let hash = fixture.write_blob(&[0, 1, 2, 3], CompressionMode::Never).await;
            let mut recorder = state.record_new(recording_handle);
            assert_eq!(BLOCK_SIZE as u64, fixture.fs().block_size());
            let message_count = (fixture.fs().block_size() as usize / size_of::<Message>()) * 2 + 1;
            for _ in 0..message_count {
                recorder.record(hash, 0).unwrap();
            }
        }
        state.stop_profiler();

        let mut replay_handle = FakeReaderWriter::new();
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

        fixture.close().await;
    }
}
