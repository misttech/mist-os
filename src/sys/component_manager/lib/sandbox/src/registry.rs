// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use crate::Capability;

lazy_static! {
    static ref REGISTRY: Mutex<Registry> = Mutex::new(Registry::default());
}

/// Registers a capability with a task.
pub(crate) fn spawn_task(
    capability: Capability,
    koid: zx::Koid,
    fut: impl Future<Output = ()> + Send + 'static,
) {
    let mut registry = REGISTRY.lock().unwrap();
    let guard = scopeguard::guard((), move |_| {
        REGISTRY.lock().unwrap().remove(koid);
    });
    let task = fasync::Task::spawn(async move {
        let _guard = guard;
        fut.await;
    });
    let existing = registry.insert(koid, Entry { capability, task: Some(task) });
    assert!(existing.is_none());
}

/// Get a capability from the global registry and returns it, if it exists.
pub(crate) fn get(koid: zx::Koid) -> Option<Capability> {
    let registry = REGISTRY.lock().unwrap();
    registry.get(koid).map(|entry| entry.capability.clone())
}

pub struct Entry {
    pub capability: Capability,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub task: Option<fasync::Task<()>>,
}

/// The [Registry] stores capabilities that have been converted to FIDL, providing a way to get
/// the original Rust object back from a FIDL representation of a capability.
///
/// There should only be a single Registry, outside of unit tests.
#[derive(Default)]
pub struct Registry {
    entries: HashMap<zx::Koid, Entry>,
}

impl Registry {
    /// Inserts an entry into the registry.
    ///
    /// If an entry with the same koid already exists, replaces the entry with the new one
    /// and returns the old one.
    ///
    /// Returns None if the entry with the given koid did not previously exist.
    pub(crate) fn insert(&mut self, koid: zx::Koid, entry: Entry) -> Option<Entry> {
        self.entries.insert(koid, entry)
    }

    /// Removes an entry from the registry, if one with a matching koid exists.
    pub(crate) fn remove(&mut self, koid: zx::Koid) -> Option<Entry> {
        self.entries.remove(&koid)
    }

    /// Gets an entry from the registry, if one with a matching koid exists.
    pub(crate) fn get(&self, koid: zx::Koid) -> Option<&Entry> {
        self.entries.get(&koid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Unit;
    use assert_matches::assert_matches;
    use futures::channel::oneshot;

    /// Tests that a capability can be inserted and retrieved from a Registry.
    #[test]
    fn insert_get_remove() {
        let mut registry = Registry::default();

        // Insert a Unit capability into the registry.
        let koid = zx::Koid::from_raw(123);
        let unit = Unit::default();
        assert!(registry.insert(koid, Entry { capability: unit.into(), task: None }).is_none());

        // Get a capability with the same koid. It should be a Unit.
        let entry = registry.get(koid).unwrap();
        let got_unit = entry.capability.clone();
        assert_matches!(got_unit, Capability::Unit(_));

        // Remove a capability with the same koid. It should be a Unit.
        let entry = registry.remove(koid).unwrap();
        let got_unit = entry.capability;
        assert_matches!(got_unit, Capability::Unit(_));

        assert!(registry.remove(koid).is_none());
    }

    /// Tests that a capability added with a [remove_when_done] task is removed
    /// when the wrapped task completes.
    #[fuchsia::test(allow_stalls = false)]
    async fn insert_with_task_remove_when_done() {
        let (sender, receiver) = oneshot::channel::<()>();
        // This task completes when the sender is dropped.
        let task = fasync::Task::spawn(async move {
            let _ = receiver.await;
        });

        let koid = zx::Koid::from_raw(123);
        let unit = Unit::default();

        spawn_task(unit.into(), koid, task);

        // Drop the sender so `task` completes and `remove_when_done_task` removes the entry.
        drop(sender);

        // Allow the spawned future to complete.
        let _ = fasync::TestExecutor::poll_until_stalled(std::future::pending::<()>()).await;

        // Remove a capability with the same koid. It should not exist.
        assert!(get(koid).is_none());
    }
}
