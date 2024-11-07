// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use crate::logs::stats::LogStreamStats;
use crate::logs::stored_message::StoredMessage;
use derivative::Derivative;
use diagnostics_log_encoding::{Header, TRACING_FORMAT_LOG_RECORD_TYPE};
use fidl_fuchsia_diagnostics::StreamMode;
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_async::condition::{Condition, WakerEntry};
use futures::Stream;
use pin_project::{pin_project, pinned_drop};
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing::debug;
use zerocopy::FromBytes;
use zx::AsHandleRef as _;

// The buffer needs to be big enough for at least 1 message which is MAX_DATAGRAM_LEN_BYTES (32768)
// bytes, but we allow up to 65536 bytes.
const MIN_BUFFER_SIZE: usize = (MAX_DATAGRAM_LEN_BYTES * 2) as usize;
const FXT_HEADER_SIZE: usize = 8;

pub type OnNoLogs = Box<dyn Fn(Arc<ComponentIdentity>) + Send + Sync>;

pub struct SharedBuffer {
    inner: Condition<Inner>,

    // Callback for when a container has no logs.
    on_no_logs: OnNoLogs,
}

struct Inner {
    // The underlying buffer.
    buffer: Buffer,

    // Head sequence number.  It's u64, so we don't worry about wrapping.  The offset in the buffer
    // is this modulo buffer length.
    head: u64,

    // Tail sequence number.
    tail: u64,

    // Registered containers.
    containers: Containers,
}

impl SharedBuffer {
    pub fn new(capacity: usize, on_no_logs: OnNoLogs) -> Self {
        Self {
            inner: Condition::new(Inner {
                buffer: Buffer::new(capacity),
                head: 0,
                tail: 0,
                containers: Containers::new(),
            }),
            on_no_logs,
        }
    }

    pub fn new_container_buffer(
        self: &Arc<Self>,
        identity: Arc<ComponentIdentity>,
        stats: Arc<LogStreamStats>,
    ) -> ContainerBuffer {
        ContainerBuffer {
            shared_buffer: Arc::clone(self),
            container_id: self.inner.lock().containers.new_container(identity, stats),
        }
    }

    /// Returns the number of registered containers in use by the buffer.
    #[cfg(test)]
    pub fn container_count(&self) -> usize {
        self.inner.lock().containers.len()
    }
}

impl Inner {
    // Returns list of containers that no longer have any messages.
    #[must_use]
    fn ingest(&mut self, msg: &[u8], container_id: ContainerId) -> Vec<Arc<ComponentIdentity>> {
        if msg.len() < FXT_HEADER_SIZE {
            debug!("message too short ({})", msg.len());
            if let Some(container) = self.containers.get(container_id) {
                container.stats.increment_invalid(msg.len());
            }
            return vec![];
        }

        let header = Header(u64::read_from_bytes(&msg[..FXT_HEADER_SIZE]).unwrap());

        // NOTE: Some tests send messages that are bigger than the header indicates.  We ignore the
        // tail of any such messages.
        let msg_len = header.size_words() as usize * 8;

        // Check the correct type and size.
        if header.raw_type() != TRACING_FORMAT_LOG_RECORD_TYPE || msg.len() < msg_len {
            debug!("bad type or size ({}, {}, {})", header.raw_type(), msg_len, msg.len());
            if let Some(container) = self.containers.get(container_id) {
                container.stats.increment_invalid(msg.len());
            }
            return vec![];
        }

        let mut on_no_logs = Vec::new();

        // Make sure there's space.
        let mut space = self.space();
        let total_len = msg_len + FXT_HEADER_SIZE;

        while space < total_len {
            space += self.pop(&mut on_no_logs).expect("No messages in buffer!");
        }

        let Some(container_info) = self.containers.get_mut(container_id) else {
            return vec![];
        };

        if container_info.msg_ids.end == container_info.msg_ids.start {
            container_info.first_index = self.head;
        }

        let msg_id = container_info.msg_ids.end;
        container_info.msg_ids.end += 1;

        let (container_msg_id, remaining) =
            u64::mut_from_prefix(self.buffer.slice_from_index_mut(self.head)).unwrap();
        *container_msg_id = (container_id.0 as u64) << 32 | (msg_id & 0xffff_ffff);
        remaining[..msg_len].copy_from_slice(&msg[..msg_len]);

        self.head += total_len as u64;

        on_no_logs
    }

    fn space(&self) -> usize {
        self.buffer.len() - (self.head - self.tail) as usize
    }

    /// Pops a message and returns its total size.
    ///
    /// NOTE: This will pop the oldest message in terms of when it was inserted which is *not*
    /// necessarily the message with the *oldest* timestamp because we might not have received the
    /// messages in perfect timestamp order.  This should be close enough for all use cases we care
    /// about, and besides, the timestamps can't be trusted anyway.
    fn pop(&mut self, on_no_logs: &mut Vec<Arc<ComponentIdentity>>) -> Option<usize> {
        if self.head == self.tail {
            return None;
        }
        let (container_id, msg_id, record, timestamp) = self.parse_record(self.tail);
        let total_len = record.len() + FXT_HEADER_SIZE;
        self.tail += total_len as u64;
        let container = self.containers.get_mut(container_id).unwrap();
        container.stats.increment_rolled_out(total_len);
        assert_eq!(container.msg_ids.start as u32, msg_id);
        container.msg_ids.start += 1;
        if let Some(timestamp) = timestamp {
            container.last_rolled_out_timestamp = timestamp;
        }
        if container.msg_ids.end == container.msg_ids.start {
            if container.should_free() {
                self.containers.free(container_id.0);
            } else {
                on_no_logs.push(Arc::clone(&container.identity));
            }
        }
        Some(total_len)
    }

    /// Parses the record and returns (container_id, msg_id, message, timestamp).
    fn parse_record(&self, index: u64) -> (ContainerId, u32, &[u8], Option<zx::BootInstant>) {
        let buf = self.buffer.slice_from_index(index);
        let (container_msg_id, msg) = u64::read_from_prefix(buf).unwrap();
        let (header, remainder) = u64::read_from_prefix(msg).unwrap();
        let header = Header(header);
        let record_len = header.size_words() as usize * 8;
        (
            ContainerId((container_msg_id >> 32) as u32),
            container_msg_id as u32,
            &msg[..record_len],
            i64::read_from_prefix(remainder).map(|(t, _)| zx::BootInstant::from_nanos(t)).ok(),
        )
    }
}

struct Containers {
    slab: Vec<Slot<ContainerInfo>>,
    free_index: usize, // usize::MAX if there are no free entries.
}

#[derive(Clone, Copy, Debug)]
struct ContainerId(u32);

impl Containers {
    fn new() -> Self {
        Self { slab: Vec::new(), free_index: usize::MAX }
    }

    fn get(&self, id: ContainerId) -> Option<&ContainerInfo> {
        self.slab.get(id.0 as usize).and_then(|c| match c {
            Slot::Used(c) => Some(c),
            _ => None,
        })
    }

    fn get_mut(&mut self, id: ContainerId) -> Option<&mut ContainerInfo> {
        self.slab.get_mut(id.0 as usize).and_then(|c| match c {
            Slot::Used(c) => Some(c),
            _ => None,
        })
    }

    fn free(&mut self, index: u32) {
        let index = index as usize;
        assert!(matches!(
            std::mem::replace(&mut self.slab[index], Slot::Free(self.free_index)),
            Slot::Used(_)
        ));
        self.free_index = index;
    }

    fn new_container(
        &mut self,
        identity: Arc<ComponentIdentity>,
        stats: Arc<LogStreamStats>,
    ) -> ContainerId {
        let free_index = self.free_index;
        ContainerId(if free_index != usize::MAX {
            self.free_index = match std::mem::replace(
                &mut self.slab[free_index],
                Slot::Used(ContainerInfo::new(identity, stats)),
            ) {
                Slot::Free(next) => next,
                _ => unreachable!(),
            };
            free_index as u32
        } else {
            assert!(self.slab.len() <= u32::MAX as usize);
            self.slab.push(Slot::Used(ContainerInfo::new(identity, stats)));
            (self.slab.len() - 1) as u32
        })
    }

    /// Returns the number of registered containers.  This is not performant.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slab.iter().filter(|c| matches!(c, Slot::Used(_))).count()
    }
}

enum Slot<T> {
    Used(T),
    Free(usize),
}

#[derive(Derivative)]
#[derivative(Debug)]
struct ContainerInfo {
    // The number of references that prevent this object from being freed.
    refs: usize,

    // The identity of the container.
    identity: Arc<ComponentIdentity>,

    // The first index in the shared buffer for a message for this container.  This index
    // might have been rolled out.  This is used as an optimisation to set the starting
    // cursor position.
    first_index: u64,

    // The first and last message IDs stored in the shared buffer.
    msg_ids: Range<u64>,

    // Whether the container is terminated.
    terminated: bool,

    // Inspect instrumentation.
    #[derivative(Debug = "ignore")]
    stats: Arc<LogStreamStats>,

    // The timestamp of the message most recently rolled out.
    last_rolled_out_timestamp: zx::BootInstant,
}

impl ContainerInfo {
    fn new(identity: Arc<ComponentIdentity>, stats: Arc<LogStreamStats>) -> Self {
        Self {
            refs: 0,
            identity,
            first_index: 0,
            msg_ids: 0..0,
            terminated: false,
            stats,
            last_rolled_out_timestamp: zx::BootInstant::ZERO,
        }
    }

    fn should_free(&self) -> bool {
        self.terminated && self.refs == 0 && self.msg_ids.start == self.msg_ids.end
    }
}

pub struct ContainerBuffer {
    shared_buffer: Arc<SharedBuffer>,
    container_id: ContainerId,
}

impl ContainerBuffer {
    /// Ingests a new message.
    ///
    /// If the message is invalid, it is dropped.
    pub fn push_back(&self, msg: &[u8]) {
        let on_no_logs;

        {
            let mut inner = self.shared_buffer.inner.lock();

            on_no_logs = inner.ingest(msg, self.container_id);

            for waker in inner.drain_wakers() {
                waker.wake();
            }
        }

        for identity in on_no_logs {
            (*self.shared_buffer.on_no_logs)(identity);
        }
    }

    /// Returns a cursor.
    pub fn cursor(&self, mode: StreamMode) -> Option<Cursor> {
        let mut inner = self.shared_buffer.inner.lock();
        let head = inner.head;
        let Some(container) = inner.containers.get_mut(self.container_id) else {
            // We've hit a race where the container has terminated.
            return None;
        };

        container.refs += 1;

        let (index, next_id, end_id) = match mode {
            StreamMode::Snapshot => {
                (container.first_index, container.msg_ids.start, Some(container.msg_ids.end))
            }
            StreamMode::Subscribe => (head, container.msg_ids.end, None),
            StreamMode::SnapshotThenSubscribe => {
                (container.first_index, container.msg_ids.start, None)
            }
        };

        Some(Cursor {
            index,
            container_id: self.container_id,
            next_id,
            end_id,
            buffer: Arc::clone(&self.shared_buffer),
            waker_entry: WakerEntry::new(),
            stats: Arc::clone(&container.stats),
        })
    }

    /// Marks the buffer as terminated which will force all cursors to end.  The component's data
    /// will remain in the buffer until the messages are rolled out.
    pub fn terminate(&self) {
        let mut inner = self.shared_buffer.inner.lock();
        if let Some(container) = inner.containers.get_mut(self.container_id) {
            container.terminated = true;
            if container.should_free() {
                inner.containers.free(self.container_id.0);
            }
            for waker in inner.drain_wakers() {
                waker.wake();
            }
        }
    }

    /// Returns true if the container has no messages.
    pub fn is_empty(&self) -> bool {
        self.shared_buffer
            .inner
            .lock()
            .containers
            .get(self.container_id)
            .map_or(true, |c| c.msg_ids.start == c.msg_ids.end)
    }
}

impl Drop for ContainerBuffer {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[pin_project(PinnedDrop)]
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Cursor {
    // Index in the buffer that we should continue searching from.
    index: u64,

    container_id: ContainerId,

    // The next expected message ID.
    next_id: u64,

    // The ID that we should end at (exclusive).
    end_id: Option<u64>,

    #[derivative(Debug = "ignore")]
    buffer: Arc<SharedBuffer>,

    // waker_entry is used to register a waker for subscribing cursors.
    #[pin]
    #[derivative(Debug = "ignore")]
    waker_entry: WakerEntry<Inner>,

    #[derivative(Debug = "ignore")]
    stats: Arc<LogStreamStats>,
}

/// The next element in the stream or a marker of the number of items rolled out since last polled.
#[derive(Debug, PartialEq)]
pub enum LazyItem<T> {
    /// The next item in the stream.
    Next(Arc<T>),
    /// A count of the items dropped between the last call to poll_next and this one.
    ItemsRolledOut(u64, zx::BootInstant),
}

impl Stream for Cursor {
    type Item = LazyItem<StoredMessage>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.end_id.map_or(false, |id| self.next_id == id) {
            return Poll::Ready(None);
        }

        let this = self.project();

        let mut inner = this.buffer.inner.lock();

        let container = match inner.containers.get(*this.container_id) {
            None => return Poll::Ready(None),
            Some(container) => container,
        };

        // See if messages have been rolled out.
        if container.msg_ids.start > *this.next_id {
            let mut next_id = container.msg_ids.start;
            if let Some(end_id) = *this.end_id {
                if end_id < next_id {
                    next_id = end_id;
                }
            }
            let rolled_out = next_id - *this.next_id;
            *this.next_id = next_id;
            return Poll::Ready(Some(LazyItem::ItemsRolledOut(
                rolled_out,
                container.last_rolled_out_timestamp,
            )));
        }

        if inner.tail > *this.index {
            *this.index = inner.tail;
        }

        // Some optimisations to reduce the amount of searching we might have to do.
        if container.first_index > *this.index {
            *this.index = container.first_index;
        }
        if *this.next_id == container.msg_ids.end {
            *this.index = inner.head;
        } else {
            // Scan forward until we find a record matching this container.
            while *this.index < inner.head {
                let (container_id, msg_id, record, _timestamp) = inner.parse_record(*this.index);
                let record_len = record.len();

                // Move index to the next record.
                *this.index += record_len as u64 + 8;
                assert!(*this.index <= inner.head);

                if container_id.0 == this.container_id.0 {
                    assert_eq!(*this.next_id as u32, msg_id);
                    *this.next_id += 1;
                    if let Some(msg) = StoredMessage::new(record.into(), this.stats) {
                        return Poll::Ready(Some(LazyItem::Next(Arc::new(msg))));
                    } else {
                        // The message is corrupt.  Just skip it.
                    }
                }
            }
        }

        if container.terminated {
            Poll::Ready(None)
        } else {
            inner.add_waker(this.waker_entry, cx.waker().clone());
            Poll::Pending
        }
    }
}

#[pinned_drop]
impl PinnedDrop for Cursor {
    fn drop(self: Pin<&mut Self>) {
        let mut inner = self.buffer.inner.lock();
        if let Some(container) = inner.containers.get_mut(self.container_id) {
            container.refs -= 1;
            if container.should_free() {
                inner.containers.free(self.container_id.0);
            }
        }
    }
}

/// Buffer wraps a vmo and mapping for the VMO.
struct Buffer {
    _vmo: zx::Vmo,
    _vmar: zx::Vmar,
    base: usize,
    len: usize,
}

impl Buffer {
    fn new(capacity: usize) -> Self {
        let capacity = std::cmp::max(
            MIN_BUFFER_SIZE,
            capacity.next_multiple_of(zx::system_get_page_size() as usize),
        );

        let vmo = zx::Vmo::create(capacity as u64).unwrap();
        let name = zx::Name::new("LogBuffer").unwrap();
        vmo.set_name(&name).unwrap();
        let root_vmar = fuchsia_runtime::vmar_root_self();

        // Allocate a buffer but repeat the mapping for the first 64 KiB of the buffer at the end
        // which makes dealing with wrapping much easier.  NOTE: dropping the vmar will drop the
        // mappings.
        let (vmar, base) = root_vmar
            .allocate(
                0,
                capacity + 65536,
                zx::VmarFlags::CAN_MAP_READ
                    | zx::VmarFlags::CAN_MAP_WRITE
                    | zx::VmarFlags::CAN_MAP_SPECIFIC,
            )
            .unwrap();
        vmar.map(
            /* vmar_offset */ 0,
            &vmo,
            /* vmo_offset */ 0,
            /* len */ capacity,
            zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE | zx::VmarFlags::SPECIFIC,
        )
        .unwrap();
        vmar.map(
            /* vmar_offset */ capacity,
            &vmo,
            /* vmo_offset */ 0,
            /* len */ 65536,
            zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE | zx::VmarFlags::SPECIFIC,
        )
        .unwrap();
        Self { _vmo: vmo, _vmar: vmar, base, len: capacity }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Returns a 64 KiB slice at the specified index.  The slice is valid even where the index
    /// involves wrapping.
    fn slice_from_index(&self, index: u64) -> &[u8] {
        let index = index as usize % self.len;
        // SAFETY: Safe because we mapped this range.
        unsafe { std::slice::from_raw_parts((self.base + index) as *const u8, 65536) }
    }

    /// Returns a 64 KiB mutable slice at the specified index.  The slice is valid even where the
    /// index involves wrapping.
    fn slice_from_index_mut(&mut self, index: u64) -> &mut [u8] {
        let index = index as usize % self.len;
        // SAFETY: Safe because we mapped this range.
        unsafe { std::slice::from_raw_parts_mut((self.base + index) as *mut u8, 65536) }
    }
}

#[cfg(test)]
mod tests {
    use super::SharedBuffer;
    use crate::logs::shared_buffer::LazyItem;
    use crate::logs::testing::make_message;
    use assert_matches::assert_matches;
    use fidl_fuchsia_diagnostics::StreamMode;
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use futures::poll;
    use futures::stream::StreamExt as _;
    use std::pin::pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[fuchsia::test]
    async fn push_one_message() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_buffer.push_back(msg.bytes());

        // Make sure the cursor can find it.
        let cursor = container_buffer.cursor(StreamMode::Snapshot).unwrap();
        assert_eq!(
            cursor
                .map(|item| {
                    match item {
                        LazyItem::Next(item) => assert_eq!(item.bytes(), msg.bytes()),
                        _ => panic!("Unexpected item {item:?}"),
                    }
                })
                .count()
                .await,
            1
        );
    }

    #[fuchsia::test]
    async fn message_too_short() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));

        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn bad_type() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0x77; 16]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn message_truncated() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_buffer.push_back(&msg.bytes()[..msg.bytes().len() - 1]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn buffer_wrapping() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());

        // Create one message so we can figure out what the metadata costs.
        let msg1 = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_buffer.push_back(msg1.bytes());

        // The maximum message size is 32760 because size words must fit in 11 bits, so that's 4095
        // * 8.
        let delta = 32760 - msg1.bytes().len();
        let msg2 = make_message(
            std::str::from_utf8(&vec![66; 1 + delta]).unwrap(),
            None,
            zx::BootInstant::from_nanos(2),
        );
        container_buffer.push_back(msg2.bytes());

        // And one last message to take us up to offset 65528 in the buffer.
        let delta = 65528 - 2 * (msg1.bytes().len() + 8) - (msg2.bytes().len() + 8);
        let msg3 = make_message(
            std::str::from_utf8(&vec![67; 1 + delta]).unwrap(),
            None,
            zx::BootInstant::from_nanos(3),
        );
        container_buffer.push_back(msg3.bytes());

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 3);

        // The next message we write should wrap, and will cause the first message to be dropped.
        let msg4 = make_message("d", None, zx::BootInstant::from_nanos(4));
        container_buffer.push_back(msg4.bytes());

        let mut expected = [msg2.bytes(), msg3.bytes(), msg4.bytes()].into_iter();
        assert_eq!(
            container_buffer
                .cursor(StreamMode::Snapshot)
                .unwrap()
                .map(|item| match item {
                    LazyItem::Next(item) => assert_eq!(item.bytes(), expected.next().unwrap()),
                    _ => panic!("Unexpected item {item:?}"),
                })
                .count()
                .await,
            3
        );
    }

    #[fuchsia::test]
    async fn on_no_logs() {
        let identity = Arc::new(vec!["a"].into());
        let on_no_logs = Arc::new(AtomicU64::new(0));
        let buffer = {
            let on_no_logs = Arc::clone(&on_no_logs);
            let identity = Arc::clone(&identity);
            Arc::new(SharedBuffer::new(
                65536,
                Box::new(move |i| {
                    assert_eq!(i, identity);
                    on_no_logs.fetch_add(1, Ordering::Relaxed);
                }),
            ))
        };
        let container_a = buffer.new_container_buffer(identity, Arc::default());
        let container_b = buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default());

        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_a.push_back(msg.bytes());

        // Repeatedly write messages to b until a is rolled out.
        while container_a.cursor(StreamMode::Snapshot).unwrap().count().await == 1 {
            container_b.push_back(msg.bytes());
        }

        assert_eq!(on_no_logs.load(Ordering::Relaxed), 1);
    }

    #[fuchsia::test]
    fn terminate_drops_container() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));

        // terminate when buffer has no logs.
        let container_a = buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        assert_eq!(buffer.container_count(), 1);
        container_a.terminate();

        assert_eq!(buffer.container_count(), 0);

        // terminate when buffer has logs.
        let container_a = buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_a.push_back(msg.bytes());
        assert_eq!(buffer.container_count(), 1);
        container_a.terminate();

        // The container should still be there because it has logs.
        assert_eq!(buffer.container_count(), 1);

        // Roll out the logs.
        let container_b = buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default());
        assert_eq!(buffer.container_count(), 2);

        // Repeatedly write messages to b until a's message is dropped and then the container will
        // be dropped.
        while buffer.container_count() != 1 {
            container_b.push_back(msg.bytes());
        }

        assert!(container_a.cursor(StreamMode::Subscribe).is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn cursor_subscribe() {
        for mode in [StreamMode::Subscribe, StreamMode::SnapshotThenSubscribe] {
            let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
            let container =
                Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
            let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
            container.push_back(msg.bytes());

            let (sender, mut receiver) = mpsc::unbounded();

            // Run the cursor in a separate task so that we can test it gets woken correctly.
            {
                let container = Arc::clone(&container);
                fasync::Task::spawn(async move {
                    let mut cursor = pin!(container.cursor(mode).unwrap());
                    while let Some(item) = cursor.next().await {
                        sender.unbounded_send(item).unwrap();
                    }
                })
                .detach();
            }

            // The existing message should only be returned with SnapshotThenSubscribe
            if mode == StreamMode::SnapshotThenSubscribe {
                assert_matches!(
                    receiver.next().await,
                    Some(LazyItem::Next(item)) if item.bytes() == msg.bytes()
                );
            }

            assert!(fasync::TestExecutor::poll_until_stalled(receiver.next()).await.is_pending());

            container.push_back(msg.bytes());

            // The message should arrive now.
            assert_matches!(
                receiver.next().await,
                Some(LazyItem::Next(item)) if item.bytes() == msg.bytes()
            );

            container.terminate();

            // The cursor should terminate now.
            assert!(receiver.next().await.is_none());
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn cursor_rolled_out() {
        // On the first pass we roll out before the cursor has started.  On the second pass, we roll
        // out after the cursor has started.
        for pass in 0..2 {
            let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
            let container_a =
                Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
            let container_b =
                Arc::new(buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default()));
            let container_c =
                Arc::new(buffer.new_container_buffer(Arc::new(vec!["c"].into()), Arc::default()));
            let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

            container_a.push_back(msg.bytes());
            container_a.push_back(msg.bytes());
            container_b.push_back(msg.bytes());
            container_a.push_back(msg.bytes());

            let mut cursor = pin!(container_a.cursor(StreamMode::Snapshot).unwrap());

            // Get the first stored message on the first pass.
            if pass == 0 {
                assert!(cursor.next().await.is_some());
            }

            // Roll out messages until container_b is rolled out.
            while container_b.cursor(StreamMode::Snapshot).unwrap().count().await == 1 {
                container_c.push_back(msg.bytes());
            }

            // We should have rolled out the second message in container a.
            assert_matches!(
                cursor.next().await,
                Some(LazyItem::ItemsRolledOut(rolled_out, t))
                    if t == zx::BootInstant::from_nanos(1) && rolled_out == pass + 1
            );

            // And there should be one more message remaining.
            assert_eq!(cursor.count().await, 1);
        }
    }

    #[fuchsia::test]
    async fn drained_post_termination_cursors() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

        let mut cursor_a = pin!(container.cursor(StreamMode::Subscribe).unwrap());
        let mut cursor_b = pin!(container.cursor(StreamMode::SnapshotThenSubscribe).unwrap());

        container.push_back(msg.bytes());
        container.push_back(msg.bytes());
        container.push_back(msg.bytes());
        container.push_back(msg.bytes());
        container.push_back(msg.bytes());

        let mut cursor_c = pin!(container.cursor(StreamMode::Snapshot).unwrap());
        assert!(cursor_a.next().await.is_some());
        assert!(cursor_b.next().await.is_some());
        assert!(cursor_c.next().await.is_some());

        container.terminate();

        // All cursors should return the 4 remaining messages.
        assert_eq!(cursor_a.count().await, 4);
        assert_eq!(cursor_b.count().await, 4);
        assert_eq!(cursor_c.count().await, 4);
    }

    #[fuchsia::test]
    async fn empty_post_termination_cursors() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));

        let cursor_a = container.cursor(StreamMode::Subscribe).unwrap();
        let cursor_b = container.cursor(StreamMode::SnapshotThenSubscribe).unwrap();
        let cursor_c = container.cursor(StreamMode::Snapshot).unwrap();

        container.terminate();

        assert_eq!(cursor_a.count().await, 0);
        assert_eq!(cursor_b.count().await, 0);
        assert_eq!(cursor_c.count().await, 0);
    }

    #[fuchsia::test]
    async fn snapshot_then_subscribe_works_when_only_dropped_notifications_are_returned() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_a =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
        let container_b =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_a.push_back(msg.bytes());
        container_a.push_back(msg.bytes());
        container_a.push_back(msg.bytes());
        let mut cursor = pin!(container_a.cursor(StreamMode::SnapshotThenSubscribe).unwrap());

        // Roll out all the messages.
        while container_a.cursor(StreamMode::Snapshot).unwrap().count().await > 0 {
            container_b.push_back(msg.bytes());
        }

        assert_matches!(cursor.next().await, Some(LazyItem::ItemsRolledOut(3, _)));

        assert!(poll!(cursor.next()).is_pending());

        container_a.terminate();
        assert_eq!(cursor.count().await, 0);
    }

    #[fuchsia::test]
    async fn recycled_container_slot() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_a =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_a.push_back(msg.bytes());

        let mut cursor = pin!(container_a.cursor(StreamMode::SnapshotThenSubscribe).unwrap());
        assert_matches!(cursor.next().await, Some(LazyItem::Next(_)));

        // Roll out all the messages.
        let container_b =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default()));
        while container_a.cursor(StreamMode::Snapshot).unwrap().count().await > 0 {
            container_b.push_back(msg.bytes());
        }

        container_a.terminate();

        // This should create a new container that uses a new slot and shouldn't interfere with
        // container_a.
        let container_c =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["c"].into()), Arc::default()));
        container_c.push_back(msg.bytes());
        container_c.push_back(msg.bytes());

        // The original cursor should have finished.
        assert_matches!(cursor.next().await, None);
    }
}
