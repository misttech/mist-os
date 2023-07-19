// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::object_record::{ChildValue, ObjectKey, ObjectKeyData, ObjectValue},
    crate::lsm_tree::cache::{ObjectCache, ObjectCachePlaceholder, ObjectCacheResult},
    async_trait::async_trait,
    linked_hash_map::{Entry, LinkedHashMap},
    std::{
        boxed::Box,
        ops::Drop,
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
    },
};

fn filter(key: &ObjectKey) -> bool {
    match key.data {
        // Attribute and keys could also be added here to some immediate benefit, but would be
        // somewhat redundant with a node cache planned to be added after.
        ObjectKeyData::Child { .. } => true,
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
        let entry_value = match &value {
            Some(ObjectValue::Child(child)) => Some(CacheValue::Value(child.clone())),
            _ => None,
        };
        self.replace_entry(entry_value);
    }
}

enum CacheValue {
    Placeholder(u64),
    Value(ChildValue),
}

/// Supports caching for directory entries only right now.
pub struct TreeCache {
    inner: Mutex<LinkedHashMap<ObjectKey, CacheValue>>,
    placeholder_counter: AtomicU64,
}

impl TreeCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LinkedHashMap::with_capacity(ITEM_LIMIT + 1)),
            placeholder_counter: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl ObjectCache<ObjectKey, ObjectValue> for TreeCache {
    async fn lookup_or_reserve(&self, key: &ObjectKey) -> ObjectCacheResult<'_, ObjectValue> {
        if !filter(key) {
            return ObjectCacheResult::NoCache;
        }
        let mut inner = self.inner.lock().unwrap();
        match inner.get_refresh(key) {
            Some(CacheValue::Value(entry)) => {
                ObjectCacheResult::Value(ObjectValue::Child(entry.clone()))
            }
            Some(CacheValue::Placeholder(_)) => ObjectCacheResult::NoCache,
            _ => {
                let placeholder_id = self.placeholder_counter.fetch_add(1, Ordering::Relaxed);
                inner.insert(key.clone(), CacheValue::Placeholder(placeholder_id));
                ObjectCacheResult::Placeholder(Box::new(Placeholder {
                    cache: self,
                    key: key.clone(),
                    placeholder_id,
                }))
            }
        }
    }

    fn invalidate(&self, key: &ObjectKey) {
        if !filter(key) {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            super::object_record::{ObjectDescriptor, ObjectKey, ObjectValue},
            TreeCache,
        },
        crate::lsm_tree::cache::{ObjectCache, ObjectCacheResult},
    };

    #[fuchsia::test]
    async fn test_basic_operations() {
        let cache = TreeCache::new();
        let key = ObjectKey::child(1, "apple");
        let value = ObjectValue::child(1, ObjectDescriptor::File);

        let placeholder = match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };
        placeholder.complete(Some(&value));

        let result = match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value);

        cache.invalidate(&key);

        match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Placeholder(placeholder) => placeholder.complete(None),
            _ => panic!("Expected cache miss with placeholder returned."),
        };
    }

    #[fuchsia::test]
    async fn test_no_caching_for_filtered_item() {
        let cache = TreeCache::new();
        let key = ObjectKey::extent(1, 1, 1..2);

        assert!(matches!(cache.lookup_or_reserve(&key).await, ObjectCacheResult::NoCache));
    }

    // Two clients looking for the same key don't interfere with each other. Prevents priority
    // inversion.
    #[fuchsia::test]
    async fn test_two_parallel_clients() {
        let cache = TreeCache::new();
        let key = ObjectKey::child(1, "apple");
        let value1 = ObjectValue::child(1, ObjectDescriptor::File);
        let value2 = ObjectValue::child(2, ObjectDescriptor::File);

        let placeholder1 = match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };

        // Another search should not get a placeholder, as one is already held.
        assert!(matches!(cache.lookup_or_reserve(&key).await, ObjectCacheResult::NoCache));

        // Invalidate the current placeholder.
        cache.invalidate(&key);

        // Get a new placeholder
        let placeholder2 = match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Placeholder(placeholder) => placeholder,
            _ => panic!("Expected cache miss with placeholder returned."),
        };

        // Complete them out of order.
        placeholder2.complete(Some(&value2));
        placeholder1.complete(Some(&value1));

        // Result should be from the second placeholder, as the first was invalidated.
        let result = match cache.lookup_or_reserve(&key).await {
            ObjectCacheResult::Value(value) => value,
            _ => panic!("Expected to find item."),
        };
        assert_eq!(&result, &value2);
    }
}
