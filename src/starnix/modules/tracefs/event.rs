// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::{AtomicBool, Ordering};

use starnix_core::mm::memory::MemoryObject;
use starnix_core::vfs::OutputBuffer;
use starnix_sync::Mutex;
use starnix_types::PAGE_SIZE;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error, from_status_like_fdio};
use zerocopy::{Immutable, IntoBytes};
use zx::{BootInstant, BootTimeline};

// The default ring buffer size (2MB).
// TODO(https://fxbug.dev/357665908): This should be based on /sys/kernel/tracing/buffer_size_kb.
const DEFAULT_RING_BUFFER_SIZE_BYTES: u64 = 2097152;

// A page header consists of a u64 timestamp and a u64 commit field.
const PAGE_HEADER_SIZE: u64 = 2 * std::mem::size_of::<u64>() as u64;
const COMMIT_FIELD_OFFSET: u64 = std::mem::size_of::<u64>() as u64;

// The event id for atrace events.
const FTRACE_PRINT_ID: u16 = 5;

// Used for inspect tracking.
const DROPPED_PAGES: &str = "dropped_pages";

#[repr(C)]
#[derive(Debug, Default, IntoBytes, Immutable)]
struct PrintEventHeader {
    common_type: u16,
    common_flags: u8,
    common_preempt_count: u8,
    common_pid: i32,
    ip: u64,
}

#[repr(C)]
#[derive(Debug)]
struct PrintEvent<'a> {
    header: PrintEventHeader,
    data: &'a [u8],
}

impl<'a> PrintEvent<'a> {
    fn new(pid: i32, data: &'a [u8]) -> Self {
        Self {
            header: PrintEventHeader {
                common_type: FTRACE_PRINT_ID,
                common_pid: pid,
                // Perfetto doesn't care about any other field.
                ..Default::default()
            },
            data,
        }
    }

    fn size(&self) -> usize {
        std::mem::size_of::<PrintEventHeader>() + self.data.len() + 1
    }

    fn get_bytes(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.header.as_bytes());
        buf.extend_from_slice(self.data);
        buf.extend_from_slice(b"\n");
    }
}

#[repr(C)]
#[derive(Debug, Default, IntoBytes, PartialEq, Immutable)]
struct TraceEventHeader {
    // u32 where:
    //   type_or_length: bottom 5 bits. If 0, `data` is read for length. Always set to 0 for now.
    //   time_delta: top 27 bits
    time_delta: u32,

    // If type_or_length is 0, holds the length of the trace message.
    // We always write length here for simplicity.
    data: u32,
}

impl TraceEventHeader {
    fn new(size: usize) -> Self {
        // The size reported in the event's header includes the size of `size` (a u32) and the size
        // of the event data.
        let size = (std::mem::size_of::<u32>() + size) as u32;
        Self { time_delta: 0, data: size }
    }

    fn set_time_delta(&mut self, nanos: u32) {
        self.time_delta = nanos << 5;
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct TraceEvent<'a> {
    /// Common metadata among all trace event types.
    header: TraceEventHeader, // u64

    /// The event data.
    ///
    /// Atrace events are reported as PrintFtraceEvents. When we support multiple types of events,
    /// this can be updated to be more generic.
    event: PrintEvent<'a>,
}

impl<'a> TraceEvent<'a> {
    pub fn new(pid: i32, data: &'a [u8]) -> Self {
        let event: PrintEvent<'_> = PrintEvent::new(pid, data);
        let header = TraceEventHeader::new(event.size());
        Self { header, event }
    }

    fn size(&self) -> usize {
        std::mem::size_of::<TraceEventHeader>() + self.event.size()
    }

    fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.size());
        bytes.extend_from_slice(self.header.as_bytes());
        self.event.get_bytes(&mut bytes);
        bytes
    }

    fn set_timestamp(&mut self, timestamp: BootInstant, prev_timestamp: BootInstant) {
        // Debug assert here so if it happens, we can notice it happened and hopefully fix it.
        // In non-debug, use 0 as the delta. It will be less disruptive to the process and the
        // resulting trace data.
        debug_assert!(timestamp >= prev_timestamp, "Timestamp must be >= prev_timestamp");
        let nanos: u32 = (timestamp - prev_timestamp).into_nanos().try_into().unwrap_or(0);
        self.header.set_time_delta(nanos);
    }
}

struct TraceEventQueueMetadata {
    /// The offset where new reads happen in the ring buffer.
    head: u64,

    /// The offset of the end of the last committed event in the ring buffer.
    ///
    /// When a writer can preempt another writer, only the last writer to commit its event moves
    /// this commit page.
    commit: u64,

    /// The offset where new writes happen in the ring buffer. This can be later in the ring buffer
    /// compared to `commit` when a writer has reserved space for an event but not yet committed it.
    tail: u64,

    /// The max size of an event.
    max_event_size: u64,

    /// The timestamp of the last event in the queue. If the queue is empty, then the time the queue
    /// was created.
    prev_timestamp: BootInstant,

    /// If true, the queue doesn't have a full page of events to read.
    ///
    /// TODO(https://fxbug.dev/357665908): Support partial page reads.
    is_readable: bool,

    /// If true, overwrites old pages of events when queue is full. Defaults to true.
    overwrite: bool,

    /// The number of pages of events dropped because the ring buffer was full and the queue is in
    /// overwrite mode.
    dropped_pages: u64,
}

impl TraceEventQueueMetadata {
    fn new() -> Self {
        Self {
            head: 0,
            commit: PAGE_HEADER_SIZE,
            tail: PAGE_HEADER_SIZE,
            max_event_size: *PAGE_SIZE - PAGE_HEADER_SIZE,
            prev_timestamp: BootInstant::get(),
            is_readable: false,
            overwrite: true,
            dropped_pages: 0,
        }
    }

    /// The offset of the head page in the `ring_buffer` VMO.
    fn head_page_offset(&self) -> u64 {
        self.head - (self.head % *PAGE_SIZE)
    }

    /// The offset of the commit page in the `ring_buffer` VMO.
    fn commit_page_offset(&self) -> u64 {
        self.commit - (self.commit % *PAGE_SIZE)
    }

    /// The offset of the tail page in the `ring_buffer` VMO.
    fn tail_page_offset(&self) -> u64 {
        self.tail - (self.tail % *PAGE_SIZE)
    }

    /// The offset of the `commit` field in the current commit page's page header.
    fn commit_field_offset(&self) -> u64 {
        self.commit_page_offset() + COMMIT_FIELD_OFFSET
    }

    /// Reserves space in the ring buffer to commit an event. Returns the offset of the start of the
    /// reserved space.
    ///
    /// If the current tail page doesn't have enough space to fit the event but the queue is not
    /// full or is in overwrite mode, returns the offset after the page header of the next page.
    ///
    /// The caller needs to handle clearing old events if queue is in overwrite mode and
    /// head page has moved forward one.
    fn reserve(&mut self, event_size: u64) -> Result<u64, Errno> {
        if event_size > self.max_event_size {
            return error!(EINVAL);
        }

        let prev_tail_page = self.tail_page_offset();
        let mut reserve_start = self.tail;
        let maybe_new_tail = (self.tail + event_size as u64) % DEFAULT_RING_BUFFER_SIZE_BYTES;
        let maybe_new_tail_page = maybe_new_tail - (maybe_new_tail % *PAGE_SIZE);

        if prev_tail_page != maybe_new_tail_page {
            // From https://docs.kernel.org/trace/ring-buffer-design.html:
            // When the tail meets the head page, if the buffer is in overwrite mode, the head page
            // will be pushed ahead one, otherwise, the write will fail.
            if maybe_new_tail_page == self.head_page_offset() {
                if self.overwrite {
                    self.head += *PAGE_SIZE;
                    self.dropped_pages += 1;
                } else {
                    return error!(ENOMEM);
                }
            }

            // Fix commit and tail to point to the offset after the page header.
            reserve_start = maybe_new_tail_page + PAGE_HEADER_SIZE;
        }
        self.tail = reserve_start + event_size as u64;

        Ok(reserve_start)
    }

    /// Moves the commit offset ahead to indicate a write has been committed.
    /// reserve() accounted for moving commit
    fn commit(&mut self, event_size: u64) {
        let prev_commit_page = self.commit_page_offset();
        self.commit = (self.commit + event_size as u64) % DEFAULT_RING_BUFFER_SIZE_BYTES;

        let new_commit_page = self.commit_page_offset();
        if prev_commit_page != new_commit_page {
            self.commit = new_commit_page + PAGE_HEADER_SIZE + event_size as u64;
            // Allow more reads when a page of events are available.
            self.is_readable = true;
        }
    }

    /// Returns the offset of the page to read from. Moves the head page forward a page.
    fn read(&mut self) -> Result<u64, Errno> {
        if !self.is_readable {
            return error!(EAGAIN);
        }

        let head_page = self.head_page_offset();
        self.head = (self.head + *PAGE_SIZE) % DEFAULT_RING_BUFFER_SIZE_BYTES;

        // If the read meets the last commit, then there is nothing more to read.
        if self.head_page_offset() == self.commit_page_offset() {
            self.is_readable = false;
        }

        Ok(head_page)
    }
}

/// Stores all trace events.
pub struct TraceEventQueue {
    /// If true, atrace events written to /sys/kernel/tracing/trace_marker will be stored as
    /// TraceEvents in `ring_buffer`.
    tracing_enabled: AtomicBool,

    /// Metadata about `ring_buffer`.
    metadata: Mutex<TraceEventQueueMetadata>,

    /// The trace events.
    ///
    /// From https://docs.kernel.org/trace/ring-buffer-map.html, if this memory is mapped, it should
    /// start with a meta-page but Perfetto doesn't seem to parse this.
    ///
    /// Each page in this VMO consists of:
    ///   A page header:
    ///     // The timestamp of the last event in the previous page. If this is the first page, then
    ///     // the timestamp tracing was enabled. This is used with time_delta in each
    ///     // event header to calculate an event's timestamp.
    ///     timestamp: u64
    ///
    ///     // The size in bytes of events committed in this page.
    ///     commit: u64
    ///
    ///   // Each event must fit on the remainder of the page (i.e. be smaller than a page minus the
    ///   // size of the page header.
    ///   N trace events
    ring_buffer: MemoryObject,

    /// Insepct node used for diagnostics.
    tracefs_node: fuchsia_inspect::Node,
}

impl<'a> TraceEventQueue {
    pub fn new(inspect_node: &fuchsia_inspect::Node) -> Result<Self, Errno> {
        let tracefs_node = inspect_node.create_child("tracefs");
        let metadata = TraceEventQueueMetadata::new();
        let ring_buffer: MemoryObject = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 0)
            .map_err(|_| errno!(ENOMEM))?
            .into();
        let ring_buffer = ring_buffer.with_zx_name(b"starnix:tracefs");
        Ok(Self {
            tracing_enabled: AtomicBool::new(false),
            metadata: Mutex::new(metadata),
            ring_buffer,
            tracefs_node,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.tracing_enabled.load(Ordering::Relaxed)
    }

    pub fn enable(&self) -> Result<(), Errno> {
        // Use the metadata mutex to make sure the state of the metadata and the enabled flag
        // are changed at the same time.
        let mut metadata = self.metadata.lock();
        metadata.prev_timestamp = BootInstant::get();
        self.ring_buffer
            .set_size(DEFAULT_RING_BUFFER_SIZE_BYTES)
            .map_err(|e| from_status_like_fdio!(e))?;
        self.initialize_page(0, metadata.prev_timestamp)?;
        self.tracing_enabled.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Disables the event queue and resets it to empty.
    /// The number of dropped pages are recorded for reading via tracefs.
    pub fn disable(&self) -> Result<(), Errno> {
        // Use the metadata mutex to make sure the state of the metadata and the enabled flag
        // are changed at the same time.
        let mut metadata = self.metadata.lock();
        self.tracefs_node.record_uint(DROPPED_PAGES, metadata.dropped_pages);
        *metadata = TraceEventQueueMetadata::new();
        self.ring_buffer.set_size(0).map_err(|e| from_status_like_fdio!(e))?;
        self.tracing_enabled.store(false, Ordering::Relaxed);

        Ok(())
    }

    /// Reads a page worth of events. Currently only reads pages that are full.
    ///
    /// From https://docs.kernel.org/trace/ring-buffer-design.html, when memory is mapped, a reader
    /// page can be swapped with the header page to avoid copying memory.
    pub fn read(&self, buf: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        // Read the offset, which also moves the read pointer forward in the metadata, then unlock.
        let offset = {
            let mut metadata = self.metadata.lock();
            metadata.read()?
        };

        // self.ring_buffer is  vmo backed memory. So reads past the allocated size return in error.
        // Enabling and disabling the queue can change the size of the ring_buffer, but this is done
        // using thread safe kernel, so if there is a race between this read and disabling the queue,
        // the worst that will happen is an error of either EAGAIN or ENOMEM.
        buf.write_all(
            &self.ring_buffer.read_to_vec(offset, *PAGE_SIZE).map_err(|_| errno!(ENOMEM))?,
        )
    }

    /// Write `event` into `ring_buffer`.
    /// If `event` does not fit in the current page, move on to the next.
    ///
    /// Should eventually allow for a writer to preempt another writer.
    /// See https://docs.kernel.org/trace/ring-buffer-design.html.
    /// Returns the delta duration between this event and the previous event written.
    pub fn push_event(
        &self,
        mut event: TraceEvent<'a>,
    ) -> Result<zx::Duration<BootTimeline>, Errno> {
        let mut metadata = self.metadata.lock();

        // The timestamp for the current event must be after the metadata.prev_timestamp.
        // This is because the event data header only stores the delta time, not the entire timestamp.
        // This is stored as an unsigned 27 bit value, so the delta must be a positive value to be
        // stored correctly.
        // To make sure this is the case, the timestamp and delta calculation are done while holding
        // the metadata lock. This definitely could be refined, potentially using an atomic to hold
        // the previous timestamp or similar synchronization to make sure the previous timestamp is not
        // updated past this timestamp.
        let timestamp = BootInstant::get();

        event.set_timestamp(timestamp, metadata.prev_timestamp);

        // Get the offset of `ring_buffer` to write this event to.
        let old_tail_page = metadata.tail_page_offset();
        let offset = metadata.reserve(event.size() as u64)?;

        // Clear old events and reset the page header if we've moved to the next page.
        let new_tail_page = metadata.tail_page_offset();
        if new_tail_page != old_tail_page {
            self.initialize_page(new_tail_page, metadata.prev_timestamp)?;
        }

        // Write the event and update the commit offset.
        self.ring_buffer.write(&event.as_bytes(), offset).map_err(|e| from_status_like_fdio!(e))?;
        metadata.commit(event.size() as u64);

        // Update the page header's `commit` field with the new size of committed data on the page.
        self.ring_buffer
            .write(
                &((metadata.commit % *PAGE_SIZE) - PAGE_HEADER_SIZE).to_le_bytes(),
                metadata.commit_field_offset(),
            )
            .map_err(|e| from_status_like_fdio!(e))?;

        let delta = timestamp - metadata.prev_timestamp;
        metadata.prev_timestamp = timestamp;

        Ok(delta)
    }

    #[cfg(test)]
    /// Returns the timestamp of the previous event in `ring_buffer`.
    fn prev_timestamp(&self) -> BootInstant {
        self.metadata.lock().prev_timestamp
    }

    /// Initializes a new page by setting the header's timestamp and clearing the rest of the page
    /// with 0's.
    fn initialize_page(&self, offset: u64, prev_timestamp: BootInstant) -> Result<(), Errno> {
        self.ring_buffer
            .write(&prev_timestamp.into_nanos().to_le_bytes(), offset)
            .map_err(|e| from_status_like_fdio!(e))?;
        let timestamp_size = std::mem::size_of::<BootInstant>() as u64;
        self.ring_buffer
            .op_range(zx::VmoOp::ZERO, offset + timestamp_size, *PAGE_SIZE - timestamp_size)
            .map_err(|e| from_status_like_fdio!(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TraceEvent, TraceEventQueue, TraceEventQueueMetadata, DEFAULT_RING_BUFFER_SIZE_BYTES,
        PAGE_HEADER_SIZE,
    };
    use starnix_core::vfs::buffers::VecOutputBuffer;
    use starnix_core::vfs::OutputBuffer;
    use starnix_types::PAGE_SIZE;
    use starnix_uapi::error;

    #[fuchsia::test]
    fn metadata_errors() {
        let mut metadata = TraceEventQueueMetadata::new();
        assert_eq!(metadata.read(), error!(EAGAIN));
        assert_eq!(metadata.reserve(*PAGE_SIZE), error!(EINVAL));
    }

    #[fuchsia::test]
    fn metadata_push_event_simple() {
        let mut metadata = TraceEventQueueMetadata::new();
        let event_size = 30;
        let reserved_offset = metadata.reserve(event_size).expect("reserve failed");
        assert_eq!(reserved_offset, PAGE_HEADER_SIZE);
        assert_eq!(metadata.head, 0);
        assert_eq!(metadata.commit, PAGE_HEADER_SIZE);
        assert_eq!(metadata.tail, PAGE_HEADER_SIZE + event_size);

        metadata.commit(event_size);
        assert_eq!(metadata.head, 0);
        assert_eq!(metadata.commit, PAGE_HEADER_SIZE + event_size);
        assert_eq!(metadata.tail, PAGE_HEADER_SIZE + event_size);
    }

    #[fuchsia::test]
    fn metadata_push_event_next_page() {
        let mut metadata = TraceEventQueueMetadata::new();
        // Set up pointers to be near the end of the page.
        metadata.commit = *PAGE_SIZE - 1;
        metadata.tail = *PAGE_SIZE - 1;

        // Reserving space for an event should only move the tail pointer.
        let event_size = 30;
        let reserved_offset = metadata.reserve(event_size).expect("reserve failed");
        assert_eq!(reserved_offset, *PAGE_SIZE + PAGE_HEADER_SIZE);
        assert_eq!(metadata.head, 0);
        assert_eq!(metadata.commit, *PAGE_SIZE - 1);
        assert_eq!(metadata.tail, *PAGE_SIZE + PAGE_HEADER_SIZE + event_size);

        // Committing an event should only move the commit pointer.
        metadata.commit(event_size);
        assert_eq!(metadata.head, 0);
        assert_eq!(metadata.commit, *PAGE_SIZE + PAGE_HEADER_SIZE + event_size);
        assert_eq!(metadata.tail, *PAGE_SIZE + PAGE_HEADER_SIZE + event_size);
    }

    #[fuchsia::test]
    fn metadata_reserve_full() {
        let mut metadata = TraceEventQueueMetadata::new();
        metadata.commit = DEFAULT_RING_BUFFER_SIZE_BYTES;
        metadata.tail = DEFAULT_RING_BUFFER_SIZE_BYTES;

        // If not overwriting, reserve should fail.
        metadata.overwrite = false;
        assert_eq!(metadata.reserve(30), error!(ENOMEM));

        // Otherwise, reserving should wrap around to the front of the ring buffer.
        metadata.overwrite = true;
        assert_eq!(metadata.reserve(30), Ok(PAGE_HEADER_SIZE));
        assert_eq!(metadata.head_page_offset(), *PAGE_SIZE);
        assert_eq!(metadata.dropped_pages, 1);
    }

    #[fuchsia::test]
    fn metadata_read_simple() {
        let mut metadata = TraceEventQueueMetadata::new();
        metadata.is_readable = true;

        assert_eq!(metadata.read(), Ok(0));
        assert_eq!(metadata.head, *PAGE_SIZE);
    }

    #[fuchsia::test]
    fn metadata_read_meets_commit() {
        let mut metadata = TraceEventQueueMetadata::new();
        metadata.is_readable = true;
        metadata.commit = *PAGE_SIZE + PAGE_HEADER_SIZE + 30;

        assert_eq!(metadata.read(), Ok(0));
        assert_eq!(metadata.head, *PAGE_SIZE);
        assert!(!metadata.is_readable);
        assert_eq!(metadata.read(), error!(EAGAIN));
    }

    #[fuchsia::test]
    fn read_empty_queue() {
        let inspect_node = fuchsia_inspect::Node::default();
        let queue = TraceEventQueue::new(&inspect_node).expect("create queue");
        let mut buffer = VecOutputBuffer::new(*PAGE_SIZE as usize);
        assert_eq!(queue.read(&mut buffer), error!(EAGAIN));
    }

    #[fuchsia::test]
    fn enable_disable_queue() {
        let inspect_node = fuchsia_inspect::Node::default();
        let queue = TraceEventQueue::new(&inspect_node).expect("create queue");
        assert_eq!(queue.ring_buffer.get_size(), 0);

        // Enable tracing and check the queue's state.
        assert!(queue.enable().is_ok());
        assert_eq!(queue.ring_buffer.get_size(), DEFAULT_RING_BUFFER_SIZE_BYTES);

        // Confirm we can push an event.
        let event = TraceEvent::new(1234, b"B|1234|slice_name");
        let event_size = event.size() as u64;
        let result = queue.push_event(event);
        assert!(result.is_ok());
        assert!(result.ok().expect("delta").into_nanos() > 0);
        assert_eq!(queue.metadata.lock().commit, PAGE_HEADER_SIZE + event_size);

        // Disable tracing and check that the queue's state has been reset.
        assert!(queue.disable().is_ok());
        assert_eq!(queue.ring_buffer.get_size(), 0);
        assert_eq!(queue.metadata.lock().commit, PAGE_HEADER_SIZE);
    }

    #[fuchsia::test]
    fn create_trace_event() {
        // Create an event.
        let event: TraceEvent<'_> = TraceEvent::new(1234, b"B|1234|slice_name");
        let event_size = event.size();
        assert_eq!(event_size, 42);
    }

    // This can be removed when we support reading incomplete pages.
    #[fuchsia::test]
    fn single_trace_event_fails_read() {
        let inspect_node = fuchsia_inspect::Node::default();
        let queue = TraceEventQueue::new(&inspect_node).expect("create queue");
        queue.enable().expect("enable queue");
        // Create an event.
        let event = TraceEvent::new(1234, b"B|1234|slice_name");

        // Push the event into the queue.
        let result = queue.push_event(event);
        assert!(result.is_ok());
        assert!(result.ok().expect("delta").into_nanos() > 0);

        let mut buffer = VecOutputBuffer::new(*PAGE_SIZE as usize);
        assert_eq!(queue.read(&mut buffer), error!(EAGAIN));
    }

    #[fuchsia::test]
    fn page_overflow() {
        let inspect_node = fuchsia_inspect::Node::default();
        let queue = TraceEventQueue::new(&inspect_node).expect("create queue");
        queue.enable().expect("enable queue");
        let queue_start_timestamp = queue.prev_timestamp();
        let pid = 1234;
        let data = b"B|1234|loooooooooooooooooooooooooooooooooooooooooooooooooooooooooo\
        ooooooooooooooooooooooooooooooooooooooooooooooooooooooooongevent";
        let expected_event = TraceEvent::new(pid, data);
        assert_eq!(expected_event.size(), 155);

        // Push the event into the queue.
        for _ in 0..27 {
            let event = TraceEvent::new(pid, data);
            let result = queue.push_event(event);
            assert!(result.is_ok());
            assert!(result.ok().expect("delta").into_nanos() > 0);
        }

        // Read a page of data.
        let mut buffer = VecOutputBuffer::new(*PAGE_SIZE as usize);
        assert_eq!(queue.read(&mut buffer), Ok(*PAGE_SIZE as usize));
        assert_eq!(buffer.bytes_written() as u64, *PAGE_SIZE);

        let mut expected_page_header: Vec<u8> = vec![];
        expected_page_header
            .extend_from_slice(&(queue_start_timestamp.into_nanos() as u64).to_le_bytes());
        expected_page_header.extend_from_slice(&(expected_event.size() * 26).to_le_bytes());

        assert!(buffer.data().starts_with(&expected_page_header));

        // Try reading another page.
        let mut buffer = VecOutputBuffer::new(*PAGE_SIZE as usize);
        assert_eq!(queue.read(&mut buffer), error!(EAGAIN));
    }
}
