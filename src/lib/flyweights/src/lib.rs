// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types implementing the [flyweight pattern](https://en.wikipedia.org/wiki/Flyweight_pattern)
//! for reusing object allocations.
//!
//! # Performance tradeoffs
//!
//! These types are most useful in scenarios where there are many duplicated values allocated for a
//! particular type and where reducing the memory overhead of holding those values is more important
//! than the runtime throughput of creating them.
//!
//! In order to deduplicate allocated values, the types in this library first check a global cache
//! to see if the value already exists which is will often be slower than naively allocating the
//! value.
//!
//! While creation of these types will generally be slower than their counter-parts, this can be
//! offset by them having O(1) operations for hashing and equality comparison.
//!
//! As with any performance optimization, you should only use this library if you can measure the
//! benefit it provides to your program. Pay careful attention to creating flyweights in hot paths
//! as it may regress runtime performance.
//!
//! # Allocation lifecycle
//!
//! Intended for long-running system services with user-provided values, flyweights are removed from
//! the global cache when the last reference to them is dropped. While this incurs some overhead
//! it is important to prevent the value cache from becoming a denial-of-service vector.

#![warn(missing_docs)]

use once_cell::sync::Lazy;
use serde::{
    de::{Deserializer, Visitor},
    ser::Serializer,
    Deserialize, Serialize,
};
use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt::{Debug, Display, Formatter, Result as FmtResult},
    hash::{Hash, Hasher},
    ops::Deref,
    sync::{Arc, Mutex},
};

/// The global string cache for `FlyStr`.
///
/// If a live `FlyStr` contains an `Arc<Box<str>>`, the `Arc<Box<str>>` must also be in this cache
/// and it must have a refcount of >= 2.
static CACHE: Lazy<Mutex<HashSet<Storage>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Wrapper type for stored `Arc`s that lets us query the cache without an owned value. Implementing
/// `Borrow<str> for Arc<Box<str>>` upstream *might* be possible with specialization but this is
/// easy enough.
#[derive(Eq, Hash, PartialEq)]
struct Storage(Arc<Box<str>>);

impl Borrow<str> for Storage {
    fn borrow(&self) -> &str {
        self.0.as_ref()
    }
}

/// An immutable string type which only stores a single copy of each string allocated. Internally
/// represented as an `Arc` to the backing allocation. Occupies a single pointer width.
///
/// # Performance
///
/// It's slower to construct than a regular `String` but trades that for reduced standing memory
/// usage by deduplicating strings. `PartialEq` and `Hash` are implemented on the underlying pointer
/// value rather than the pointed-to data for faster equality comparisons and indexing, which is
/// sound by virtue of the type guaranteeing that only one `FlyStr` pointer value will exist at
/// any time for a given string's contents.
///
/// The string value itself is dropped from the global cache when the last `FlyStr` referencing it
/// is dropped.
#[derive(Clone)]
pub struct FlyStr(Arc<Box<str>>);

static_assertions::const_assert_eq!(std::mem::size_of::<FlyStr>(), std::mem::size_of::<usize>());

impl FlyStr {
    /// Create a `FlyStr`, allocating it in the cache if the value is not already cached.
    ///
    /// # Performance
    ///
    /// Creating an instance of this type requires accessing the global cache of strings, which
    /// involves taking a lock. When multiple threads are allocating lots of strings there may be
    /// contention.
    ///
    /// Each string allocated is hashed for lookup in the cache.
    pub fn new(s: impl AsRef<str> + Into<String>) -> Self {
        let mut cache = CACHE.lock().unwrap();

        if let Some(existing) = cache.get(s.as_ref()) {
            Self(existing.0.clone())
        } else {
            let new = Arc::new(s.into().into_boxed_str());
            cache.insert(Storage(Arc::clone(&new)));
            Self(new)
        }
    }

    /// Returns the underlying string slice.
    pub fn as_str(&self) -> &str {
        &**self.0
    }
}

impl Drop for FlyStr {
    fn drop(&mut self) {
        // Lock the cache before checking the count to ensure consistency.
        let mut cache = CACHE.lock().unwrap();

        // Check whether we're the last reference outside the cache, if so remove from cache.
        if Arc::strong_count(&self.0) == 2 {
            assert!(cache.remove(&**self.0), "cache must have a reference if refcount is 2");
        }
    }
}

impl Default for FlyStr {
    fn default() -> Self {
        Self::new("")
    }
}

impl From<&'_ str> for FlyStr {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<&'_ String> for FlyStr {
    fn from(s: &String) -> Self {
        Self::new(&**s)
    }
}

impl From<String> for FlyStr {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<Box<str>> for FlyStr {
    fn from(s: Box<str>) -> Self {
        Self::new(s)
    }
}

impl From<&Box<str>> for FlyStr {
    fn from(s: &Box<str>) -> Self {
        Self::new(&**s)
    }
}

impl Into<String> for FlyStr {
    fn into(self) -> String {
        self.as_str().to_owned()
    }
}

impl Into<String> for &'_ FlyStr {
    fn into(self) -> String {
        self.as_str().to_owned()
    }
}

impl Deref for FlyStr {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for FlyStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for FlyStr {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for FlyStr {}

impl Hash for FlyStr {
    fn hash<H: Hasher>(&self, h: &mut H) {
        h.write_usize(self.0.as_ptr() as usize);
    }
}

impl PartialOrd for FlyStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}
impl Ord for FlyStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialEq<str> for FlyStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&'_ str> for FlyStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for FlyStr {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == &**other
    }
}

impl PartialOrd<str> for FlyStr {
    fn partial_cmp(&self, other: &str) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl Debug for FlyStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Debug::fmt(self.as_str(), f)
    }
}

impl Display for FlyStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Display::fmt(self.as_str(), f)
    }
}

impl Serialize for FlyStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'d> Deserialize<'d> for FlyStr {
    fn deserialize<D: Deserializer<'d>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(FlyStrVisitor)
    }
}

struct FlyStrVisitor;

impl Visitor<'_> for FlyStrVisitor {
    type Value = FlyStr;
    fn expecting(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str("a string")
    }

    fn visit_borrowed_str<'de, E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    // These tests all manipulate the process-global cache in the parent module. On target devices
    // we run each test case in its own process, so the test cases can't pollute each other. On
    // host, we run tests with a process for each suite (which is the Rust upstream default), and
    // we need to manually isolate the tests from each other.
    #[cfg(not(target_os = "fuchsia"))]
    use serial_test::serial;

    fn reset_global_cache() {
        CACHE.lock().unwrap().clear();
    }
    fn num_strings_in_global_cache() -> usize {
        CACHE.lock().unwrap().len()
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_formatting_is_equivalent_to_str() {
        let original = "hello, world!";
        let cached = FlyStr::new(original);
        assert_eq!(format!("{original}"), format!("{cached}"));
        assert_eq!(format!("{original:?}"), format!("{cached:?}"));
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_equality_works() {
        let contents = "hello, world!";
        let cached = FlyStr::new(contents);
        assert_eq!(cached, cached, "must be equal to itself");
        assert_eq!(cached, contents, "must be equal to the original");
        assert_eq!(cached, contents.to_owned(), "must be equal to an owned copy of the original");

        // test inequality too
        assert_ne!(cached, "goodbye");
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_comparison_works() {
        let lesser = FlyStr::new("1hello");
        let greater = FlyStr::new("2hello");

        // lesser as method receiver
        assert!(lesser < greater);
        assert!(lesser <= greater);

        // greater as method receiver
        assert!(greater > lesser);
        assert!(greater >= lesser);

        // lesser with itself as method receiver
        assert!(lesser <= lesser);
        assert!(lesser >= lesser);

        // greater with itself as method receiver
        assert!(greater <= greater);
        assert!(greater >= greater);
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn only_one_copy_allocated() {
        reset_global_cache();
        assert_eq!(num_strings_in_global_cache(), 0);

        let contents = "hello, world!";

        let original = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        assert_eq!(Arc::strong_count(&original.0), 2, "one copy on stack, one in cache");

        let cloned = original.clone();
        assert_eq!(num_strings_in_global_cache(), 1, "cloning just incremented refcount");
        assert_eq!(Arc::strong_count(&original.0), 3, "two copies on stack, one in cache");
        assert_eq!(original.0.as_ptr(), cloned.0.as_ptr(), "clone must point to same allocation");

        let deduped = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "new string was deduped");
        assert_eq!(Arc::strong_count(&deduped.0), 4, "three copies on stack, one in cache");
        assert_eq!(original.0.as_ptr(), deduped.0.as_ptr(), "dedupe must point to same allocation");
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn cached_strings_dropped_when_refs_dropped() {
        reset_global_cache();

        let alloced = FlyStr::new("hello, world!");
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        drop(alloced);
        assert_eq!(num_strings_in_global_cache(), 0, "last reference dropped");
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn equality_and_hashing_with_pointer_value_works_correctly() {
        let first = FlyStr::new("hello, world!");
        let second = FlyStr::new("hello, moon!");

        let mut set = HashSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first), "must be able to look up string in set");

        // re-insert the same cmstring
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second);
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn comparison_for_btree_storage_works() {
        let first = FlyStr::new("hello, world!");
        let second = FlyStr::new("hello, moon!");

        let mut set = BTreeSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first), "must be able to look up string in set");

        // re-insert the same cmstring
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second);
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
    }

    #[test]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn serde_works() {
        let s = FlyStr::new("hello, world!");

        let as_json = serde_json::to_string(&s).unwrap();
        assert_eq!(as_json, "\"hello, world!\"");

        assert_eq!(s, serde_json::from_str::<FlyStr>(&as_json).unwrap());
    }
}
