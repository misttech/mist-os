// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod anon_node;
mod dir_entry;
mod dirent_sink;
mod dynamic_file;
mod epoll;
mod fd_number;
mod fd_table;
mod file_object;
mod file_system;
mod file_write_guard;
mod fs_context;
mod fs_node;
mod memory_file;
mod namespace;
mod record_locks;
mod simple_file;
mod splice;
mod static_directory;
mod stubs;
mod symlink_node;
mod vec_directory;
mod wd_number;
mod xattr;

pub mod aio;
pub mod buffers;
pub mod directory_file;
pub mod eventfd;
pub mod file_server;
pub mod fs_args;
pub mod fs_registry;
pub mod fsverity;
pub mod inotify;
pub mod io_uring;
pub mod path;
pub mod pidfd;
pub mod pipe;
pub mod rw_queue;
pub mod socket;
pub mod syscalls;
pub mod timer;

pub use anon_node::*;
pub use buffers::*;
pub use dir_entry::*;
pub use directory_file::*;
pub use dirent_sink::*;
pub use dynamic_file::*;
pub use epoll::*;
pub use fd_number::*;
pub use fd_table::*;
pub use file_object::*;
pub use file_system::*;
pub use file_write_guard::*;
pub use fs_context::*;
pub use fs_node::*;
pub use memory_file::*;
pub use namespace::*;
pub use path::*;
pub use pidfd::*;
pub use record_locks::*;
pub use simple_file::*;
pub use static_directory::*;
pub use stubs::*;
pub use symlink_node::*;
pub use vec_directory::*;
pub use wd_number::*;
pub use xattr::*;

use crate::device::binder::BinderDriver;
use crate::task::CurrentTask;
use starnix_lifecycle::{ObjectReleaser, ReleaserAction};
use starnix_uapi::ownership::{Releasable, ReleaseGuard};
use std::cell::RefCell;
use std::ops::DerefMut;
use std::sync::Arc;

pub enum FileObjectReleaserAction {}
impl ReleaserAction<FileObject> for FileObjectReleaserAction {
    fn release(file_object: ReleaseGuard<FileObject>) {
        LocalReleasers::register(file_object);
    }
}
pub type FileReleaser = ObjectReleaser<FileObject, FileObjectReleaserAction>;

pub enum FsNodeReleaserAction {}
impl ReleaserAction<FsNode> for FsNodeReleaserAction {
    fn release(fs_node: ReleaseGuard<FsNode>) {
        LocalReleasers::register(fs_node);
    }
}
pub type FsNodeReleaser = ObjectReleaser<FsNode, FsNodeReleaserAction>;

pub enum BinderDriverReleaserAction {}
impl ReleaserAction<BinderDriver> for BinderDriverReleaserAction {
    fn release(driver: ReleaseGuard<BinderDriver>) {
        LocalReleasers::register(driver);
    }
}
pub type BinderDriverReleaser = ObjectReleaser<BinderDriver, BinderDriverReleaserAction>;

/// An object-safe/dyn-compatible trait to wrap `Releasable` types.
trait CurrentTaskReleasable {
    fn release_with_task(&mut self, context: &CurrentTask);
}

// Blanket impl for Option since we can't take `self` by value and remain object-safe/dyn-compat.
impl<T> CurrentTaskReleasable for Option<T>
where
    for<'a> T: Releasable<Context<'a> = &'a CurrentTask>,
{
    fn release_with_task(&mut self, context: &CurrentTask) {
        if let Some(this) = self.take() {
            <T as Releasable>::release(this, context);
        }
    }
}

thread_local! {
    /// Container of all `FileObject` that are not used anymore, but have not been closed yet.
    static RELEASERS: RefCell<Option<LocalReleasers>> = RefCell::new(Some(LocalReleasers::default()));
}

#[derive(Default)]
struct LocalReleasers {
    /// The list of entities to be deferred released.
    releasables: Vec<Box<dyn CurrentTaskReleasable>>,
}

impl LocalReleasers {
    /// Register the container to be deferred released.
    fn register<T: for<'a> Releasable<Context<'a> = &'a CurrentTask> + 'static>(to_release: T) {
        RELEASERS.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .expect("not finalized")
                .releasables
                .push(Box::new(Some(to_release)));
        });
    }

    fn is_empty(&self) -> bool {
        self.releasables.is_empty()
    }
}

impl Releasable for LocalReleasers {
    type Context<'a> = &'a CurrentTask;

    fn release(self, context: Self::Context<'_>) {
        for mut releasable in self.releasables {
            releasable.release_with_task(context);
        }
    }
}

/// Service to handle delayed releases.
///
/// Delayed releases are cleanup code that is run at specific point where the lock level is
/// known. The starnix kernel must ensure that delayed releases are run regularly.
#[derive(Debug, Default)]
pub struct DelayedReleaser {}

impl DelayedReleaser {
    pub fn flush_file(&self, file: &FileHandle, id: FdTableId) {
        LocalReleasers::register(FlushedFile(Arc::clone(file), id));
    }

    /// Run all current delayed releases for the current thread.
    pub fn apply(&self, current_task: &CurrentTask) {
        loop {
            let releasers = RELEASERS.with(|cell| {
                std::mem::take(cell.borrow_mut().as_mut().expect("not finalized").deref_mut())
            });
            if releasers.is_empty() {
                return;
            }
            releasers.release(current_task);
        }
    }

    /// Prevent any further releasables from being registered on this thread.
    ///
    /// This function should be called during thread teardown to ensure that we do not
    /// register any new releasables on this thread after we have finalized the delayed
    /// releasables for the last time.
    pub fn finalize() {
        RELEASERS.with(|cell| {
            assert!(cell.borrow().as_ref().expect("not finalized").is_empty());
            *cell.borrow_mut() = None;
        });
    }
}

struct FlushedFile(FileHandle, FdTableId);

impl Releasable for FlushedFile {
    type Context<'a> = &'a CurrentTask;
    fn release(self, context: &CurrentTask) {
        self.0.flush(context, self.1);
    }
}
