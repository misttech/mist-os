// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_handle::{ObjectHandle, ReadObjectHandle},
    anyhow::Error,
    event_listener::{Event, EventListener},
    std::{cell::UnsafeCell, sync::RwLock, vec::Vec},
    storage_device::buffer::BufferFuture,
};

const CHUNK_SIZE: usize = 128 * 1024;

fn block_aligned_size(source: &impl ReadObjectHandle) -> usize {
    let block_size = source.block_size() as usize;
    let source_size = source.get_size() as usize;
    source_size.checked_next_multiple_of(block_size).unwrap()
}

/// An `ObjectHandle` that caches the data that it reads in from a `ReadObjectHandle`.
pub struct CachingObjectHandle<S> {
    source: S,

    // TODO(https://fxbug.dev/294080669): Add a way to purge chunks that haven't been recently used when
    // under memory pressure.
    buffer: UnsafeCell<Vec<u8>>,

    chunk_states: RwLock<Vec<ChunkState>>,

    // Reads that are waiting on another read to load a chunk will wait on this event. There's only
    // 1 event for all chunks so a read may receive a notification for a different chunk than the
    // one it's waiting for and need to re-wait. It's rare for multiple chunks to be loading at once
    // and even rarer for a read to be waiting on a chunk to be loaded while another chunk is also
    // loading.
    event: Event,
}

// SAFETY: Only `buffer` isn't Sync. Access to `buffer` is synchronized with `chunk_states`.
unsafe impl<S> Sync for CachingObjectHandle<S> {}

impl<S: ReadObjectHandle> CachingObjectHandle<S> {
    pub fn new(source: S) -> Self {
        let block_size = source.block_size() as usize;
        assert!(CHUNK_SIZE % block_size == 0);
        let aligned_size = block_aligned_size(&source);
        let chunk_count = aligned_size.div_ceil(CHUNK_SIZE);

        Self {
            source,
            buffer: UnsafeCell::new(vec![0; aligned_size]),
            chunk_states: RwLock::new(vec![ChunkState::Missing; chunk_count]),
            event: Event::new(),
        }
    }

    /// Reads in the required data from `source` and caches it. A slice into the cached data is
    /// returned. The slice will be shorter than `length` when reading beyond the end of `source`.
    pub async fn read(&self, offset: usize, length: usize) -> Result<&[u8], Error> {
        let source_size = self.source.get_size() as usize;
        if offset >= source_size {
            return Ok(&[]);
        }
        let read_end = std::cmp::min(offset + length, source_size);
        let first_chunk = offset / CHUNK_SIZE;
        let last_chunk = read_end.div_ceil(CHUNK_SIZE);

        for chunk in first_chunk..last_chunk {
            self.load_chunk(chunk).await?;
        }
        // SAFETY: All of the relevant chunks were loaded above which means that they are present
        // and won't be modified again.
        Ok(unsafe { &(&*self.buffer.get())[offset..read_end] })
    }

    async fn load_chunk(&self, chunk_num: usize) -> Result<(), Error> {
        enum Action {
            Wait(EventListener),
            Load,
        }
        loop {
            let action = {
                let chunk_states = self.chunk_states.read().unwrap();
                match chunk_states[chunk_num] {
                    ChunkState::Missing => Action::Load,
                    ChunkState::Present => {
                        return Ok(());
                    }
                    ChunkState::Pending => Action::Wait(self.event.listen()),
                }
            };
            match action {
                Action::Wait(listener) => listener.await,
                Action::Load => {
                    {
                        let mut chunk_states = self.chunk_states.write().unwrap();
                        if chunk_states[chunk_num] != ChunkState::Missing {
                            // If the state changed between dropping the read lock and acquiring the
                            // write lock then go back to the start.
                            continue;
                        }
                        chunk_states[chunk_num] = ChunkState::Pending;
                    }
                    // If this future is dropped or reading fails then put the chunk back into the
                    // `Missing` state.
                    let drop_guard = scopeguard::guard((), |_| {
                        {
                            let mut chunk_states = self.chunk_states.write().unwrap();
                            debug_assert!(chunk_states[chunk_num] == ChunkState::Pending);
                            chunk_states[chunk_num] = ChunkState::Missing;
                        }
                        self.event.notify(usize::MAX);
                    });

                    let read_start = chunk_num * CHUNK_SIZE;
                    let read_end =
                        std::cmp::min(read_start + CHUNK_SIZE, block_aligned_size(&self.source));
                    let mut read_buf = self.source.allocate_buffer(read_end - read_start).await;
                    let amount_read =
                        self.source.read(read_start as u64, read_buf.as_mut()).await?;

                    // SAFETY: This chunk is currently pending which prevents this chunk of the
                    // buffer from being read. The chunk_states lock ensures that only 1 future can
                    // put a chunk into the pending state at a time and that's the only future that
                    // can be modifying this chunk of the buffer.
                    unsafe {
                        let buf = &mut *self.buffer.get();
                        buf[read_start..(read_start + amount_read)]
                            .copy_from_slice(&read_buf.as_slice()[..amount_read]);
                        buf[(read_start + amount_read)..read_end].fill(0);
                    };

                    {
                        let mut chunk_states = self.chunk_states.write().unwrap();
                        debug_assert!(chunk_states[chunk_num] == ChunkState::Pending);
                        chunk_states[chunk_num] = ChunkState::Present;
                    }
                    self.event.notify(usize::MAX);

                    scopeguard::ScopeGuard::into_inner(drop_guard);
                    return Ok(());
                }
            }
        }
    }
}

impl<S: ReadObjectHandle> ObjectHandle for CachingObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        self.source.set_trace(v);
    }

    fn object_id(&self) -> u64 {
        self.source.object_id()
    }

    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.source.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.source.block_size()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChunkState {
    Missing,
    Present,
    Pending,
}

#[cfg(test)]
mod tests {
    use {
        super::{CachingObjectHandle, CHUNK_SIZE},
        crate::object_handle::{ObjectHandle, ReadObjectHandle},
        anyhow::Error,
        async_trait::async_trait,
        event_listener::Event,
        std::sync::{
            atomic::{AtomicBool, AtomicU8, Ordering},
            Arc,
        },
        storage_device::{
            buffer::{BufferFuture, MutableBufferRef},
            fake_device::FakeDevice,
            Device,
        },
    };

    // Fills a buffer with a pattern seeded by counter.
    fn fill_buf(buf: &mut [u8], counter: u8) {
        for (i, chunk) in buf.chunks_exact_mut(2).enumerate() {
            chunk[0] = counter;
            chunk[1] = i as u8;
        }
    }

    // Returns a buffer filled with fill_buf.
    fn make_buf(counter: u8, size: usize) -> Vec<u8> {
        let mut buf = vec![0; size];
        fill_buf(&mut buf, counter);
        buf
    }

    struct FakeSource {
        device: Arc<dyn Device>,
        size: usize,
        started: AtomicBool,
        wake: Event,
        counter: AtomicU8,
    }

    impl FakeSource {
        // `device` is only used to provide allocate_buffer; reads don't go to the device.
        fn new(device: Arc<dyn Device>, size: usize) -> Self {
            FakeSource {
                started: AtomicBool::new(false),
                size,
                wake: Event::new(),
                device,
                counter: AtomicU8::new(1),
            }
        }

        fn start(&self) {
            self.started.store(true, Ordering::SeqCst);
            self.wake.notify(usize::MAX);
        }

        async fn wait_for_start(&self) {
            while !self.started.load(Ordering::SeqCst) {
                let listener = self.wake.listen();
                if self.started.load(Ordering::SeqCst) {
                    break;
                }
                listener.await;
            }
        }
    }

    #[async_trait]
    impl ReadObjectHandle for FakeSource {
        async fn read(&self, _offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
            let counter = self.counter.fetch_add(1, Ordering::Relaxed);
            self.wait_for_start().await;
            fill_buf(buf.as_mut_slice(), counter);
            Ok(buf.len())
        }

        fn get_size(&self) -> u64 {
            self.size as u64
        }
    }

    impl ObjectHandle for FakeSource {
        fn object_id(&self) -> u64 {
            unreachable!();
        }

        fn block_size(&self) -> u64 {
            self.device.block_size().into()
        }

        fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
            self.device.allocate_buffer(size)
        }
    }

    #[fuchsia::test]
    async fn test_read_with_missing_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let slice = caching_object_handle.read(0, 4096).await.unwrap();
        assert_eq!(slice, make_buf(1, 4096));
    }

    #[fuchsia::test]
    async fn test_read_with_present_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let expected = make_buf(1, 4096);
        let slice = caching_object_handle.read(0, 4096).await.unwrap();
        assert_eq!(slice, expected);

        // The chunk was already populated so this read receives the same value as the above read.
        let slice = caching_object_handle.read(0, 4096).await.unwrap();
        assert_eq!(slice, expected);
    }

    #[fuchsia::test]
    async fn test_read_with_pending_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, 4096));
        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(0, 4096));

        // The first future will transition the chunk from `Missing` to `Pending` and then wait
        // on the source.
        assert!(futures::poll!(&mut read_fut1).is_pending());
        // The second future will wait on the event.
        assert!(futures::poll!(&mut read_fut2).is_pending());
        caching_object_handle.source.start();
        // Even though the source is ready the second future can't make progress.
        assert!(futures::poll!(&mut read_fut2).is_pending());
        // The first future reads from the source, transition the chunk from `Pending` to
        // `Present`, and then notifies the event.
        let expected = make_buf(1, 4096);
        assert_eq!(read_fut1.await.unwrap(), expected);
        // The event has been notified and the second future can now complete.
        assert_eq!(read_fut2.await.unwrap(), expected);
    }

    #[fuchsia::test]
    async fn test_read_with_notification_for_other_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, CHUNK_SIZE + 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, 4096));
        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(CHUNK_SIZE, 4096));
        let mut read_fut3 = std::pin::pin!(caching_object_handle.read(0, 4096));

        // The first and second futures will transition their chunks from `Missing` to `Pending`
        // and then wait on the source.
        assert!(futures::poll!(&mut read_fut1).is_pending());
        assert!(futures::poll!(&mut read_fut2).is_pending());
        // The third future will wait on the event.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        caching_object_handle.source.start();
        // Even though the source is ready the third future can't make progress.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        // The second future will read from the source and notify the event.
        assert_eq!(read_fut2.await.unwrap(), make_buf(2, 4096));
        // The event was notified but the first chunk is still `Pending` so the third future
        // resumes waiting.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        // The first future will read from the source, transition the first chunk to `Present`,
        // and notify the event.
        let expected = make_buf(1, 4096);
        assert_eq!(read_fut1.await.unwrap(), expected);
        // The first chunk is now present so the third future can complete.
        assert_eq!(read_fut3.await.unwrap(), expected);
    }

    #[fuchsia::test]
    async fn test_read_with_dropped_future() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(0, 4096));
        {
            let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, 4096));

            // The first future will transition the chunk from `Missing` to `Pending` and then
            // wait on the source.
            assert!(futures::poll!(&mut read_fut1).is_pending());
            // The second future will wait on the event.
            assert!(futures::poll!(&mut read_fut2).is_pending());
            caching_object_handle.source.start();
            // Even though the source is ready the second future can't make progress.
            assert!(futures::poll!(&mut read_fut2).is_pending());
        }
        // The first future was dropped which transitioned the chunk from `Pending` to `Missing`
        // and notified the event. When the second future is polled it transitions the chunk
        // from `Missing` back to `Pending`, reads from the source, and then transitions the
        // chunk to `Present`.
        assert_eq!(read_fut2.await.unwrap(), make_buf(2, 4096));
    }

    #[fuchsia::test]
    async fn test_read_past_end_of_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 300);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let slice = caching_object_handle.read(500, 10).await.unwrap();
        assert!(slice.is_empty());
    }

    #[fuchsia::test]
    async fn test_read_more_than_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        const SOURCE_SIZE: usize = 300;
        const READ_SIZE: usize = SOURCE_SIZE + 200;
        let source = FakeSource::new(device, SOURCE_SIZE);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let slice = caching_object_handle.read(0, READ_SIZE).await.unwrap();
        assert_eq!(slice, make_buf(1, SOURCE_SIZE));
    }

    #[fuchsia::test]
    async fn test_read_spanning_multiple_chunks() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        const SOURCE_SIZE: usize = CHUNK_SIZE + 10;
        let source = FakeSource::new(device, SOURCE_SIZE);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let slice = caching_object_handle.read(0, SOURCE_SIZE).await.unwrap();
        assert_eq!(slice[0..CHUNK_SIZE], make_buf(1, CHUNK_SIZE));
        assert_eq!(slice[CHUNK_SIZE..SOURCE_SIZE], make_buf(2, SOURCE_SIZE - CHUNK_SIZE));
    }

    #[fuchsia::test]
    async fn test_empty_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 0);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let slice = caching_object_handle.read(0, 10).await.unwrap();
        assert!(slice.is_empty());
    }
}
