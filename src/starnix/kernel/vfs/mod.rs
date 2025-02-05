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
mod userfault_file;
mod vec_directory;
mod wd_number;
mod xattr;

pub mod aio;
pub mod buffers;
pub mod crypt_service;
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
pub use userfault_file::*;
pub use vec_directory::*;
pub use wd_number::*;
pub use xattr::*;

#[cfg(not(feature = "starnix_lite"))]
use crate::device::binder::BinderDriver;
use crate::task::CurrentTask;
use starnix_lifecycle::{ObjectReleaser, ReleaserAction};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::ownership::{Releasable, ReleaseGuard};
use std::cell::RefCell;
use std::ops::DerefMut;
use std::sync::Arc;

/// Register the container to be deferred released.
// This could be done through a blanket implementation, but doesn't compile because of the lifetime
// dependency.
//
// Can be replaced when https://github.com/rust-lang/rust/issues/100013 is fixed by:
//
// fn register<T: for<'a, 'b> Releasable<Context<'a, 'b> = CurrentTaskAndLocked<'a, 'b>> + 'static>(
//     to_release: T,
// ) {
//     RELEASERS.with(|cell| {
//         cell.borrow_mut()
//             .as_mut()
//             .expect("not finalized")
//             .releasables
//             .push(Box::new(Some(to_release)));
//     });
// }
macro_rules! register {
    ($arg:expr) => {
        RELEASERS.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .expect("DelayedReleaser hasn't been finalized yet")
                .releasables
                .push(Box::new(Some($arg)));
        });
    };
}

/// Macro impl for Option since we can't take `self` by value and remain object-safe/dyn-compat.
///
// This could be done through a blanket implementation, but doesn't compile because of the lifetime
// dependency.
//
// Can be replaced when https://github.com/rust-lang/rust/issues/100013 is fixed by:
//
// impl<T> CurrentTaskAndLockedReleasable for Option<T>
// where
//    for<'b, 'a> T: Releasable<Context<'a, 'b> = CurrentTaskAndLocked<'a, 'b>>,
// {
//     fn release_with_context(&mut self, context: CurrentTaskAndLocked<'_, '_>) {
//         if let Some(this) = self.take() {
//             <T as Releasable>::release(this, context);
//         }
//     }
// }
macro_rules! impl_ctr_for_option {
    ($arg:ty) => {
        impl CurrentTaskAndLockedReleasable for Option<$arg> {
            fn release_with_context(&mut self, context: CurrentTaskAndLocked<'_, '_>) {
                if let Some(this) = self.take() {
                    <$arg as Releasable>::release(this, context);
                }
            }
        }
    };
}

pub enum FileObjectReleaserAction {}
impl ReleaserAction<FileObject> for FileObjectReleaserAction {
    fn release(file_object: ReleaseGuard<FileObject>) {
        register!(file_object);
    }
}
pub type FileReleaser = ObjectReleaser<FileObject, FileObjectReleaserAction>;
impl_ctr_for_option!(ReleaseGuard<FileObject>);

pub enum FsNodeReleaserAction {}
impl ReleaserAction<FsNode> for FsNodeReleaserAction {
    fn release(fs_node: ReleaseGuard<FsNode>) {
        register!(fs_node);
    }
}
pub type FsNodeReleaser = ObjectReleaser<FsNode, FsNodeReleaserAction>;
impl_ctr_for_option!(ReleaseGuard<FsNode>);

#[cfg(not(feature = "starnix_lite"))]
pub enum BinderDriverReleaserAction {}
#[cfg(not(feature = "starnix_lite"))]
impl ReleaserAction<BinderDriver> for BinderDriverReleaserAction {
    fn release(driver: ReleaseGuard<BinderDriver>) {
        register!(driver);
    }
}
#[cfg(not(feature = "starnix_lite"))]
pub type BinderDriverReleaser = ObjectReleaser<BinderDriver, BinderDriverReleaserAction>;
impl_ctr_for_option!(ReleaseGuard<BinderDriver>);

pub type CurrentTaskAndLocked<'a, 'b> = (&'b mut Locked<'a, FileOpsCore>, &'b CurrentTask);

/// An object-safe/dyn-compatible trait to wrap `Releasable` types.
trait CurrentTaskAndLockedReleasable {
    fn release_with_context(&mut self, context: CurrentTaskAndLocked<'_, '_>);
}

thread_local! {
    /// Container of all `FileObject` that are not used anymore, but have not been closed yet.
    static RELEASERS: RefCell<Option<LocalReleasers>> = RefCell::new(Some(LocalReleasers::default()));
}

#[derive(Default)]
struct LocalReleasers {
    /// The list of entities to be deferred released.
    releasables: Vec<Box<dyn CurrentTaskAndLockedReleasable>>,
}

impl LocalReleasers {
    fn is_empty(&self) -> bool {
        self.releasables.is_empty()
    }
}

impl Releasable for LocalReleasers {
    type Context<'a: 'b, 'b> = CurrentTaskAndLocked<'a, 'b>;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        let (locked, current_task) = context;
        for mut releasable in self.releasables {
            releasable.release_with_context((locked, current_task));
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
        register!(FlushedFile(Arc::clone(file), id));
    }

    /// Run all current delayed releases for the current thread.
    pub fn apply<'a>(
        &self,
        locked: &'a mut Locked<'a, FileOpsCore>,
        current_task: &'a CurrentTask,
    ) {
        loop {
            let releasers = RELEASERS.with(|cell| {
                std::mem::take(cell.borrow_mut().as_mut().expect("not finalized").deref_mut())
            });
            if releasers.is_empty() {
                return;
            }
            releasers.release((locked, current_task));
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
    type Context<'a: 'b, 'b> = CurrentTaskAndLocked<'a, 'b>;
    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        let (locked, current_task) = context;
        self.0.flush(locked, current_task, self.1);
    }
}
impl_ctr_for_option!(FlushedFile);
