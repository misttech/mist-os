// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::DictExt;
use cm_types::{BorrowedName, IterablePath, Name};
use fidl_fuchsia_component_sandbox as fsandbox;
use lazy_static::lazy_static;
use sandbox::{Capability, Dict};
use std::fmt;
use std::marker::PhantomData;

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
    pub fn insert(&self, key: Name, value: T) -> Result<(), fsandbox::CapabilityStoreError> {
        let dict: Dict = value.into();
        self.inner.insert(key, dict.into())
    }

    pub fn get(&self, key: &BorrowedName) -> Option<T> {
        self.inner.get(key).expect("structured map entry must be cloneable").map(|cap| {
            let Capability::Dictionary(dict) = cap else {
                unreachable!("structured map entry must be a dict: {cap:?}");
            };
            T::from_dict(dict)
        })
    }

    pub fn remove(&self, key: &Name) -> Option<T> {
        self.inner.remove(&*key).map(|cap| {
            let Capability::Dictionary(dict) = cap else {
                unreachable!("structured map entry must be a dict: {cap:?}");
            };
            T::from_dict(dict)
        })
    }

    pub fn append(&self, other: &Self) -> Result<(), ()> {
        self.inner.append(&other.inner)
    }

    pub fn enumerate(&self) -> impl Iterator<Item = (Name, T)> {
        self.inner.enumerate().map(|(key, capability_res)| match capability_res {
            Ok(Capability::Dictionary(dict)) => (key, T::from_dict(dict)),
            Ok(cap) => unreachable!("structured map entry must be a dict: {cap:?}"),
            Err(_) => panic!("structured map entry must be cloneable"),
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
    /// Dictionary of capabilities from or to the parent.
    static ref PARENT: Name = "parent".parse().unwrap();

    /// Dictionary of capabilities from a component's environment.
    static ref ENVIRONMENT: Name = "environment".parse().unwrap();

    /// Dictionary of debug capabilities in a component's environment.
    static ref DEBUG: Name = "debug".parse().unwrap();

    /// Dictionary of runner capabilities in a component's environment.
    static ref RUNNERS: Name = "runners".parse().unwrap();

    /// Dictionary of resolver capabilities in a component's environment.
    static ref RESOLVERS: Name = "resolvers".parse().unwrap();

    /// Dictionary of capabilities the component exposes to the framework.
    static ref FRAMEWORK: Name = "framework".parse().unwrap();
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
        if let Some(_) = environment.0.keys().next() {
            dict.insert(ENVIRONMENT.clone(), Dict::from(environment).into()).unwrap();
        }
        Self(dict)
    }

    /// Creates a new ComponentInput with entries cloned from this ComponentInput.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Result<Self, ()> {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dest = Dict::new();
        shallow_copy(&self.0, &dest, &*PARENT)?;
        shallow_copy(&self.0, &dest, &*ENVIRONMENT)?;
        Ok(Self(dest))
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's parent.
    pub fn capabilities(&self) -> Dict {
        get_or_insert(&self.0, &*PARENT)
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's environment.
    pub fn environment(&self) -> ComponentEnvironment {
        ComponentEnvironment(get_or_insert(&self.0, &*ENVIRONMENT))
    }

    pub fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::CapabilityStoreError> {
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
        Self(Dict::new())
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
        get_or_insert(&self.0, &*DEBUG)
    }

    /// Capabilities listed in the `runners` portion of its environment.
    pub fn runners(&self) -> Dict {
        get_or_insert(&self.0, &*RUNNERS)
    }

    /// Capabilities listed in the `resolvers` portion of its environment.
    pub fn resolvers(&self) -> Dict {
        get_or_insert(&self.0, &*RESOLVERS)
    }

    pub fn shallow_copy(&self) -> Result<Self, ()> {
        // Note: We call [Dict::shallow_copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::shallow_copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dest = Dict::new();
        shallow_copy(&self.0, &dest, &*DEBUG)?;
        shallow_copy(&self.0, &dest, &*RUNNERS)?;
        shallow_copy(&self.0, &dest, &*RESOLVERS)?;
        Ok(Self(dest))
    }
}

impl From<ComponentEnvironment> for Dict {
    fn from(e: ComponentEnvironment) -> Self {
        e.0
    }
}

/// Contains the capabilities a component makes available to its parent or the framework. Stored as
/// a [Dict] containing two nested [Dict]s for the capabilities made available to the parent and to
/// the framework.
#[derive(Clone, Debug)]
pub struct ComponentOutput(Dict);

impl Default for ComponentOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl StructuredDict for ComponentOutput {
    fn from_dict(dict: Dict) -> Self {
        Self(dict)
    }
}

impl ComponentOutput {
    pub fn new() -> Self {
        Self(Dict::new())
    }

    /// Creates a new ComponentOutput with entries cloned from this ComponentOutput.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Result<Self, ()> {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dest = Dict::new();
        shallow_copy(&self.0, &dest, &*PARENT)?;
        shallow_copy(&self.0, &dest, &*FRAMEWORK)?;
        Ok(Self(dest))
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's parent.
    /// framework. Lazily adds the dictionary if it does not exist yet.
    pub fn capabilities(&self) -> Dict {
        get_or_insert(&self.0, &*PARENT)
    }

    /// Returns the sub-dictionary containing capabilities exposed by the component to the
    /// framework. Lazily adds the dictionary if it does not exist yet.
    pub fn framework(&self) -> Dict {
        get_or_insert(&self.0, &*FRAMEWORK)
    }
}

impl From<ComponentOutput> for Dict {
    fn from(e: ComponentOutput) -> Self {
        e.0
    }
}

fn shallow_copy(src: &Dict, dest: &Dict, key: &Name) -> Result<(), ()> {
    if let Some(d) = src.get(key).expect("must be cloneable") {
        let Capability::Dictionary(d) = d else {
            unreachable!("{key} entry must be a dict: {d:?}");
        };
        dest.insert(key.clone(), d.shallow_copy()?.into()).ok();
    }
    Ok(())
}

fn get_or_insert(this: &Dict, key: &Name) -> Dict {
    let cap = this
        .get_or_insert(&key, || Capability::Dictionary(Dict::new()))
        .expect("capabilities must be cloneable");
    let Capability::Dictionary(dict) = cap else {
        unreachable!("{key} entry must be a dict: {cap:?}");
    };
    dict
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
        let dict1 = {
            let dict = Dict::new();
            dict.insert("a".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
            dict
        };
        let dict2 = {
            let dict = Dict::new();
            dict.insert("b".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
            dict
        };
        let dict2_alt = {
            let dict = Dict::new();
            dict.insert("c".parse().unwrap(), Dict::new().into())
                .expect("dict entry already exists");
            dict
        };
        let name1 = Name::new("1").unwrap();
        let name2 = Name::new("2").unwrap();

        let map: StructuredDictMap<Dict> = Default::default();
        assert_matches!(map.get(&name1), None);
        assert!(map.insert(name1.clone(), dict1).is_ok());
        let d = map.get(&name1).unwrap();
        let key = DictKey::new("a").unwrap();
        assert_matches!(d.get(&key), Ok(Some(_)));

        assert!(map.insert(name2.clone(), dict2).is_ok());
        let d = map.remove(&name2).unwrap();
        assert_matches!(map.remove(&name2), None);
        let key = DictKey::new("b").unwrap();
        assert_matches!(d.get(&key), Ok(Some(_)));

        assert!(map.insert(name2.clone(), dict2_alt).is_ok());
        let d = map.get(&name2).unwrap();
        let key = DictKey::new("c").unwrap();
        assert_matches!(d.get(&key), Ok(Some(_)));
    }
}
