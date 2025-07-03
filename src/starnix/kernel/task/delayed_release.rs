// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::ownership::Releasable;
use std::cell::RefCell;
use std::ops::DerefMut;

/// Register the container to be deferred released.
pub fn register_delayed_release<
    T: for<'a> Releasable<Context<'a> = CurrentTaskAndLocked<'a>> + 'static,
>(
    to_release: T,
) {
    RELEASERS.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .expect("DelayedReleaser hasn't been finalized yet")
            .releasables
            .push(Box::new(Some(to_release)));
    });
}

impl<T> CurrentTaskAndLockedReleasable for Option<T>
where
    for<'a> T: Releasable<Context<'a> = CurrentTaskAndLocked<'a>>,
{
    fn release_with_context(&mut self, context: CurrentTaskAndLocked<'_>) {
        if let Some(this) = self.take() {
            <T as Releasable>::release(this, context);
        }
    }
}

pub type CurrentTaskAndLocked<'a> = (&'a mut Locked<FileOpsCore>, &'a CurrentTask);

/// An object-safe/dyn-compatible trait to wrap `Releasable` types.
pub trait CurrentTaskAndLockedReleasable {
    fn release_with_context(&mut self, context: CurrentTaskAndLocked<'_>);
}

thread_local! {
    /// Container of all `FileObject` that are not used anymore, but have not been closed yet.
    pub static RELEASERS: RefCell<Option<LocalReleasers>> =
        RefCell::new(Some(LocalReleasers::default()));
}

#[derive(Default)]
pub struct LocalReleasers {
    /// The list of entities to be deferred released.
    pub releasables: Vec<Box<dyn CurrentTaskAndLockedReleasable>>,
}

impl LocalReleasers {
    fn is_empty(&self) -> bool {
        self.releasables.is_empty()
    }
}

impl Releasable for LocalReleasers {
    type Context<'a> = CurrentTaskAndLocked<'a>;

    fn release<'a>(self, context: CurrentTaskAndLocked<'a>) {
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
    /// Run all current delayed releases for the current thread.
    pub fn apply<'a>(&self, locked: &'a mut Locked<FileOpsCore>, current_task: &'a CurrentTask) {
        loop {
            let releasers = RELEASERS.with(|cell| {
                std::mem::take(
                    cell.borrow_mut()
                        .as_mut()
                        .expect("DelayedReleaser hasn't been finalized yet")
                        .deref_mut(),
                )
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
            assert!(cell
                .borrow()
                .as_ref()
                .expect("DelayedReleaser hasn't been finalized yet")
                .is_empty());
            *cell.borrow_mut() = None;
        });
    }
}
