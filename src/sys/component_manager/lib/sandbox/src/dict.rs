// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Capability;
use derivative::Derivative;
use fidl_fuchsia_component_sandbox as fsandbox;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(target_os = "fuchsia")]
use fuchsia_async as fasync;

pub type Key = cm_types::Name;

/// A capability that represents a dictionary of capabilities.
#[derive(Debug, Clone)]
pub struct Dict {
    inner: Arc<Mutex<DictInner>>,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct DictInner {
    /// The contents of the [Dict].
    pub(crate) entries: BTreeMap<Key, Capability>,

    /// When an external request tries to access a non-existent entry,
    /// this closure will be invoked with the name of the entry.
    #[derivative(Debug = "ignore")]
    // Currently this is only used on target, but it's compatible with host.
    #[allow(dead_code)]
    pub(crate) not_found: Arc<dyn Fn(&str) -> () + 'static + Send + Sync>,

    /// Tasks that serve the Dictionary protocol.
    #[cfg(target_os = "fuchsia")]
    #[derivative(Debug = "ignore")]
    pub(crate) tasks: fasync::TaskGroup,
}

impl Default for Dict {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DictInner {
                entries: BTreeMap::new(),
                not_found: Arc::new(|_key: &str| {}),
                #[cfg(target_os = "fuchsia")]
                tasks: fasync::TaskGroup::new(),
            })),
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
            inner: Arc::new(Mutex::new(DictInner {
                entries: BTreeMap::new(),
                not_found: Arc::new(not_found),
                #[cfg(target_os = "fuchsia")]
                tasks: fasync::TaskGroup::new(),
            })),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, DictInner> {
        self.inner.lock().unwrap()
    }

    /// Inserts an entry, mapping `key` to `capability`. If an entry already
    /// exists at `key`, a `fsandbox::DictionaryError::AlreadyExists` will be
    /// returned.
    pub fn insert(
        &self,
        key: Key,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError> {
        let mut this = self.lock();
        match this.entries.insert(key, capability) {
            Some(_) => Err(fsandbox::DictionaryError::AlreadyExists),
            None => Ok(()),
        }
    }

    /// Returns a clone of the capability associated with `key`. If there is no
    /// entry for `key`, `None` is returned.
    ///
    /// If the value could not be cloned, returns an error.
    pub fn get(&self, key: &Key) -> Result<Option<Capability>, ()> {
        self.lock().entries.get(key).map(|c| c.try_clone()).transpose().map_err(|_| ())
    }

    /// Removes `key` from the entries, returning the capability at `key` if the
    /// key was already in the entries.
    pub fn remove(&self, key: &Key) -> Option<Capability> {
        self.lock().entries.remove(key)
    }

    /// Returns an iterator over a clone of the entries, sorted by key.
    ///
    /// If a capability is not cloneable, an error returned for the value.
    pub fn enumerate(&self) -> impl Iterator<Item = (Key, Result<Capability, ()>)> {
        let entries = {
            let this = self.lock();
            let entries: Vec<_> = this
                .entries
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
        let keys: Vec<_> = self.lock().entries.keys().cloned().collect();
        keys.into_iter()
    }

    /// Removes all entries from the Dict and returns them as an iterator.
    pub fn drain(&self) -> impl Iterator<Item = (Key, Capability)> {
        let entries = {
            let mut this = self.lock();
            std::mem::replace(&mut this.entries, BTreeMap::new())
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
            let this = self.lock();
            let res: Result<BTreeMap<_, _>, _> = this
                .entries
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
            let mut copy = copy.lock();
            let _ = std::mem::replace(&mut copy.entries, copied_entries);
        }
        Ok(copy)
    }
}
