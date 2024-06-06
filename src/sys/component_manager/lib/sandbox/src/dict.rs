// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Capability;
use derivative::Derivative;
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

pub type Key = cm_types::Name;

/// A capability that represents a dictionary of capabilities.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Dict {
    entries: Arc<Mutex<BTreeMap<Key, Capability>>>,

    /// When an external request tries to access a non-existent entry,
    /// this closure will be invoked with the name of the entry.
    #[derivative(Debug = "ignore")]
    pub(crate) not_found: Arc<dyn Fn(&str) -> () + 'static + Send + Sync>,

    /// Tasks that serve dictionary iterators.
    #[derivative(Debug = "ignore")]
    #[allow(dead_code)]
    pub(crate) iterator_tasks: fasync::TaskGroup,
}

impl Default for Dict {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            not_found: Arc::new(|_key: &str| {}),
            iterator_tasks: fasync::TaskGroup::new(),
        }
    }
}

impl Clone for Dict {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            not_found: self.not_found.clone(),
            iterator_tasks: fasync::TaskGroup::new(),
        }
    }
}

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty dictionary. When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    pub fn new_with_not_found(not_found: impl Fn(&str) -> () + 'static + Send + Sync) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            not_found: Arc::new(not_found),
            iterator_tasks: fasync::TaskGroup::new(),
        }
    }

    pub(crate) fn lock_entries(&self) -> MutexGuard<'_, BTreeMap<Key, Capability>> {
        self.entries.lock().unwrap()
    }

    /// Inserts an entry, mapping `key` to `capability`. If an entry already
    /// exists at `key`, a `fsandbox::DictionaryError::AlreadyExists` will be
    /// returned.
    pub fn insert(
        &self,
        key: Key,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError> {
        let mut entries = self.lock_entries();
        match entries.insert(key, capability) {
            Some(_) => Err(fsandbox::DictionaryError::AlreadyExists),
            None => Ok(()),
        }
    }

    /// Returns a clone of the capability associated with `key`. If there is no
    /// entry for `key`, `None` is returned.
    pub fn get(&self, key: &Key) -> Option<Capability> {
        self.lock_entries().get(key).cloned()
    }

    /// Removes `key` from the entries, returning the capability at `key` if the
    /// key was already in the entries.
    pub fn remove(&self, key: &Key) -> Option<Capability> {
        self.lock_entries().remove(key)
    }

    /// Returns an iterator over a clone of the entries, sorted by key.
    pub fn enumerate(&self) -> impl Iterator<Item = (Key, Capability)> {
        self.lock_entries().clone().into_iter()
    }

    /// Returns an iterator over the keys, in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = Key> {
        let keys: Vec<_> = self.lock_entries().keys().cloned().collect();
        keys.into_iter()
    }

    /// Removes all entries from the Dict and returns them as an iterator.
    pub fn drain(&self) -> impl Iterator<Item = (Key, Capability)> {
        let entries = {
            let mut entries = self.lock_entries();
            std::mem::replace(&mut *entries, BTreeMap::new())
        };
        entries.into_iter()
    }

    /// Creates a new Dict with entries cloned from this Dict.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Self {
        let copy = Dict::new();
        copy.lock_entries().clone_from(&self.lock_entries());
        copy
    }
}
