// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{FdTableId, FileHandle, FileObject},
    task::CurrentTask,
};
use starnix_uapi::ownership::{Releasable, ReleaseGuard};
use std::{
    cell::RefCell,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    sync::Arc,
};

thread_local! {
    /// Container of all `FileObject` that are not used anymore, but have not been closed yet.
    static RELEASERS: RefCell<LocalReleasers> = RefCell::new(LocalReleasers::default());
}

#[derive(Debug, Default)]
struct LocalReleasers {
    closed_files: Vec<ReleaseGuard<FileObject>>,
    flushed_files: Vec<(FileHandle, FdTableId)>,
}

impl LocalReleasers {
    fn is_empty(&self) -> bool {
        self.closed_files.is_empty() && self.flushed_files.is_empty()
    }
}

impl Releasable for LocalReleasers {
    type Context<'a> = &'a CurrentTask;

    fn release(self, context: Self::Context<'_>) {
        for file in self.closed_files {
            file.release(context);
        }
        for (file, id) in self.flushed_files {
            file.flush(context, id);
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
        RELEASERS.with(|cell| {
            cell.borrow_mut().flushed_files.push((Arc::clone(file), id));
        });
    }

    /// Run all current delayed releases for the current thread.
    pub fn apply(&self, current_task: &CurrentTask) {
        loop {
            let releasers = RELEASERS.with(|cell| std::mem::take(cell.borrow_mut().deref_mut()));
            if releasers.is_empty() {
                return;
            }
            releasers.release(current_task);
        }
    }
}

pub trait ReleaserAction<T> {
    fn release(t: ReleaseGuard<T>);
}

/// Wrapper around `FileObject` that ensures that a unused `FileObject` is added to the current
/// delayed releasers to be released at the next release point.
pub struct ObjectReleaser<T, F: ReleaserAction<T>>(MaybeUninit<ReleaseGuard<T>>, PhantomData<F>);

impl<T, F: ReleaserAction<T>> From<T> for ObjectReleaser<T, F> {
    fn from(object: T) -> Self {
        Self(MaybeUninit::new(object.into()), Default::default())
    }
}

impl<T, F: ReleaserAction<T>> Drop for ObjectReleaser<T, F> {
    fn drop(&mut self) {
        let content = std::mem::replace(&mut self.0, MaybeUninit::uninit());
        // SAFETY
        // The `MaybeUninit` is initialize with a value and only ever extracted in this `drop` method, so
        // it is guaranteed that it is initialized at this point.
        let object = unsafe { content.assume_init() };
        F::release(object);
    }
}

impl<T: std::fmt::Debug, F: ReleaserAction<T>> std::fmt::Debug for ObjectReleaser<T, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(f)
    }
}

impl<T, F: ReleaserAction<T>> std::ops::Deref for ObjectReleaser<T, F> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY
        // The value is initialized by the factory method and only every uninitialized in
        // Drop. The content is initialized for all the usual object lifecycle.
        let guard = unsafe { self.0.assume_init_ref() };
        guard.deref()
    }
}

impl<T, F: ReleaserAction<T>> std::borrow::Borrow<T> for ObjectReleaser<T, F> {
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T, F: ReleaserAction<T>> std::convert::AsRef<T> for ObjectReleaser<T, F> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

pub enum FileObjectReleaserAction {}
impl ReleaserAction<FileObject> for FileObjectReleaserAction {
    fn release(file_object: ReleaseGuard<FileObject>) {
        RELEASERS.with(|cell| {
            cell.borrow_mut().closed_files.push(file_object);
        });
    }
}

pub type FileReleaser = ObjectReleaser<FileObject, FileObjectReleaserAction>;
