// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::sandbox_util::DictExt,
    cm_types::{IterablePath, Name},
    fidl_fuchsia_component_sandbox as fsandbox,
    lazy_static::lazy_static,
    sandbox::{Capability, Dict},
    std::{fmt, marker::PhantomData},
};

/// This trait is implemented by types that wrap a [Dict] and wish to present an abstracted
/// interface over the [Dict].
///
/// All such types are defined in this module, so this trait is private.
///
/// See also: [StructuredDictMap]
trait StructuredDict: Into<Dict> + Default + Clone + fmt::Debug {
    /// Converts from [Dict] to `Self`.
    ///
    /// REQUIRES: [Dict] is a valid representation of `Self`.
    ///
    /// IMPORTANT: The caller should know that [Dict] is a valid representation of [Self]. This
    /// function is not guaranteed to perform any validation.
    fn from_dict(dict: Dict) -> Self;
}

/// A collection type for mapping [Name] to [StructuredDict], using [Dict] as the underlying
/// representation.
///
/// For example, this can be used to store a map of child or collection names to [ComponentInput]s
/// (where [ComponentInput] is the type that implements [StructuredDict]).
///
/// Because the representation of this type is [Dict], this type itself implements
/// [StructuredDict].
#[derive(Clone, Debug, Default)]
#[allow(private_bounds)]
pub struct StructuredDictMap<T: StructuredDict> {
    inner: Dict,
    phantom: PhantomData<T>,
}

impl<T: StructuredDict> StructuredDict for StructuredDictMap<T> {
    fn from_dict(dict: Dict) -> Self {
        Self { inner: dict, phantom: Default::default() }
    }
}

#[allow(private_bounds)]
impl<T: StructuredDict> StructuredDictMap<T> {
    pub fn insert(&mut self, key: Name, value: T) -> Result<(), fsandbox::DictionaryError> {
        let mut entries = self.inner.lock_entries();
        let dict: Dict = value.into();
        entries.insert(key, dict.into())
    }

    pub fn get(&self, key: &Name) -> Option<T> {
        let entries = self.inner.lock_entries();
        entries.get(key).map(|cap| {
            let Capability::Dictionary(dict) = cap else {
                unreachable!("structured map entry must be a dict: {cap:?}");
            };
            T::from_dict(dict.clone())
        })
    }

    pub fn remove(&mut self, key: &Name) -> Option<T> {
        let mut entries = self.inner.lock_entries();
        entries.remove(key).map(|cap| {
            let Capability::Dictionary(dict) = cap else {
                unreachable!("structured map entry must be a dict: {cap:?}");
            };
            T::from_dict(dict.clone())
        })
    }
}

impl<T: StructuredDict> From<StructuredDictMap<T>> for Dict {
    fn from(m: StructuredDictMap<T>) -> Self {
        m.inner
    }
}

// Dictionary keys for different kinds of sandboxes.
lazy_static! {
    /// Dictionary of capabilities from the parent.
    static ref PARENT: Name = "parent".parse().unwrap();

    /// Dictionary of capabilities from a component's environment.
    static ref ENVIRONMENT: Name = "environment".parse().unwrap();

    /// Dictionary of debug capabilities in a component's environment.
    static ref DEBUG: Name = "debug".parse().unwrap();
}

/// Contains the capabilities component receives from its parent and environment. Stored as a
/// [Dict] containing two nested [Dict]s for the parent and environment.
#[derive(Clone, Debug)]
pub struct ComponentInput(Dict);

impl Default for ComponentInput {
    fn default() -> Self {
        Self::new(ComponentEnvironment::new())
    }
}

impl StructuredDict for ComponentInput {
    fn from_dict(dict: Dict) -> Self {
        Self(dict)
    }
}

impl ComponentInput {
    pub fn new(environment: ComponentEnvironment) -> Self {
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(PARENT.clone(), Dict::new().into()).ok();
        entries.insert(ENVIRONMENT.clone(), Dict::from(environment).into()).ok();
        drop(entries);
        Self(dict)
    }

    /// Creates a new ComponentInput with entries cloned from this ComponentInput.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Self {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(PARENT.clone(), self.capabilities().shallow_copy().into()).ok();
        entries
            .insert(ENVIRONMENT.clone(), Dict::from(self.environment()).shallow_copy().into())
            .ok();
        drop(entries);
        Self(dict)
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's parent.
    pub fn capabilities(&self) -> Dict {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*PARENT).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("parent entry must be a dict: {cap:?}");
        };
        dict.clone()
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's environment.
    pub fn environment(&self) -> ComponentEnvironment {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*ENVIRONMENT).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("environment entry must be a dict: {cap:?}");
        };
        ComponentEnvironment(dict.clone())
    }

    pub fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError> {
        self.capabilities().insert_capability(path, capability.into())
    }
}

impl From<ComponentInput> for Dict {
    fn from(e: ComponentInput) -> Self {
        e.0
    }
}

/// The capabilities a component has in its environment. Stored as a [Dict] containing a nested
/// [Dict] holding the environment's debug capabilities.
#[derive(Clone, Debug)]
pub struct ComponentEnvironment(Dict);

impl Default for ComponentEnvironment {
    fn default() -> Self {
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(DEBUG.clone(), Dict::new().into()).ok();
        drop(entries);
        Self(dict)
    }
}

impl StructuredDict for ComponentEnvironment {
    fn from_dict(dict: Dict) -> Self {
        Self(dict)
    }
}

impl ComponentEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capabilities listed in the `debug_capabilities` portion of its environment.
    pub fn debug(&self) -> Dict {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*DEBUG).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("debug entry must be a dict: {cap:?}");
        };
        dict.clone()
    }

    pub fn shallow_copy(&self) -> Self {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(DEBUG.clone(), self.debug().shallow_copy().into()).ok();
        drop(entries);
        Self(dict)
    }
}

impl From<ComponentEnvironment> for Dict {
    fn from(e: ComponentEnvironment) -> Self {
        e.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use sandbox::DictKey;

    impl StructuredDict for Dict {
        fn from_dict(dict: Dict) -> Self {
            dict
        }
    }

    #[fuchsia::test]
    async fn structured_dict_map() {
        let dict1 = Dict::new();
        {
            let mut entries = dict1.lock_entries();
            entries
                .insert("a".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
        }
        let dict2 = Dict::new();
        {
            let mut entries = dict2.lock_entries();
            entries
                .insert("b".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
        }
        let dict2_alt = Dict::new();
        {
            let mut entries = dict2_alt.lock_entries();
            entries
                .insert("c".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
        }
        let name1 = Name::new("1").unwrap();
        let name2 = Name::new("2").unwrap();

        let mut map: StructuredDictMap<Dict> = Default::default();
        assert_matches!(map.get(&name1), None);
        assert!(map.insert(name1.clone(), dict1).is_ok());
        let d = map.get(&name1).unwrap();
        {
            let entries = d.lock_entries();
            let key = DictKey::new("a").unwrap();
            assert!(entries.get(&key).is_some());
        }

        assert!(map.insert(name2.clone(), dict2).is_ok());
        let d = map.remove(&name2).unwrap();
        assert_matches!(map.remove(&name2), None);
        {
            let entries = d.lock_entries();
            let key = DictKey::new("b").unwrap();
            assert!(entries.get(&key).is_some());
        }

        assert!(map.insert(name2.clone(), dict2_alt).is_ok());
        let d = map.get(&name2).unwrap();
        {
            let entries = d.lock_entries();
            let key = DictKey::new("c").unwrap();
            assert!(entries.get(&key).is_some());
        }
    }
}
