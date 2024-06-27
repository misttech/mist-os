// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Capability;
use derivative::Derivative;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

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
    ///
    /// If the value could not be cloned, returns an error.
    pub fn get(&self, key: &Key) -> Result<Option<Capability>, ()> {
        self.lock_entries().get(key).map(|c| c.try_clone()).transpose().map_err(|_| ())
    }

    /// Removes `key` from the entries, returning the capability at `key` if the
    /// key was already in the entries.
    pub fn remove(&self, key: &Key) -> Option<Capability> {
        self.lock_entries().remove(key)
    }

    /// Returns an iterator over a clone of the entries, sorted by key.
    ///
    /// If a capability is not cloneable, an error returned for the value.
    pub fn enumerate(&self) -> impl Iterator<Item = (Key, Result<Capability, ()>)> {
        let entries = {
            let entries = self.lock_entries();
            let entries: Vec<_> = entries
                .iter()
                .map(|(k, v)| {
                    let k = k.clone();
                    let v = v.try_clone();
                    (k, v)
                })
                .collect();
            entries
        };
        entries.into_iter()
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
    ///
    /// If any value in the dictionary could not be cloned, returns an error.
    pub fn shallow_copy(&self) -> Result<Self, ()> {
        let copy = Self::new();
        let copied_entries = {
            let entries = self.lock_entries();
            let res: Result<BTreeMap<_, _>, _> = entries
                .iter()
                .map(|(k, v)| {
                    let k = k.clone();
                    let v = v.try_clone().map_err(|_| ())?;
                    Ok((k, v))
                })
                .collect();
            res?
        };
        {
            let mut entries = copy.lock_entries();
            let _ = std::mem::replace(&mut *entries, copied_entries);
        }
        Ok(copy)
    }
}
