// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::object_record::{ObjectKey, ObjectKeyData, ObjectValue};
use crate::lsm_tree::cache::{ObjectCache, ObjectCachePlaceholder, ObjectCacheResult};
use linked_hash_map::{Entry, LinkedHashMap};
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

fn filter(key: &ObjectKey) -> bool {
    match key.data {
        // Attribute and keys could also be added here to some immediate benefit, but would be
        // somewhat redundant with a node cache planned to be added after.
        ObjectKeyData::Object => true,
        ObjectKeyData::ExtendedAttribute { .. } => true,
        _ => false,
    }
}

// Limiting to ~100KiB of space usage. 56 bytes of linear overhead per item plus the overhead of
// the structure. This is just used directly for now, we can parameterize it in the type if this is
// ever desired to vary.
const ITEM_LIMIT: usize = 1535;

struct Placeholder<'a> {
    cache: &'a TreeCache,
    key: ObjectKey,
    placeholder_id: u64,
}

impl Placeholder<'_> {
    fn replace_entry(&mut self, value: Option<CacheValue>) {
        let key = std::mem::replace(&mut self.key, ObjectKey::object(0));
        let mut inner = self.cache.inner.lock().unwrap();
        // The value is present...
        if let Entry::Occupied(mut entry) = inner.entry(key) {
            // And the same placeholder as the token has...
            let is_current = match entry.get() {
                CacheValue::Placeholder(placeholder_id) => &self.placeholder_id == placeholder_id,
                _ => false,
            };
            if is_current {
                match value {
                    Some(v) => *(entry.get_mut()) = v,
                    None => {
                        entry.remove();
                    }
                }
            }
        }
    }
}

impl Drop for Placeholder<'_> {
    fn drop(&mut self) {
        self.replace_entry(None);
    }
}

impl<'a> ObjectCachePlaceholder<ObjectValue> for Placeholder<'a> {
    fn complete(mut self: Box<Self>, value: Option<&ObjectValue>) {
        let entry_value = match value {
            value @ Some(ObjectValue::Object { .. }) => value.cloned(),
            value @ Some(ObjectValue::ExtendedAttribute(_)) => value.cloned(),
            _ => None,
        }
        .map(|v| CacheValue::Value(v));
        self.replace_entry(entry_value);
    }
}

enum CacheValue {
    Placeholder(u64),
    Value(ObjectValue),
}

/// Supports caching for Objects directly for now. Speeds up stat calls.
pub struct TreeCache {
    inner: Mutex<LinkedHashMap<ObjectKey, CacheValue, BuildHasherDefault<rustc_hash::FxHasher>>>,
    placeholder_counter: AtomicU64,
}

impl TreeCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LinkedHashMap::with_capacity_and_hasher(
                ITEM_LIMIT + 1,
                BuildHasherDefault::<rustc_hash::FxHasher>::default(),
            )),
            placeholder_counter: AtomicU64::new(0),
        }
    }
}

impl ObjectCache<ObjectKey, ObjectValue> for TreeCache {
    fn lookup_or_reserve(&self, key: &ObjectKey) -> ObjectCacheResult<'_, ObjectValue> {
        if !filter(key) {
            return ObjectCacheResult::NoCache;
        }
        let mut inner = self.inner.lock().unwrap();
        match inner.get_refresh(key) {
            Some(CacheValue::Value(entry)) => ObjectCacheResult::Value(entry.clone()),
            Some(CacheValue::Placeholder(_)) => ObjectCacheResult::NoCache,
            _ => {
                let placeholder_id = self.placeholder_counter.fetch_add(1, Ordering::Relaxed);
                inner.insert(key.clone(), CacheValue::Placeholder(placeholder_id));
                if inner.len() > ITEM_LIMIT {
                    let _ = inner.pop_front();
                }
                ObjectCacheResult::Placeholder(Box::new(Placeholder {
                    cache: self,
                    key: key.clone(),
                    placeholder_id,
                }))
            }
        }
    }

    fn invalidate(&self, key: ObjectKey, value: Option<ObjectValue>) {
        if !filter(&key) {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if let Entry::Occupied(mut entry) = inner.entry(key) {
            if let Some(replacement) = value {
                *(entry.get_mut()) = CacheValue::Value(replacement);
            } else {
                entry.remove();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::object_record::{ObjectKey, ObjectValue, Timestamp};
    use super::{TreeCache, ITEM_LIMIT};
    use crate::lsm_tree::cache::{ObjectCache, ObjectCacheResult};
    use assert_matches::assert_matches;

    #[fuchsia::test]
    async fn test_basic_operations() {
        let cache = TreeCache::new();
        let key = ObjectKey::object(1);
        let now = Timestamp::now();
        let value = ObjectValue::file(1, 0, now, now, now, now, 0, None);

        let placeholder = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };
        placeholder.complete(Some(&value));

        let result = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value);

        cache.invalidate(key.clone(), None);

        match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Placeholder(placeholder) => placeholder.complete(None),
            _ => panic!("Expected cache miss with placeholder returned."),
        };
    }

    #[fuchsia::test]
    async fn test_enforce_limits() {
        let cache = TreeCache::new();
        let now = Timestamp::now();

        for i in 1..(ITEM_LIMIT as u64 + 2) {
            let key = ObjectKey::object(i);
            let value = ObjectValue::file(1, 0, now, now, now, now, 0, None);
            let placeholder = match cache.lookup_or_reserve(&key) {
                ObjectCacheResult::Placeholder(placeholder) => placeholder,
                _ => panic!("Expected cache miss with placeholder returned."),
            };
            placeholder.complete(Some(&value));
        }

        // Item 1 should be evicted.
        assert_matches!(
            cache.lookup_or_reserve(&ObjectKey::object(1)),
            ObjectCacheResult::Placeholder(_)
        );

        // And item 2 has been evicted by the lookup of item 1.
        for i in 3..(ITEM_LIMIT as u64 + 2) {
            let key = ObjectKey::object(i);
            assert_matches!(cache.lookup_or_reserve(&key), ObjectCacheResult::Value(_));
        }
    }

    #[fuchsia::test]
    async fn test_invalidate_inserts() {
        let cache = TreeCache::new();
        let key = ObjectKey::object(1);
        let now = Timestamp::now();
        let value1 = ObjectValue::file(1, 0, now, now, now, now, 0, None);
        let value2 = ObjectValue::file(2, 0, now, now, now, now, 0, None);

        let placeholder = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };
        placeholder.complete(Some(&value1));

        let result = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value1);

        cache.invalidate(key.clone(), Some(value2.clone()));

        let result = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value2);
    }

    #[fuchsia::test]
    async fn test_no_caching_for_filtered_item() {
        let cache = TreeCache::new();
        let key = ObjectKey::extent(1, 1, 1..2);

        assert!(matches!(cache.lookup_or_reserve(&key), ObjectCacheResult::NoCache));
    }

    // Two clients looking for the same key don't interfere with each other. Prevents priority
    // inversion.
    #[fuchsia::test]
    async fn test_two_parallel_clients() {
        let cache = TreeCache::new();
        let key = ObjectKey::object(1);
        let now = Timestamp::now();
        let value1 = ObjectValue::file(1, 0, now, now, now, now, 0, None);
        let value2 = ObjectValue::file(2, 0, now, now, now, now, 0, None);

        let placeholder1 = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };

        // Another search should not get a placeholder, as one is already held.
        assert!(matches!(cache.lookup_or_reserve(&key), ObjectCacheResult::NoCache));

        // Invalidate the current placeholder.
        cache.invalidate(key.clone(), None);

        // Get a new placeholder
        let placeholder2 = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };

        // Complete them out of order.
        placeholder2.complete(Some(&value2));
        placeholder1.complete(Some(&value1));

        // Result should be from the second placeholder, as the first was invalidated.
        let result = match cache.lookup_or_reserve(&key) {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value2);
    }
}
