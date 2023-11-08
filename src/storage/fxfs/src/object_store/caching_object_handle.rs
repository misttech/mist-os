// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        object_handle::{ObjectHandle, ReadObjectHandle},
        round::round_down,
    },
    anyhow::Error,
    async_trait::async_trait,
    event_listener::{Event, EventListener},
    std::{sync::RwLock, vec::Vec},
    storage_device::buffer::{Buffer, MutableBufferRef},
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
    inner: RwLock<Inner>,

    // Reads that are waiting on another read to load a chunk will wait on this event. There's only
    // 1 event for all chunks so a read may receive a notification for a different chunk than the
    // one it's waiting for and need to re-wait. It's rare for multiple chunks to be loading at once
    // and even rarer for a read to be waiting on a chunk to be loaded while another chunk is also
    // loading.
    event: Event,
}

struct Inner {
    // TODO(fxbug.dev/294080669): Add a way to purge chunks that haven't been recently used when
    // under memory pressure.
    buffer: Vec<u8>,
    chunk_states: Vec<ChunkState>,
}

impl<S: ReadObjectHandle> CachingObjectHandle<S> {
    pub fn new(source: S) -> Self {
        let block_size = source.block_size() as usize;
        assert!(CHUNK_SIZE % block_size == 0);
        let aligned_size = block_aligned_size(&source);
        let chunk_count = aligned_size.div_ceil(CHUNK_SIZE);

        Self {
            source,
            event: Event::new(),
            inner: RwLock::new(Inner {
                buffer: vec![0; aligned_size],
                chunk_states: vec![ChunkState::Missing; chunk_count],
            }),
        }
    }

    async fn read(&self, offset: usize, mut read_buf: &mut [u8]) -> Result<usize, Error> {
        let source_size = self.source.get_size() as usize;
        if offset >= source_size {
            return Ok(0);
        }
        let read_len = if offset + read_buf.len() > source_size {
            source_size - offset
        } else {
            read_buf.len()
        };

        // If the read spans multiple chunks that are missing then this will sequentially load each
        // chunk. Loading multiple chunks in a single read rarely ever happens and the overhead of
        // doing it in parallel isn't worth the slow down to the much more common cases of loading
        // only 1 chunk or reading from already populated chunks.
        for chunk in ChunkingIterator::new(offset, read_len) {
            let (head, tail) = read_buf.split_at_mut(chunk.read_end - chunk.read_start);
            read_buf = tail;
            self.read_chunk(chunk, head).await?;
        }
        Ok(read_len)
    }

    async fn read_chunk(
        &self,
        read_chunk: ChunkingIteratorChunk,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        debug_assert!(read_chunk.read_end - read_chunk.read_start == buf.len());
        enum Action {
            Wait(EventListener),
            Load,
        }
        loop {
            let action = {
                let inner = self.inner.read().unwrap();
                match inner.chunk_states[read_chunk.chunk_num] {
                    ChunkState::Missing => Action::Load,
                    ChunkState::Present => {
                        buf.copy_from_slice(
                            &inner.buffer[read_chunk.read_start..read_chunk.read_end],
                        );
                        return Ok(());
                    }
                    ChunkState::Pending => Action::Wait(self.event.listen()),
                }
            };
            match action {
                Action::Wait(listener) => listener.await,
                Action::Load => {
                    {
                        let mut inner = self.inner.write().unwrap();
                        if inner.chunk_states[read_chunk.chunk_num] != ChunkState::Missing {
                            // If the state changed between dropping the read lock and acquiring the
                            // write lock then go back to the start.
                            continue;
                        }
                        inner.chunk_states[read_chunk.chunk_num] = ChunkState::Pending;
                    }
                    // If this future is dropped or reading fails then put the chunk back into the
                    // `Missing` state.
                    let drop_guard = scopeguard::guard((), |_| {
                        {
                            let mut inner = self.inner.write().unwrap();
                            debug_assert!(
                                inner.chunk_states[read_chunk.chunk_num] == ChunkState::Pending
                            );
                            inner.chunk_states[read_chunk.chunk_num] = ChunkState::Missing;
                        }
                        self.event.notify(usize::MAX)
                    });

                    let read_start = read_chunk.chunk_num * CHUNK_SIZE;
                    let read_end =
                        std::cmp::min(read_start + CHUNK_SIZE, block_aligned_size(&self.source));
                    let mut read_buf = self.source.allocate_buffer(read_end - read_start);
                    let amount_read =
                        self.source.read(read_start as u64, read_buf.as_mut()).await?;
                    read_buf.as_mut_slice()[amount_read..].fill(0);

                    {
                        let mut inner = self.inner.write().unwrap();
                        inner.buffer[read_start..read_end].copy_from_slice(read_buf.as_slice());
                        buf.copy_from_slice(
                            &inner.buffer[read_chunk.read_start..read_chunk.read_end],
                        );
                        inner.chunk_states[read_chunk.chunk_num] = ChunkState::Present;
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

    fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.source.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.source.block_size()
    }
}

#[async_trait]
impl<S: ReadObjectHandle> ReadObjectHandle for CachingObjectHandle<S> {
    async fn read(&self, offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        Self::read(self, offset as usize, buf.as_mut_slice()).await
    }

    fn get_size(&self) -> u64 {
        self.source.get_size()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChunkState {
    Missing,
    Present,
    Pending,
}

/// An iterator that splits a range into smaller ranges along `CHUNK_SIZE` boundaries.
struct ChunkingIterator {
    read_start: usize,
    read_len: usize,
}

impl ChunkingIterator {
    fn new(start: usize, len: usize) -> Self {
        Self { read_start: start, read_len: len }
    }
}

#[derive(PartialEq, Eq, Debug)]
struct ChunkingIteratorChunk {
    chunk_num: usize,
    read_start: usize,
    read_end: usize,
}

impl Iterator for ChunkingIterator {
    type Item = ChunkingIteratorChunk;
    fn next(&mut self) -> Option<Self::Item> {
        if self.read_len == 0 {
            return None;
        }
        let aligned_start = round_down(self.read_start, CHUNK_SIZE);
        let next_start = aligned_start + CHUNK_SIZE;
        let chunk = ChunkingIteratorChunk {
            chunk_num: aligned_start / CHUNK_SIZE,
            read_start: self.read_start,
            read_end: std::cmp::min(next_start, self.read_start + self.read_len),
        };
        self.read_start = next_start;
        self.read_len -= chunk.read_end - chunk.read_start;
        Some(chunk)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{CachingObjectHandle, ChunkingIterator, ChunkingIteratorChunk, CHUNK_SIZE},
        crate::object_handle::{ObjectHandle, ReadObjectHandle},
        anyhow::Error,
        async_trait::async_trait,
        event_listener::Event,
        std::sync::{
            atomic::{AtomicBool, AtomicU8, Ordering},
            Arc,
        },
        storage_device::{
            buffer::{Buffer, MutableBufferRef},
            fake_device::FakeDevice,
            Device,
        },
    };

    #[fuchsia::test]
    fn test_chunking_iterator_first_whole_chunk() {
        let elements: Vec<_> = ChunkingIterator::new(0, CHUNK_SIZE).collect();
        assert_eq!(
            elements,
            [ChunkingIteratorChunk { chunk_num: 0, read_start: 0, read_end: CHUNK_SIZE }]
        );
    }

    #[fuchsia::test]
    fn test_chunking_iterator_multiple_whole_chunks() {
        let elements: Vec<_> = ChunkingIterator::new(CHUNK_SIZE * 2, CHUNK_SIZE * 3).collect();
        assert_eq!(
            elements,
            [
                ChunkingIteratorChunk {
                    chunk_num: 2,
                    read_start: CHUNK_SIZE * 2,
                    read_end: CHUNK_SIZE * 3
                },
                ChunkingIteratorChunk {
                    chunk_num: 3,
                    read_start: CHUNK_SIZE * 3,
                    read_end: CHUNK_SIZE * 4
                },
                ChunkingIteratorChunk {
                    chunk_num: 4,
                    read_start: CHUNK_SIZE * 4,
                    read_end: CHUNK_SIZE * 5
                }
            ]
        );
    }

    #[fuchsia::test]
    fn test_chunking_iterator_partial_chunk() {
        let elements: Vec<_> = ChunkingIterator::new(CHUNK_SIZE / 4, CHUNK_SIZE / 2).collect();
        assert_eq!(
            elements,
            [ChunkingIteratorChunk {
                chunk_num: 0,
                read_start: CHUNK_SIZE / 4,
                read_end: CHUNK_SIZE / 4 * 3,
            },]
        );
    }

    #[fuchsia::test]
    fn test_chunking_iterator_multiple_unaligned_chunks() {
        let elements: Vec<_> =
            ChunkingIterator::new(CHUNK_SIZE * 2 + CHUNK_SIZE / 4, CHUNK_SIZE * 2 + CHUNK_SIZE / 4)
                .collect();
        assert_eq!(
            elements,
            [
                ChunkingIteratorChunk {
                    chunk_num: 2,
                    read_start: CHUNK_SIZE * 2 + CHUNK_SIZE / 4,
                    read_end: CHUNK_SIZE * 3,
                },
                ChunkingIteratorChunk {
                    chunk_num: 3,
                    read_start: CHUNK_SIZE * 3,
                    read_end: CHUNK_SIZE * 4
                },
                ChunkingIteratorChunk {
                    chunk_num: 4,
                    read_start: CHUNK_SIZE * 4,
                    read_end: CHUNK_SIZE * 4 + CHUNK_SIZE / 2,
                },
            ]
        );
    }

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

    #[async_trait]
    impl ObjectHandle for FakeSource {
        fn object_id(&self) -> u64 {
            unreachable!();
        }

        fn block_size(&self) -> u64 {
            self.device.block_size().into()
        }

        fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
            self.device.allocate_buffer(size)
        }
    }

    #[fuchsia::test]
    async fn test_read_with_missing_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf = vec![0; 4096];
        assert_eq!(caching_object_handle.read(0, &mut buf).await.unwrap(), 4096);
        assert_eq!(buf, make_buf(1, 4096));
    }

    #[fuchsia::test]
    async fn test_read_with_present_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let expected = make_buf(1, 4096);
        let mut buf1 = vec![0; 4096];
        assert_eq!(caching_object_handle.read(0, &mut buf1).await.unwrap(), 4096);
        assert_eq!(buf1, expected);

        // The chunk was already populated so this read receives the same value as the above read.
        let mut buf2 = vec![0; 4096];
        assert_eq!(caching_object_handle.read(0, &mut buf2).await.unwrap(), 4096);
        assert_eq!(buf2, expected);
    }

    #[fuchsia::test]
    async fn test_read_with_pending_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf1 = vec![0; 4096];
        let mut buf2 = vec![0; 4096];
        {
            let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, &mut buf1));
            let mut read_fut2 = std::pin::pin!(caching_object_handle.read(0, &mut buf2));

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
            assert_eq!(read_fut1.await.unwrap(), 4096);
            // The event has been notified and the second future can now complete.
            assert_eq!(read_fut2.await.unwrap(), 4096);
        }

        let expected = make_buf(1, 4096);
        assert_eq!(buf1, expected);
        assert_eq!(buf2, expected);
    }

    #[fuchsia::test]
    async fn test_read_with_notification_for_other_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, CHUNK_SIZE + 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf1 = vec![0; 4096];
        let mut buf2 = vec![0; 4096];
        let mut buf3 = vec![0; 4096];
        {
            let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, &mut buf1));
            let mut read_fut2 = std::pin::pin!(caching_object_handle.read(CHUNK_SIZE, &mut buf2));
            let mut read_fut3 = std::pin::pin!(caching_object_handle.read(0, &mut buf3));

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
            assert_eq!(read_fut2.await.unwrap(), 4096);
            // The event was notified but the first chunk is still `Pending` so the third future
            // resumes waiting.
            assert!(futures::poll!(&mut read_fut3).is_pending());
            // The first future will read from the source, transition the first chunk to `Present`,
            // and notify the event.
            assert_eq!(read_fut1.await.unwrap(), 4096);
            // The first chunk is now present so the third future can complete.
            assert_eq!(read_fut3.await.unwrap(), 4096);
        }

        let expected1 = make_buf(1, 4096);
        let expected2 = make_buf(2, 4096);
        assert_eq!(buf1, expected1);
        assert_eq!(buf2, expected2);
        assert_eq!(buf3, expected1);
    }

    #[fuchsia::test]
    async fn test_read_with_dropped_future() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf1 = vec![0; 4096];
        let mut buf2 = vec![0; 4096];
        {
            let mut read_fut2 = std::pin::pin!(caching_object_handle.read(0, &mut buf2));
            {
                let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0, &mut buf1));

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
            assert_eq!(read_fut2.await.unwrap(), 4096);
        }

        // The first future didn't complete so it's buffer wasn't modified.
        assert_eq!(buf1, vec![0; 4096]);
        // The second future completed and it shows that the chunk was populated from the second
        // read on the source.
        assert_eq!(buf2, make_buf(2, 4096));
    }

    #[fuchsia::test]
    async fn test_read_past_end_of_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 300);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf = vec![3; 10];
        assert_eq!(caching_object_handle.read(500, &mut buf).await.unwrap(), 0);
        assert_eq!(buf, vec![3; 10]);
    }

    #[fuchsia::test]
    async fn test_read_more_than_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        const SOURCE_SIZE: usize = 300;
        const READ_SIZE: usize = SOURCE_SIZE + 200;
        let source = FakeSource::new(device, SOURCE_SIZE);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf = vec![3; READ_SIZE];
        assert_eq!(caching_object_handle.read(0, &mut buf).await.unwrap(), SOURCE_SIZE);
        assert_eq!(buf[..SOURCE_SIZE], make_buf(1, SOURCE_SIZE));
        assert_eq!(buf[SOURCE_SIZE..], vec![3; READ_SIZE - SOURCE_SIZE]);
    }

    #[fuchsia::test]
    async fn test_read_spanning_multiple_chunks() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        const SOURCE_SIZE: usize = CHUNK_SIZE + 10;
        let source = FakeSource::new(device, SOURCE_SIZE);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf = vec![0; SOURCE_SIZE];
        assert_eq!(caching_object_handle.read(0, &mut buf).await.unwrap(), SOURCE_SIZE);
        assert_eq!(buf[0..CHUNK_SIZE], make_buf(1, CHUNK_SIZE));
        assert_eq!(buf[CHUNK_SIZE..SOURCE_SIZE], make_buf(2, SOURCE_SIZE - CHUNK_SIZE));
    }

    #[fuchsia::test]
    async fn test_empty_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 0);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut buf = vec![0; 10];
        assert_eq!(caching_object_handle.read(0, &mut buf).await.unwrap(), 0);
    }
}
