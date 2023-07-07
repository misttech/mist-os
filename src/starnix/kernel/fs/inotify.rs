// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    sync::Arc,
};
use zerocopy::AsBytes;

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::Mutex,
    mm::{vmo::round_up_to_increment, MemoryAccessorExt},
    syscalls::*,
    task::{CurrentTask, EventHandler, WaitCanceler, Waiter},
    types::*,
};

const DATA_SIZE: usize = size_of::<inotify_event>();

// InotifyFileObject represents an inotify instance created by inotify_init(2) or inotify_init1(2).
#[derive(Default)]
pub struct InotifyFileObject {
    state: Mutex<InotifyState>,
}

#[derive(Default)]
struct InotifyState {
    events: VecDeque<InotifyEvent>,

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
    watchers: Mutex<HashMap<WeakKey<FileObject>, InotifyWatcher>>,
}

// Serialized to inotify_event, see inotify(7).
#[derive(PartialEq, Eq)]
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
        Anon::new_file(current_task, Box::<InotifyFileObject>::default(), flags)
    }

    /// Adds a watch to the inotify instance.
    ///
    /// Attaches an InotifyWatcher to the DirEntry's FsNode.
    /// Inotify keeps the DirEntryHandle in case it is evicted from dcache.
    pub fn add_watch(
        &self,
        dir_entry: DirEntryHandle,
        mask: InotifyMask,
        inotify_file: &Arc<FileObject>,
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
    pub fn remove_watch(&self, watch_id: WdNumber, file: &Arc<FileObject>) -> Result<(), Errno> {
        let dir_entry;
        {
            let mut state = self.state.lock();
            dir_entry = state.watches.remove(&watch_id).ok_or_else(|| errno!(EINVAL))?;
            state.events.push_back(InotifyEvent::new(
                watch_id,
                InotifyMask::IGNORED,
                0,
                FsString::new(),
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
        name: FsString,
        oneshot: bool,
    ) {
        let mut state = self.state.lock();
        let new_event = InotifyEvent::new(watch_id, event_mask, cookie, name);
        if Some(&new_event) == state.events.back() {
            debug_assert!(!oneshot, "oneshot watchers cannot create 2 equivalent events");
            return;
        }
        state.events.push_back(new_event);
        if oneshot {
            state.watches.remove(&watch_id);
            state.events.push_back(InotifyEvent::new(
                watch_id,
                InotifyMask::IGNORED,
                0,
                FsString::new(),
            ));
        }
    }

    fn available(&self) -> usize {
        let state = self.state.lock();
        state.events.iter().fold(0, |total, event| total.saturating_add(event.size()))
    }
}

impl FileOps for InotifyFileObject {
    fileops_impl_nonseekable!();

    fn write(
        &self,
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
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);

        let mut state = self.state.lock();
        if let Some(front) = state.events.front() {
            if data.available() < front.size() {
                return error!(EINVAL);
            }
        } else {
            // TODO(fxbug.dev/79283): implement blocking read.
            return error!(EAGAIN);
        }

        let mut bytes_read: usize = 0;
        while let Some(front) = state.events.front() {
            if data.available() < front.size() {
                break;
            }
            // Linux always dequeues an available event as long as there's enough buffer space to
            // copy it out, even if the copy below fails. Emulate this behaviour.
            bytes_read += state.events.pop_front().unwrap().write_to(data)?;
        }
        Ok(bytes_read)
    }

    fn ioctl(
        &self,
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
                current_task.mm.write_object(addr, &size).map(|_| SUCCESS)
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }

    fn close(&self, file: &FileObject) {
        let dir_entries = {
            let state = self.state.lock();
            state.watches.values().cloned().collect::<Vec<_>>()
        };

        for dir_entry in dir_entries {
            dir_entry.node.watchers.remove_by_ref(file);
        }
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
    fn add(&self, mask: InotifyMask, watch_id: WdNumber, inotify: WeakKey<FileObject>) {
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
        inotify: &WeakKey<FileObject>,
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

    fn remove(&self, inotify: &WeakKey<FileObject>) {
        let mut watchers = self.watchers.lock();
        watchers.remove(inotify);
    }

    fn remove_by_ref(&self, inotify: &FileObject) {
        let mut watchers = self.watchers.lock();
        watchers.retain(|weak_key, _| weak_key.0.as_ptr() != inotify as *const _);
    }

    /// Notifies all watchers that have the specified event mask.
    pub fn notify(&self, event_mask: InotifyMask, name: &FsString) {
        // Clone inotify references so that we don't hold watchers lock when notifying.
        struct InotifyWatch {
            watch_id: WdNumber,
            file: Arc<FileObject>,
            oneshot: bool,
        }
        let mut watches: Vec<InotifyWatch> = vec![];
        {
            let mut watchers = self.watchers.lock();
            watchers.retain(|inotify, watcher| {
                if watcher.mask.contains(event_mask) {
                    let oneshot = watcher.mask.contains(InotifyMask::ONESHOT);
                    if let Some(file) = inotify.0.upgrade() {
                        watches.push(InotifyWatch { watch_id: watcher.watch_id, file, oneshot });
                        !oneshot
                    } else {
                        false
                    }
                } else {
                    true
                }
            });
        }

        for watch in watches {
            let inotify = watch
                .file
                .downcast_file::<inotify::InotifyFileObject>()
                .expect("failed to downcast to inotify");
            inotify.notify(watch.watch_id, event_mask, 0, name.clone(), watch.oneshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fs::buffers::VecOutputBuffer, testing::*};

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
    async fn notify_from_watchers() {
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

        // Generate 1 event.
        root.node.watchers.notify(InotifyMask::ACCESS, &"".into());

        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.watches.len(), 1);
            assert_eq!(state.events.len(), 1);
        }

        // Generate another event.
        root.node.watchers.notify(InotifyMask::ATTRIB, &"".into());

        assert_eq!(inotify.available(), DATA_SIZE * 2);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.len(), 2);
        }

        // Read 1 event.
        let mut buffer = VecOutputBuffer::new(DATA_SIZE);
        let bytes_read = file.read(&current_task, &mut buffer).expect("read into buffer");

        assert_eq!(bytes_read, DATA_SIZE);
        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.len(), 1);
        }

        // Read other event.
        buffer.reset();
        let bytes_read = file.read(&current_task, &mut buffer).expect("read into buffer");

        assert_eq!(bytes_read, DATA_SIZE);
        assert_eq!(inotify.available(), 0);
        {
            let state = inotify.state.lock();
            assert_eq!(state.events.len(), 0);
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
        root.node.watchers.notify(InotifyMask::ACCESS, &"".into());
        root.node.watchers.notify(InotifyMask::ACCESS, &"".into());

        assert_eq!(inotify.available(), DATA_SIZE);
        {
            let state = inotify.state.lock();
            assert_eq!(state.watches.len(), 1);
            assert_eq!(state.events.len(), 1);
        }
    }
}
