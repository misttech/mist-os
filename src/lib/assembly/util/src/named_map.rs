// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{DuplicateKeyError, InsertUniqueExt, MapEntry};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::BTreeMap;

/// A named set of things, which are mapped by a String key.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NamedMap<T> {
    /// The name of the Map.
    pub name: String,

    /// The entries in the map.
    pub entries: BTreeMap<String, T>,
}

impl<T> NamedMap<T>
where
    T: std::fmt::Debug + std::cmp::PartialEq,
{
    /// Create a new, named, map.
    pub fn new(name: &str) -> Self {
        Self { name: name.to_owned(), entries: BTreeMap::new() }
    }

    /// Insert `value` into the map returning an error if the key exists and the value is
    /// different.
    ///
    /// If the key is unique, return Ok(())
    /// If the key is a duplicate, but the value is the same, return Err((true, anyhow::Error))
    /// If the key is a duplicate, and the value is different, return Err((false, anyhow::Error))
    fn try_insert_check_for_duplicate(
        &mut self,
        name: String,
        value: T,
    ) -> Result<(), (bool, anyhow::Error)> {
        let result = self.entries.try_insert_unique(MapEntry(name, value)).map_err(|e| {
            // Indicate whether the error is due to a duplicate by adding a boolean to the return.
            let duplicate = e.previous_value() == e.new_value();
            (
                duplicate,
                format!(
                    "key: '{}'\n  existing value: {:#?}\n  new value: {:#?}",
                    e.key(),
                    e.previous_value(),
                    e.new_value()
                ),
            )
        });
        // The error is mapped a second time to separate the borrow of entries
        // from the borrow of name.
        result.map_err(|(d, e)| (d, anyhow!("duplicate entry in {}:\n  {}", self.name, e)))
    }

    /// Insert `value` into the map ensuring that `name` is unique.
    pub fn try_insert_unique(&mut self, name: String, value: T) -> Result<()> {
        // Ignore the returned 'duplicate' boolean. If there is an error, return it.
        self.try_insert_check_for_duplicate(name, value).map_err(|(_, e)| e)
    }

    /// Insert `value` into the map ensuring that if `name` is found that the values are identical.
    pub fn try_insert_unique_ignore_duplicates(&mut self, name: String, value: T) -> Result<()> {
        match self.try_insert_check_for_duplicate(name, value) {
            // If the key is unique or the value is a duplicate, return Ok(())
            Ok(_) | Err((true, _)) => Ok(()),
            // Otherwise, forward the error.
            Err((false, e)) => Err(e),
        }
    }
}

impl<T> std::ops::Deref for NamedMap<T> {
    type Target = BTreeMap<String, T>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl<T> std::ops::DerefMut for NamedMap<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

impl<T> IntoIterator for NamedMap<T> {
    type Item = (String, T);
    type IntoIter = std::collections::btree_map::IntoIter<String, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a NamedMap<T> {
    type Item = (&'a String, &'a T);
    type IntoIter = std::collections::btree_map::Iter<'a, String, T>;
    fn into_iter(self) -> Self::IntoIter {
        (&self.entries).into_iter()
    }
}

impl<T> FromIterator<(String, T)> for NamedMap<T> {
    fn from_iter<I: IntoIterator<Item = (String, T)>>(iter: I) -> Self {
        Self { name: "from iterator".into(), entries: BTreeMap::from_iter(iter) }
    }
}

impl<T, const N: usize> From<[(String, T); N]> for NamedMap<T> {
    fn from(arr: [(String, T); N]) -> Self {
        Self { name: "from".into(), entries: BTreeMap::from(arr) }
    }
}

impl<T> From<NamedMap<T>> for BTreeMap<String, T> {
    fn from(map: NamedMap<T>) -> Self {
        map.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_insert_unique() {
        let mut map = NamedMap::new("test");
        map.try_insert_unique("a".into(), "alpha").unwrap();
        map.try_insert_unique("b".into(), "beta").unwrap();
        assert!(map.try_insert_unique("a".into(), "alpha").is_err());
    }

    #[test]
    fn test_try_insert_unique_ignore_duplicates() {
        let mut map = NamedMap::new("test");
        map.try_insert_unique("a".into(), "alpha").unwrap();
        map.try_insert_unique("b".into(), "beta").unwrap();
        map.try_insert_unique_ignore_duplicates("a".into(), "alpha").unwrap();
        assert!(map.try_insert_unique_ignore_duplicates("a".into(), "beta").is_err());
    }
}
