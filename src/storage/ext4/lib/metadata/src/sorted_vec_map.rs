// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::{iter, slice};

/// An ordered map built on a `Vec`.
///
/// This map is optimized for reducing the memory usage of data that rarely or never changes.
/// Insertions and removals take linear time while lookups take logarithmic time.
#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Clone, Default)]
pub struct SortedVecMap<K, V> {
    vec: Vec<(K, V)>,
}

impl<K, V> SortedVecMap<K, V> {
    /// Constructs a new, empty `SortedVecMap`.
    pub fn new() -> Self {
        Self { vec: Vec::new() }
    }

    /// Returns true if there are not entries in the map.
    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    /// Returns a reference to the value corresponding to the key.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        if let Ok(index) = self.index_of(key) {
            Some(&self.vec[index].1)
        } else {
            None
        }
    }

    /// Returns a mutable reference to the value corresponding to the key.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        if let Ok(index) = self.index_of(key) {
            Some(&mut self.vec[index].1)
        } else {
            None
        }
    }

    /// Inserts a key-value pair into the map. If the map did not have this key present, `None` is
    /// returned. If the map did have this key present, the value is updated, and the old value is
    /// returned. The key is not updated.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        match self.index_of(&key) {
            Ok(index) => {
                let old = std::mem::replace(&mut self.vec[index], (key, value));
                Some(old.1)
            }
            Err(index) => {
                self.vec.insert(index, (key, value));
                None
            }
        }
    }

    /// Returns an iterator over the entries of the map, sorted by key.
    pub fn iter(&self) -> Iter<'_, K, V> {
        self.vec.iter().map(|entry| (&entry.0, &entry.1))
    }

    /// Returns an iterator over the entries of the map, sorted by key.
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        self.vec.iter_mut().map(|entry| (&entry.0, &mut entry.1))
    }

    /// Returns an iterator over the keys of the map, in sorted order.
    pub fn keys(&self) -> Keys<'_, K, V> {
        self.vec.iter().map(|e| &e.0)
    }

    /// Returns a mutable iterator over the values of the map, sorted by key.
    pub fn values_mut(&mut self) -> ValuesMut<'_, K, V> {
        self.vec.iter_mut().map(|e| &mut e.1)
    }

    /// Searches for the key in the map.
    ///
    /// If the key is found then `Result::Ok` is returned with the index of the entry. If the key is
    /// not found then `Result::Err` is return, containing the index where an entry with that key
    /// could be inserted while maintaining sorted order.
    fn index_of<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.vec.binary_search_by(|probe| probe.0.borrow().cmp(key))
    }
}

impl<K: Debug, V: Debug> Debug for SortedVecMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

pub type Iter<'a, K, V> = iter::Map<slice::Iter<'a, (K, V)>, fn(&(K, V)) -> (&K, &V)>;
pub type IterMut<'a, K, V> = iter::Map<slice::IterMut<'a, (K, V)>, fn(&mut (K, V)) -> (&K, &mut V)>;
pub type Keys<'a, K, V> = iter::Map<slice::Iter<'a, (K, V)>, fn(&(K, V)) -> &K>;
pub type ValuesMut<'a, K, V> = iter::Map<slice::IterMut<'a, (K, V)>, fn(&mut (K, V)) -> &mut V>;

impl<K: Ord, V> FromIterator<(K, V)> for SortedVecMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut vec = Vec::from_iter(iter);
        vec.shrink_to_fit();
        vec.sort_by(sort_comparator);
        vec.dedup_by(dedup_comparator);
        Self { vec }
    }
}

impl<'de, K, V> serde::Deserialize<'de> for SortedVecMap<K, V>
where
    K: Ord + serde::Deserialize<'de>,
    V: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor<K, V> {
            _map_type: PhantomData<SortedVecMap<K, V>>,
        }
        impl<'de, K, V> serde::de::Visitor<'de> for Visitor<K, V>
        where
            K: Ord + serde::Deserialize<'de>,
            V: serde::Deserialize<'de>,
        {
            type Value = SortedVecMap<K, V>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a map")
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut vec: Vec<(K, V)> = match map.size_hint() {
                    Some(hint) => Vec::with_capacity(hint),
                    None => Vec::new(),
                };
                let mut is_sorted_and_deduped = true;
                while let Some(entry) = map.next_entry()? {
                    if is_sorted_and_deduped {
                        if let Some(back) = vec.last() {
                            is_sorted_and_deduped = back.0.cmp(&entry.0) == Ordering::Less;
                        }
                    }
                    vec.push(entry);
                }
                if !is_sorted_and_deduped {
                    vec.sort_by(sort_comparator);
                    vec.dedup_by(dedup_comparator);
                }
                vec.shrink_to_fit();
                Ok(SortedVecMap { vec })
            }
        }
        deserializer.deserialize_map(Visitor { _map_type: PhantomData })
    }
}

impl<K: serde::Serialize, V: serde::Serialize> serde::Serialize for SortedVecMap<K, V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_map(self.iter())
    }
}

fn sort_comparator<K: Ord, V>(a: &(K, V), b: &(K, V)) -> Ordering {
    a.0.cmp(&b.0)
}

fn dedup_comparator<K: Ord, V>(a: &mut (K, V), b: &mut (K, V)) -> bool {
    a.0 == b.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_get() {
        let mut map = SortedVecMap::new();
        assert!(map.get(&1).is_none());

        map.insert(1, 21);
        assert_eq!(map.get(&1), Some(&21));

        map.insert(0, 20);
        assert_eq!(map.get(&0), Some(&20));
        assert_eq!(map.get(&1), Some(&21));

        map.insert(2, 22);
        assert_eq!(map.get(&0), Some(&20));
        assert_eq!(map.get(&1), Some(&21));
        assert_eq!(map.get(&2), Some(&22));
    }

    #[test]
    fn test_insert() {
        let mut map = SortedVecMap::new();
        for (k, v) in [(50, 50), (47, 47), (48, 48), (51, 51), (49, 49)] {
            assert!(map.insert(k, v).is_none());
        }
        assert_eq!(map.insert(48, 88), Some(48));
        assert_eq!(map.vec.as_slice(), &[(47, 47), (48, 88), (49, 49), (50, 50), (51, 51)])
    }

    #[test]
    fn test_serialize_deserialize() {
        let map: SortedVecMap<i32, i32> =
            [(56, 56), (47, 47), (53, 53), (51, 51), (49, 49)].into_iter().collect();
        let serialized = bincode::serialize(&map).unwrap();
        let deserialized: SortedVecMap<i32, i32> = bincode::deserialize(&serialized).unwrap();
        assert_eq!(map, deserialized);
    }

    #[test]
    fn test_deserialize_from_btree_map() {
        let map: BTreeMap<i32, i32> = [(56, 56), (47, 47), (53, 53), (51, 51), (49, 49)].into();
        let serialized = bincode::serialize(&map).unwrap();
        let deserialized: SortedVecMap<i32, i32> = bincode::deserialize(&serialized).unwrap();

        let map_entries: Vec<(i32, i32)> = map.into_iter().collect();
        assert_eq!(map_entries, deserialized.vec);
    }
}
