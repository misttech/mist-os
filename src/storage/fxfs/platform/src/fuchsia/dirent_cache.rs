// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::node::FxNode;
use fxfs::object_handle::INVALID_OBJECT_ID;
use linked_hash_map::LinkedHashMap;
use rustc_hash::FxHasher;
use std::borrow::Borrow;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::{Arc, Mutex};

enum CacheHolder {
    Node(Arc<dyn FxNode>),
    Timer,
}

struct DirentCacheInner {
    lru: LinkedHashMap<DirentCacheKey, CacheHolder, BuildHasherDefault<FxHasher>>,
    limit: usize,
    timer_in_queue: bool,
}

impl DirentCacheInner {
    fn insert_internal(
        &mut self,
        dir_id: u64,
        name: String,
        item: CacheHolder,
    ) -> Option<Arc<dyn FxNode>> {
        self.lru.insert(DirentCacheKey(dir_id, name), item);
        if self.lru.len() > self.limit {
            if let CacheHolder::Node(node) = self.lru.pop_front().unwrap().1 {
                // Drop outside the lock.
                return Some(node);
            } else {
                self.timer_in_queue = false;
            }
        }
        None
    }
}

/// A cache for directory entry lookup to return the node directly. Uses an LRU to keep things alive
/// and may be periodically cleaned up with calls to `recycle_stale_files` dropping items that
/// haven't been refreshed since the previous call to it.
pub struct DirentCache {
    inner: Mutex<DirentCacheInner>,
}

/// Cache for directory entry object ids.
#[fxfs_trace::trace]
impl DirentCache {
    /// The provided `limit` is the initial max size of the cache.
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Mutex::new(DirentCacheInner {
                lru: linked_hash_map_with_capacity(limit + 1),
                limit,
                timer_in_queue: false,
            }),
        }
    }

    /// Fetch the limit for the cache.
    pub fn limit(&self) -> usize {
        self.inner.lock().unwrap().limit
    }

    /// Returns the number of elements in the cache.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().lru.len()
    }

    /// Lookup directory entry by name and directory object id.
    pub fn lookup(&self, key: &(u64, &str)) -> Option<Arc<dyn FxNode>> {
        assert_ne!(key.0, INVALID_OBJECT_ID, "Looked up dirent key reserved for timer.");
        if let CacheHolder::Node(node) =
            self.inner.lock().unwrap().lru.get_refresh(key as &dyn DirentCacheKeyRef)?
        {
            return Some(node.clone());
        }
        None
    }

    /// Insert an object id for a directory entry.
    pub fn insert(&self, dir_id: u64, name: String, node: Arc<dyn FxNode>) {
        assert_ne!(dir_id, INVALID_OBJECT_ID, "Looked up dirent key reserved for timer.");
        let _dropped =
            self.inner.lock().unwrap().insert_internal(dir_id, name, CacheHolder::Node(node));
    }

    /// Remove an entry from the cache.
    pub fn remove(&self, key: &(u64, &str)) {
        let _dropped_item = self.inner.lock().unwrap().lru.remove(key as &dyn DirentCacheKeyRef);
    }

    /// Remove all items from the cache.
    pub fn clear(&self) {
        let _dropped = {
            let mut this = self.inner.lock().unwrap();
            this.timer_in_queue = false;
            let limit = this.limit;
            std::mem::replace(&mut this.lru, linked_hash_map_with_capacity(limit + 1))
        };
    }

    /// Set a new limit for the cache size.
    pub fn set_limit(&self, limit: usize) {
        #[allow(clippy::collection_is_never_read)]
        let mut dropped_items;
        {
            let mut this = self.inner.lock().unwrap();
            this.limit = limit;
            if this.lru.len() <= limit {
                return;
            }
            dropped_items = Vec::with_capacity(this.lru.len() - limit);
            while this.lru.len() > limit {
                match this.lru.pop_front().unwrap().1 {
                    CacheHolder::Node(node) => dropped_items.push(node),
                    CacheHolder::Timer => this.timer_in_queue = false,
                }
            }
        }
    }

    /// Drop entries that haven't been refreshed since the last call to this method.
    #[trace]
    pub fn recycle_stale_files(&self) {
        // Drop outside the lock.
        #[allow(clippy::collection_is_never_read)]
        let mut dropped_items = Vec::new();
        {
            let mut this = self.inner.lock().unwrap();
            if this.timer_in_queue {
                while let CacheHolder::Node(node) = this.lru.pop_front().unwrap().1 {
                    dropped_items.push(node);
                }
                this.timer_in_queue = false;
            }

            if this.lru.len() > 0 {
                this.timer_in_queue = true;
                if let Some(node) =
                    this.insert_internal(INVALID_OBJECT_ID, "".to_string(), CacheHolder::Timer)
                {
                    dropped_items.push(node);
                }
            }
        }
    }
}

fn linked_hash_map_with_capacity(
    capacity: usize,
) -> LinkedHashMap<DirentCacheKey, CacheHolder, BuildHasherDefault<FxHasher>> {
    LinkedHashMap::with_capacity_and_hasher(capacity, BuildHasherDefault::<FxHasher>::default())
}

/// Hash function for both `DirentCacheKey` and `DirentCacheKeyRef` to ensure that both types hash
/// the same way.
fn hash_key<H: Hasher>(directory_object_id: u64, name: &str, state: &mut H) {
    directory_object_id.hash(state);
    name.hash(state);
}

#[derive(PartialEq, Eq)]
struct DirentCacheKey(u64, String);

impl Hash for DirentCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_key(self.0, &self.1, state);
    }
}

/// This trait allows for looking up an entry in the `DirentCache` using a `&str` for the name
/// instead of a `String`.
trait DirentCacheKeyRef {
    fn directory_object_id(&self) -> u64;
    fn name(&self) -> &str;
}

impl<'a> Borrow<dyn DirentCacheKeyRef + 'a> for DirentCacheKey {
    fn borrow(&self) -> &(dyn DirentCacheKeyRef + 'a) {
        self
    }
}

impl Hash for dyn DirentCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_key(self.directory_object_id(), self.name(), state);
    }
}

impl PartialEq for dyn DirentCacheKeyRef + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.directory_object_id() == other.directory_object_id() && self.name() == other.name()
    }
}

impl Eq for dyn DirentCacheKeyRef + '_ {}

impl DirentCacheKeyRef for DirentCacheKey {
    fn directory_object_id(&self) -> u64 {
        self.0
    }
    fn name(&self) -> &str {
        &self.1
    }
}

impl DirentCacheKeyRef for (u64, &str) {
    fn directory_object_id(&self) -> u64 {
        self.0
    }
    fn name(&self) -> &str {
        self.1
    }
}

#[cfg(test)]
mod tests {
    use crate::fuchsia::directory::FxDirectory;
    use crate::fuchsia::dirent_cache::DirentCache;
    use crate::fuchsia::node::FxNode;
    use fxfs::object_store::ObjectDescriptor;
    use fxfs_macros::ToWeakNode;
    use std::sync::Arc;

    #[derive(ToWeakNode)]
    struct FakeNode(u64);
    impl FxNode for FakeNode {
        fn object_id(&self) -> u64 {
            self.0
        }
        fn parent(&self) -> Option<Arc<FxDirectory>> {
            unreachable!();
        }
        fn set_parent(&self, _parent: Arc<FxDirectory>) {
            unreachable!();
        }
        fn open_count_add_one(&self) {}
        fn open_count_sub_one(self: Arc<Self>) {}

        fn object_descriptor(&self) -> ObjectDescriptor {
            ObjectDescriptor::File
        }
    }

    #[fuchsia::test]
    async fn test_simple_lru() {
        let cache = DirentCache::new(5);
        for i in 1..6 {
            cache.insert(1, i.to_string(), Arc::new(FakeNode(i)));
        }

        // Refresh entry 2. Puts it at the top of the used list.
        assert!(cache.lookup(&(1, "2")).is_some());

        // Add 2 more items. This will expire 1 and 3 since 2 was refreshed.
        for i in 6..8 {
            cache.insert(1, i.to_string(), Arc::new(FakeNode(i)));
        }

        // 2 is still there, but 1 and 3 aren't.
        assert!(cache.lookup(&(1, "1")).is_none());
        assert!(cache.lookup(&(1, "2")).is_some());
        assert!(cache.lookup(&(1, "3")).is_none());

        // Remove 2 and now it's gone.
        cache.remove(&(1, "2"));
        assert!(cache.lookup(&(1, "2")).is_none());

        // All remaining items are still there.
        for i in 4..8 {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }

        // Add one more, as there's space from the removal and everything is still there.
        cache.insert(1, "8".to_string(), Arc::new(FakeNode(8)));
        for i in 4..9 {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }
    }

    #[fuchsia::test]
    async fn test_change_limit() {
        let cache = DirentCache::new(10);

        for i in 1..16 {
            cache.insert(1, i.to_string(), Arc::new(FakeNode(i)));
        }

        // Only the last ten should be there.
        for i in 1..6 {
            assert!(cache.lookup(&(1, &i.to_string())).is_none(), "Shouldn't have item {}", i);
        }
        for i in 6..16 {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }

        // Lower the limit and see that only the last five are left.
        cache.set_limit(5);
        for i in 1..11 {
            assert!(cache.lookup(&(1, &i.to_string())).is_none(), "Shouldn't have item {}", i);
        }
        for i in 11..16 {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }
    }

    #[fuchsia::test]
    async fn test_cache_clear() {
        let cache = DirentCache::new(10);

        for i in 1..6 {
            cache.insert(1, i.to_string(), Arc::new(FakeNode(i)));
        }

        // All entries should be present.
        for i in 1..6 {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }

        // Clear, then none should be present.
        cache.clear();
        for i in 1..6 {
            assert!(cache.lookup(&(1, &i.to_string())).is_none(), "Shouldn't have item {}", i);
        }
    }

    #[fuchsia::test]
    async fn test_timeout() {
        let cache = DirentCache::new(20);

        cache.recycle_stale_files();

        // Put in 10 items.
        for i in 1..11 {
            cache.insert(1, i.to_string(), Arc::new(FakeNode(i)));
        }

        cache.recycle_stale_files();

        // Refresh only the odd numbered entries.
        for i in (1..11).step_by(2) {
            assert!(cache.lookup(&(1, &i.to_string())).is_some(), "Missing item {}", i);
        }

        cache.recycle_stale_files();

        // Only the refreshed dd numbered nodes should be left.
        for i in 1..11 {
            match cache.lookup(&(1, &i.to_string())) {
                Some(_) => assert_eq!(i % 2, 1, "Even number {} found.", i),
                None => assert_eq!(i % 2, 0, "Odd number {} missing.", i),
            }
        }
    }
}
