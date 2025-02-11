// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::drop_event::DropEvent;
use crate::object_handle::ReadObjectHandle;
use crate::serialized_types::{Version, Versioned, VersionedLatest};
use anyhow::Error;
use async_trait::async_trait;
use fprint::TypeFingerprint;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

pub use fxfs_macros::impl_fuzzy_hash;

// Force keys to be sorted first by a u64, so that they can be located approximately based on only
// that integer without the whole key.
pub trait SortByU64 {
    // Return the u64 that is used as the first value when deciding on sort order of the key.
    fn get_leading_u64(&self) -> u64;
}

/// An extension to `std::hash::Hash` to support values which should be partitioned and hashed into
/// buckets, where nearby keys will have the same hash value.  This is used for existence filtering
/// in layer files (see `Layer::maybe_contains_key`).
///
/// For point-based keys, this can be the same as `std::hash::Hash`, but for range-based keys, the
/// hash can collapse nearby ranges into the same hash value.  Since a range-based key may span
/// several buckets, `FuzzyHash::fuzzy_hash` must be called to split the key up into each of the
/// possible values that it overlaps with.
pub trait FuzzyHash: Hash + Sized {
    type Iter: Iterator<Item = u64>;
    /// To support range-based keys, multiple hash values may need to be checked for a given key.
    /// For example, an extent [0..1024) might return extents [0..512), [512..1024), each of which
    /// will have a unique return value for `Self::hash`.  For point-based keys, a single hash
    /// suffices, in which case None is returned and the hash value of `self` should be checked.
    /// Note that in general only a small number of partitions (e.g. 2) should be checked at once.
    /// Debug assertions will fire if too large of a range is checked.
    fn fuzzy_hash(&self) -> Self::Iter;

    /// Returns whether the type is a range-based key.
    fn is_range_key(&self) -> bool {
        false
    }
}

impl_fuzzy_hash!(u8);
impl_fuzzy_hash!(u32);
impl_fuzzy_hash!(u64);
impl_fuzzy_hash!(String);
impl_fuzzy_hash!(Vec<u8>);

/// Keys and values need to implement the following traits.  For merging, they need to implement
/// MergeableKey.  TODO: Use trait_alias when available.
pub trait Key:
    Clone
    + Debug
    + Hash
    + FuzzyHash
    + OrdUpperBound
    + Send
    + SortByU64
    + Sync
    + Versioned
    + VersionedLatest
    + std::marker::Unpin
    + 'static
{
}

pub trait RangeKey: Key {
    /// Returns if two keys overlap.
    fn overlaps(&self, other: &Self) -> bool;
}

impl<K> Key for K where
    K: Clone
        + Debug
        + Hash
        + FuzzyHash
        + OrdUpperBound
        + Send
        + SortByU64
        + Sync
        + Versioned
        + VersionedLatest
        + std::marker::Unpin
        + 'static
{
}

pub trait MergeableKey: Key + Eq + LayerKey + OrdLowerBound {}
impl<K> MergeableKey for K where K: Key + Eq + LayerKey + OrdLowerBound {}

/// Trait required for supporting Layer functionality.
pub trait LayerValue:
    Clone + Send + Sync + Versioned + VersionedLatest + Debug + std::marker::Unpin + 'static
{
}
impl<V> LayerValue for V where
    V: Clone + Send + Sync + Versioned + VersionedLatest + Debug + std::marker::Unpin + 'static
{
}

/// Superset of `LayerValue` to additionally support tree searching, requires comparison and an
/// `DELETED_MARKER` for indicating empty values used to indicate deletion in the `LSMTree`.
pub trait Value: PartialEq + LayerValue {
    /// Value used to represent that the entry is actually empty, and should be ignored.
    const DELETED_MARKER: Self;
}

/// ItemRef is a struct that contains references to key and value, which is useful since in many
/// cases since keys and values are stored separately so &Item is not possible.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ItemRef<'a, K, V> {
    pub key: &'a K,
    pub value: &'a V,
    pub sequence: u64,
}

impl<K: Clone, V: Clone> ItemRef<'_, K, V> {
    pub fn cloned(&self) -> Item<K, V> {
        Item { key: self.key.clone(), value: self.value.clone(), sequence: self.sequence }
    }

    pub fn boxed(&self) -> BoxedItem<K, V> {
        Box::new(self.cloned())
    }
}

impl<'a, K, V> Clone for ItemRef<'a, K, V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, K, V> Copy for ItemRef<'a, K, V> {}

/// Item is a struct that combines a key and a value.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct Item<K, V> {
    pub key: K,
    pub value: V,
    /// |sequence| is a monotonically increasing sequence number for the Item, which is set when the
    /// Item is inserted into the tree.  In practice, this is the journal file offset at the time of
    /// committing the transaction containing the Item.  Note that two or more Items may share the
    /// same |sequence|.
    pub sequence: u64,
}

pub type BoxedItem<K, V> = Box<Item<K, V>>;

// Nb: type-fprint doesn't support generics yet.
impl<K: TypeFingerprint, V: TypeFingerprint> TypeFingerprint for Item<K, V> {
    fn fingerprint() -> String {
        "struct {key:".to_owned()
            + &K::fingerprint()
            + ",value:"
            + &V::fingerprint()
            + ",sequence:u64}"
    }
}

impl<K, V> Item<K, V> {
    pub fn new(key: K, value: V) -> Item<K, V> {
        Item { key, value, sequence: 0u64 }
    }

    pub fn new_with_sequence(key: K, value: V, sequence: u64) -> Item<K, V> {
        Item { key, value, sequence }
    }

    pub fn as_item_ref(&self) -> ItemRef<'_, K, V> {
        self.into()
    }

    pub fn boxed(self) -> BoxedItem<K, V> {
        Box::new(self)
    }
}

impl<'a, K, V> From<&'a Item<K, V>> for ItemRef<'a, K, V> {
    fn from(item: &'a Item<K, V>) -> ItemRef<'a, K, V> {
        ItemRef { key: &item.key, value: &item.value, sequence: item.sequence }
    }
}

impl<'a, K, V> From<&'a BoxedItem<K, V>> for ItemRef<'a, K, V> {
    fn from(item: &'a BoxedItem<K, V>) -> ItemRef<'a, K, V> {
        ItemRef { key: &item.key, value: &item.value, sequence: item.sequence }
    }
}

/// The find functions will return items with keys that are greater-than or equal to the search key,
/// so for keys that are like extents, the keys should sort (via OrdUpperBound) using the end
/// of their ranges, and you should set the search key accordingly.
///
/// For example, let's say the tree holds extents 100..200, 200..250 and you want to perform a read
/// for range 150..250, you should search for 0..151 which will first return the extent 100..200
/// (and then the iterator can be advanced to 200..250 after). When merging, keys can overlap, so
/// consider the case where we want to merge an extent with range 100..300 with an existing extent
/// of 200..250. In that case, we want to treat the extent with range 100..300 as lower than the key
/// 200..250 because we'll likely want to split the extents (e.g. perhaps we want 100..200,
/// 200..250, 250..300), so for merging, we need to use a different comparison function and we deal
/// with that using the OrdLowerBound trait.
///
/// If your keys don't have overlapping ranges that need to be merged, then these can be the same as
/// std::cmp::Ord (use the DefaultOrdUpperBound and DefaultOrdLowerBound traits).

pub trait OrdUpperBound {
    fn cmp_upper_bound(&self, other: &Self) -> std::cmp::Ordering;
}

pub trait DefaultOrdUpperBound: OrdUpperBound + Ord {}

impl<T: DefaultOrdUpperBound> OrdUpperBound for T {
    fn cmp_upper_bound(&self, other: &Self) -> std::cmp::Ordering {
        // Default to using cmp.
        self.cmp(other)
    }
}

pub trait OrdLowerBound {
    fn cmp_lower_bound(&self, other: &Self) -> std::cmp::Ordering;
}

pub trait DefaultOrdLowerBound: OrdLowerBound + Ord {}

impl<T: DefaultOrdLowerBound> OrdLowerBound for T {
    fn cmp_lower_bound(&self, other: &Self) -> std::cmp::Ordering {
        // Default to using cmp.
        self.cmp(other)
    }
}

/// Result returned by `merge_type()` to determine how to properly merge values within a layerset.
#[derive(Clone, PartialEq)]
pub enum MergeType {
    /// Always includes every layer in the merger, when seeking or advancing. Always correct, but
    /// always as slow as possible.
    FullMerge,

    /// Stops seeking older layers when an exact key match is found in a newer one. Useful for keys
    /// that only replace data, or with `next_key()` implementations to decide on continued merging.
    OptimizedMerge,
}

/// Determines how to iterate forward from the current key, and how many older layers to include
/// when merging. See the different variants of `MergeKeyType` for more details.
pub trait LayerKey: Clone {
    /// Called to determine how to perform merge behaviours while advancing through a layer set.
    fn merge_type(&self) -> MergeType {
        // Defaults to full merge. The slowest, but most predictable in behaviour.
        MergeType::FullMerge
    }

    /// The next_key() call allows for an optimisation which allows the merger to avoid querying a
    /// layer if it knows it has found the next possible key.  It only makes sense for this to
    /// return Some() when merge_type() returns OptimizedMerge. Consider the following example
    /// showing two layers with range based keys.
    ///
    ///      +----------+------------+
    ///  0   |  0..100  |  100..200  |
    ///      +----------+------------+
    ///  1              |  100..200  |
    ///                 +------------+
    ///
    /// If you search and find the 0..100 key, then only layer 0 will be touched.  If you then want
    /// to advance to the 100..200 record, you can find it in layer 0 but unless you know that it
    /// immediately follows the 0..100 key i.e. that there is no possible key, K, such that
    /// 0..100 < K < 100..200, the merger has to consult all other layers to check.  next_key should
    /// return a key, N, such that if the merger encounters a key that is <= N (using
    /// OrdLowerBound), it can stop touching more layers.  The key N should also be the the key to
    /// search for in other layers if the merger needs to do so.  In the example above, it should be
    /// a key that is > 0..100 and 99..100, but <= 100..200 (using OrdUpperBound).  In practice,
    /// what this means is that for range based keys, OrdUpperBound should use the end of the range,
    /// OrdLowerBound should use the start of the range, and next_key should return end..end + 1.
    /// This is purely an optimisation; the default None will be correct but not performant.
    fn next_key(&self) -> Option<Self> {
        None
    }
    /// Returns the search key for this extent; that is, a key which is <= this key under Ord and
    /// OrdLowerBound.  Note that this is only used for Query::LimitedRange queries (where
    /// `Self::partition` returns Some).
    /// For example, if the tree has extents 50..150 and 150..200 and we wish to read 100..200, we'd
    /// search for 0..101 which would set the iterator to 50..150.
    fn search_key(&self) -> Self {
        unreachable!()
    }
}

/// See `Layer::len`.
pub enum ItemCount {
    Precise(usize),
    Estimate(usize),
}

impl std::ops::Deref for ItemCount {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Precise(size) => size,
            Self::Estimate(size) => size,
        }
    }
}

/// Layer is a trait that all layers need to implement (mutable and immutable).
#[async_trait]
pub trait Layer<K, V>: Send + Sync {
    /// If the layer is persistent, returns the handle to its contents.  Returns None for in-memory
    /// layers.
    fn handle(&self) -> Option<&dyn ReadObjectHandle> {
        None
    }

    /// Some layer implementations may choose to cache data in-memory.  Calling this function will
    /// request that the layer purges unused cached data.  This is intended to run on a timer.
    fn purge_cached_data(&self) {}

    /// Searches for a key. Bound::Excluded is not supported. Bound::Unbounded positions the
    /// iterator on the first item in the layer.
    async fn seek(&self, bound: std::ops::Bound<&K>)
        -> Result<BoxedLayerIterator<'_, K, V>, Error>;

    /// Returns the number of items in the layer file, or an estimate if not known.
    /// Old persistent layer formats did not keep track of how many entries they have, hence the
    /// estimate.  If this is wrong, bloom filter sizing might be off, but that won't affect
    /// correctness, and will wash out with future compactions anyways.
    fn estimated_len(&self) -> ItemCount;

    /// Returns whether the layer *might* contain records relevant to `key`.  Note that this can
    /// return true even if the layer has no records relevant to `key`, but it will never return
    /// false if there are such records.  (As such, always returning true is a trivially correct
    /// implementation.)
    fn maybe_contains_key(&self, _key: &K) -> bool {
        true
    }

    /// Locks the layer preventing it from being closed. This will never block i.e. there can be
    /// many locks concurrently.  The lock is purely advisory: seek will still work even if lock has
    /// not been called; it merely causes close to wait until all locks are released.  Returns None
    /// if close has been called for the layer.
    fn lock(&self) -> Option<Arc<DropEvent>>;

    /// Waits for existing locks readers to finish and then returns.  Subsequent calls to lock will
    /// return None.
    async fn close(&self);

    /// Returns the version number used by structs in this layer
    fn get_version(&self) -> Version;

    /// Records inspect data for the layer into `node`.  Called lazily when inspect is queried.
    fn record_inspect_data(self: Arc<Self>, _node: &fuchsia_inspect::Node) {}
}

/// Something that implements LayerIterator is returned by the seek function.
#[async_trait]
pub trait LayerIterator<K, V>: Send + Sync {
    /// Advances the iterator.
    async fn advance(&mut self) -> Result<(), Error>;

    /// Returns the current item. This will be None if called when the iterator is first crated i.e.
    /// before either seek or advance has been called, and None if the iterator has reached the end
    /// of the layer.
    fn get(&self) -> Option<ItemRef<'_, K, V>>;

    /// Creates an iterator that only yields items from the underlying iterator for which
    /// `predicate` returns `true`.
    async fn filter<P>(self, predicate: P) -> Result<FilterLayerIterator<Self, P, K, V>, Error>
    where
        P: for<'b> Fn(ItemRef<'b, K, V>) -> bool + Send + Sync,
        Self: Sized,
        K: Send + Sync,
        V: Send + Sync,
    {
        FilterLayerIterator::new(self, predicate).await
    }
}

pub type BoxedLayerIterator<'iter, K, V> = Box<dyn LayerIterator<K, V> + 'iter>;

impl<'iter, K, V> LayerIterator<K, V> for BoxedLayerIterator<'iter, K, V> {
    // Manual expansion of `async_trait` to avoid double boxing the `Future`.
    fn advance<'a, 'b>(&'a mut self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        (**self).advance()
    }
    fn get(&self) -> Option<ItemRef<'_, K, V>> {
        (**self).get()
    }
}

/// Mutable layers need an iterator that implements this in order to make merge_into work.
pub(super) trait LayerIteratorMut<K, V>: Sync {
    /// Advances the iterator.
    fn advance(&mut self);

    /// Returns the current item. This will be None if called when the iterator is first crated i.e.
    /// before either seek or advance has been called, and None if the iterator has reached the end
    /// of the layer.
    fn get(&self) -> Option<ItemRef<'_, K, V>>;

    /// Inserts the item before the item that the iterator is located at.  The insert won't be
    /// visible until the changes are committed (see `commit`).
    fn insert(&mut self, item: Item<K, V>);

    /// Erases the current item and positions the iterator on the next item, if any.  The change
    /// won't be visible until committed (see `commit`).
    fn erase(&mut self);

    /// Commits the changes.  This does not wait for existing readers to finish.
    fn commit(&mut self);
}

/// Trait for writing new layers.
pub trait LayerWriter<K, V>: Sized
where
    K: Debug + Send + Versioned + Sync,
    V: Debug + Send + Versioned + Sync,
{
    /// Writes the given item to this layer.
    fn write(&mut self, item: ItemRef<'_, K, V>) -> impl Future<Output = Result<(), Error>> + Send;

    /// Flushes any buffered items to the backing storage.
    fn flush(&mut self) -> impl Future<Output = Result<(), Error>> + Send;
}

/// A `LayerIterator`` that filters the items of another `LayerIterator`.
pub struct FilterLayerIterator<I, P, K, V> {
    iter: I,
    predicate: P,
    _key: PhantomData<K>,
    _value: PhantomData<V>,
}

impl<I, P, K, V> FilterLayerIterator<I, P, K, V>
where
    I: LayerIterator<K, V>,
    P: for<'b> Fn(ItemRef<'b, K, V>) -> bool + Send + Sync,
{
    async fn new(iter: I, predicate: P) -> Result<Self, Error> {
        let mut filter = Self { iter, predicate, _key: PhantomData, _value: PhantomData };
        filter.skip_filtered().await?;
        Ok(filter)
    }

    async fn skip_filtered(&mut self) -> Result<(), Error> {
        loop {
            match self.iter.get() {
                Some(item) if !(self.predicate)(item) => {}
                _ => return Ok(()),
            }
            self.iter.advance().await?;
        }
    }
}

#[async_trait]
impl<I, P, K, V> LayerIterator<K, V> for FilterLayerIterator<I, P, K, V>
where
    I: LayerIterator<K, V>,
    P: for<'b> Fn(ItemRef<'b, K, V>) -> bool + Send + Sync,
    K: Send + Sync,
    V: Send + Sync,
{
    async fn advance(&mut self) -> Result<(), Error> {
        self.iter.advance().await?;
        self.skip_filtered().await
    }

    fn get(&self) -> Option<ItemRef<'_, K, V>> {
        self.iter.get()
    }
}

#[cfg(test)]
mod test_types {
    use crate::lsm_tree::types::{
        impl_fuzzy_hash, DefaultOrdLowerBound, DefaultOrdUpperBound, FuzzyHash, LayerKey,
        MergeType, SortByU64,
    };

    impl DefaultOrdUpperBound for i32 {}
    impl DefaultOrdLowerBound for i32 {}
    impl SortByU64 for i32 {
        fn get_leading_u64(&self) -> u64 {
            if self >= &0 {
                return u64::try_from(*self).unwrap() + u64::try_from(i32::MAX).unwrap() + 1;
            }
            u64::try_from(self + i32::MAX + 1).unwrap()
        }
    }
    impl LayerKey for i32 {
        fn merge_type(&self) -> MergeType {
            MergeType::FullMerge
        }
    }

    impl_fuzzy_hash!(i32);
}
