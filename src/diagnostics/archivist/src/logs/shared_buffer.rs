// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use crate::logs::repository::ARCHIVIST_MONIKER;
use crate::logs::stats::LogStreamStats;
use crate::logs::stored_message::StoredMessage;
use derivative::Derivative;
use diagnostics_log_encoding::encode::add_dropped_count;
use diagnostics_log_encoding::{Header, TRACING_FORMAT_LOG_RECORD_TYPE};
use fidl_fuchsia_diagnostics::StreamMode;
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_async as fasync;
use fuchsia_async::condition::{Condition, WakerEntry};
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::Stream;
use log::debug;
use pin_project::{pin_project, pinned_drop};
use ring_buffer::{self, ring_buffer_record_len, RingBuffer};
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut, Range};
use std::pin::{pin, Pin};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use zerocopy::FromBytes;
use zx::AsHandleRef as _;

// Aim to keep 25% of the buffer free. This is expressed as a fraction: numerator / denominator.
const SPACE_THRESHOLD_NUMERATOR: usize = 1;
const SPACE_THRESHOLD_DENOMINATOR: usize = 4;

// The default amount of time that Archivist will sleep for to reduce how often it wakes up to
// handle log messages.
const DEFAULT_SLEEP_TIME: Duration = Duration::from_millis(200);

pub fn create_ring_buffer(capacity: usize) -> ring_buffer::Reader {
    RingBuffer::create(calculate_real_size_given_desired_capacity(capacity))
}

fn calculate_real_size_given_desired_capacity(capacity: usize) -> usize {
    // We always keep spare space in the buffer so that we don't drop messages.  This is controlled
    // by SPACE_THRESHOLD_NUMERATOR & SPACE_THRESHOLD_DENOMINATOR.  Round up capacity so that
    // `capacity` reflects the actual amount of log data we can store.
    let page_size = zx::system_get_page_size() as usize;
    (capacity * SPACE_THRESHOLD_DENOMINATOR
        / (SPACE_THRESHOLD_DENOMINATOR - SPACE_THRESHOLD_NUMERATOR))
        .next_multiple_of(page_size)
}

const IOB_PEER_CLOSED_KEY_BASE: u64 = 0x8000_0000_0000_0000;

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

    // The task responsible for monitoring the space in the IOBuffer and popping messages when it
    // gets full. It will also wake cursors whenever new data arrives.
    _buffer_monitor_task: fasync::Task<()>,
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
    // The ring buffer.
    ring_buffer: Arc<RingBuffer>,

    // Registered containers.
    containers: Containers,

    // Socket thread message queue.
    thread_msg_queue: VecDeque<ThreadMessage>,

    // The index in the buffer that we have scanned for messages.
    last_scanned: u64,

    // A copy of the tail index in the ring buffer.
    tail: u64,

    // The IOBuffer peers that we must watch.
    iob_peers: Slab<(ContainerId, zx::Iob)>,
}

enum ThreadMessage {
    // The thread should terminate.
    Terminate,

    // Process all pending socket messages and report via the Sender when done.
    Flush(oneshot::Sender<()>),
}

pub struct SharedBufferOptions {
    // To reduce how often Archivist wakes when new messages are written, Archivist will sleep for
    // this time. This will impact how quickly messages show up via the cursors.
    pub sleep_time: Duration,
}

impl Default for SharedBufferOptions {
    fn default() -> Self {
        Self { sleep_time: DEFAULT_SLEEP_TIME }
    }
}

impl SharedBuffer {
    /// Returns a new shared buffer and the container for Archivist.
    pub fn new(
        ring_buffer: ring_buffer::Reader,
        on_inactive: OnInactive,
        options: SharedBufferOptions,
    ) -> Arc<Self> {
        let this = Arc::new_cyclic(|weak: &Weak<Self>| Self {
            inner: Condition::new(InnerAndSockets(
                Inner {
                    ring_buffer: Arc::clone(&ring_buffer),
                    containers: Containers::default(),
                    thread_msg_queue: VecDeque::default(),
                    last_scanned: 0,
                    tail: 0,
                    iob_peers: Slab::default(),
                },
                Slab::default(),
            )),
            on_inactive,
            port: zx::Port::create(),
            event: zx::Event::create(),
            socket_thread: Mutex::default(),
            _buffer_monitor_task: fasync::Task::spawn(Self::buffer_monitor_task(
                Weak::clone(weak),
                ring_buffer,
                options.sleep_time,
            )),
        });

        *this.socket_thread.lock() = Some({
            let this = Arc::clone(&this);
            std::thread::spawn(move || this.socket_thread(options.sleep_time))
        });
        this
    }

    pub fn new_container_buffer(
        self: &Arc<Self>,
        identity: Arc<ComponentIdentity>,
        stats: Arc<LogStreamStats>,
    ) -> ContainerBuffer {
        let mut inner_and_sockets = self.inner.lock();
        let Inner { containers, ring_buffer, .. } = &mut inner_and_sockets.0;
        ContainerBuffer {
            shared_buffer: Arc::clone(self),
            container_id: containers.new_container(ring_buffer, Arc::clone(&identity), stats),
        }
    }

    pub async fn flush(&self) {
        let (sender, receiver) = oneshot::channel();
        self.inner.lock().0.thread_msg_queue.push_back(ThreadMessage::Flush(sender));
        self.event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        // Ignore failure if Archivist is shutting down.
        let _ = receiver.await;
    }

    /// Returns the number of registered containers in use by the buffer.
    #[cfg(test)]
    pub fn container_count(&self) -> usize {
        self.inner.lock().0.containers.len()
    }

    /// Terminates the socket thread. The socket thread will drain the sockets before terminating.
    pub async fn terminate(&self) {
        self.inner.lock().0.thread_msg_queue.push_back(ThreadMessage::Terminate);
        self.event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        let join_handle = self.socket_thread.lock().take().unwrap();
        fasync::unblock(|| {
            let _ = join_handle.join();
        })
        .await;
    }

    fn socket_thread(&self, sleep_time: Duration) {
        const INTERRUPT_KEY: u64 = u64::MAX;
        let mut sockets_ready = Vec::new();
        let mut iob_peer_closed = Vec::new();
        let mut interrupt_needs_arming = true;
        let mut msg = None;

        loop {
            let mut deadline = if msg.is_some() {
                zx::MonotonicInstant::INFINITE_PAST
            } else {
                if interrupt_needs_arming {
                    self.event
                        .wait_async_handle(
                            &self.port,
                            INTERRUPT_KEY,
                            zx::Signals::USER_0,
                            zx::WaitAsyncOpts::empty(),
                        )
                        .unwrap();
                    interrupt_needs_arming = false;
                }

                // Wait so that we're not constantly waking up for every log message that is queued.
                // Ignore errors here.
                let _ = self.event.wait_handle(
                    zx::Signals::USER_0,
                    zx::MonotonicInstant::after(sleep_time.into()),
                );
                zx::MonotonicInstant::INFINITE
            };

            // Gather the list of sockets that are ready to read.
            loop {
                match self.port.wait(deadline) {
                    Ok(packet) => {
                        if packet.key() == INTERRUPT_KEY {
                            interrupt_needs_arming = true;
                            // To maintain proper ordering, we must capture the message here whilst
                            // we are still gathering the list of sockets that are ready to read.
                            // If we wait till later, we introduce windows where we might miss a
                            // socket that should be read.
                            if msg.is_none() {
                                msg = self.inner.lock().0.thread_msg_queue.pop_front();
                            }
                        } else if packet.key() & IOB_PEER_CLOSED_KEY_BASE != 0 {
                            iob_peer_closed.push(packet.key() as u32);
                        } else {
                            sockets_ready.push(SocketId(packet.key() as u32))
                        }
                    }
                    Err(zx::Status::TIMED_OUT) => break,
                    Err(status) => panic!("port wait error: {status:?}"),
                }
                deadline = zx::MonotonicInstant::INFINITE_PAST;
            }

            let mut on_inactive = OnInactiveNotifier::new(self);
            let mut inner_and_sockets = self.inner.lock();
            let InnerAndSockets(inner, sockets) = &mut *inner_and_sockets;

            if !iob_peer_closed.is_empty() {
                // See the comment on `is_active()` for why this is required.
                inner.update_message_ids(inner.ring_buffer.head());

                for iob_peer_closed in iob_peer_closed.drain(..) {
                    let container_id = inner.iob_peers.free(iob_peer_closed).0;
                    if let Some(container) = inner.containers.get_mut(container_id) {
                        container.iob_count -= 1;
                        if container.iob_count == 0 && !container.is_active() {
                            if container.should_free() {
                                inner.containers.free(container_id);
                            } else {
                                on_inactive.push(&container.identity);
                            }
                        }
                    }
                }
            }

            for socket_id in sockets_ready.drain(..) {
                inner.read_socket(sockets, socket_id, &mut on_inactive, |socket| {
                    socket
                        .socket
                        .wait_async_handle(
                            &self.port,
                            socket_id.0 as u64,
                            zx::Signals::OBJECT_READABLE | zx::Signals::OBJECT_PEER_CLOSED,
                            zx::WaitAsyncOpts::empty(),
                        )
                        .unwrap();
                });
            }

            // Now that we've processed the sockets, we can process the message.
            if let Some(m) = msg.take() {
                match m {
                    ThreadMessage::Terminate => return,
                    ThreadMessage::Flush(sender) => {
                        let _ = sender.send(());
                    }
                }

                // See if there's another message.
                msg = inner.thread_msg_queue.pop_front();
                if msg.is_none() {
                    // If there are no more messages, we must clear the signal so that we get
                    // notified when the next message arrives. This must be done whilst we are
                    // holding the lock.
                    self.event.signal_handle(zx::Signals::USER_0, zx::Signals::empty()).unwrap();
                }
            }
        }
    }

    async fn buffer_monitor_task(
        this: Weak<Self>,
        mut ring_buffer: ring_buffer::Reader,
        sleep_time: Duration,
    ) {
        let mut last_head = 0;
        loop {
            // Sleep to limit how often we wake up.
            fasync::Timer::new(sleep_time).await;
            let head = ring_buffer.wait(last_head).await;
            let Some(this) = this.upgrade() else { return };
            let mut on_inactive = OnInactiveNotifier::new(&this);
            let mut inner = this.inner.lock();
            inner.check_space(head, &mut on_inactive);
            for waker in inner.drain_wakers() {
                waker.wake();
            }
            last_head = head;
        }
    }
}

impl Inner {
    // Returns list of containers that no longer have any messages.
    fn ingest(&mut self, msg: &[u8], container_id: ContainerId) {
        if msg.len() < std::mem::size_of::<Header>() {
            debug!("message too short ({})", msg.len());
            if let Some(container) = self.containers.get(container_id) {
                container.stats.increment_invalid(msg.len());
            }
            return;
        }

        let header = Header::read_from_bytes(&msg[..std::mem::size_of::<Header>()]).unwrap();

        // NOTE: Some tests send messages that are bigger than the header indicates. We ignore the
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

        let Some(container) = self.containers.get_mut(container_id) else {
            return;
        };

        let mut data;
        let msg = if container.dropped_count > 0 {
            data = msg.to_vec();
            if !add_dropped_count(&mut data, container.dropped_count) {
                debug!("unable to add dropped count to invalid message");
                container.stats.increment_invalid(data.len());
                return;
            }
            &data
        } else {
            msg
        };

        if container.iob.write(Default::default(), 0, msg).is_err() {
            // We were unable to write the message to the buffer, most likely due to lack of
            // space. We drop this message and then we'll add to the dropped count for the next
            // message.
            container.dropped_count += 1
        } else {
            container.dropped_count = 0;
        }
    }

    /// Pops a message and returns its total size. The caller should call `update_message_ids(head)`
    /// prior to calling this.
    ///
    /// NOTE: This will pop the oldest message in terms of when it was inserted which is *not*
    /// necessarily the message with the *oldest* timestamp because we might not have received the
    /// messages in perfect timestamp order. This should be close enough for all use cases we care
    /// about, and besides, the timestamps can't be trusted anyway.
    fn pop(&mut self, head: u64, on_inactive: &mut OnInactiveNotifier<'_>) -> Option<usize> {
        if head == self.tail {
            return None;
        }

        // SAFETY: There can be no concurrent writes between `tail..head` and the *only* place
        // we increment the tail index is just before we leave this function.
        let record_len = {
            let (container_id, message, timestamp) = unsafe { self.parse_message(self.tail..head) };
            let record_len = ring_buffer_record_len(message.len());

            let container = self.containers.get_mut(container_id).unwrap();

            container.stats.increment_rolled_out(record_len);
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

            record_len
        };

        // NOTE: This should go last. After incrementing `tail`, the `message` can be overwritten.
        self.ring_buffer.increment_tail(record_len);
        self.tail += record_len as u64;

        // The caller should have called `update_message_ids(head)` prior to calling this.
        assert!(self.last_scanned >= self.tail);

        Some(record_len)
    }

    /// Parses the first message within `range` returns (container_id, message, timestamp).
    ///
    /// # Panics
    ///
    /// This will panic if the ring buffer has been corrupted. Only the kernel and Archivist can
    /// write to the ring buffer and so we trust both not to corrupt the ring buffer.
    ///
    /// # Safety
    ///
    /// `range` *must* be within the written range of the ring buffer so that there is no concurrent
    /// write access to that range. The returned slice is only valid whilst this remains true.
    unsafe fn parse_message(
        &self,
        range: Range<u64>,
    ) -> (ContainerId, &[u8], Option<zx::BootInstant>) {
        let (tag, msg) = self
            .ring_buffer
            .first_message_in(range)
            .expect("Unable to read message from ring buffer");
        (
            ContainerId(tag as u32),
            msg,
            (msg.len() >= 16)
                .then(|| zx::BootInstant::from_nanos(i64::read_from_bytes(&msg[8..16]).unwrap())),
        )
    }

    /// Reads a socket. Calls `rearm` to rearm the socket once it has been drained.
    fn read_socket(
        &mut self,
        sockets: &mut Slab<Socket>,
        socket_id: SocketId,
        on_inactive: &mut OnInactiveNotifier<'_>,
        rearm: impl FnOnce(&mut Socket),
    ) {
        let Some(socket) = sockets.get_mut(socket_id.0) else { return };
        let container_id = socket.container_id;

        loop {
            self.check_space(self.ring_buffer.head(), on_inactive);

            let mut data = Vec::with_capacity(MAX_DATAGRAM_LEN_BYTES as usize);

            // Read directly into the buffer leaving space for the header.
            let len = match socket.socket.read_uninit(data.spare_capacity_mut()) {
                Ok(d) => d.len(),
                Err(zx::Status::SHOULD_WAIT) => {
                    // The socket has been drained.
                    rearm(socket);
                    return;
                }
                Err(_) => break,
            };

            // SAFETY: `read_uninit` will have written to `len` bytes.
            unsafe {
                data.set_len(len);
            }

            let container = self.containers.get_mut(container_id).unwrap();
            if data.len() < 16 {
                container.stats.increment_invalid(data.len());
                continue;
            }

            let header = Header::read_from_bytes(&data[..std::mem::size_of::<Header>()]).unwrap();
            let msg_len = header.size_words() as usize * 8;
            if header.raw_type() != TRACING_FORMAT_LOG_RECORD_TYPE || msg_len != data.len() {
                debug!("bad type or size ({}, {}, {})", header.raw_type(), msg_len, data.len());
                container.stats.increment_invalid(data.len());
                continue;
            }

            if container.dropped_count > 0 && !add_dropped_count(&mut data, container.dropped_count)
            {
                debug!("unable to add dropped count to invalid message");
                container.stats.increment_invalid(data.len());
                continue;
            }

            if container.iob.write(Default::default(), 0, &data).is_err() {
                // We were unable to write the message to the buffer, most likely due to lack of
                // space. We drop this message and then we'll add to the dropped count for the next
                // message.
                container.dropped_count += 1
            } else {
                container.dropped_count = 0;
            }
        }

        // This path is taken when the socket should be closed.

        // See the comment on `is_active()` for why this is required.
        self.update_message_ids(self.ring_buffer.head());

        let container = self.containers.get_mut(container_id).unwrap();
        container.remove_socket(socket_id, sockets);
        if !container.is_active() {
            if container.should_free() {
                self.containers.free(container_id);
            } else {
                on_inactive.push(&container.identity);
            }
        }
    }

    /// Scans the ring buffer and updates `msg_ids` for the containers.
    fn update_message_ids(&mut self, head: u64) {
        while self.last_scanned < head {
            // SAFETY: This is safe because `head` must be within the ring buffer range and we make
            // sure that `self.last_scanned` is always >= `tail` in `pop()`.
            let (container_id, message, _) = unsafe { self.parse_message(self.last_scanned..head) };
            let msg_len = message.len();
            let severity = (msg_len >= 8)
                .then(|| Header::read_from_bytes(&message[0..8]).unwrap().severity().into());
            let container = self.containers.get_mut(container_id).unwrap();
            container.msg_ids.end += 1;
            if let Some(severity) = severity {
                container.stats.ingest_message(msg_len, severity);
            }
            self.last_scanned += ring_buffer_record_len(msg_len) as u64;
        }
    }

    /// Ensures the buffer keeps the required amount of space.
    fn check_space(&mut self, head: u64, on_inactive: &mut OnInactiveNotifier<'_>) {
        self.update_message_ids(head);
        let capacity = self.ring_buffer.capacity();
        let mut space = capacity
            .checked_sub((head - self.tail) as usize)
            .unwrap_or_else(|| panic!("bad range: {:?}", self.tail..head));
        let required_space = capacity * SPACE_THRESHOLD_NUMERATOR / SPACE_THRESHOLD_DENOMINATOR;
        while space < required_space {
            let Some(amount) = self.pop(head, on_inactive) else { break };
            space += amount;
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
        buffer: &RingBuffer,
        identity: Arc<ComponentIdentity>,
        stats: Arc<LogStreamStats>,
    ) -> ContainerId {
        ContainerId(self.slab.insert(|id| {
            let (iob, _) = buffer.new_iob_writer(id as u64).unwrap();
            ContainerInfo::new(identity, stats, iob)
        }))
    }

    fn free(&mut self, id: ContainerId) {
        self.slab.free(id.0);
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
struct ContainerInfo {
    // The number of references (typically cursors) that prevent this object from being freed.
    refs: usize,

    // The identity of the container.
    identity: Arc<ComponentIdentity>,

    // The first index in the shared buffer for a message for this container. This index
    // might have been rolled out. This is used as an optimisation to set the starting
    // cursor position.
    first_index: u64,

    // The first and last message IDs stored in the shared buffer. The last message ID can be out of
    // date if there are concurrent writers.
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

    // The IOBuffer used for writing.
    iob: zx::Iob,

    // The number of client IOBuffers.
    iob_count: usize,

    // The number of messages dropped when forwarding from a socket to an IOBuffer.
    dropped_count: u64,
}

impl ContainerInfo {
    fn new(identity: Arc<ComponentIdentity>, stats: Arc<LogStreamStats>, iob: zx::Iob) -> Self {
        Self {
            refs: 0,
            identity,
            first_index: 0,
            msg_ids: 0..0,
            terminated: false,
            stats,
            last_rolled_out_timestamp: zx::BootInstant::ZERO,
            first_socket_id: SocketId::NULL,
            iob,
            iob_count: 0,
            dropped_count: 0,
        }
    }

    fn should_free(&self) -> bool {
        self.terminated && self.refs == 0 && !self.is_active()
    }

    // Returns true if the container is considered active which is the case if it has sockets, io
    // buffers, or buffered messages.
    //
    // NOTE: Whenever a socket or iob is closed, `update_message_ids` must called to ensure
    // `msg_ids.end` is correctly set.
    fn is_active(&self) -> bool {
        self.first_socket_id != SocketId::NULL
            || self.iob_count > 0
            || self.msg_ids.end != self.msg_ids.start
            || ARCHIVIST_MONIKER.get().is_some_and(|m| *self.identity == *m)
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
        self.stats.close_socket();
        debug!(identity:% = self.identity; "Socket closed.");
    }
}

pub struct ContainerBuffer {
    shared_buffer: Arc<SharedBuffer>,
    container_id: ContainerId,
}

impl ContainerBuffer {
    /// Returns the tag used by IOBuffers used for this component.
    pub fn iob_tag(&self) -> u64 {
        self.container_id.0 as u64
    }

    /// Ingests a new message.
    ///
    /// If the message is invalid, it is dropped.
    pub fn push_back(&self, msg: &[u8]) {
        self.shared_buffer.inner.lock().ingest(msg, self.container_id);
    }

    /// Returns an IOBuffer for the container.
    pub fn iob(&self) -> zx::Iob {
        let mut inner = self.shared_buffer.inner.lock();

        inner.containers.get_mut(self.container_id).unwrap().iob_count += 1;

        let (ep0, ep1) = inner.ring_buffer.new_iob_writer(self.container_id.0 as u64).unwrap();

        inner.iob_peers.insert(|idx| {
            ep1.wait_async_handle(
                &self.shared_buffer.port,
                idx as u64 | IOB_PEER_CLOSED_KEY_BASE,
                zx::Signals::IOB_PEER_CLOSED,
                zx::WaitAsyncOpts::empty(),
            )
            .unwrap();

            (self.container_id, ep1)
        });

        ep0
    }

    /// Returns a cursor.
    pub fn cursor(&self, mode: StreamMode) -> Option<Cursor> {
        let mut inner = self.shared_buffer.inner.lock();
        let Some(mut container) = inner.containers.get_mut(self.container_id) else {
            // We've hit a race where the container has terminated.
            return None;
        };

        container.refs += 1;
        let stats = Arc::clone(&container.stats);

        let (index, next_id, end) = match mode {
            StreamMode::Snapshot => {
                (container.first_index, container.msg_ids.start, CursorEnd::Snapshot(None))
            }
            StreamMode::Subscribe => {
                // Call `update_message_ids` so that `msg_ids.end` is up to date.
                let head = inner.ring_buffer.head();
                inner.update_message_ids(head);
                container = inner.containers.get_mut(self.container_id).unwrap();
                (head, container.msg_ids.end, CursorEnd::Stream)
            }
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
            waker_entry: self.shared_buffer.inner.waker_entry(),
            stats,
        })
    }

    /// Marks the buffer as terminated which will force all cursors to end and close all sockets.
    /// The component's data will remain in the buffer until the messages are rolled out. This will
    /// *not* drain sockets or close IOBuffers.
    pub fn terminate(&self) {
        let mut guard = self.shared_buffer.inner.lock();
        let InnerAndSockets(inner, sockets) = &mut *guard;

        // See the comment on `is_active()` for why this is required.
        inner.update_message_ids(inner.ring_buffer.head());

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

    /// Returns true if the container has messages, sockets or IOBuffers.
    pub fn is_active(&self) -> bool {
        self.shared_buffer
            .inner
            .lock()
            .containers
            .get(self.container_id)
            .is_some_and(|c| c.is_active())
    }

    /// Adds a socket for this container.
    pub fn add_socket(&self, socket: zx::Socket) {
        let mut guard = self.shared_buffer.inner.lock();
        let InnerAndSockets(inner, sockets) = &mut *guard;
        let Some(container) = inner.containers.get_mut(self.container_id) else { return };
        container.stats.open_socket();
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

        let mut head = inner.ring_buffer.head();
        inner.update_message_ids(head);

        let mut container = match inner.containers.get(*this.container_id) {
            None => return Poll::Ready(None),
            Some(container) => container,
        };

        let end_id = match &mut this.end {
            CursorEnd::Snapshot(None) => {
                // Drain the sockets associated with this container first so that we capture
                // pending messages. Some tests rely on this.
                let mut socket_id = container.first_socket_id;
                while socket_id != SocketId::NULL {
                    let socket = sockets.get_mut(socket_id.0).unwrap();
                    let next = socket.next;
                    // The socket doesn't need to be rearmed here.
                    inner.read_socket(sockets, socket_id, &mut on_inactive, |_| {});
                    socket_id = next;
                }

                // Call `update_message_ids` so that `msg_ids.end` is up to date.
                head = inner.ring_buffer.head();
                inner.update_message_ids(head);
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

        if *this.next_id == container.msg_ids.end && *this.index < inner.last_scanned {
            *this.index = inner.last_scanned;
        }

        // Scan forward until we find a record matching this container.
        while *this.index < head {
            // SAFETY: `*this.index..head` must be within the written region of the ring-buffer.
            let (container_id, message, _timestamp) =
                unsafe { inner.parse_message(*this.index..head) };

            // Move index to the next record.
            *this.index += ring_buffer_record_len(message.len()) as u64;
            assert!(*this.index <= head);

            if container_id.0 == this.container_id.0 {
                *this.next_id += 1;
                if let Some(msg) = StoredMessage::new(message.into(), this.stats) {
                    return Poll::Ready(Some(LazyItem::Next(Arc::new(msg))));
                } else {
                    // The message is corrupt. Just skip it.
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
    /// Returns the number of used entries. This is not performant.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slab.iter().filter(|c| matches!(c, Slot::Used(_))).count()
    }

    fn free(&mut self, index: u32) -> T {
        let index = index as usize;
        let value = match std::mem::replace(&mut self.slab[index], Slot::Free(self.free_index)) {
            Slot::Free(_) => panic!("Slot already free"),
            Slot::Used(value) => value,
        };
        self.free_index = index;
        value
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

/// Sends on-inactive notifications when there are no more sockets. This *must* be dropped when no
/// locks are held.
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
    use super::{create_ring_buffer, SharedBuffer, SharedBufferOptions};
    use crate::logs::shared_buffer::LazyItem;
    use crate::logs::stats::LogStreamStats;
    use crate::logs::testing::make_message;
    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fidl_fuchsia_diagnostics::StreamMode;
    use fuchsia_async as fasync;
    use fuchsia_async::TimeoutExt;
    use fuchsia_inspect::{Inspector, InspectorConfig};
    use fuchsia_inspect_derive::WithInspect;
    use futures::channel::mpsc;
    use futures::future::OptionFuture;
    use futures::stream::{FuturesUnordered, StreamExt as _};
    use futures::{poll, FutureExt};
    use ring_buffer::MAX_MESSAGE_SIZE;
    use std::future::poll_fn;
    use std::pin::pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::task::Poll;
    use std::time::Duration;

    async fn yield_to_executor() {
        let mut first_time = true;
        poll_fn(|cx| {
            if first_time {
                cx.waker().wake_by_ref();
                first_time = false;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }

    #[fuchsia::test]
    async fn push_one_message() {
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );
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
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );

        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn bad_type() {
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        container_buffer.push_back(&[0x77; 16]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn message_truncated() {
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_buffer.push_back(&msg.bytes()[..msg.bytes().len() - 1]);

        assert_eq!(container_buffer.cursor(StreamMode::Snapshot).unwrap().count().await, 0);
    }

    #[fuchsia::test]
    async fn buffer_wrapping() {
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            SharedBufferOptions { sleep_time: Duration::ZERO },
        );
        let container_buffer =
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), Arc::default());

        // Keep writing messages until we wrap.
        let mut i = 0;
        loop {
            let msg = make_message(&format!("{i}"), None, zx::BootInstant::from_nanos(i));
            container_buffer.push_back(msg.bytes());
            i += 1;

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;

            let inner = buffer.inner.lock();
            if inner.ring_buffer.head() > inner.ring_buffer.capacity() as u64 {
                break;
            }
        }

        // Read back all the messages.
        let mut cursor = pin!(container_buffer.cursor(StreamMode::Snapshot).unwrap());

        let mut j;
        assert_matches!(cursor.next().await,
        Some(LazyItem::Next(item)) => {
            j = item.timestamp().into_nanos();
            let msg = make_message(&format!("{j}"),
                                   None,
                                   item.timestamp());
            assert_eq!(&*item, &msg);
        });

        j += 1;
        while j != i {
            assert_matches!(cursor.next().await,
            Some(LazyItem::Next(item)) => {
                assert_eq!(&*item, &make_message(&format!("{j}"),
                                                 None,
                                                 item.timestamp()));
            });
            j += 1;
        }

        assert_eq!(cursor.next().await, None);
    }

    #[fuchsia::test]
    async fn on_inactive() {
        let identity = Arc::new(vec!["a"].into());
        let on_inactive = Arc::new(AtomicU64::new(0));
        let buffer = {
            let on_inactive = Arc::clone(&on_inactive);
            let identity = Arc::clone(&identity);
            Arc::new(SharedBuffer::new(
                create_ring_buffer(MAX_MESSAGE_SIZE),
                Box::new(move |i| {
                    assert_eq!(i, identity);
                    on_inactive.fetch_add(1, Ordering::Relaxed);
                }),
                SharedBufferOptions { sleep_time: Duration::ZERO },
            ))
        };
        let container_a = buffer.new_container_buffer(identity, Arc::default());
        let container_b = buffer.new_container_buffer(Arc::new(vec!["b"].into()), Arc::default());

        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));
        container_a.push_back(msg.bytes());

        // Repeatedly write messages to b until a is rolled out.
        while container_a.cursor(StreamMode::Snapshot).unwrap().count().await == 1 {
            container_b.push_back(msg.bytes());

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;
        }

        assert_eq!(on_inactive.load(Ordering::Relaxed), 1);
    }

    #[fuchsia::test]
    async fn terminate_drops_container() {
        // Silence a clippy warning; SharedBuffer needs an executor.
        async {}.await;

        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            SharedBufferOptions { sleep_time: Duration::ZERO },
        );

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

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;
        }

        assert!(container_a.cursor(StreamMode::Subscribe).is_none());
    }

    #[fuchsia::test]
    async fn cursor_subscribe() {
        for mode in [StreamMode::Subscribe, StreamMode::SnapshotThenSubscribe] {
            let buffer = SharedBuffer::new(
                create_ring_buffer(MAX_MESSAGE_SIZE),
                Box::new(|_| {}),
                Default::default(),
            );
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

            // No message should arrive. We can only use a timeout here.
            assert_eq!(
                OptionFuture::from(Some(receiver.next()))
                    .on_timeout(Duration::from_millis(500), || None)
                    .await,
                None
            );

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

    #[fuchsia::test]
    async fn cursor_rolled_out() {
        // On the first pass we roll out before the cursor has started. On the second pass, we roll
        // out after the cursor has started.
        for pass in 0..2 {
            let buffer = SharedBuffer::new(
                create_ring_buffer(MAX_MESSAGE_SIZE),
                Box::new(|_| {}),
                SharedBufferOptions { sleep_time: Duration::ZERO },
            );
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

            const A_MESSAGE_COUNT: usize = 50;
            for _ in 0..A_MESSAGE_COUNT - 2 {
                container_a.push_back(msg.bytes());
            }

            let mut cursor = pin!(container_a.cursor(StreamMode::Snapshot).unwrap());

            let mut expected = A_MESSAGE_COUNT;

            // Get the first stored message on the first pass.
            if pass == 0 {
                assert!(cursor.next().await.is_some());
                expected -= 1;
            }

            // Roll out messages until container_b is rolled out.
            while container_b.cursor(StreamMode::Snapshot).unwrap().count().await == 1 {
                container_c.push_back(msg.bytes());

                // Yield to the executor to allow messages to be rolled out.
                yield_to_executor().await;
            }

            // We should have rolled out some messages in container a.
            assert_matches!(
                cursor.next().await,
                Some(LazyItem::ItemsRolledOut(rolled_out, t))
                    if t == zx::BootInstant::from_nanos(1) && rolled_out > 0
                    => expected -= rolled_out as usize
            );

            // And check how many are remaining.
            assert_eq!(cursor.count().await, expected);
        }
    }

    #[fuchsia::test]
    async fn drained_post_termination_cursors() {
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );
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
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        );
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
        let buffer = SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            SharedBufferOptions { sleep_time: Duration::ZERO },
        );
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

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;
        }

        assert_matches!(cursor.next().await, Some(LazyItem::ItemsRolledOut(3, _)));

        assert!(poll!(cursor.next()).is_pending());

        container_a.terminate();
        assert_eq!(cursor.count().await, 0);
    }

    #[fuchsia::test]
    async fn recycled_container_slot() {
        let buffer = Arc::new(SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            SharedBufferOptions { sleep_time: Duration::ZERO },
        ));
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

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;
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
        let buffer = Arc::new(SharedBuffer::new(
            create_ring_buffer(65536),
            Box::new(|_| {}),
            Default::default(),
        ));
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
                    sockets_opened: 1u64,
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
        let buffer = Arc::new(SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            Box::new(|_| {}),
            Default::default(),
        ));
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
        let buffer = Arc::new(SharedBuffer::new(
            create_ring_buffer(MAX_MESSAGE_SIZE),
            {
                let on_inactive = Arc::clone(&on_inactive);
                let a_identity = Arc::clone(&a_identity);
                Box::new(move |id| {
                    assert_eq!(id, a_identity);
                    on_inactive.fetch_add(1, Ordering::Relaxed);
                })
            },
            SharedBufferOptions { sleep_time: Duration::ZERO },
        ));
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

            // Yield to the executor to allow messages to be rolled out.
            yield_to_executor().await;
        }

        assert_eq!(on_inactive.load(Ordering::Relaxed), 0);

        // Close the socket.
        std::mem::drop(local);

        // We don't know when the socket thread will run so we have to loop.
        while on_inactive.load(Ordering::Relaxed) != 1 {
            fasync::Timer::new(Duration::from_millis(50)).await;
        }
    }

    #[fuchsia::test]
    async fn flush() {
        let a_identity = Arc::new(vec!["a"].into());
        let buffer = Arc::new(SharedBuffer::new(
            create_ring_buffer(1024 * 1024),
            Box::new(|_| {}),
            Default::default(),
        ));
        let container_a = Arc::new(buffer.new_container_buffer(a_identity, Arc::default()));
        let msg = make_message("a", None, zx::BootInstant::from_nanos(1));

        let (local, remote) = zx::Socket::create_datagram();
        container_a.add_socket(remote);

        let cursor = pin!(container_a.cursor(StreamMode::Subscribe).unwrap());

        const COUNT: usize = 1000;
        for _ in 0..COUNT {
            local.write(msg.bytes()).unwrap();
        }

        // Race two flush futures.
        let mut flush_futures = FuturesUnordered::from_iter([buffer.flush(), buffer.flush()]);
        flush_futures.next().await;

        let messages: Option<Vec<_>> = cursor.take(COUNT).collect().now_or_never();
        assert!(messages.is_some());

        // Make sure the other one finishes too.
        flush_futures.next().await;

        // Make sure we can still terminate the buffer.
        buffer.terminate().await;
    }
}
