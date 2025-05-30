// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines structures for tracking and managing data associated with a zx::EventPair.

use fuchsia_async as fasync;
use futures::lock::{Mutex, MutexGuard};
use futures::FutureExt as _;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use zx::AsHandleRef;

/// Implements a mapping of [`fidl::EventPair`] objects to an associated data.
/// When the client end of the event pair is closed, the associated entry will
/// be removed from the map.
pub(crate) struct TokenMap<Data> {
    /// The current mappings of [`zx::Koid`] (kernel ids) to the data. Stored in
    /// a Mutex so that entries can be cleaned up asynchronously.
    entries: Arc<Mutex<HashMap<zx::Koid, TokenMapEntry<Data>>>>,
}

impl<Data> Default for TokenMap<Data> {
    fn default() -> Self {
        Self { entries: Arc::new(Mutex::new(Default::default())) }
    }
}

/// An entry in the [`TokenMap`]. Stores the data, in addition to an
/// asynchronous task that waits until the [`fidl::EventPair`] is dropped to
/// clean up this entry in the map.
#[derive(Default)]
pub(crate) struct TokenMapEntry<Data> {
    data: Data,

    #[expect(dead_code)]
    // Hold on to the task that watches for the EventPair to be dropped, so the
    // entry can be cleaned up. This field will never be accessed directly.
    task: Option<fasync::Task<()>>,
}

impl<Data> TokenMap<Data> {
    async fn insert(
        &self,
        data: Data,
        koid: zx::Koid,
        fut: impl Future<Output = ()> + Send + 'static,
    ) where
        Data: Send + 'static,
    {
        let mut entries = self.entries.lock().await;
        let task = fasync::Task::spawn({
            let entries = Arc::downgrade(&self.entries);
            async move {
                fut.await;
                if let Some(entries) = entries.upgrade() {
                    let _: Option<_> = entries.lock().await.remove(&koid);
                }
            }
        });
        let existing = entries.insert(koid, TokenMapEntry { data, task: Some(task) });
        assert!(existing.is_none());
    }

    /// Inserts a new [`TokenMap::Data`] into the map. The returned
    /// [`fidl::EventPair`] can be used to fetch the data again later.
    pub async fn insert_data(&self, data: Data) -> fidl::EventPair
    where
        Data: Send + 'static,
    {
        let (watcher, token) = zx::EventPair::create();
        self.insert(
            data,
            token.get_koid().expect("unable to fetch koid for event_pair"),
            fasync::OnSignals::new(watcher, zx::Signals::OBJECT_PEER_CLOSED).map(|_| ()),
        )
        .await;
        token
    }

    /// Fetches the associated [`TokenMap::Data`] for the provided
    /// [`AsHandleRef`]. If there is no such data (or if the data has since been
    /// cleaned up) this will return None. Otherwise, it will return a
    /// [`MapData`] object containing a reference to the underlying data, and a
    /// mutex lock of the map.
    pub async fn get<Handle: AsHandleRef>(&self, handle_ref: Handle) -> Option<MapData<'_, Data>> {
        let koid = handle_ref.get_koid().expect("unable to fetch koid for provided handle");
        let data = self.entries.lock().await;
        MapData::new(data, koid)
    }
}

/// A reference to a single entry within a [`TokenMap`] stored as a
/// [`MutexGuard`] over the whole map, and a [`zx::Koid`] representing the entry
/// in question.
pub(crate) struct MapData<'a, Data> {
    data: MutexGuard<'a, HashMap<zx::Koid, TokenMapEntry<Data>>>,
    koid: zx::Koid,
}

impl<'a, Data> MapData<'a, Data> {
    /// Constructs a new [`MapData`] object, ensuring first that there is an
    /// entry for the provided [`zx::Koid`].
    fn new(
        data: MutexGuard<'a, HashMap<zx::Koid, TokenMapEntry<Data>>>,
        koid: zx::Koid,
    ) -> Option<Self> {
        if data.contains_key(&koid) {
            Some(MapData { data, koid })
        } else {
            None
        }
    }
}

/// Provides immutable access to the [`Data`] stored in this [`MapData`].
impl<Data> std::ops::Deref for MapData<'_, Data> {
    type Target = Data;

    fn deref(&self) -> &Self::Target {
        &self.data.get(&self.koid).expect("entry must exist").data
    }
}

/// Provides mutable access to the [`Data`] stored in this [`MapData`].
impl<Data> std::ops::DerefMut for MapData<'_, Data> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data.get_mut(&self.koid).expect("entry must exist").data
    }
}
