// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod bloom_filter;
pub mod cache;
pub mod merge;
pub mod persistent_layer;
pub mod skip_list_layer;
pub mod types;

use crate::drop_event::DropEvent;
use crate::log::*;
use crate::object_handle::{ReadObjectHandle, WriteBytes};
use crate::serialized_types::{Version, LATEST_VERSION};
use anyhow::Error;
use cache::{ObjectCache, ObjectCacheResult};
use persistent_layer::{PersistentLayer, PersistentLayerWriter};
use skip_list_layer::SkipListLayer;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use types::{
    Item, ItemRef, Key, Layer, LayerIterator, LayerKey, LayerWriter, MergeableKey, OrdLowerBound,
    Value,
};

pub use merge::Query;

const SKIP_LIST_LAYER_ITEMS: usize = 512;

// For serialization.
pub use persistent_layer::{
    LayerHeader as PersistentLayerHeader, LayerHeaderV39 as PersistentLayerHeaderV39,
    LayerInfo as PersistentLayerInfo, LayerInfoV39 as PersistentLayerInfoV39,
    OldLayerInfo as OldPersistentLayerInfo, OldLayerInfoV32 as OldPersistentLayerInfoV32,
};

pub async fn layers_from_handles<K: Key, V: Value>(
    handles: impl IntoIterator<Item = impl ReadObjectHandle + 'static>,
) -> Result<Vec<Arc<dyn Layer<K, V>>>, Error> {
    let mut layers = Vec::new();
    for handle in handles {
        layers.push(PersistentLayer::open(handle).await? as Arc<dyn Layer<K, V>>);
    }
    Ok(layers)
}

#[derive(Eq, PartialEq, Debug)]
pub enum Operation {
    Insert,
    ReplaceOrInsert,
    MergeInto,
}

pub type MutationCallback<K, V> = Option<Box<dyn Fn(Operation, &Item<K, V>) + Send + Sync>>;

struct Inner<K, V> {
    mutable_layer: Arc<SkipListLayer<K, V>>,
    layers: Vec<Arc<dyn Layer<K, V>>>,
    mutation_callback: MutationCallback<K, V>,
}

#[derive(Default)]
pub(super) struct Counters {
    num_seeks: usize,
    // The following two metrics are used to compute the effectiveness of the bloom filters.
    // `layer_files_total` tracks the number of layer files we might have looked at across all
    // seeks, and `layer_files_skipped` tracks how many we skipped thanks to the bloom filter.
    layer_files_total: usize,
    layer_files_skipped: usize,
}

/// LSMTree manages a tree of layers to provide a key/value store.  Each layer contains deltas on
/// the preceding layer.  The top layer is an in-memory mutable layer.  Layers can be compacted to
/// form a new combined layer.
pub struct LSMTree<K, V> {
    data: RwLock<Inner<K, V>>,
    merge_fn: merge::MergeFn<K, V>,
    cache: Box<dyn ObjectCache<K, V>>,
    counters: Arc<Mutex<Counters>>,
}

#[fxfs_trace::trace]
impl<'tree, K: MergeableKey, V: Value> LSMTree<K, V> {
    /// Creates a new empty tree.
    pub fn new(merge_fn: merge::MergeFn<K, V>, cache: Box<dyn ObjectCache<K, V>>) -> Self {
        LSMTree {
            data: RwLock::new(Inner {
                mutable_layer: Self::new_mutable_layer(),
                layers: Vec::new(),
                mutation_callback: None,
            }),
            merge_fn,
            cache,
            counters: Arc::new(Mutex::new(Default::default())),
        }
    }

    /// Opens an existing tree from the provided handles to the layer objects.
    pub async fn open(
        merge_fn: merge::MergeFn<K, V>,
        handles: impl IntoIterator<Item = impl ReadObjectHandle + 'static>,
        cache: Box<dyn ObjectCache<K, V>>,
    ) -> Result<Self, Error> {
        Ok(LSMTree {
            data: RwLock::new(Inner {
                mutable_layer: Self::new_mutable_layer(),
                layers: layers_from_handles(handles).await?,
                mutation_callback: None,
            }),
            merge_fn,
            cache,
            counters: Arc::new(Mutex::new(Default::default())),
        })
    }

    /// Replaces the immutable layers.
    pub fn set_layers(&self, layers: Vec<Arc<dyn Layer<K, V>>>) {
        self.data.write().unwrap().layers = layers;
    }

    /// Appends to the given layers at the end i.e. they should be base layers.  This is supposed
    /// to be used after replay when we are opening a tree and we have discovered the base layers.
    pub async fn append_layers(
        &self,
        handles: impl IntoIterator<Item = impl ReadObjectHandle + 'static>,
    ) -> Result<(), Error> {
        let mut layers = layers_from_handles(handles).await?;
        self.data.write().unwrap().layers.append(&mut layers);
        Ok(())
    }

    /// Resets the immutable layers.
    pub fn reset_immutable_layers(&self) {
        self.data.write().unwrap().layers = Vec::new();
    }

    /// Seals the current mutable layer and creates a new one.
    pub fn seal(&self) {
        // We need to be sure there are no mutations currently in-progress.  This is currently
        // guaranteed by ensuring that all mutations take a read lock on `data`.
        let mut data = self.data.write().unwrap();
        let layer = std::mem::replace(&mut data.mutable_layer, Self::new_mutable_layer());
        data.layers.insert(0, layer);
    }

    /// Resets the tree to an empty state.
    pub fn reset(&self) {
        let mut data = self.data.write().unwrap();
        data.layers = Vec::new();
        data.mutable_layer = Self::new_mutable_layer();
    }

    /// Writes the items yielded by the iterator into the supplied object.
    #[trace]
    pub async fn compact_with_iterator<W: WriteBytes + Send>(
        &self,
        mut iterator: impl LayerIterator<K, V>,
        num_items: usize,
        writer: W,
        block_size: u64,
    ) -> Result<(), Error> {
        let mut writer =
            PersistentLayerWriter::<W, K, V>::new(writer, num_items, block_size).await?;
        while let Some(item_ref) = iterator.get() {
            debug!(item_ref:?; "compact: writing");
            writer.write(item_ref).await?;
            iterator.advance().await?;
        }
        writer.flush().await
    }

    /// Returns an empty layer-set for this tree.
    pub fn empty_layer_set(&self) -> LayerSet<K, V> {
        LayerSet { layers: Vec::new(), merge_fn: self.merge_fn, counters: self.counters.clone() }
    }

    /// Adds all the layers (including the mutable layer) to `layer_set`.
    pub fn add_all_layers_to_layer_set(&self, layer_set: &mut LayerSet<K, V>) {
        let data = self.data.read().unwrap();
        layer_set.layers.reserve_exact(data.layers.len() + 1);
        layer_set
            .layers
            .push(LockedLayer::from(data.mutable_layer.clone() as Arc<dyn Layer<K, V>>));
        for layer in &data.layers {
            layer_set.layers.push(layer.clone().into());
        }
    }

    /// Returns a clone of the current set of layers (including the mutable layer), after which one
    /// can get an iterator.
    pub fn layer_set(&self) -> LayerSet<K, V> {
        let mut layer_set = self.empty_layer_set();
        self.add_all_layers_to_layer_set(&mut layer_set);
        layer_set
    }

    /// Returns the current set of immutable layers after which one can get an iterator (for e.g.
    /// compacting).  Since these layers are immutable, getting an iterator should not block
    /// anything else.
    pub fn immutable_layer_set(&self) -> LayerSet<K, V> {
        let data = self.data.read().unwrap();
        let mut layers = Vec::with_capacity(data.layers.len());
        for layer in &data.layers {
            layers.push(layer.clone().into());
        }
        LayerSet { layers, merge_fn: self.merge_fn, counters: self.counters.clone() }
    }

    /// Inserts an item into the mutable layer.
    /// Returns error if item already exists.
    pub fn insert(&self, item: Item<K, V>) -> Result<(), Error> {
        let key = item.key.clone();
        let val = if item.value == V::DELETED_MARKER { None } else { Some(item.value.clone()) };
        {
            // `seal` below relies on us holding a read lock whilst we do the mutation.
            let data = self.data.read().unwrap();
            if let Some(mutation_callback) = data.mutation_callback.as_ref() {
                mutation_callback(Operation::Insert, &item);
            }
            data.mutable_layer.insert(item)?;
        }
        self.cache.invalidate(key, val);
        Ok(())
    }

    /// Replaces or inserts an item into the mutable layer.
    pub fn replace_or_insert(&self, item: Item<K, V>) {
        let key = item.key.clone();
        let val = if item.value == V::DELETED_MARKER { None } else { Some(item.value.clone()) };
        {
            // `seal` below relies on us holding a read lock whilst we do the mutation.
            let data = self.data.read().unwrap();
            if let Some(mutation_callback) = data.mutation_callback.as_ref() {
                mutation_callback(Operation::ReplaceOrInsert, &item);
            }
            data.mutable_layer.replace_or_insert(item);
        }
        self.cache.invalidate(key, val);
    }

    /// Merges the given item into the mutable layer.
    pub fn merge_into(&self, item: Item<K, V>, lower_bound: &K) {
        let key = item.key.clone();
        {
            // `seal` below relies on us holding a read lock whilst we do the mutation.
            let data = self.data.read().unwrap();
            if let Some(mutation_callback) = data.mutation_callback.as_ref() {
                mutation_callback(Operation::MergeInto, &item);
            }
            data.mutable_layer.merge_into(item, lower_bound, self.merge_fn);
        }
        self.cache.invalidate(key, None);
    }

    /// Searches for an exact match for the given key. If the value is equal to
    /// `Value::DELETED_MARKER` the item is considered missing and will not be returned.
    pub async fn find(&self, search_key: &K) -> Result<Option<Item<K, V>>, Error>
    where
        K: Eq,
    {
        // It is important that the cache lookup is done prior to fetching the layer set as the
        // placeholder returned acts as a sort of lock for the validity of the item that may be
        // inserted later via that placeholder.
        let token = match self.cache.lookup_or_reserve(search_key) {
            ObjectCacheResult::Value(value) => {
                if value == V::DELETED_MARKER {
                    return Ok(None);
                } else {
                    return Ok(Some(Item::new(search_key.clone(), value)));
                }
            }
            ObjectCacheResult::Placeholder(token) => Some(token),
            ObjectCacheResult::NoCache => None,
        };
        let layer_set = self.layer_set();
        let mut merger = layer_set.merger();

        Ok(match merger.query(Query::Point(search_key)).await?.get() {
            Some(ItemRef { key, value, sequence })
                if key == search_key && *value != V::DELETED_MARKER =>
            {
                if let Some(token) = token {
                    token.complete(Some(value));
                }
                Some(Item { key: key.clone(), value: value.clone(), sequence })
            }
            _ => None,
        })
    }

    pub fn mutable_layer(&self) -> Arc<SkipListLayer<K, V>> {
        self.data.read().unwrap().mutable_layer.clone()
    }

    /// Sets a mutation callback which is a callback that is triggered whenever any mutations are
    /// applied to the tree.  This might be useful for tests that want to record the precise
    /// sequence of mutations that are applied to the tree.
    pub fn set_mutation_callback(&self, mutation_callback: MutationCallback<K, V>) {
        self.data.write().unwrap().mutation_callback = mutation_callback;
    }

    /// Returns the earliest version used by a layer in the tree.
    pub fn get_earliest_version(&self) -> Version {
        let mut earliest_version = LATEST_VERSION;
        for layer in self.layer_set().layers {
            let layer_version = layer.get_version();
            if layer_version < earliest_version {
                earliest_version = layer_version;
            }
        }
        return earliest_version;
    }

    /// Returns a new mutable layer.
    pub fn new_mutable_layer() -> Arc<SkipListLayer<K, V>> {
        SkipListLayer::new(SKIP_LIST_LAYER_ITEMS)
    }

    /// Replaces the mutable layer.
    pub fn set_mutable_layer(&self, layer: Arc<SkipListLayer<K, V>>) {
        self.data.write().unwrap().mutable_layer = layer;
    }

    /// Records inspect data for the LSM tree into `node`.  Called lazily when inspect is queried.
    pub fn record_inspect_data(&self, root: &fuchsia_inspect::Node) {
        let layer_set = self.layer_set();
        root.record_child("layers", move |node| {
            let mut index = 0;
            for layer in layer_set.layers {
                node.record_child(format!("{index}"), move |node| {
                    layer.1.record_inspect_data(node)
                });
                index += 1;
            }
        });
        {
            let counters = self.counters.lock().unwrap();
            root.record_uint("num_seeks", counters.num_seeks as u64);
            root.record_uint(
                "bloom_filter_success_percent",
                if counters.layer_files_total == 0 {
                    0
                } else {
                    (counters.layer_files_skipped * 100).div_ceil(counters.layer_files_total) as u64
                },
            );
        }
    }
}

/// This is an RAII wrapper for a layer which holds a lock on the layer (via the Layer::lock
/// method).
pub struct LockedLayer<K, V>(Arc<DropEvent>, Arc<dyn Layer<K, V>>);

impl<K, V> LockedLayer<K, V> {
    pub async fn close_layer(self) {
        let layer = self.1;
        std::mem::drop(self.0);
        layer.close().await;
    }
}

impl<K, V> From<Arc<dyn Layer<K, V>>> for LockedLayer<K, V> {
    fn from(layer: Arc<dyn Layer<K, V>>) -> Self {
        let event = layer.lock().unwrap();
        Self(event, layer)
    }
}

impl<K, V> std::ops::Deref for LockedLayer<K, V> {
    type Target = Arc<dyn Layer<K, V>>;

    fn deref(&self) -> &Self::Target {
        &self.1
    }
}

impl<K, V> AsRef<dyn Layer<K, V>> for LockedLayer<K, V> {
    fn as_ref(&self) -> &(dyn Layer<K, V> + 'static) {
        self.1.as_ref()
    }
}

/// A LayerSet provides a snapshot of the layers at a particular point in time, and allows you to
/// get an iterator.  Iterators borrow the layers so something needs to hold reference count.
pub struct LayerSet<K, V> {
    pub layers: Vec<LockedLayer<K, V>>,
    merge_fn: merge::MergeFn<K, V>,
    counters: Arc<Mutex<Counters>>,
}

impl<K: Key + LayerKey + OrdLowerBound, V: Value> LayerSet<K, V> {
    pub fn sum_len(&self) -> usize {
        let mut size = 0;
        for layer in &self.layers {
            size += *layer.estimated_len()
        }
        size
    }

    pub fn merger(&self) -> merge::Merger<'_, K, V> {
        merge::Merger::new(
            self.layers.iter().map(|x| x.as_ref()),
            self.merge_fn,
            self.counters.clone(),
        )
    }
}

impl<K, V> fmt::Debug for LayerSet<K, V> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_list()
            .entries(self.layers.iter().map(|l| {
                if let Some(handle) = l.handle() {
                    format!("{}", handle.object_id())
                } else {
                    format!("{:?}", Arc::as_ptr(l))
                }
            }))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::LSMTree;
    use crate::drop_event::DropEvent;
    use crate::lsm_tree::cache::{
        NullCache, ObjectCache, ObjectCachePlaceholder, ObjectCacheResult,
    };
    use crate::lsm_tree::merge::{MergeLayerIterator, MergeResult};
    use crate::lsm_tree::types::{
        BoxedLayerIterator, FuzzyHash, Item, ItemCount, ItemRef, Key, Layer, LayerIterator,
        LayerKey, OrdLowerBound, OrdUpperBound, SortByU64, Value,
    };
    use crate::lsm_tree::{layers_from_handles, Query};
    use crate::object_handle::ObjectHandle;
    use crate::serialized_types::{
        versioned_type, Version, Versioned, VersionedLatest, LATEST_VERSION,
    };
    use crate::testing::fake_object::{FakeObject, FakeObjectHandle};
    use crate::testing::writer::Writer;
    use anyhow::{anyhow, Error};
    use async_trait::async_trait;
    use fprint::TypeFingerprint;
    use fxfs_macros::FuzzyHash;
    use rand::seq::SliceRandom;
    use rand::thread_rng;
    use std::hash::Hash;
    use std::sync::{Arc, Mutex};

    #[derive(
        Clone,
        Eq,
        PartialEq,
        Debug,
        Hash,
        FuzzyHash,
        serde::Serialize,
        serde::Deserialize,
        TypeFingerprint,
        Versioned,
    )]
    struct TestKey(std::ops::Range<u64>);

    versioned_type! { 1.. => TestKey }

    impl SortByU64 for TestKey {
        fn get_leading_u64(&self) -> u64 {
            self.0.start
        }
    }

    impl LayerKey for TestKey {}

    impl OrdUpperBound for TestKey {
        fn cmp_upper_bound(&self, other: &TestKey) -> std::cmp::Ordering {
            self.0.end.cmp(&other.0.end)
        }
    }

    impl OrdLowerBound for TestKey {
        fn cmp_lower_bound(&self, other: &Self) -> std::cmp::Ordering {
            self.0.start.cmp(&other.0.start)
        }
    }

    fn emit_left_merge_fn(
        _left: &MergeLayerIterator<'_, TestKey, u64>,
        _right: &MergeLayerIterator<'_, TestKey, u64>,
    ) -> MergeResult<TestKey, u64> {
        MergeResult::EmitLeft
    }

    impl Value for u64 {
        const DELETED_MARKER: Self = 0;
    }

    #[fuchsia::test]
    async fn test_iteration() {
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        let items = [Item::new(TestKey(1..1), 1), Item::new(TestKey(2..2), 2)];
        tree.insert(items[0].clone()).expect("insert error");
        tree.insert(items[1].clone()).expect("insert error");
        let layers = tree.layer_set();
        let mut merger = layers.merger();
        let mut iter = merger.query(Query::FullScan).await.expect("seek failed");
        let ItemRef { key, value, .. } = iter.get().expect("missing item");
        assert_eq!((key, value), (&items[0].key, &items[0].value));
        iter.advance().await.expect("advance failed");
        let ItemRef { key, value, .. } = iter.get().expect("missing item");
        assert_eq!((key, value), (&items[1].key, &items[1].value));
        iter.advance().await.expect("advance failed");
        assert!(iter.get().is_none());
    }

    #[fuchsia::test]
    async fn test_compact() {
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        let items = [
            Item::new(TestKey(1..1), 1),
            Item::new(TestKey(2..2), 2),
            Item::new(TestKey(3..3), 3),
            Item::new(TestKey(4..4), 4),
        ];
        tree.insert(items[0].clone()).expect("insert error");
        tree.insert(items[1].clone()).expect("insert error");
        tree.seal();
        tree.insert(items[2].clone()).expect("insert error");
        tree.insert(items[3].clone()).expect("insert error");
        tree.seal();
        let object = Arc::new(FakeObject::new());
        let handle = FakeObjectHandle::new(object.clone());
        {
            let layer_set = tree.immutable_layer_set();
            let mut merger = layer_set.merger();
            let iter = merger.query(Query::FullScan).await.expect("create merger");
            tree.compact_with_iterator(
                iter,
                items.len(),
                Writer::new(&handle).await,
                handle.block_size(),
            )
            .await
            .expect("compact failed");
        }
        tree.set_layers(layers_from_handles([handle]).await.expect("layers_from_handles failed"));
        let handle = FakeObjectHandle::new(object.clone());
        let tree = LSMTree::open(emit_left_merge_fn, [handle], Box::new(NullCache {}))
            .await
            .expect("open failed");

        let layers = tree.layer_set();
        let mut merger = layers.merger();
        let mut iter = merger.query(Query::FullScan).await.expect("seek failed");
        for i in 1..5 {
            let ItemRef { key, value, .. } = iter.get().expect("missing item");
            assert_eq!((key, value), (&TestKey(i..i), &i));
            iter.advance().await.expect("advance failed");
        }
        assert!(iter.get().is_none());
    }

    #[fuchsia::test]
    async fn test_find() {
        let items = [
            Item::new(TestKey(1..1), 1),
            Item::new(TestKey(2..2), 2),
            Item::new(TestKey(3..3), 3),
            Item::new(TestKey(4..4), 4),
        ];
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        tree.insert(items[0].clone()).expect("insert error");
        tree.insert(items[1].clone()).expect("insert error");
        tree.seal();
        tree.insert(items[2].clone()).expect("insert error");
        tree.insert(items[3].clone()).expect("insert error");

        let item = tree.find(&items[1].key).await.expect("find failed").expect("not found");
        assert_eq!(item, items[1]);
        assert!(tree.find(&TestKey(100..100)).await.expect("find failed").is_none());
    }

    #[fuchsia::test]
    async fn test_find_no_return_deleted_values() {
        let items = [Item::new(TestKey(1..1), 1), Item::new(TestKey(2..2), u64::DELETED_MARKER)];
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        tree.insert(items[0].clone()).expect("insert error");
        tree.insert(items[1].clone()).expect("insert error");

        let item = tree.find(&items[0].key).await.expect("find failed").expect("not found");
        assert_eq!(item, items[0]);
        assert!(tree.find(&items[1].key).await.expect("find failed").is_none());
    }

    #[fuchsia::test]
    async fn test_empty_seal() {
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        tree.seal();
        let item = Item::new(TestKey(1..1), 1);
        tree.insert(item.clone()).expect("insert error");
        let object = Arc::new(FakeObject::new());
        let handle = FakeObjectHandle::new(object.clone());
        {
            let layer_set = tree.immutable_layer_set();
            let mut merger = layer_set.merger();
            let iter = merger.query(Query::FullScan).await.expect("create merger");
            tree.compact_with_iterator(iter, 0, Writer::new(&handle).await, handle.block_size())
                .await
                .expect("compact failed");
        }
        tree.set_layers(layers_from_handles([handle]).await.expect("layers_from_handles failed"));
        let found_item = tree.find(&item.key).await.expect("find failed").expect("not found");
        assert_eq!(found_item, item);
        assert!(tree.find(&TestKey(2..2)).await.expect("find failed").is_none());
    }

    #[fuchsia::test]
    async fn test_filter() {
        let items = [
            Item::new(TestKey(1..1), 1),
            Item::new(TestKey(2..2), 2),
            Item::new(TestKey(3..3), 3),
            Item::new(TestKey(4..4), 4),
        ];
        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        tree.insert(items[0].clone()).expect("insert error");
        tree.insert(items[1].clone()).expect("insert error");
        tree.insert(items[2].clone()).expect("insert error");
        tree.insert(items[3].clone()).expect("insert error");

        let layers = tree.layer_set();
        let mut merger = layers.merger();

        // Filter out odd keys (which also guarantees we skip the first key which is an edge case).
        let mut iter = merger
            .query(Query::FullScan)
            .await
            .expect("seek failed")
            .filter(|item: ItemRef<'_, TestKey, u64>| item.key.0.start % 2 == 0)
            .await
            .expect("filter failed");

        assert_eq!(iter.get(), Some(items[1].as_item_ref()));
        iter.advance().await.expect("advance failed");
        assert_eq!(iter.get(), Some(items[3].as_item_ref()));
        iter.advance().await.expect("advance failed");
        assert!(iter.get().is_none());
    }

    #[fuchsia::test]
    async fn test_insert_order_agnostic() {
        let items = [
            Item::new(TestKey(1..1), 1),
            Item::new(TestKey(2..2), 2),
            Item::new(TestKey(3..3), 3),
            Item::new(TestKey(4..4), 4),
            Item::new(TestKey(5..5), 5),
            Item::new(TestKey(6..6), 6),
        ];
        let a = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        for item in &items {
            a.insert(item.clone()).expect("insert error");
        }
        let b = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        let mut shuffled = items.clone();
        shuffled.shuffle(&mut thread_rng());
        for item in &shuffled {
            b.insert(item.clone()).expect("insert error");
        }
        let layers = a.layer_set();
        let mut merger = layers.merger();
        let mut iter_a = merger.query(Query::FullScan).await.expect("seek failed");
        let layers = b.layer_set();
        let mut merger = layers.merger();
        let mut iter_b = merger.query(Query::FullScan).await.expect("seek failed");

        for item in items {
            assert_eq!(Some(item.as_item_ref()), iter_a.get());
            assert_eq!(Some(item.as_item_ref()), iter_b.get());
            iter_a.advance().await.expect("advance failed");
            iter_b.advance().await.expect("advance failed");
        }
        assert!(iter_a.get().is_none());
        assert!(iter_b.get().is_none());
    }

    struct AuditCacheInner<'a, V: Value> {
        lookups: u64,
        completions: u64,
        invalidations: u64,
        drops: u64,
        result: Option<ObjectCacheResult<'a, V>>,
    }

    impl<V: Value> AuditCacheInner<'_, V> {
        fn stats(&self) -> (u64, u64, u64, u64) {
            (self.lookups, self.completions, self.invalidations, self.drops)
        }
    }

    struct AuditCache<'a, V: Value> {
        inner: Arc<Mutex<AuditCacheInner<'a, V>>>,
    }

    impl<V: Value> AuditCache<'_, V> {
        fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(AuditCacheInner {
                    lookups: 0,
                    completions: 0,
                    invalidations: 0,
                    drops: 0,
                    result: None,
                })),
            }
        }
    }

    struct AuditPlaceholder<'a, V: Value> {
        inner: Arc<Mutex<AuditCacheInner<'a, V>>>,
        completed: Mutex<bool>,
    }

    impl<V: Value> ObjectCachePlaceholder<V> for AuditPlaceholder<'_, V> {
        fn complete(self: Box<Self>, _: Option<&V>) {
            self.inner.lock().unwrap().completions += 1;
            *self.completed.lock().unwrap() = true;
        }
    }

    impl<V: Value> Drop for AuditPlaceholder<'_, V> {
        fn drop(&mut self) {
            if !*self.completed.lock().unwrap() {
                self.inner.lock().unwrap().drops += 1;
            }
        }
    }

    impl<K: Key + std::cmp::PartialEq, V: Value> ObjectCache<K, V> for AuditCache<'_, V> {
        fn lookup_or_reserve(&self, _key: &K) -> ObjectCacheResult<'_, V> {
            {
                let mut inner = self.inner.lock().unwrap();
                inner.lookups += 1;
                if inner.result.is_some() {
                    return std::mem::take(&mut inner.result).unwrap();
                }
            }
            ObjectCacheResult::Placeholder(Box::new(AuditPlaceholder {
                inner: self.inner.clone(),
                completed: Mutex::new(false),
            }))
        }

        fn invalidate(&self, _key: K, _value: Option<V>) {
            self.inner.lock().unwrap().invalidations += 1;
        }
    }

    #[fuchsia::test]
    async fn test_cache_handling() {
        let item = Item::new(TestKey(1..1), 1);
        let cache = Box::new(AuditCache::new());
        let inner = cache.inner.clone();
        let a = LSMTree::new(emit_left_merge_fn, cache);

        // Zero counters.
        assert_eq!(inner.lock().unwrap().stats(), (0, 0, 0, 0));

        // Look for an item, but don't find it. So no insertion. It is dropped.
        assert!(a.find(&item.key).await.expect("Failed find").is_none());
        assert_eq!(inner.lock().unwrap().stats(), (1, 0, 0, 1));

        // Insert attempts to invalidate.
        let _ = a.insert(item.clone());
        assert_eq!(inner.lock().unwrap().stats(), (1, 0, 1, 1));

        // Look for item, find it and insert into the cache.
        assert_eq!(
            a.find(&item.key).await.expect("Failed find").expect("Item should be found.").value,
            item.value
        );
        assert_eq!(inner.lock().unwrap().stats(), (2, 1, 1, 1));

        // Insert or replace attempts to invalidate as well.
        a.replace_or_insert(item.clone());
        assert_eq!(inner.lock().unwrap().stats(), (2, 1, 2, 1));
    }

    #[fuchsia::test]
    async fn test_cache_hit() {
        let item = Item::new(TestKey(1..1), 1);
        let cache = Box::new(AuditCache::new());
        let inner = cache.inner.clone();
        let a = LSMTree::new(emit_left_merge_fn, cache);

        // Zero counters.
        assert_eq!(inner.lock().unwrap().stats(), (0, 0, 0, 0));

        // Insert attempts to invalidate.
        let _ = a.insert(item.clone());
        assert_eq!(inner.lock().unwrap().stats(), (0, 0, 1, 0));

        // Set up the item to find in the cache.
        inner.lock().unwrap().result = Some(ObjectCacheResult::Value(item.value.clone()));

        // Look for item, find it in cache, so no insert.
        assert_eq!(
            a.find(&item.key).await.expect("Failed find").expect("Item should be found.").value,
            item.value
        );
        assert_eq!(inner.lock().unwrap().stats(), (1, 0, 1, 0));
    }

    #[fuchsia::test]
    async fn test_cache_says_uncacheable() {
        let item = Item::new(TestKey(1..1), 1);
        let cache = Box::new(AuditCache::new());
        let inner = cache.inner.clone();
        let a = LSMTree::new(emit_left_merge_fn, cache);
        let _ = a.insert(item.clone());

        // One invalidation from the insert.
        assert_eq!(inner.lock().unwrap().stats(), (0, 0, 1, 0));

        // Set up the NoCache response to find in the cache.
        inner.lock().unwrap().result = Some(ObjectCacheResult::NoCache);

        // Look for item, it is uncacheable, so no insert.
        assert_eq!(
            a.find(&item.key).await.expect("Failed find").expect("Should find item").value,
            item.value
        );
        assert_eq!(inner.lock().unwrap().stats(), (1, 0, 1, 0));
    }

    struct FailLayer {
        drop_event: Mutex<Option<Arc<DropEvent>>>,
    }

    impl FailLayer {
        fn new() -> Self {
            Self { drop_event: Mutex::new(Some(Arc::new(DropEvent::new()))) }
        }
    }

    #[async_trait]
    impl<K: Key, V: Value> Layer<K, V> for FailLayer {
        async fn seek(
            &self,
            _bound: std::ops::Bound<&K>,
        ) -> Result<BoxedLayerIterator<'_, K, V>, Error> {
            Err(anyhow!("Purposely failed seek"))
        }

        fn lock(&self) -> Option<Arc<DropEvent>> {
            self.drop_event.lock().unwrap().clone()
        }

        fn estimated_len(&self) -> ItemCount {
            ItemCount::Estimate(0)
        }

        async fn close(&self) {
            let listener = match std::mem::replace(&mut (*self.drop_event.lock().unwrap()), None) {
                Some(drop_event) => drop_event.listen(),
                None => return,
            };
            listener.await;
        }

        fn get_version(&self) -> Version {
            LATEST_VERSION
        }
    }

    #[fuchsia::test]
    async fn test_failed_lookup() {
        let cache = Box::new(AuditCache::new());
        let inner = cache.inner.clone();
        let a = LSMTree::new(emit_left_merge_fn, cache);
        a.set_layers(vec![Arc::new(FailLayer::new())]);

        // Zero counters.
        assert_eq!(inner.lock().unwrap().stats(), (0, 0, 0, 0));

        // Lookup should fail and drop the placeholder.
        assert!(a.find(&TestKey(1..1)).await.is_err());
        assert_eq!(inner.lock().unwrap().stats(), (1, 0, 0, 1));
    }
}

#[cfg(fuzz)]
mod fuzz {
    use crate::lsm_tree::types::{
        FuzzyHash, Item, LayerKey, OrdLowerBound, OrdUpperBound, SortByU64, Value,
    };
    use crate::serialized_types::{
        versioned_type, Version, Versioned, VersionedLatest, LATEST_VERSION,
    };
    use arbitrary::Arbitrary;
    use fprint::TypeFingerprint;
    use fuzz::fuzz;
    use fxfs_macros::FuzzyHash;
    use std::hash::Hash;

    #[derive(
        Arbitrary,
        Clone,
        Eq,
        Hash,
        FuzzyHash,
        PartialEq,
        Debug,
        serde::Serialize,
        serde::Deserialize,
        TypeFingerprint,
        Versioned,
    )]
    struct TestKey(std::ops::Range<u64>);

    versioned_type! { 1.. => TestKey }

    impl Versioned for u64 {}
    versioned_type! { 1.. => u64 }

    impl LayerKey for TestKey {}

    impl SortByU64 for TestKey {
        fn get_leading_u64(&self) -> u64 {
            self.0.start
        }
    }

    impl OrdUpperBound for TestKey {
        fn cmp_upper_bound(&self, other: &TestKey) -> std::cmp::Ordering {
            self.0.end.cmp(&other.0.end)
        }
    }

    impl OrdLowerBound for TestKey {
        fn cmp_lower_bound(&self, other: &Self) -> std::cmp::Ordering {
            self.0.start.cmp(&other.0.start)
        }
    }

    impl Value for u64 {
        const DELETED_MARKER: Self = 0;
    }

    // Note: This code isn't really dead. it's used below in
    // `fuzz_lsm_tree_action`. However, the `#[fuzz]` proc macro attribute
    // obfuscates the usage enough to confuse the compiler.
    #[allow(dead_code)]
    #[derive(Arbitrary)]
    enum FuzzAction {
        Insert(Item<TestKey, u64>),
        ReplaceOrInsert(Item<TestKey, u64>),
        MergeInto(Item<TestKey, u64>, TestKey),
        Find(TestKey),
        Seal,
    }

    #[fuzz]
    fn fuzz_lsm_tree_actions(actions: Vec<FuzzAction>) {
        use super::cache::NullCache;
        use super::LSMTree;
        use crate::lsm_tree::merge::{MergeLayerIterator, MergeResult};
        use futures::executor::block_on;

        fn emit_left_merge_fn(
            _left: &MergeLayerIterator<'_, TestKey, u64>,
            _right: &MergeLayerIterator<'_, TestKey, u64>,
        ) -> MergeResult<TestKey, u64> {
            MergeResult::EmitLeft
        }

        let tree = LSMTree::new(emit_left_merge_fn, Box::new(NullCache {}));
        for action in actions {
            match action {
                FuzzAction::Insert(item) => {
                    let _ = tree.insert(item);
                }
                FuzzAction::ReplaceOrInsert(item) => {
                    tree.replace_or_insert(item);
                }
                FuzzAction::Find(key) => {
                    block_on(tree.find(&key)).expect("find failed");
                }
                FuzzAction::MergeInto(item, bound) => tree.merge_into(item, &bound),
                FuzzAction::Seal => tree.seal(),
            };
        }
    }
}
