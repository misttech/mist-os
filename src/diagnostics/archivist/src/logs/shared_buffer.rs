// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use crate::logs::stats::LogStreamStats;
use crate::logs::stored_message::StoredMessage;
use derivative::Derivative;
use diagnostics_log_encoding::{Header, FXT_HEADER_SIZE, TRACING_FORMAT_LOG_RECORD_TYPE};
use fidl_fuchsia_diagnostics::StreamMode;
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_async as fasync;
use fuchsia_async::condition::{Condition, WakerEntry};
use fuchsia_sync::Mutex;
use futures::Stream;
use log::debug;
use pin_project::{pin_project, pinned_drop};
use std::ops::{Deref, DerefMut, Range};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use zerocopy::FromBytes;
use zx::AsHandleRef as _;

// The buffer needs to be big enough for at least 1 message which is MAX_DATAGRAM_LEN_BYTES (32768)
// bytes, but we allow up to 65536 bytes.
const MIN_BUFFER_SIZE: usize = (MAX_DATAGRAM_LEN_BYTES * 2) as usize;

pub type OnInactive = Box<dyn Fn(Arc<ComponentIdentity>) + Send + Sync>;

pub struct SharedBuffer {
    inner: Condition<InnerAndSockets>,

    // Callback for when a container is inactive (i.e. no logs and no sockets).
    on_inactive: OnInactive,

    // The port that we use to service the sockets.
    port: zx::Port,

    // Event used for termination.
    event: zx::Event,

    // The thread that we use to service the sockets.
    socket_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

// We have to do this to avoid "cannot borrow as mutable because it is also borrowed as immutable"
// errors.
struct InnerAndSockets(Inner, Slab<Socket>);

impl Deref for InnerAndSockets {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl DerefMut for InnerAndSockets {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.0
    }
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
    pub fn new(capacity: usize, on_inactive: OnInactive) -> Arc<Self> {
        let this = Arc::new(Self {
            inner: Condition::new(InnerAndSockets(
                Inner {
                    buffer: Buffer::new(capacity),
                    head: 0,
                    tail: 0,
                    containers: Containers::default(),
                },
                Slab::default(),
            )),
            on_inactive,
            port: zx::Port::create(),
            event: zx::Event::create(),
            socket_thread: Mutex::default(),
        });
        *this.socket_thread.lock() = Some({
            let this = Arc::clone(&this);
            let ehandle = fasync::EHandle::local();
            std::thread::spawn(move || this.socket_thread(ehandle))
        });
        this
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
        self.inner.lock().0.containers.len()
    }

    /// Terminates the socket thread.  The socket thread will drain the sockets before terminating.
    pub async fn terminate(&self) {
        self.event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        let join_handle = self.socket_thread.lock().take().unwrap();
        fasync::unblock(|| {
            let _ = join_handle.join();
        })
        .await;
    }

    fn socket_thread(&self, ehandle: fasync::EHandle) {
        let mut sockets_ready = Vec::new();

        // Register event used for termination.
        const TERMINATE_KEY: u64 = u64::MAX;
        self.event
            .wait_async_handle(
                &self.port,
                TERMINATE_KEY,
                zx::Signals::USER_0,
                zx::WaitAsyncOpts::empty(),
            )
            .unwrap();

        let mut terminate = false;
        let mut finished = false;
        let mut on_inactive = OnInactiveNotifier::new(self);

        while !finished {
            let mut deadline = if terminate {
                // Run through the sockets one last time.
                finished = true;
                zx::MonotonicInstant::INFINITE_PAST
            } else {
                // Wait for 200ms so that we're not constantly waking up for every log message that
                // is queued.  Ignore errors here.  We only do this on release builds.
                #[cfg(not(debug_assertions))]
                let _ = self.event.wait_handle(
                    zx::Signals::USER_0,
                    zx::MonotonicInstant::after(zx::Duration::from_millis(200)),
                );

                zx::MonotonicInstant::INFINITE
            };

            // Gather the list of sockets that are ready to read.
            loop {
                match self.port.wait(deadline) {
                    Ok(packet) => {
                        if packet.key() == TERMINATE_KEY {
                            terminate = true;
                        } else {
                            sockets_ready.push(SocketId(packet.key() as u32))
                        }
                    }
                    Err(zx::Status::TIMED_OUT) => break,
                    Err(status) => panic!("port wait error: {status:?}"),
                }
                deadline = zx::MonotonicInstant::INFINITE_PAST;
            }

            let mut inner = self.inner.lock();

            {
                let InnerAndSockets(inner, sockets) = &mut *inner;

                for socket_id in sockets_ready.drain(..) {
                    let Some(socket) = sockets.get(socket_id.0) else {
                        // Spurious packet for a removed socket.
                        continue;
                    };

                    if inner.read_socket(socket, &mut on_inactive) {
                        let container_id = socket.container_id;
                        if let Some(container) = inner.containers.get_mut(container_id) {
                            container.remove_socket(socket_id, sockets);
                            if !container.is_active() {
                                if container.should_free() {
                                    inner.containers.free(container_id);
                                } else {
                                    on_inactive.push(&container.identity);
                                }
                            }
                        }
                    } else {
                        socket
                            .socket
                            .wait_async_handle(
                                &self.port,
                                socket_id.0 as u64,
                                zx::Signals::OBJECT_READABLE | zx::Signals::OBJECT_PEER_CLOSED,
                                zx::WaitAsyncOpts::empty(),
                            )
                            .unwrap();
                    }
                }
            }

            let wake_tasks = || {
                for waker in inner.drain_wakers() {
                    waker.wake();
                }

                std::mem::drop(inner);

                on_inactive.notify();
            };

            if cfg!(test) {
                // Tests don't always use a multi-threaded executor so we can't use poll_tasks.
                wake_tasks()
            } else {
                // This results in a performance win because we can typically poll woken tasks
                // without needing to wake up any other threads.
                ehandle.poll_tasks(wake_tasks);
            }
        }
    }
}

impl Inner {
    // Returns list of containers that no longer have any messages.
    fn ingest(
        &mut self,
        msg: &[u8],
        container_id: ContainerId,
        on_inactive: &mut OnInactiveNotifier<'_>,
    ) {
        if msg.len() < std::mem::size_of::<Header>() {
            debug!("message too short ({})", msg.len());
            if let Some(container) = self.containers.get(container_id) {
                container.stats.increment_invalid(msg.len());
            }
            return;
        }

        let header = Header::read_from_bytes(&msg[..std::mem::size_of::<Header>()]).unwrap();

        // NOTE: Some tests send messages that are bigger than the header indicates.  We ignore the
        // tail of any such messages.
        let msg_len = header.size_words() as usize * 8;

        // Check the correct type and size.
        if header.raw_type() != TRACING_FORMAT_LOG_RECORD_TYPE || msg.len() < msg_len {
            debug!("bad type or size ({}, {}, {})", header.raw_type(), msg_len, msg.len());
            if let Some(container) = self.containers.get(container_id) {
                container.stats.increment_invalid(msg.len());
            }
            return;
        }

        // Make sure there's space.
        let mut space = self.space();
        let total_len = msg_len + FXT_HEADER_SIZE;

        while space < total_len {
            space += self.pop(on_inactive).expect("No messages in buffer!");
        }

        let Some(container_info) = self.containers.get_mut(container_id) else {
            return;
        };

        if container_info.msg_ids.end == container_info.msg_ids.start {
            container_info.first_index = self.head;
        }

        let msg_id = container_info.msg_ids.end;
        container_info.msg_ids.end += 1;

        let (container_msg_id, remaining) =
            u64::mut_from_prefix(self.buffer.slice_from_index_mut(self.head)).unwrap();
        *container_msg_id = ((container_id.0 as u64) << 32) | (msg_id & 0xffff_ffff);
        remaining[..msg_len].copy_from_slice(&msg[..msg_len]);

        self.head += total_len as u64;
    }

    fn space(&self) -> usize {
        self.buffer.len() - (self.head - self.tail) as usize
    }

    fn ensure_space(&mut self, required: usize, on_inactive: &mut OnInactiveNotifier<'_>) {
        let mut space = self.space();
        while space < required {
            space += self.pop(on_inactive).expect("No messages in buffer!");
        }
    }

    /// Pops a message and returns its total size.
    ///
    /// NOTE: This will pop the oldest message in terms of when it was inserted which is *not*
    /// necessarily the message with the *oldest* timestamp because we might not have received the
    /// messages in perfect timestamp order.  This should be close enough for all use cases we care
    /// about, and besides, the timestamps can't be trusted anyway.
    fn pop(&mut self, on_inactive: &mut OnInactiveNotifier<'_>) -> Option<usize> {
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
        if !container.is_active() {
            if container.should_free() {
                self.containers.free(container_id);
            } else {
                on_inactive.push(&container.identity);
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

    /// Returns true if the socket should be closed.
    fn read_socket(&mut self, socket: &Socket, on_inactive: &mut OnInactiveNotifier<'_>) -> bool {
        loop {
            self.ensure_space(FXT_HEADER_SIZE + MAX_DATAGRAM_LEN_BYTES as usize, on_inactive);

            let dest = self.buffer.slice_from_index_mut(self.head);

            // Read directly into the buffer leaving space for the header.
            let amount_read = match socket
                .socket
                .read(&mut dest[FXT_HEADER_SIZE..FXT_HEADER_SIZE + MAX_DATAGRAM_LEN_BYTES as usize])
            {
                Ok(a) => a,
                Err(zx::Status::SHOULD_WAIT) => return false,
                Err(_) => return true,
            };

            let Some(container) = self.containers.get_mut(socket.container_id) else {
                // Container no longer exists.
                return true;
            };

            if amount_read < 8 {
                container.stats.increment_invalid(amount_read);
                continue;
            }
            let header = Header::read_from_bytes(
                &dest[FXT_HEADER_SIZE..FXT_HEADER_SIZE + std::mem::size_of::<Header>()],
            )
            .unwrap();
            let msg_len = header.size_words() as usize * 8;
            if header.raw_type() != TRACING_FORMAT_LOG_RECORD_TYPE || msg_len != amount_read {
                debug!("bad type or size ({}, {}, {})", header.raw_type(), msg_len, amount_read);
                container.stats.increment_invalid(amount_read);
                continue;
            }

            container.stats.ingest_message(amount_read, header.severity().into());

            if container.msg_ids.end == container.msg_ids.start {
                container.first_index = self.head;
            }

            let msg_id = container.msg_ids.end;
            container.msg_ids.end += 1;

            // Every message in the shared buffer contains an 8 byte header consisting of a 32 bit
            // container ID, followed by a the least significant 32 bits of the per-container
            // message ID.
            let (container_msg_id, _) = u64::mut_from_prefix(dest).unwrap();
            *container_msg_id = ((socket.container_id.0 as u64) << 32) | (msg_id & 0xffff_ffff);

            self.head += (FXT_HEADER_SIZE + amount_read) as u64;
        }
    }
}

#[derive(Default)]
struct Containers {
    slab: Slab<ContainerInfo>,
}

#[derive(Clone, Copy, Debug)]
struct ContainerId(u32);

impl Containers {
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slab.len()
    }

    fn get(&self, id: ContainerId) -> Option<&ContainerInfo> {
        self.slab.get(id.0)
    }

    fn get_mut(&mut self, id: ContainerId) -> Option<&mut ContainerInfo> {
        self.slab.get_mut(id.0)
    }

    fn new_container(
        &mut self,
        identity: Arc<ComponentIdentity>,
        stats: Arc<LogStreamStats>,
    ) -> ContainerId {
        ContainerId(self.slab.insert(|_| ContainerInfo::new(identity, stats)))
    }

    fn free(&mut self, id: ContainerId) {
        self.slab.free(id.0)
    }
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

    // The first socket ID for this container.
    first_socket_id: SocketId,
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
            first_socket_id: SocketId::NULL,
        }
    }

    fn should_free(&self) -> bool {
        self.terminated && self.refs == 0 && !self.is_active()
    }

    fn is_active(&self) -> bool {
        self.msg_ids.end != self.msg_ids.start || self.first_socket_id != SocketId::NULL
    }

    // # Panics
    //
    // This will panic if the socket isn't found.
    fn remove_socket(&mut self, socket_id: SocketId, sockets: &mut Slab<Socket>) {
        let Socket { prev, next, .. } = *sockets.get(socket_id.0).unwrap();
        if prev == SocketId::NULL {
            self.first_socket_id = next;
        } else {
            sockets.get_mut(prev.0).unwrap().next = next;
        }
        if next != SocketId::NULL {
            sockets
                .get_mut(next.0)
                .unwrap_or_else(|| panic!("next {next:?} has been freed!"))
                .prev = prev;
        }
        sockets.free(socket_id.0);
        debug!(identity:% = self.identity; "Socket closed.");
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
        let mut on_inactive = OnInactiveNotifier::new(&self.shared_buffer);
        let mut inner = self.shared_buffer.inner.lock();

        inner.ingest(msg, self.container_id, &mut on_inactive);

        for waker in inner.drain_wakers() {
            waker.wake();
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
        let stats = Arc::clone(&container.stats);

        let (index, next_id, end) = match mode {
            StreamMode::Snapshot => {
                (container.first_index, container.msg_ids.start, CursorEnd::Snapshot(None))
            }
            StreamMode::Subscribe => (head, container.msg_ids.end, CursorEnd::Stream),
            StreamMode::SnapshotThenSubscribe => {
                (container.first_index, container.msg_ids.start, CursorEnd::Stream)
            }
        };

        Some(Cursor {
            index,
            container_id: self.container_id,
            next_id,
            end,
            buffer: Arc::clone(&self.shared_buffer),
            waker_entry: WakerEntry::new(),
            stats,
        })
    }

    /// Marks the buffer as terminated which will force all cursors to end and close all sockets.
    /// The component's data will remain in the buffer until the messages are rolled out.  This will
    /// *not* drain sockets.
    pub fn terminate(&self) {
        let mut guard = self.shared_buffer.inner.lock();
        let InnerAndSockets(inner, sockets) = &mut *guard;
        if let Some(container) = inner.containers.get_mut(self.container_id) {
            container.terminated = true;
            if container.first_socket_id != SocketId::NULL {
                loop {
                    container.remove_socket(container.first_socket_id, sockets);
                    if container.first_socket_id == SocketId::NULL {
                        break;
                    }
                }
            }
            if container.should_free() {
                inner.containers.free(self.container_id);
            }
            for waker in guard.drain_wakers() {
                waker.wake();
            }
        }
    }

    /// Returns true if the container has messages or sockets.
    pub fn is_active(&self) -> bool {
        self.shared_buffer.inner.lock().containers.get(self.container_id).is_some_and(|c| {
            c.msg_ids.start != c.msg_ids.end || c.first_socket_id != SocketId::NULL
        })
    }

    /// Adds a socket for this container.
    pub fn add_socket(&self, socket: zx::Socket) {
        let mut guard = self.shared_buffer.inner.lock();
        let InnerAndSockets(inner, sockets) = &mut *guard;
        let Some(container) = inner.containers.get_mut(self.container_id) else { return };
        let next = container.first_socket_id;
        let socket_id = SocketId(sockets.insert(|socket_id| {
            socket
                .wait_async_handle(
                    &self.shared_buffer.port,
                    socket_id as u64,
                    zx::Signals::OBJECT_READABLE | zx::Signals::OBJECT_PEER_CLOSED,
                    zx::WaitAsyncOpts::empty(),
                )
                .unwrap();
            Socket { socket, container_id: self.container_id, prev: SocketId::NULL, next }
        }));
        if next != SocketId::NULL {
            sockets.get_mut(next.0).unwrap().prev = socket_id;
        }
        container.first_socket_id = socket_id;
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

    // The ID, if any, that we should end at (exclusive).
    end: CursorEnd,

    #[derivative(Debug = "ignore")]
    buffer: Arc<SharedBuffer>,

    // waker_entry is used to register a waker for subscribing cursors.
    #[pin]
    #[derivative(Debug = "ignore")]
    waker_entry: WakerEntry<InnerAndSockets>,

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
        let mut this = self.project();
        let mut on_inactive = OnInactiveNotifier::new(this.buffer);
        let mut guard = this.buffer.inner.lock();
        let InnerAndSockets(inner, sockets) = &mut *guard;

        let mut container = match inner.containers.get(*this.container_id) {
            None => return Poll::Ready(None),
            Some(container) => container,
        };

        let end_id = match &mut this.end {
            CursorEnd::Snapshot(None) => {
                // Drain the sockets associated with this container first so that we capture
                // pending messages.  Some tests rely on this.
                let mut socket_id = container.first_socket_id;
                while socket_id != SocketId::NULL {
                    let socket = sockets.get(socket_id.0).unwrap();
                    let next = socket.next;
                    if inner.read_socket(socket, &mut on_inactive) {
                        let container = inner.containers.get_mut(*this.container_id).unwrap();
                        container.remove_socket(socket_id, sockets);
                        if !container.is_active() {
                            on_inactive.push(&container.identity);
                        }
                    }
                    socket_id = next;
                }

                container = inner.containers.get(*this.container_id).unwrap();
                *this.end = CursorEnd::Snapshot(Some(container.msg_ids.end));
                container.msg_ids.end
            }
            CursorEnd::Snapshot(Some(end)) => *end,
            CursorEnd::Stream => u64::MAX,
        };

        if *this.next_id == end_id {
            return Poll::Ready(None);
        }

        // See if messages have been rolled out.
        if container.msg_ids.start > *this.next_id {
            let mut next_id = container.msg_ids.start;
            if end_id < next_id {
                next_id = end_id;
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
            guard.add_waker(this.waker_entry, cx.waker().clone());
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
                inner.containers.free(self.container_id);
            }
        }
    }
}

#[derive(Debug)]
enum CursorEnd {
    // The id will be None when the cursor is first created and is set after the cursor
    // is first polled.
    Snapshot(Option<u64>),
    Stream,
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

/// Implements a simple Slab allocator.
struct Slab<T> {
    slab: Vec<Slot<T>>,
    free_index: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self { slab: Vec::new(), free_index: usize::MAX }
    }
}

enum Slot<T> {
    Used(T),
    Free(usize),
}

impl<T> Slab<T> {
    /// Returns the number of used entries.  This is not performant.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slab.iter().filter(|c| matches!(c, Slot::Used(_))).count()
    }

    fn free(&mut self, index: u32) {
        let index = index as usize;
        assert!(matches!(
            std::mem::replace(&mut self.slab[index], Slot::Free(self.free_index)),
            Slot::Used(_)
        ));
        self.free_index = index;
    }

    fn get(&self, id: u32) -> Option<&T> {
        self.slab.get(id as usize).and_then(|s| match s {
            Slot::Used(s) => Some(s),
            _ => None,
        })
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut T> {
        self.slab.get_mut(id as usize).and_then(|s| match s {
            Slot::Used(s) => Some(s),
            _ => None,
        })
    }

    fn insert(&mut self, value: impl FnOnce(u32) -> T) -> u32 {
        let free_index = self.free_index;
        if free_index != usize::MAX {
            self.free_index = match std::mem::replace(
                &mut self.slab[free_index],
                Slot::Used(value(free_index as u32)),
            ) {
                Slot::Free(next) => next,
                _ => unreachable!(),
            };
            free_index as u32
        } else {
            // This is < rather than <= because we reserve 0xffff_ffff to be used as a NULL value
            // (see SocketId::NULL below).
            assert!(self.slab.len() < u32::MAX as usize);
            self.slab.push(Slot::Used(value(self.slab.len() as u32)));
            (self.slab.len() - 1) as u32
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketId(u32);

impl SocketId {
    const NULL: Self = SocketId(0xffff_ffff);
}

struct Socket {
    socket: zx::Socket,
    container_id: ContainerId,
    // Sockets are stored as a linked list for each container.
    prev: SocketId,
    next: SocketId,
}

/// Sends on-inactive notifications when dropped.  This *must* be dropped when no locks are held.
struct OnInactiveNotifier<'a>(&'a SharedBuffer, Vec<Arc<ComponentIdentity>>);

impl<'a> OnInactiveNotifier<'a> {
    fn new(buffer: &'a SharedBuffer) -> Self {
        Self(buffer, Vec::new())
    }

    fn push(&mut self, identity: &Arc<ComponentIdentity>) {
        self.1.push(Arc::clone(identity));
    }

    fn notify(&mut self) {
        for identity in self.1.drain(..) {
            (*self.0.on_inactive)(identity);
        }
    }
}

impl Drop for OnInactiveNotifier<'_> {
    fn drop(&mut self) {
        self.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::SharedBuffer;
    use crate::logs::shared_buffer::LazyItem;
    use crate::logs::stats::LogStreamStats;
    use crate::logs::testing::make_message;
    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fidl_fuchsia_diagnostics::StreamMode;
    use fuchsia_async as fasync;
    use fuchsia_inspect::{Inspector, InspectorConfig};
    use fuchsia_inspect_derive::WithInspect;
    use futures::channel::mpsc;
    use futures::poll;
    use futures::stream::{FuturesUnordered, StreamExt as _};
    use std::pin::pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[fuchsia::test]
    async fn push_one_message() {
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));

        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn bad_type() {
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0x77; 16]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn message_truncated() {
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_buffer.push_back(&msg.bytes()[..msg.bytes().len() - 1]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn buffer_wrapping() {
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
    async fn on_inactive() {
        let identity = Arc::new(vec!["a"].into());
        let on_inactive = Arc::new(AtomicU64::new(0));
        let buffer = {
            let on_inactive = Arc::clone(&on_inactive);
            let identity = Arc::clone(&identity);
            Arc::new(SharedBuffer::new(
                65536,
                Box::new(move |i| {
                    assert_eq!(i, identity);
                    on_inactive.fetch_add(1, Ordering::Relaxed);
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

        assert_eq!(on_inactive.load(Ordering::Relaxed), 1);
    }

    #[fuchsia::test]
    async fn terminate_drops_container() {
        // Silence a clippy warning; SharedBuffer needs an executor.
        async {}.await;

        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));

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
            let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
            let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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
        let buffer = SharedBuffer::new(65536, Box::new(|_| {}));
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

    #[fuchsia::test]
    async fn socket_increments_logstats() {
        let inspector = Inspector::new(InspectorConfig::default());
        let stats: Arc<LogStreamStats> =
            Arc::new(LogStreamStats::default().with_inspect(inspector.root(), "test").unwrap());
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_a = Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), stats));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

        let (local, remote) = zx::Socket::create_datagram();
        container_a.add_socket(remote);

        let cursor_a = container_a.cursor(StreamMode::Subscribe).unwrap();

        // Use FuturesUnordered so that we can make sure that the cursor is woken when a message is
        // received (FuturesUnordered uses separate wakers for all the futures it manages).
        let mut futures = FuturesUnordered::new();
        futures.push(async move {
            let mut cursor_a = pin!(cursor_a);
            cursor_a.next().await
        });
        let mut next = futures.next();
        assert!(futures::poll!(&mut next).is_pending());

        local.write(msg.bytes()).unwrap();

        let cursor_b = pin!(container_a.cursor(StreamMode::Snapshot).unwrap());

        assert_eq!(
            cursor_b
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

        // If cursor_a wasn't woken, this will hang.
        next.await;
        // Validate logstats (must happen after the socket was handled)
        assert_data_tree!(
            inspector,
            root: contains {
                test: {
                    url: "",
                    last_timestamp: AnyProperty,
                    sockets_closed: 0u64,
                    sockets_opened: 0u64,
                    invalid: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    total: {
                        number: 1u64,
                        bytes: 88u64,
                    },
                    rolled_out: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    trace: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    debug: {
                        number: 1u64,
                        bytes: 88u64,
                    },
                    info: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    warn: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    error: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                    fatal: {
                        number: 0u64,
                        bytes: 0u64,
                    },
                }
            }
        );
    }

    #[fuchsia::test]
    async fn socket() {
        let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
        let container_a =
            Arc::new(buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

        let (local, remote) = zx::Socket::create_datagram();
        container_a.add_socket(remote);

        let cursor_a = container_a.cursor(StreamMode::Subscribe).unwrap();

        // Use FuturesUnordered so that we can make sure that the cursor is woken when a message is
        // received (FuturesUnordered uses separate wakers for all the futures it manages).
        let mut futures = FuturesUnordered::new();
        futures.push(async move {
            let mut cursor_a = pin!(cursor_a);
            cursor_a.next().await
        });
        let mut next = futures.next();
        assert!(futures::poll!(&mut next).is_pending());

        local.write(msg.bytes()).unwrap();

        let cursor_b = pin!(container_a.cursor(StreamMode::Snapshot).unwrap());

        assert_eq!(
            cursor_b
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

        // If cursor_a wasn't woken, this will hang.
        next.await;
    }

    #[fuchsia::test]
    async fn socket_on_inactive() {
        let on_inactive = Arc::new(AtomicU64::new(0));
        let a_identity = Arc::new(vec!["a"].into());
        let buffer = Arc::new(SharedBuffer::new(65536, {
            let on_inactive = Arc::clone(&on_inactive);
            let a_identity = Arc::clone(&a_identity);
            Box::new(move |id| {
                assert_eq!(id, a_identity);
                on_inactive.fetch_add(1, Ordering::Relaxed);
            })
        }));
        let container_a = Arc::new(buffer.new_container_buffer(a_identity, Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

        let (local, remote) = zx::Socket::create_datagram();
        container_a.add_socket(remote);

        local.write(msg.bytes()).unwrap();

        let cursor = pin!(container_a.cursor(StreamMode::Snapshot).unwrap());

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

        // Now roll out a's messages.
        let container_b = buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default());
        while container_a.cursor(StreamMode::Snapshot).unwrap().count().await == 1 {
            container_b.push_back(msg.bytes());
        }

        assert_eq!(on_inactive.load(Ordering::Relaxed), 0);

        // Close the socket.
        std::mem::drop(local);

        // We don't know when the socket thread will run so we have to loop.
        while on_inactive.load(Ordering::Relaxed) != 1 {
            fasync::Timer::new(std::time::Duration::from_millis(50)).await;
        }
    }
}
