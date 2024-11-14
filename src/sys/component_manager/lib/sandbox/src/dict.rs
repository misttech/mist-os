// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Capability, CapabilityBound};
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

    /// When an external request tries to access a non-existent entry, this closure will be invoked
    /// with the name of the entry.
    #[derivative(Debug = "ignore")]
    // Currently this is only used on target, but it's compatible with host.
    #[allow(dead_code)]
    pub(crate) not_found: Arc<dyn Fn(&str) -> () + 'static + Send + Sync>,

    /// Tasks that serve the Dictionary protocol. This is an `Option` because dictionaries are not
    /// necessarily used in async contexts but a TaskGroup will fail construction if there is no
    /// async executor.
    #[cfg(target_os = "fuchsia")]
    #[derivative(Debug = "ignore")]
    task_group: Option<fasync::TaskGroup>,

    /// Functions that will be invoked whenever the contents of this dictionary changes.
    #[derivative(Debug = "ignore")]
    update_notifiers: Vec<UpdateNotifierFn>,
}

impl CapabilityBound for Dict {
    fn debug_typename() -> &'static str {
        "Dictionary"
    }
}

impl Drop for DictInner {
    fn drop(&mut self) {
        // When this dictionary doesn't exist anymore, then neither do its entries (or at least
        // these references of them). Notify anyone listening that all of the entries have been
        // removed.
        let keys = self.entries.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.call_update_notifiers(EntryUpdate::Remove(&key))
        }
    }
}

/// Represents a change to a dictionary, where an entry is either added or removed.
#[derive(Debug, Copy, Clone)]
pub enum EntryUpdate<'a> {
    Add(&'a Key, &'a Capability),
    Remove(&'a Key),
    Idle,
}

/// Represents whether an update notifier should be retained and thus continue to be called for
/// future updates, or if it should be dropped and no longer be called.
pub enum UpdateNotifierRetention {
    Retain,
    Drop_,
}

/// A function that will be called when the contents of a dictionary changes. Note that this
/// function will be called while the internal dictionary structure is locked, so it must not
/// interact with the dictionary on which it is registered. It shouldn't even hold a strong
/// reference to the dictionary it's registered on, as that'll create a cycle and make LSAN sad.
pub type UpdateNotifierFn =
    Box<dyn for<'a> FnMut(EntryUpdate<'a>) -> UpdateNotifierRetention + Sync + Send>;

impl Default for Dict {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DictInner {
                entries: BTreeMap::new(),
                not_found: Arc::new(|_key: &str| {}),
                #[cfg(target_os = "fuchsia")]
                task_group: None,
                update_notifiers: vec![],
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
                task_group: None,
                update_notifiers: vec![],
            })),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, DictInner> {
        self.inner.lock().unwrap()
    }

    /// Registers a new update notifier function with this dictionary. The function will be called
    /// whenever an entry is added to or removed from this dictionary. The function will be
    /// immediately called with an `EntryUpdate::Add` value for any entries already in this
    /// dictionary.
    ///
    /// Note that this function will be called while the internal dictionary structure is locked,
    /// so it must not interact with the dictionary on which it is registered. It shouldn't even
    /// hold a strong reference to the dictionary it's registered on, as that'll create a cycle and
    /// make LSAN sad.
    pub fn register_update_notifier(&self, mut notifier_fn: UpdateNotifierFn) {
        let mut guard = self.lock();
        for (key, value) in guard.entries.iter() {
            if let UpdateNotifierRetention::Drop_ = (notifier_fn)(EntryUpdate::Add(key, value)) {
                // The notifier has signaled that it doesn't want to process any more updates.
                return;
            }
        }
        if let UpdateNotifierRetention::Retain = (notifier_fn)(EntryUpdate::Idle) {
            guard.update_notifiers.push(notifier_fn);
        }
    }

    /// Inserts an entry, mapping `key` to `capability`. If an entry already exists at `key`, a
    /// `fsandbox::DictionaryError::AlreadyExists` will be returned.
    pub fn insert(
        &self,
        key: Key,
        capability: Capability,
    ) -> Result<(), fsandbox::CapabilityStoreError> {
        let mut this = self.lock();
        if this.entries.contains_key(&key) {
            this.call_update_notifiers(EntryUpdate::Remove(&key));
        }
        this.call_update_notifiers(EntryUpdate::Add(&key, &capability));
        match this.entries.insert(key, capability) {
            Some(_) => Err(fsandbox::CapabilityStoreError::ItemAlreadyExists),
            None => Ok(()),
        }
    }

    /// Returns a clone of the capability associated with `key`. If there is no entry for `key`,
    /// `None` is returned.
    ///
    /// If the value could not be cloned, returns an error.
    pub fn get(&self, key: &Key) -> Result<Option<Capability>, ()> {
        self.lock().entries.get(key).map(|c| c.try_clone()).transpose().map_err(|_| ())
    }

    /// Removes `key` from the entries, returning the capability at `key` if the key was already in
    /// the entries.
    pub fn remove(&self, key: &Key) -> Option<Capability> {
        let mut this = self.lock();
        let result = this.entries.remove(key);
        if result.is_some() {
            this.call_update_notifiers(EntryUpdate::Remove(&key))
        }
        result
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
        #[allow(clippy::needless_collect)] // This is needed for a 'static iterator
        let keys: Vec<_> = self.lock().entries.keys().cloned().collect();
        keys.into_iter()
    }

    /// Removes all entries from the Dict and returns them as an iterator.
    pub fn drain(&self) -> impl Iterator<Item = (Key, Capability)> {
        let entries = {
            let mut this = self.lock();
            let keys = this.entries.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                this.call_update_notifiers(EntryUpdate::Remove(&key))
            }
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

impl DictInner {
    #[cfg(target_os = "fuchsia")]
    pub(crate) fn tasks(&mut self) -> &mut fasync::TaskGroup {
        self.task_group.get_or_insert_with(|| fasync::TaskGroup::new())
    }

    /// Calls the update notifiers registered on this dictionary with the given update. Any
    /// notifiers that signal that they should be dropped will be removed from
    /// `self.update_notifiers.
    fn call_update_notifiers<'a>(&'a mut self, update: EntryUpdate<'a>) {
        let mut retained_notifier_fns = vec![];
        for mut notifier_fn in self.update_notifiers.drain(..) {
            if let UpdateNotifierRetention::Retain = (notifier_fn)(update) {
                retained_notifier_fns.push(notifier_fn)
            }
        }
        self.update_notifiers = retained_notifier_fns
    }
}
