// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::MemoryAccessorExt;
use crate::task::{CurrentTask, EventHandler, Kernel, WaitCanceler, WaitQueue, Waiter};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    default_ioctl, fileops_impl_nonseekable, fileops_impl_noop_sync, fs_args, inotify, Anon,
    BytesFile, BytesFileOps, DirEntryHandle, FileHandle, FileObject, FileOps, FileReleaser,
    FsNodeOps, FsStr, FsString, WdNumber,
};
use starnix_sync::{FileOpsCore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::auth::CAP_SYS_ADMIN;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::inotify_mask::InotifyMask;
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{errno, error, inotify_event, FIONREAD};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::mem::size_of;
use std::sync::atomic::{AtomicI32, Ordering};
use zerocopy::IntoBytes;

const DATA_SIZE: usize = size_of::<inotify_event>();

type FileHandleKey = WeakKey<FileReleaser>;

// InotifyFileObject represents an inotify instance created by inotify_init(2) or inotify_init1(2).
pub struct InotifyFileObject {
    state: Mutex<InotifyState>,
}

struct InotifyState {
    events: InotifyEventQueue,

    watches: HashMap<WdNumber, DirEntryHandle>,

    // Last created WdNumber, stored as raw i32. WdNumber's are unique per inotify instance.
    last_watch_id: i32,
}

// InotifyWatcher's attach to a FsNode.
pub struct InotifyWatcher {
    pub watch_id: WdNumber,

    pub mask: InotifyMask,
}

#[derive(Default)]
pub struct InotifyWatchers {
    watchers: Mutex<BTreeMap<FileHandleKey, InotifyWatcher>>,
}

#[derive(Default)]
struct InotifyEventQueue {
    // queue can contain max_queued_events inotify events, plus one optional IN_Q_OVERFLOW event
    // if more events arrive.
    queue: VecDeque<InotifyEvent>,

    // Waiters to notify of new inotify events.
    waiters: WaitQueue,

    // Total size of InotifyEvent objects in queue, when serialized into inotify_event.
    size_bytes: usize,

    // This value is copied from /proc/sys/fs/inotify/max_queued_events on creation and is
    // constant afterwards, even if the proc file is modified.
    max_queued_events: usize,
}

// Serialized to inotify_event, see inotify(7).
#[derive(Debug, PartialEq, Eq)]
struct InotifyEvent {
    watch_id: WdNumber,

    mask: InotifyMask,

    cookie: u32,

    name: FsString,
}

impl InotifyState {
    fn next_watch_id(&mut self) -> WdNumber {
        self.last_watch_id += 1;
        WdNumber::from_raw(self.last_watch_id)
    }
}

impl InotifyFileObject {
    /// Allocate a new, empty inotify instance.
    pub fn new_file(current_task: &CurrentTask, non_blocking: bool) -> FileHandle {
        let flags =
            OpenFlags::RDONLY | if non_blocking { OpenFlags::NONBLOCK } else { OpenFlags::empty() };
        let max_queued_events =
            current_task.kernel().system_limits.inotify.max_queued_events.load(Ordering::Relaxed);
        assert!(max_queued_events >= 0);
        Anon::new_file(
            current_task,
            Box::new(InotifyFileObject {
                state: InotifyState {
                    events: InotifyEventQueue::new_with_max(max_queued_events as usize),
                    watches: Default::default(),
                    last_watch_id: 0,
                }
                .into(),
            }),
            flags,
            "inotify",
        )
    }

    /// Adds a watch to the inotify instance.
    ///
    /// Attaches an InotifyWatcher to the DirEntry's FsNode.
    /// Inotify keeps the DirEntryHandle in case it is evicted from dcache.
    pub fn add_watch(
        &self,
        dir_entry: DirEntryHandle,
        mask: InotifyMask,
        inotify_file: &FileHandle,
    ) -> Result<WdNumber, Errno> {
        let weak_key = WeakKey::from(inotify_file);
        if let Some(watch_id) = dir_entry.node.watchers.maybe_update(mask, &weak_key)? {
            return Ok(watch_id);
        }

        let watch_id;
        {
            let mut state = self.state.lock();
            watch_id = state.next_watch_id();
            state.watches.insert(watch_id, dir_entry.clone());
        }
        dir_entry.node.watchers.add(mask, watch_id, weak_key);
        Ok(watch_id)
    }

    /// Removes a watch to the inotify instance.
    ///
    /// Detaches the corresponding InotifyWatcher from FsNode.
    pub fn remove_watch(&self, watch_id: WdNumber, file: &FileHandle) -> Result<(), Errno> {
        let dir_entry;
        {
            let mut state = self.state.lock();
            dir_entry = state.watches.remove(&watch_id).ok_or_else(|| errno!(EINVAL))?;
            state.events.enqueue(InotifyEvent::new(
                watch_id,
                InotifyMask::IGNORED,
                0,
                FsString::default(),
            ));
        }
        dir_entry.node.watchers.remove(&WeakKey::from(file));
        Ok(())
    }

    fn notify(
        &self,
        watch_id: WdNumber,
        event_mask: InotifyMask,
        cookie: u32,
        name: &FsStr,
        remove_watcher_after_notify: bool,
    ) {
        // Holds a DirEntry pending deletion to be dropped after releasing the state mutex.
        #[allow(clippy::collection_is_never_read)]
        let _dir_entry: Option<DirEntryHandle>;
        {
            let mut state = self.state.lock();
            state.events.enqueue(InotifyEvent::new(watch_id, event_mask, cookie, name.to_owned()));
            if remove_watcher_after_notify {
                _dir_entry = state.watches.remove(&watch_id);
                state.events.enqueue(InotifyEvent::new(
                    watch_id,
                    InotifyMask::IGNORED,
                    0,
                    FsString::default(),
                ));
            }
        }
    }

    fn available(&self) -> usize {
        let state = self.state.lock();
        state.events.size_bytes
    }
}

impl FileOps for InotifyFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);

        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            let mut state = self.state.lock();
            if let Some(front) = state.events.front() {
                if data.available() < front.size() {
                    return error!(EINVAL);
                }
            } else {
                return error!(EAGAIN);
            }

            let mut bytes_read: usize = 0;
            while let Some(front) = state.events.front() {
                if data.available() < front.size() {
                    break;
                }
                // Linux always dequeues an available event as long as there's enough buffer space to
                // copy it out, even if the copy below fails. Emulate this behaviour.
                bytes_read += state.events.dequeue().unwrap().write_to(data)?;
            }
            Ok(bytes_read)
        })
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            FIONREAD => {
                let addr = UserRef::<i32>::new(user_addr);
                let size = i32::try_from(self.available()).unwrap_or(i32::MAX);
                current_task.write_object(addr, &size).map(|_| SUCCESS)
            }
            _ => default_ioctl(file, locked, current_task, request, arg),
        }
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.state.lock().events.waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        if self.available() > 0 {
            Ok(FdEvents::POLLIN)
        } else {
            Ok(FdEvents::empty())
        }
    }

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
    ) {
        let dir_entries = {
            let mut state = self.state.lock();
            state.watches.drain().map(|(_key, value)| value).collect::<Vec<_>>()
        };

        for dir_entry in dir_entries {
            dir_entry.node.watchers.remove_by_ref(file);
        }
    }
}

impl InotifyEventQueue {
    fn new_with_max(max_queued_events: usize) -> Self {
        InotifyEventQueue {
            queue: Default::default(),
            waiters: Default::default(),
            size_bytes: 0,
            max_queued_events,
        }
    }

    fn enqueue(&mut self, mut event: InotifyEvent) {
        if self.queue.len() > self.max_queued_events {
            return;
        }
        if self.queue.len() == self.max_queued_events {
            // If this event will overflow the queue, discard it and enqueue IN_Q_OVERFLOW instead.
            event = InotifyEvent::new(
                WdNumber::from_raw(-1),
                InotifyMask::Q_OVERFLOW,
                0,
                FsString::default(),
            );
        }
        if Some(&event) == self.queue.back() {
            // From https://man7.org/linux/man-pages/man7/inotify.7.html
            // If successive output inotify events produced on the inotify file
            // descriptor are identical (same wd, mask, cookie, and name), then
            // they are coalesced into a single event if the older event has not
            // yet been read.
            return;
        }
        self.size_bytes += event.size();
        self.queue.push_back(event);
        self.waiters.notify_fd_events(FdEvents::POLLIN);
    }

    fn front(&self) -> Option<&InotifyEvent> {
        self.queue.front()
    }

    fn dequeue(&mut self) -> Option<InotifyEvent> {
        let maybe_event = self.queue.pop_front();
        if let Some(event) = maybe_event.as_ref() {
            self.size_bytes -= event.size();
        }
        maybe_event
    }
}

impl InotifyEvent {
    // Creates a new InotifyEvent and pads name with at least 1 null-byte, aligned to DATA_SIZE.
    fn new(watch_id: WdNumber, mask: InotifyMask, cookie: u32, mut name: FsString) -> Self {
        if !name.is_empty() {
            let len = round_up_to_increment(name.len() + 1, DATA_SIZE)
                .expect("padded name should not overflow");
            name.resize(len, 0);
        }
        InotifyEvent { watch_id, mask, cookie, name }
    }

    fn size(&self) -> usize {
        DATA_SIZE + self.name.len()
    }

    fn write_to(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        let event = inotify_event {
            wd: self.watch_id.raw(),
            mask: self.mask.bits(),
            cookie: self.cookie,
            len: self.name.len().try_into().map_err(|_| errno!(EINVAL))?,
            // name field is zero-sized; the bytes for the name follows the struct linearly in memory.
            name: Default::default(),
        };

        let mut bytes_written = data.write(event.as_bytes())?;
        if !self.name.is_empty() {
            bytes_written += data.write(self.name.as_bytes())?;
        }

        debug_assert!(bytes_written == self.size());
        Ok(bytes_written)
    }
}

impl InotifyWatchers {
    fn add(&self, mask: InotifyMask, watch_id: WdNumber, inotify: FileHandleKey) {
        let mut watchers = self.watchers.lock();
        watchers.insert(inotify, inotify::InotifyWatcher { watch_id, mask });
    }

    // Checks if inotify is already part of watchers. Replaces mask if found and returns the WdNumber.
    // Combines mask if IN_MASK_ADD is specified in mask. Returns None if no present in watchers.
    //
    // Errors if:
    //  - both IN_MASK_ADD and IN_MASK_CREATE are specified in mask, or
    //  - IN_MASK_CREATE is specified and existing entry is found.
    fn maybe_update(
        &self,
        mask: InotifyMask,
        inotify: &FileHandleKey,
    ) -> Result<Option<WdNumber>, Errno> {
        let combine_existing = mask.contains(InotifyMask::MASK_ADD);
        let create_new = mask.contains(InotifyMask::MASK_CREATE);
        if combine_existing && create_new {
            return error!(EINVAL);
        }

        let mut watchers = self.watchers.lock();
        if let Some(watcher) = watchers.get_mut(inotify) {
            if create_new {
                return error!(EEXIST);
            }

            if combine_existing {
                watcher.mask.insert(mask);
            } else {
                watcher.mask = mask;
            }
            Ok(Some(watcher.watch_id))
        } else {
            Ok(None)
        }
    }

    fn remove(&self, inotify: &FileHandleKey) {
        let mut watchers = self.watchers.lock();
        watchers.remove(inotify);
    }

    fn remove_by_ref(&self, inotify: &FileObject) {
        let mut watchers = self.watchers.lock();
        watchers.retain(|weak_key, _| {
            if let Some(handle) = weak_key.0.upgrade() {
                !std::ptr::eq(handle.as_ref().as_ref(), inotify)
            } else {
                false
            }
        })
    }

    /// Notifies all watchers that listen for the specified event mask with
    /// struct inotify_event { event_mask, cookie, name }.
    ///
    /// If event_mask is IN_DELETE_SELF, all watchers are removed after this event.
    /// cookie is used to link a pair of IN_MOVE_FROM/IN_MOVE_TO events only.
    /// mode is used to check whether IN_ISDIR should be combined with event_mask.
    pub fn notify(
        &self,
        mut event_mask: InotifyMask,
        cookie: u32,
        name: &FsStr,
        mode: FileMode,
        is_dead: bool,
    ) {
        if cookie != 0 {
            // From https://man7.org/linux/man-pages/man7/inotify.7.html,
            // cookie is only used for rename events.
            debug_assert!(
                event_mask.contains(InotifyMask::MOVE_FROM)
                    || event_mask.contains(InotifyMask::MOVE_TO)
            );
        }
        // Clone inotify references so that we don't hold watchers lock when notifying.
        struct InotifyWatch {
            watch_id: WdNumber,
            file: FileHandle,
            should_remove: bool,
        }
        let mut watches: Vec<InotifyWatch> = vec![];
        {
            let mut watchers = self.watchers.lock();
            watchers.retain(|inotify, watcher| {
                let mut should_remove = event_mask == InotifyMask::DELETE_SELF;
                if watcher.mask.contains(event_mask)
                    && !(is_dead && watcher.mask.contains(InotifyMask::EXCL_UNLINK))
                {
                    should_remove = should_remove || watcher.mask.contains(InotifyMask::ONESHOT);
                    if let Some(file) = inotify.0.upgrade() {
                        watches.push(InotifyWatch {
                            watch_id: watcher.watch_id,
                            file,
                            should_remove,
                        });
                    } else {
                        should_remove = true;
                    }
                }
                !should_remove
            });
        }

        if mode.is_dir() {
            // Linux does not report IN_ISDIR with IN_DELETE_SELF or IN_MOVE_SELF for directories.
            if event_mask != InotifyMask::DELETE_SELF && event_mask != InotifyMask::MOVE_SELF {
                event_mask |= InotifyMask::ISDIR;
            }
        }

        for watch in watches {
            let inotify = watch
                .file
                .downcast_file::<InotifyFileObject>()
                .expect("failed to downcast to inotify");
            inotify.notify(watch.watch_id, event_mask, cookie, name, watch.should_remove);
        }
    }
}

impl Kernel {
    pub fn get_next_inotify_cookie(&self) -> u32 {
        let cookie = self.next_inotify_cookie.next();
        // 0 is an invalid cookie.
        if cookie == 0 {
            return self.next_inotify_cookie.next();
        }
        cookie
    }
}

/// Corresponds to files in /proc/sys/fs/inotify/, but cannot be negative.
pub struct InotifyLimits {
    // This value is used when creating an inotify instance.
    // Updating this value does not affect already-created inotify instances.
    pub max_queued_events: AtomicI32,

    // TODO(b/297439734): Make this a real user limit on inotify instances.
    pub max_user_instances: AtomicI32,

    // TODO(b/297439734): Make this a real user limit on inotify watches.
    pub max_user_watches: AtomicI32,
}

pub trait AtomicGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicI32;
}

pub struct MaxQueuedEventsGetter;
impl AtomicGetter for MaxQueuedEventsGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicI32 {
        &current_task.kernel().system_limits.inotify.max_queued_events
    }
}

pub struct MaxUserInstancesGetter;
impl AtomicGetter for MaxUserInstancesGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicI32 {
        &current_task.kernel().system_limits.inotify.max_user_instances
    }
}

pub struct MaxUserWatchesGetter;
impl AtomicGetter for MaxUserWatchesGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicI32 {
        &current_task.kernel().system_limits.inotify.max_user_watches
    }
}

pub struct InotifyLimitProcFile<G: AtomicGetter + Send + Sync + 'static> {
    marker: std::marker::PhantomData<G>,
}

impl<G: AtomicGetter + Send + Sync + 'static> InotifyLimitProcFile<G> {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { marker: Default::default() })
    }
}

impl<G: AtomicGetter + Send + Sync + 'static> BytesFileOps for InotifyLimitProcFile<G> {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }
        let value = fs_args::parse::<i32>(data.as_slice().into())?;
        if value < 0 {
            return error!(EINVAL);
        }
        G::get_atomic(current_task).store(value, Ordering::Relaxed);
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(G::get_atomic(current_task).load(Ordering::Relaxed).to_string().into_bytes().into())
    }
}

pub type InotifyMaxQueuedEvents = InotifyLimitProcFile<MaxQueuedEventsGetter>;
pub type InotifyMaxUserInstances = InotifyLimitProcFile<MaxUserInstancesGetter>;
pub type InotifyMaxUserWatches = InotifyLimitProcFile<MaxUserWatchesGetter>;

#[cfg(test)]
mod tests {
    use super::{InotifyEvent, InotifyEventQueue, InotifyFileObject, DATA_SIZE};
    use crate::testing::{create_kernel_and_task, create_kernel_task_and_unlocked};
    use crate::vfs::buffers::VecOutputBuffer;
    use crate::vfs::{OutputBuffer, WdNumber};
    use starnix_uapi::arc_key::WeakKey;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::inotify_mask::InotifyMask;

    #[::fuchsia::test]
    fn inotify_event() {
        let event = InotifyEvent::new(WdNumber::from_raw(1), InotifyMask::ACCESS, 0, "".into());
        let mut buffer = VecOutputBuffer::new(DATA_SIZE + 100);
        let bytes_written = event.write_to(&mut buffer).expect("write_to buffer");

        assert_eq!(bytes_written, DATA_SIZE);
        assert_eq!(buffer.bytes_written(), DATA_SIZE);
    }

    #[::fuchsia::test]
    fn inotify_event_with_name() {
        // Create a name that is shorter than DATA_SIZE of 16.
        let name = "file1";
        let event = InotifyEvent::new(WdNumber::from_raw(1), InotifyMask::ACCESS, 0, name.into());
        let mut buffer = VecOutputBuffer::new(DATA_SIZE + 100);
        let bytes_written = event.write_to(&mut buffer).expect("write_to buffer");

        assert!(bytes_written > DATA_SIZE);
        assert_eq!(bytes_written % DATA_SIZE, 0);
        assert_eq!(buffer.bytes_written(), bytes_written);
    }

    #[::fuchsia::test]
    fn inotify_event_queue() {
        let mut event_queue = InotifyEventQueue::new_with_max(10);

        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::ACCESS,
            0,
            "".into(),
        ));

        assert_eq!(event_queue.queue.len(), 1);
        assert_eq!(event_queue.size_bytes, DATA_SIZE);

        let event = event_queue.dequeue();

        assert_eq!(
            event,
            Some(InotifyEvent::new(WdNumber::from_raw(1), InotifyMask::ACCESS, 0, "".into()))
        );
        assert_eq!(event_queue.queue.len(), 0);
        assert_eq!(event_queue.size_bytes, 0);
    }

    #[::fuchsia::test]
    fn inotify_event_queue_coalesce_events() {
        let mut event_queue = InotifyEventQueue::new_with_max(10);

        // Generate 2 identical events. They should combine into 1.
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::ACCESS,
            0,
            "".into(),
        ));
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::ACCESS,
            0,
            "".into(),
        ));

        assert_eq!(event_queue.queue.len(), 1);
    }

    #[::fuchsia::test]
    fn inotify_event_queue_max_queued_events() {
        let mut event_queue = InotifyEventQueue::new_with_max(1);

        // Generate 2 events, but the second event overflows the queue.
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::ACCESS,
            0,
            "".into(),
        ));
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::MODIFY,
            0,
            "".into(),
        ));

        assert_eq!(event_queue.queue.len(), 2);
        assert_eq!(event_queue.queue.get(0).unwrap().mask, InotifyMask::ACCESS);
        assert_eq!(event_queue.queue.get(1).unwrap().mask, InotifyMask::Q_OVERFLOW);

        // More events cannot be added to the queue.
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::ATTRIB,
            0,
            "".into(),
        ));
        assert_eq!(event_queue.queue.len(), 2);
        assert_eq!(event_queue.queue.get(0).unwrap().mask, InotifyMask::ACCESS);
        assert_eq!(event_queue.queue.get(1).unwrap().mask, InotifyMask::Q_OVERFLOW);

        // Dequeue 1 event.
        let _event = event_queue.dequeue();
        assert_eq!(event_queue.queue.len(), 1);

        // More events still cannot make it to the queue. This is because they would cause an overflow,
        // but there is already a Q_OVERFLOW event in the queue so we do not enqueue another one.
        event_queue.enqueue(InotifyEvent::new(
            WdNumber::from_raw(1),
            InotifyMask::DELETE,
            0,
            "".into(),
        ));
        assert_eq!(event_queue.queue.len(), 1);
        assert_eq!(event_queue.queue.get(0).unwrap().mask, InotifyMask::Q_OVERFLOW);
    }

    #[::fuchsia::test]
    async fn notify_from_watchers() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let file = InotifyFileObject::new_file(&current_task, true);
        let inotify =
            file.downcast_file::<InotifyFileObject>().expect("failed to downcast to inotify");

        // Use root as the watched directory.
        let root = current_task.fs().root().entry;
        assert!(inotify.add_watch(root.clone(), InotifyMask::ALL_EVENTS, &file).is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
        }

        // Generate 1 event.
        root.node.watchers.notify(
            InotifyMask::ACCESS,
            0,
            Default::default(),
            FileMode::IFREG,
            false,
        );

        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.watches.len(), 1);
            assert_eq!(state.events.queue.len(), 1);
        }

        // Generate another event.
        root.node.watchers.notify(
            InotifyMask::ATTRIB,
            0,
            Default::default(),
            FileMode::IFREG,
            false,
        );

        assert_eq!(inotify.available(), DATA_SIZE * 2);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.queue.len(), 2);
        }

        // Read 1 event.
        let mut buffer = VecOutputBuffer::new(DATA_SIZE);
        let bytes_read =
            file.read(&mut locked, &current_task, &mut buffer).expect("read into buffer");

        assert_eq!(bytes_read, DATA_SIZE);
        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.queue.len(), 1);
        }

        // Read other event.
        buffer.reset();
        let bytes_read =
            file.read(&mut locked, &current_task, &mut buffer).expect("read into buffer");

        assert_eq!(bytes_read, DATA_SIZE);
        assert_eq!(inotify.available(), 0);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.queue.len(), 0);
        }
    }

    #[::fuchsia::test]
    async fn notify_deletion_from_watchers() {
        let (_kernel, current_task) = create_kernel_and_task();

        let file = InotifyFileObject::new_file(&current_task, true);
        let inotify =
            file.downcast_file::<InotifyFileObject>().expect("failed to downcast to inotify");

        // Use root as the watched directory.
        let root = current_task.fs().root().entry;
        assert!(inotify.add_watch(root.clone(), InotifyMask::ALL_EVENTS, &file).is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
        }

        root.node.watchers.notify(
            InotifyMask::DELETE_SELF,
            0,
            Default::default(),
            FileMode::IFREG,
            false,
        );

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 0);
        }

        {
            let state = inotify.state.lock();
            assert_eq!(state.watches.len(), 0);
            assert_eq!(state.events.queue.len(), 2);

            assert_eq!(state.events.queue.get(0).unwrap().mask, InotifyMask::DELETE_SELF);
            assert_eq!(state.events.queue.get(1).unwrap().mask, InotifyMask::IGNORED);
        }
    }

    #[::fuchsia::test]
    async fn inotify_on_same_file() {
        let (_kernel, current_task) = create_kernel_and_task();

        let file = InotifyFileObject::new_file(&current_task, true);
        let file_key = WeakKey::from(&file);
        let inotify =
            file.downcast_file::<InotifyFileObject>().expect("failed to downcast to inotify");

        // Use root as the watched directory.
        let root = current_task.fs().root().entry;

        // Cannot add with both MASK_ADD and MASK_CREATE.
        assert!(inotify
            .add_watch(
                root.clone(),
                InotifyMask::MODIFY | InotifyMask::MASK_ADD | InotifyMask::MASK_CREATE,
                &file
            )
            .is_err());

        assert!(inotify
            .add_watch(root.clone(), InotifyMask::MODIFY | InotifyMask::MASK_CREATE, &file)
            .is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
            assert!(watchers.get(&file_key).unwrap().mask.contains(InotifyMask::MODIFY));
        }

        // Replaces existing mask.
        assert!(inotify.add_watch(root.clone(), InotifyMask::ACCESS, &file).is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
            assert!(watchers.get(&file_key).unwrap().mask.contains(InotifyMask::ACCESS));
            assert!(!watchers.get(&file_key).unwrap().mask.contains(InotifyMask::MODIFY));
        }

        // Merges with existing mask.
        assert!(inotify
            .add_watch(root.clone(), InotifyMask::MODIFY | InotifyMask::MASK_ADD, &file)
            .is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
            assert!(watchers.get(&file_key).unwrap().mask.contains(InotifyMask::ACCESS));
            assert!(watchers.get(&file_key).unwrap().mask.contains(InotifyMask::MODIFY));
        }
    }

    #[::fuchsia::test]
    async fn coalesce_events() {
        let (_kernel, current_task) = create_kernel_and_task();

        let file = InotifyFileObject::new_file(&current_task, true);
        let inotify =
            file.downcast_file::<InotifyFileObject>().expect("failed to downcast to inotify");

        // Use root as the watched directory.
        let root = current_task.fs().root().entry;
        assert!(inotify.add_watch(root.clone(), InotifyMask::ALL_EVENTS, &file).is_ok());

        {
            let watchers = root.node.watchers.watchers.lock();
            assert_eq!(watchers.len(), 1);
        }

        // Generate 2 identical events. They should combine into 1.
        root.node.watchers.notify(
            InotifyMask::ACCESS,
            0,
            Default::default(),
            FileMode::IFREG,
            false,
        );
        root.node.watchers.notify(
            InotifyMask::ACCESS,
            0,
            Default::default(),
            FileMode::IFREG,
            false,
        );

        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.watches.len(), 1);
            assert_eq!(state.events.queue.len(), 1);
        }
    }
}
