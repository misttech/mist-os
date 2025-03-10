// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the buffer traits needed by the TCP implementation. The traits
//! in this module provide a common interface for platform-specific buffers
//! used by TCP.

use netstack3_base::{Payload, SackBlock, SackBlocks, SeqNum};

use alloc::vec::Vec;
use arrayvec::ArrayVec;
use core::fmt::Debug;
use core::ops::Range;
use packet::InnerPacketBuilder;

use crate::internal::base::BufferSizes;

/// Common super trait for both sending and receiving buffer.
pub trait Buffer: Debug + Sized {
    /// Returns the capacity range `(min, max)` for this buffer type.
    fn capacity_range() -> (usize, usize);

    /// Returns information about the number of bytes in the buffer.
    ///
    /// Returns a [`BufferLimits`] instance with information about the number of
    /// bytes in the buffer.
    fn limits(&self) -> BufferLimits;

    /// Gets the target size of the buffer, in bytes.
    ///
    /// The target capacity of the buffer is distinct from the actual capacity
    /// (returned by [`Buffer::capacity`]) in that the target capacity should
    /// remain fixed unless requested otherwise, while the actual capacity can
    /// vary with usage.
    ///
    /// For fixed-size buffers this should return the same result as calling
    /// `self.capacity()`. For buffer types that support resizing, the
    /// returned value can be different but should not change unless a resize
    /// was requested.
    fn target_capacity(&self) -> usize;

    /// Requests that the buffer be resized to hold the given number of bytes.
    ///
    /// Calling this method suggests to the buffer that it should alter its size.
    /// Implementations are free to impose constraints or ignore requests
    /// entirely.
    fn request_capacity(&mut self, size: usize);
}

/// A buffer supporting TCP receiving operations.
pub trait ReceiveBuffer: Buffer {
    /// Writes `data` into the buffer at `offset`.
    ///
    /// Returns the number of bytes written.
    fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize;

    /// Marks `count` bytes available for the application to read.
    ///
    /// `has_outstanding` informs the buffer if any bytes past `count` may have
    /// been populated by out of order segments.
    ///
    /// # Panics
    ///
    /// Panics if the caller attempts to make more bytes readable than the
    /// buffer has capacity for. That is, this method panics if `self.len() +
    /// count > self.cap()`
    fn make_readable(&mut self, count: usize, has_outstanding: bool);
}

/// A buffer supporting TCP sending operations.
pub trait SendBuffer: Buffer {
    /// The payload type given to `peek_with`.
    type Payload<'a>: InnerPacketBuilder + Payload + Debug + 'a;

    /// Removes `count` bytes from the beginning of the buffer as already read.
    ///
    /// # Panics
    ///
    /// Panics if more bytes are marked as read than are available, i.e.,
    /// `count > self.len`.
    fn mark_read(&mut self, count: usize);

    /// Calls `f` with contiguous sequences of readable bytes in the buffer
    /// without advancing the reading pointer.
    ///
    /// # Panics
    ///
    /// Panics if more bytes are peeked than are available, i.e.,
    /// `offset > self.len`
    fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
    where
        F: FnOnce(Self::Payload<'a>) -> R;
}

/// Information about the number of bytes in a [`Buffer`].
#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub struct BufferLimits {
    /// The total number of bytes that the buffer can hold.
    pub capacity: usize,

    /// The number of readable bytes that the buffer currently holds.
    pub len: usize,
}

#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct OutstandingBlock {
    range: Range<SeqNum>,
    generation: usize,
}

/// Assembler for out-of-order segment data.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(super) struct Assembler {
    // `nxt` is the next sequence number to be expected. It should be before
    // any sequnce number of the out-of-order sequence numbers we keep track
    // of below.
    nxt: SeqNum,
    // Keeps track of the "age" of segments in the outstanding queue. Every time
    // a segment is inserted, the generation increases. This allows
    // RFC-compliant ordering of selective ACK blocks.
    generation: usize,
    // Holds all the sequence number ranges which we have already received.
    // These ranges are sorted and should have a gap of at least 1 byte
    // between any consecutive two. These ranges should only be after `nxt`.
    outstanding: Vec<OutstandingBlock>,
}

impl Assembler {
    /// Creates a new assembler.
    pub(super) fn new(nxt: SeqNum) -> Self {
        Self { outstanding: Vec::new(), generation: 0, nxt }
    }

    /// Returns the next sequence number expected to be received.
    pub(super) fn nxt(&self) -> SeqNum {
        self.nxt
    }

    /// Returns whether there are out-of-order segments waiting to be
    /// acknowledged.
    pub(super) fn has_out_of_order(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// Inserts a received segment.
    ///
    /// The newly added segment will be merged with as many existing ones as
    /// possible and `nxt` will be advanced to the highest ACK number possible.
    ///
    /// Returns number of bytes that should be available for the application
    /// to consume.
    ///
    /// # Panics
    ///
    /// Panics if `start` is after `end` or if `start` is before `self.nxt`.
    pub(super) fn insert(&mut self, Range { start, end }: Range<SeqNum>) -> usize {
        assert!(!start.after(end));
        assert!(!start.before(self.nxt));
        if start == end {
            return 0;
        }
        self.insert_inner(start..end);

        let Self { outstanding, nxt, generation: _ } = self;
        if outstanding[0].range.start == *nxt {
            let advanced = outstanding.remove(0);
            *nxt = advanced.range.end;
            // The following unwrap is safe because it is invalid to have
            // have a range where `end` is before `start`.
            usize::try_from(advanced.range.end - advanced.range.start).unwrap()
        } else {
            0
        }
    }

    pub(super) fn has_outstanding(&self) -> bool {
        let Self { outstanding, nxt: _, generation: _ } = self;
        !outstanding.is_empty()
    }

    fn insert_inner(&mut self, Range { mut start, mut end }: Range<SeqNum>) {
        let Self { outstanding, nxt: _, generation } = self;

        if start == end {
            return;
        }

        *generation = *generation + 1;

        if outstanding.is_empty() {
            outstanding
                .push(OutstandingBlock { range: Range { start, end }, generation: *generation });
            return;
        }

        // Search for the first segment whose `start` is greater.
        let first_after = {
            let mut cur = 0;
            while cur < outstanding.len() {
                if start.before(outstanding[cur].range.start) {
                    break;
                }
                cur += 1;
            }
            cur
        };

        let mut merge_right = 0;
        for block in &outstanding[first_after..outstanding.len()] {
            if end.before(block.range.start) {
                break;
            }
            merge_right += 1;
            if end.before(block.range.end) {
                end = block.range.end;
                break;
            }
        }

        let mut merge_left = 0;
        for block in (&outstanding[0..first_after]).iter().rev() {
            if start.after(block.range.end) {
                break;
            }
            // There is no guarantee that `end.after(range.end)`, not doing
            // the following may shrink existing coverage. For example:
            // range.start = 0, range.end = 10, start = 0, end = 1, will result
            // in only 0..1 being tracked in the resulting assembler. We didn't
            // do the symmetrical thing above when merging to the right because
            // the search guarantees that `start.before(range.start)`, thus the
            // problem doesn't exist there. The asymmetry rose from the fact
            // that we used `start` to perform the search.
            if end.before(block.range.end) {
                end = block.range.end;
            }
            merge_left += 1;
            if start.after(block.range.start) {
                start = block.range.start;
                break;
            }
        }

        if merge_left == 0 && merge_right == 0 {
            // If the new segment cannot merge with any of its neighbors, we
            // add a new entry for it.
            outstanding.insert(
                first_after,
                OutstandingBlock { range: Range { start, end }, generation: *generation },
            );
        } else {
            // Otherwise, we put the new segment at the left edge of the merge
            // window and remove all other existing segments.
            let left_edge = first_after - merge_left;
            let right_edge = first_after + merge_right;
            outstanding[left_edge] =
                OutstandingBlock { range: Range { start, end }, generation: *generation };
            for i in right_edge..outstanding.len() {
                outstanding[i - merge_left - merge_right + 1] = outstanding[i].clone();
            }
            outstanding.truncate(outstanding.len() - merge_left - merge_right + 1);
        }
    }

    /// Returns the current outstanding selective ack blocks in the assembler.
    ///
    /// The returned blocks are sorted according to [RFC 2018 section 4]:
    ///
    /// * The first SACK block (i.e., the one immediately following the kind and
    ///   length fields in the option) MUST specify the contiguous block of data
    ///   containing the segment which triggered this ACK. [...]
    /// * The SACK option SHOULD be filled out by repeating the most recently
    ///   reported SACK blocks [...]
    ///
    /// This is achieved by always returning the blocks that were most recently
    /// changed by incoming segments.
    ///
    /// [RFC 2018 section 4]:
    ///     https://datatracker.ietf.org/doc/html/rfc2018#section-4
    #[todo_unused::todo_unused("https://fxbug.dev/42078221")]
    pub(crate) fn sack_blocks(&self) -> SackBlocks {
        let Self { nxt: _, generation: _, outstanding } = self;
        // Fast exit, no outstanding blocks.
        if outstanding.is_empty() {
            return SackBlocks::default();
        }

        let mut heap = ArrayVec::<&OutstandingBlock, { SackBlocks::MAX_BLOCKS }>::new();
        for block in outstanding {
            if heap.is_full() {
                if heap.last().is_some_and(|l| l.generation < block.generation) {
                    // New block is later than the earliest block in the heap.
                    let _: Option<_> = heap.pop();
                } else {
                    // New block is earlier than the earliest block in the heap,
                    // pass.
                    continue;
                }
            }

            heap.push(block);
            // Sort heap larger generation to lower.
            heap.sort_by(|a, b| b.generation.cmp(&a.generation))
        }

        SackBlocks::from_iter(
            heap.into_iter().map(|block| SackBlock(block.range.start, block.range.end)),
        )
    }
}

/// A conversion trait that converts the object that Bindings give us into a
/// pair of receive and send buffers.
pub trait IntoBuffers<R: ReceiveBuffer, S: SendBuffer> {
    /// Converts to receive and send buffers.
    fn into_buffers(self, buffer_sizes: BufferSizes) -> (R, S);
}

#[cfg(any(test, feature = "testutils"))]
impl<R: Default + ReceiveBuffer, S: Default + SendBuffer> IntoBuffers<R, S> for () {
    fn into_buffers(self, buffer_sizes: BufferSizes) -> (R, S) {
        // Ignore buffer sizes since this is a test-only impl.
        let BufferSizes { send: _, receive: _ } = buffer_sizes;
        Default::default()
    }
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use alloc::sync::Arc;
    use alloc::vec;
    use core::cmp;

    use either::Either;
    use netstack3_base::sync::Mutex;
    use netstack3_base::{FragmentedPayload, WindowSize};

    use crate::internal::socket::accept_queue::ListenerNotifier;

    /// A circular buffer implementation.
    ///
    /// A [`RingBuffer`] holds a logically contiguous ring of memory in three
    /// regions:
    ///
    /// - *readable*: memory is available for reading and not for writing,
    /// - *writable*: memory that is available for writing and not for reading,
    /// - *reserved*: memory that was read from and is no longer available
    ///   for reading or for writing.
    ///
    /// Zero or more of these regions can be empty, and a region of memory can
    /// transition from one to another in a few different ways:
    ///
    /// *Readable* memory, once read, becomes writable.
    ///
    /// *Writable* memory, once marked as such, becomes readable.
    #[cfg_attr(any(test, feature = "testutils"), derive(Clone, PartialEq, Eq))]
    pub struct RingBuffer {
        pub(super) storage: Vec<u8>,
        /// The index where the reader starts to read.
        ///
        /// Maintains the invariant that `head < storage.len()` by wrapping
        /// around to 0 as needed.
        pub(super) head: usize,
        /// The amount of readable data in `storage`.
        ///
        /// Anything between [head, head+len) is readable. This will never exceed
        /// `storage.len()`.
        pub(super) len: usize,
    }

    impl Debug for RingBuffer {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let Self { storage, head, len } = self;
            f.debug_struct("RingBuffer")
                .field("storage (len, cap)", &(storage.len(), storage.capacity()))
                .field("head", head)
                .field("len", len)
                .finish()
        }
    }

    impl Default for RingBuffer {
        fn default() -> Self {
            Self::new(WindowSize::DEFAULT.into())
        }
    }

    impl RingBuffer {
        /// Creates a new `RingBuffer`.
        pub fn new(capacity: usize) -> Self {
            Self { storage: vec![0; capacity], head: 0, len: 0 }
        }

        /// Resets the buffer to be entirely unwritten.
        pub fn reset(&mut self) {
            let Self { storage: _, head, len } = self;
            *head = 0;
            *len = 0;
        }

        /// Calls `f` on the contiguous sequences from `start` up to `len` bytes.
        fn with_readable<'a, F, R>(storage: &'a Vec<u8>, start: usize, len: usize, f: F) -> R
        where
            F: for<'b> FnOnce(&'b [&'a [u8]]) -> R,
        {
            // Don't read past the end of storage.
            let end = start + len;
            if end > storage.len() {
                let first_part = &storage[start..storage.len()];
                let second_part = &storage[0..len - first_part.len()];
                f(&[first_part, second_part][..])
            } else {
                let all_bytes = &storage[start..end];
                f(&[all_bytes][..])
            }
        }

        /// Calls `f` with contiguous sequences of readable bytes in the buffer and
        /// discards the amount of bytes returned by `f`.
        ///
        /// # Panics
        ///
        /// Panics if the closure wants to discard more bytes than possible, i.e.,
        /// the value returned by `f` is greater than `self.len()`.
        pub fn read_with<F>(&mut self, f: F) -> usize
        where
            F: for<'a, 'b> FnOnce(&'b [&'a [u8]]) -> usize,
        {
            let Self { storage, head, len } = self;
            if storage.len() == 0 {
                return f(&[&[]]);
            }
            let nread = RingBuffer::with_readable(storage, *head, *len, f);
            assert!(nread <= *len);
            *len -= nread;
            *head = (*head + nread) % storage.len();
            nread
        }

        /// Returns the writable regions of the [`RingBuffer`].
        pub fn writable_regions(&mut self) -> impl IntoIterator<Item = &mut [u8]> {
            let BufferLimits { capacity, len } = self.limits();
            let available = capacity - len;
            let Self { storage, head, len } = self;

            let mut write_start = *head + *len;
            if write_start >= storage.len() {
                write_start -= storage.len()
            }
            let write_end = write_start + available;
            if write_end <= storage.len() {
                Either::Left([&mut self.storage[write_start..write_end]].into_iter())
            } else {
                let (b1, b2) = self.storage[..].split_at_mut(write_start);
                let b2_len = b2.len();
                Either::Right([b2, &mut b1[..(available - b2_len)]].into_iter())
            }
        }
    }

    impl Buffer for RingBuffer {
        fn capacity_range() -> (usize, usize) {
            // Arbitrarily chosen to satisfy tests so we have some semblance of
            // clamping capacity in tests.
            (16, 16 << 20)
        }

        fn limits(&self) -> BufferLimits {
            let Self { storage, len, head: _ } = self;
            let capacity = storage.len();
            BufferLimits { len: *len, capacity }
        }

        fn target_capacity(&self) -> usize {
            let Self { storage, len: _, head: _ } = self;
            storage.len()
        }

        fn request_capacity(&mut self, size: usize) {
            unimplemented!("capacity request for {size} not supported")
        }
    }

    impl ReceiveBuffer for RingBuffer {
        fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
            let BufferLimits { capacity, len } = self.limits();
            let available = capacity - len;
            let Self { storage, head, len } = self;
            if storage.len() == 0 {
                return 0;
            }

            if offset > available {
                return 0;
            }
            let start_at = (*head + *len + offset) % storage.len();
            let to_write = cmp::min(data.len(), available);
            // Write the first part of the payload.
            let first_len = cmp::min(to_write, storage.len() - start_at);
            data.partial_copy(0, &mut storage[start_at..start_at + first_len]);
            // If we have more to write, wrap around and start from the beginning
            // of the storage.
            if to_write > first_len {
                data.partial_copy(first_len, &mut storage[0..to_write - first_len]);
            }
            to_write
        }

        fn make_readable(&mut self, count: usize, _has_outstanding: bool) {
            let BufferLimits { capacity, len } = self.limits();
            debug_assert!(count <= capacity - len);
            self.len += count;
        }
    }

    impl SendBuffer for RingBuffer {
        type Payload<'a> = FragmentedPayload<'a, 2>;

        fn mark_read(&mut self, count: usize) {
            let Self { storage, head, len } = self;
            assert!(count <= *len);
            *len -= count;
            *head = (*head + count) % storage.len();
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(Self::Payload<'a>) -> R,
        {
            let Self { storage, head, len } = self;
            if storage.len() == 0 {
                return f(FragmentedPayload::new_empty());
            }
            assert!(offset <= *len);
            RingBuffer::with_readable(
                storage,
                (*head + offset) % storage.len(),
                *len - offset,
                |readable| f(readable.into_iter().map(|x| *x).collect()),
            )
        }
    }

    impl RingBuffer {
        /// Enqueues as much of `data` as possible to the end of the buffer.
        ///
        /// Returns the number of bytes actually queued.
        pub(crate) fn enqueue_data(&mut self, data: &[u8]) -> usize {
            let nwritten = self.write_at(0, &data);
            self.make_readable(nwritten, false);
            nwritten
        }
    }

    impl Buffer for Arc<Mutex<RingBuffer>> {
        fn capacity_range() -> (usize, usize) {
            RingBuffer::capacity_range()
        }

        fn limits(&self) -> BufferLimits {
            self.lock().limits()
        }

        fn target_capacity(&self) -> usize {
            self.lock().target_capacity()
        }

        fn request_capacity(&mut self, size: usize) {
            self.lock().request_capacity(size)
        }
    }

    impl ReceiveBuffer for Arc<Mutex<RingBuffer>> {
        fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
            self.lock().write_at(offset, data)
        }

        fn make_readable(&mut self, count: usize, has_outstanding: bool) {
            self.lock().make_readable(count, has_outstanding)
        }
    }

    /// An implementation of [`SendBuffer`] for tests.
    #[derive(Debug, Default)]
    pub struct TestSendBuffer {
        fake_stream: Arc<Mutex<Vec<u8>>>,
        ring: RingBuffer,
    }

    impl TestSendBuffer {
        /// Creates a new `TestSendBuffer` with a backing shared vec and a
        /// helper ring buffer.
        pub fn new(fake_stream: Arc<Mutex<Vec<u8>>>, ring: RingBuffer) -> TestSendBuffer {
            Self { fake_stream, ring }
        }
    }

    impl Buffer for TestSendBuffer {
        fn capacity_range() -> (usize, usize) {
            let (min, max) = RingBuffer::capacity_range();
            (min * 2, max * 2)
        }

        fn limits(&self) -> BufferLimits {
            let Self { fake_stream, ring } = self;
            let BufferLimits { capacity: ring_capacity, len: ring_len } = ring.limits();
            let guard = fake_stream.lock();
            let len = ring_len + guard.len();
            let capacity = ring_capacity + guard.capacity();
            BufferLimits { len, capacity }
        }

        fn target_capacity(&self) -> usize {
            let Self { fake_stream: _, ring } = self;
            ring.target_capacity()
        }

        fn request_capacity(&mut self, size: usize) {
            let Self { fake_stream: _, ring } = self;
            ring.request_capacity(size)
        }
    }

    impl SendBuffer for TestSendBuffer {
        type Payload<'a> = FragmentedPayload<'a, 2>;

        fn mark_read(&mut self, count: usize) {
            let Self { fake_stream: _, ring } = self;
            ring.mark_read(count)
        }

        fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
        where
            F: FnOnce(Self::Payload<'a>) -> R,
        {
            let Self { fake_stream, ring } = self;
            let mut guard = fake_stream.lock();
            if !guard.is_empty() {
                // Pull from the fake stream into the ring if there is capacity.
                let BufferLimits { capacity, len } = ring.limits();
                let len = (capacity - len).min(guard.len());
                let rest = guard.split_off(len);
                let first = core::mem::replace(&mut *guard, rest);
                assert_eq!(ring.enqueue_data(&first[..]), len);
            }
            ring.peek_with(offset, f)
        }
    }

    fn arc_mutex_eq<T: PartialEq>(a: &Arc<Mutex<T>>, b: &Arc<Mutex<T>>) -> bool {
        if Arc::ptr_eq(a, b) {
            return true;
        }
        (&*a.lock()) == (&*b.lock())
    }

    /// A fake implementation of client-side TCP buffers.
    #[derive(Clone, Debug, Default)]
    pub struct ClientBuffers {
        /// Receive buffer shared with core TCP implementation.
        pub receive: Arc<Mutex<RingBuffer>>,
        /// Send buffer shared with core TCP implementation.
        pub send: Arc<Mutex<Vec<u8>>>,
    }

    impl PartialEq for ClientBuffers {
        fn eq(&self, ClientBuffers { receive: other_receive, send: other_send }: &Self) -> bool {
            let Self { receive, send } = self;
            arc_mutex_eq(receive, other_receive) && arc_mutex_eq(send, other_send)
        }
    }

    impl Eq for ClientBuffers {}

    impl ClientBuffers {
        /// Creates new a `ClientBuffers` with `buffer_sizes`.
        pub fn new(buffer_sizes: BufferSizes) -> Self {
            let BufferSizes { send, receive } = buffer_sizes;
            Self {
                receive: Arc::new(Mutex::new(RingBuffer::new(receive))),
                send: Arc::new(Mutex::new(Vec::with_capacity(send))),
            }
        }
    }

    /// A fake implementation of bindings buffers for TCP.
    #[derive(Debug, Clone, Eq, PartialEq)]
    #[allow(missing_docs)]
    pub enum ProvidedBuffers {
        Buffers(WriteBackClientBuffers),
        NoBuffers,
    }

    impl Default for ProvidedBuffers {
        fn default() -> Self {
            Self::NoBuffers
        }
    }

    impl From<WriteBackClientBuffers> for ProvidedBuffers {
        fn from(buffers: WriteBackClientBuffers) -> Self {
            ProvidedBuffers::Buffers(buffers)
        }
    }

    impl From<ProvidedBuffers> for WriteBackClientBuffers {
        fn from(extra: ProvidedBuffers) -> Self {
            match extra {
                ProvidedBuffers::Buffers(buffers) => buffers,
                ProvidedBuffers::NoBuffers => Default::default(),
            }
        }
    }

    impl From<ProvidedBuffers> for () {
        fn from(_: ProvidedBuffers) -> Self {
            ()
        }
    }

    impl From<()> for ProvidedBuffers {
        fn from(_: ()) -> Self {
            Default::default()
        }
    }

    /// The variant of [`ProvidedBuffers`] that provides observing the data
    /// sent/received to TCP sockets.
    #[derive(Debug, Default, Clone)]
    pub struct WriteBackClientBuffers(pub Arc<Mutex<Option<ClientBuffers>>>);

    impl PartialEq for WriteBackClientBuffers {
        fn eq(&self, Self(other): &Self) -> bool {
            let Self(this) = self;
            arc_mutex_eq(this, other)
        }
    }

    impl Eq for WriteBackClientBuffers {}

    impl IntoBuffers<Arc<Mutex<RingBuffer>>, TestSendBuffer> for ProvidedBuffers {
        fn into_buffers(
            self,
            buffer_sizes: BufferSizes,
        ) -> (Arc<Mutex<RingBuffer>>, TestSendBuffer) {
            let buffers = ClientBuffers::new(buffer_sizes);
            if let ProvidedBuffers::Buffers(b) = self {
                *b.0.as_ref().lock() = Some(buffers.clone());
            }
            let ClientBuffers { receive, send } = buffers;
            (receive, TestSendBuffer::new(send, Default::default()))
        }
    }

    impl ListenerNotifier for ProvidedBuffers {
        fn new_incoming_connections(&mut self, _: usize) {}
    }
}

#[cfg(test)]
mod test {
    use alloc::{format, vec};

    use netstack3_base::{FragmentedPayload, WindowSize};
    use proptest::strategy::{Just, Strategy};
    use proptest::test_runner::Config;
    use proptest::{prop_assert, prop_assert_eq, proptest};
    use proptest_support::failed_seeds_no_std;
    use test_case::test_case;
    use testutil::RingBuffer;

    use super::*;
    proptest! {
        #![proptest_config(Config {
            // Add all failed seeds here.
            failure_persistence: failed_seeds_no_std!(
                "cc f621ca7d3a2b108e0dc41f7169ad028f4329b79e90e73d5f68042519a9f63999",
                "cc c449aebed201b4ec4f137f3c224f20325f4cfee0b7fd596d9285176b6d811aa9"
            ),
            ..Config::default()
        })]

        #[test]
        fn assembler_insertion(insertions in proptest::collection::vec(assembler::insertions(), 200)) {
            let mut assembler = Assembler::new(SeqNum::new(0));
            let mut num_insertions_performed = 0;
            let mut min_seq = SeqNum::new(WindowSize::MAX.into());
            let mut max_seq = SeqNum::new(0);
            for Range { start, end } in insertions {
                if min_seq.after(start) {
                    min_seq = start;
                }
                if max_seq.before(end) {
                    max_seq = end;
                }
                // assert that it's impossible to have more entries than the
                // number of insertions performed.
                prop_assert!(assembler.outstanding.len() <= num_insertions_performed);
                assembler.insert_inner(start..end);
                num_insertions_performed += 1;

                // assert that the ranges are sorted and don't overlap with
                // each other.
                for i in 1..assembler.outstanding.len() {
                    prop_assert!(
                        assembler.outstanding[i-1].range.end.before(assembler.outstanding[i].range.start)
                    );
                }
            }
            prop_assert_eq!(assembler.outstanding.first().unwrap().range.start, min_seq);
            prop_assert_eq!(assembler.outstanding.last().unwrap().range.end, max_seq);
        }

        #[test]
        fn ring_buffer_make_readable((mut rb, avail) in ring_buffer::with_written()) {
            let old_storage = rb.storage.clone();
            let old_head = rb.head;
            let old_len = rb.limits().len;
            rb.make_readable(avail, false);
            // Assert that length is updated but everything else is unchanged.
            let RingBuffer { storage, head, len } = rb;
            prop_assert_eq!(len, old_len + avail);
            prop_assert_eq!(head, old_head);
            prop_assert_eq!(storage, old_storage);
        }

        #[test]
        fn ring_buffer_write_at((mut rb, offset, data) in ring_buffer::with_offset_data()) {
            let old_head = rb.head;
            let old_len = rb.limits().len;
            prop_assert_eq!(rb.write_at(offset, &&data[..]), data.len());
            prop_assert_eq!(rb.head, old_head);
            prop_assert_eq!(rb.limits().len, old_len);
            for i in 0..data.len() {
                let masked = (rb.head + rb.len + offset + i) % rb.storage.len();
                // Make sure that data are written.
                prop_assert_eq!(rb.storage[masked], data[i]);
                rb.storage[masked] = 0;
            }
            // And the other parts of the storage are untouched.
            prop_assert_eq!(&rb.storage, &vec![0; rb.storage.len()]);
        }

        #[test]
        fn ring_buffer_read_with((mut rb, expected, consume) in ring_buffer::with_read_data()) {
            prop_assert_eq!(rb.limits().len, expected.len());
            let nread = rb.read_with(|readable| {
                assert!(readable.len() == 1 || readable.len() == 2);
                let got = readable.concat();
                assert_eq!(got, expected);
                consume
            });
            prop_assert_eq!(nread, consume);
            prop_assert_eq!(rb.limits().len, expected.len() - consume);
        }

        #[test]
        fn ring_buffer_mark_read((mut rb, readable) in ring_buffer::with_readable()) {
            const BYTE_TO_WRITE: u8 = 0x42;
            let written = rb.writable_regions().into_iter().fold(0, |acc, slice| {
                slice.fill(BYTE_TO_WRITE);
                acc + slice.len()
            });
            let old_storage = rb.storage.clone();
            let old_head = rb.head;
            let old_len = rb.limits().len;

            rb.mark_read(readable);
            let new_writable = rb.writable_regions().into_iter().fold(Vec::new(), |mut acc, slice| {
                acc.extend_from_slice(slice);
                acc
            });
            for (i, x) in new_writable.iter().enumerate().take(written) {
                prop_assert_eq!(*x, BYTE_TO_WRITE, "i={}, rb={:?}", i, rb);
            }
            prop_assert!(new_writable.len() >= written);

            let RingBuffer { storage, head, len } = rb;
            prop_assert_eq!(len, old_len - readable);
            prop_assert_eq!(head, (old_head + readable) % old_storage.len());
            prop_assert_eq!(storage, old_storage);
        }

        #[test]
        fn ring_buffer_peek_with((mut rb, expected, offset) in ring_buffer::with_read_data()) {
            prop_assert_eq!(rb.limits().len, expected.len());
            rb.peek_with(offset, |readable| {
                prop_assert_eq!(readable.to_vec(), &expected[offset..]);
                Ok(())
            })?;
            prop_assert_eq!(rb.limits().len, expected.len());
        }

        #[test]
        fn ring_buffer_writable_regions(mut rb in ring_buffer::arb_ring_buffer()) {
            const BYTE_TO_WRITE: u8 = 0x42;
            let writable_len = rb.writable_regions().into_iter().fold(0, |acc, slice| {
                slice.fill(BYTE_TO_WRITE);
                acc + slice.len()
            });
            let BufferLimits {len, capacity} = rb.limits();
            prop_assert_eq!(writable_len + len, capacity);
            for i in 0..capacity {
                let expected = if i < len {
                    0
                } else {
                    BYTE_TO_WRITE
                };
                let idx = (rb.head + i) % rb.storage.len();
                prop_assert_eq!(rb.storage[idx], expected);
            }
        }
    }

    #[test_case([Range { start: 0, end: 0 }]
        => Assembler { outstanding: vec![], nxt: SeqNum::new(0), generation: 0 })]
    #[test_case([Range { start: 0, end: 10 }]
        => Assembler { outstanding: vec![], nxt: SeqNum::new(10), generation: 1 })]
    #[test_case([Range{ start: 10, end: 15 }, Range { start: 5, end: 10 }]
        => Assembler {
            outstanding: vec![
                OutstandingBlock{ range: Range { start: SeqNum::new(5), end: SeqNum::new(15) }, generation: 2 },
            ],
            nxt: SeqNum::new(0),
            generation: 2,
        })
    ]
    #[test_case([Range{ start: 10, end: 15 }, Range { start: 0, end: 5 }, Range { start: 5, end: 10 }]
        => Assembler { outstanding: vec![], nxt: SeqNum::new(15), generation: 3 })]
    #[test_case([Range{ start: 10, end: 15 }, Range { start: 5, end: 10 }, Range { start: 0, end: 5 }]
        => Assembler { outstanding: vec![], nxt: SeqNum::new(15), generation: 3 })]
    fn assembler_examples(ops: impl IntoIterator<Item = Range<u32>>) -> Assembler {
        let mut assembler = Assembler::new(SeqNum::new(0));
        for Range { start, end } in ops.into_iter() {
            let _advanced = assembler.insert(SeqNum::new(start)..SeqNum::new(end));
        }
        assembler
    }

    #[test_case(&[] => Vec::<Range<u32>>::new(); "empty")]
    #[test_case(&[1..2] => vec![1..2]; "single")]
    #[test_case(&[1..2, 3..4] => vec![3..4, 1..2]; "latest first")]
    #[test_case(&[1..2, 3..4, 5..6, 7..8, 9..10]
        => vec![9..10, 7..8, 5..6, 3..4]; "max len")]
    #[test_case(&[1..2, 3..4, 5..6, 7..8, 9..10, 6..7]
        => vec![5..8, 9..10, 3..4, 1..2]; "gap fill")]
    #[test_case(&[1..2, 3..4, 5..6, 7..8, 9..10, 1..8]
        => vec![1..8, 9..10]; "large gap fill")]
    fn assembler_sack_blocks(ops: &[Range<u32>]) -> Vec<Range<u32>> {
        let mut assembler = Assembler::new(SeqNum::new(0));
        for Range { start, end } in ops {
            let _: usize = assembler.insert(SeqNum::new(*start)..SeqNum::new(*end));
        }
        assembler
            .sack_blocks()
            .iter()
            .map(|SackBlock(start, end)| Range { start: start.into(), end: end.into() })
            .collect()
    }

    #[test]
    // Regression test for https://fxbug.dev/42061342.
    fn ring_buffer_wrap_around() {
        const CAPACITY: usize = 16;
        let mut rb = RingBuffer::new(CAPACITY);

        // Write more than half the buffer.
        const BUF_SIZE: usize = 10;
        assert_eq!(rb.enqueue_data(&[0xAA; BUF_SIZE]), BUF_SIZE);
        rb.peek_with(0, |payload| {
            assert_eq!(payload, FragmentedPayload::new_contiguous(&[0xAA; BUF_SIZE]))
        });
        rb.mark_read(BUF_SIZE);

        // Write around the end of the buffer.
        assert_eq!(rb.enqueue_data(&[0xBB; BUF_SIZE]), BUF_SIZE);
        rb.peek_with(0, |payload| {
            assert_eq!(
                payload,
                FragmentedPayload::new([
                    &[0xBB; (CAPACITY - BUF_SIZE)],
                    &[0xBB; (BUF_SIZE * 2 - CAPACITY)]
                ])
            )
        });
        // Mark everything read, which should advance `head` around to the
        // beginning of the buffer.
        rb.mark_read(BUF_SIZE);

        // Now make a contiguous sequence of bytes readable.
        assert_eq!(rb.enqueue_data(&[0xCC; BUF_SIZE]), BUF_SIZE);
        rb.peek_with(0, |payload| {
            assert_eq!(payload, FragmentedPayload::new_contiguous(&[0xCC; BUF_SIZE]))
        });

        // Check that the unwritten bytes are left untouched. If `head` was
        // advanced improperly, this will crash.
        let read = rb.read_with(|segments| {
            assert_eq!(segments, [[0xCC; BUF_SIZE]]);
            BUF_SIZE
        });
        assert_eq!(read, BUF_SIZE);
    }

    #[test]
    fn ring_buffer_example() {
        let mut rb = RingBuffer::new(16);
        assert_eq!(rb.write_at(5, &"World".as_bytes()), 5);
        assert_eq!(rb.write_at(0, &"Hello".as_bytes()), 5);
        rb.make_readable(10, false);
        assert_eq!(
            rb.read_with(|readable| {
                assert_eq!(readable, &["HelloWorld".as_bytes()]);
                5
            }),
            5
        );
        assert_eq!(
            rb.read_with(|readable| {
                assert_eq!(readable, &["World".as_bytes()]);
                readable[0].len()
            }),
            5
        );
        assert_eq!(rb.write_at(0, &"HelloWorld".as_bytes()), 10);
        rb.make_readable(10, false);
        assert_eq!(
            rb.read_with(|readable| {
                assert_eq!(readable, &["HelloW".as_bytes(), "orld".as_bytes()]);
                6
            }),
            6
        );
        assert_eq!(rb.limits().len, 4);
        assert_eq!(
            rb.read_with(|readable| {
                assert_eq!(readable, &["orld".as_bytes()]);
                4
            }),
            4
        );
        assert_eq!(rb.limits().len, 0);

        assert_eq!(rb.enqueue_data("Hello".as_bytes()), 5);
        assert_eq!(rb.limits().len, 5);

        let () = rb.peek_with(3, |readable| {
            assert_eq!(readable.to_vec(), "lo".as_bytes());
        });

        rb.mark_read(2);

        let () = rb.peek_with(0, |readable| {
            assert_eq!(readable.to_vec(), "llo".as_bytes());
        });
    }

    mod assembler {
        use super::*;
        pub(super) fn insertions() -> impl Strategy<Value = Range<SeqNum>> {
            (0..u32::from(WindowSize::MAX)).prop_flat_map(|start| {
                (start + 1..=u32::from(WindowSize::MAX)).prop_flat_map(move |end| {
                    Just(Range { start: SeqNum::new(start), end: SeqNum::new(end) })
                })
            })
        }
    }

    mod ring_buffer {
        use super::*;
        // Use a small capacity so that we have a higher chance to exercise
        // wrapping around logic.
        const MAX_CAP: usize = 32;

        fn arb_ring_buffer_args() -> impl Strategy<Value = (usize, usize, usize)> {
            // Use a small capacity so that we have a higher chance to exercise
            // wrapping around logic.
            (1..=MAX_CAP).prop_flat_map(|cap| {
                let max_len = cap;
                //  cap      head     len
                (Just(cap), 0..cap, 0..=max_len)
            })
        }

        pub(super) fn arb_ring_buffer() -> impl Strategy<Value = RingBuffer> {
            arb_ring_buffer_args().prop_map(|(cap, head, len)| RingBuffer {
                storage: vec![0; cap],
                head,
                len,
            })
        }

        /// A strategy for a [`RingBuffer`] and a valid length to mark read.
        pub(super) fn with_readable() -> impl Strategy<Value = (RingBuffer, usize)> {
            arb_ring_buffer_args().prop_flat_map(|(cap, head, len)| {
                (Just(RingBuffer { storage: vec![0; cap], head, len }), 0..=len)
            })
        }

        /// A strategy for a [`RingBuffer`] and a valid length to make readable.
        pub(super) fn with_written() -> impl Strategy<Value = (RingBuffer, usize)> {
            arb_ring_buffer_args().prop_flat_map(|(cap, head, len)| {
                let rb = RingBuffer { storage: vec![0; cap], head, len };
                let max_written = cap - len;
                (Just(rb), 0..=max_written)
            })
        }

        /// A strategy for a [`RingBuffer`], a valid offset and data to write.
        pub(super) fn with_offset_data() -> impl Strategy<Value = (RingBuffer, usize, Vec<u8>)> {
            arb_ring_buffer_args().prop_flat_map(|(cap, head, len)| {
                let writable_len = cap - len;
                (0..=writable_len).prop_flat_map(move |offset| {
                    (0..=writable_len - offset).prop_flat_map(move |data_len| {
                        (
                            Just(RingBuffer { storage: vec![0; cap], head, len }),
                            Just(offset),
                            proptest::collection::vec(1..=u8::MAX, data_len),
                        )
                    })
                })
            })
        }

        /// A strategy for a [`RingBuffer`], its readable data, and how many
        /// bytes to consume.
        pub(super) fn with_read_data() -> impl Strategy<Value = (RingBuffer, Vec<u8>, usize)> {
            arb_ring_buffer_args().prop_flat_map(|(cap, head, len)| {
                proptest::collection::vec(1..=u8::MAX, len).prop_flat_map(move |data| {
                    // Fill the RingBuffer with the data.
                    let mut rb = RingBuffer { storage: vec![0; cap], head, len: 0 };
                    assert_eq!(rb.write_at(0, &&data[..]), len);
                    rb.make_readable(len, false);
                    (Just(rb), Just(data), 0..=len)
                })
            })
        }
    }
}
