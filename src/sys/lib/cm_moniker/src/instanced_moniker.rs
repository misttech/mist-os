// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::instanced_child_name::InstancedChildName,
    core::cmp::{self, Ord, Ordering},
    moniker::{ChildName, ChildNameBase, Moniker, MonikerBase, MonikerError},
    std::{fmt, hash::Hash},
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// An instanced moniker describes the identity of a component instance in terms of its path
/// relative to the root of the component instance tree.
///
/// A root moniker is a moniker with an empty path.
///
/// Instanced monikers are only used internally within the component manager.  Externally,
/// components are referenced by encoded moniker so as to minimize the amount of
/// information which is disclosed about the overall structure of the component instance tree.
///
/// Display notation: ".", "name1:1", "name1:1/name2:2", ...
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Eq, PartialEq, Clone, Hash, Default)]
pub struct InstancedMoniker {
    path: Vec<InstancedChildName>,
}

impl InstancedMoniker {
    /// Convert an InstancedMoniker into an allocated Moniker without InstanceIds
    pub fn without_instance_ids(&self) -> Moniker {
        let path: Vec<ChildName> = self.path().iter().map(|p| p.without_instance_id()).collect();
        Moniker::new(path)
    }

    /// Transforms an `InstancedMoniker` into a representation where all dynamic children
    /// have `0` value instance ids.
    pub fn with_zero_value_instance_ids(&self) -> InstancedMoniker {
        let path = self
            .path()
            .iter()
            .map(|c| {
                InstancedChildName::try_new(c.name(), c.collection().map(|c| c.as_str()), 0)
                    .expect("down path moniker is guaranteed to be valid")
            })
            .collect();
        InstancedMoniker::new(path)
    }
}

impl MonikerBase for InstancedMoniker {
    type Part = InstancedChildName;

    fn new(path: Vec<Self::Part>) -> Self {
        Self { path }
    }

    fn path(&self) -> &Vec<Self::Part> {
        &self.path
    }

    fn path_mut(&mut self) -> &mut Vec<Self::Part> {
        &mut self.path
    }
}

impl TryFrom<&str> for InstancedMoniker {
    type Error = MonikerError;

    fn try_from(input: &str) -> Result<Self, MonikerError> {
        Self::parse_str(input)
    }
}

impl TryFrom<Vec<&str>> for InstancedMoniker {
    type Error = MonikerError;

    fn try_from(rep: Vec<&str>) -> Result<Self, MonikerError> {
        Self::parse(&rep)
    }
}

impl cmp::Ord for InstancedMoniker {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.compare(other)
    }
}

impl PartialOrd for InstancedMoniker {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for InstancedMoniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

impl fmt::Debug for InstancedMoniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}
#[cfg(test)]
mod tests {
    use {
        super::*,
        cm_types::Name,
        moniker::{ChildNameBase, MonikerBase, MonikerError},
    };

    #[test]
    fn instanced_monikers() {
        let root = InstancedMoniker::root();
        assert_eq!(true, root.is_root());
        assert_eq!(".", format!("{}", root));
        assert_eq!(root, InstancedMoniker::try_from(vec![]).unwrap());

        let m = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", Some("coll"), 2).unwrap(),
        ]);
        assert_eq!(false, m.is_root());
        assert_eq!("a:1/coll:b:2", format!("{}", m));
        assert_eq!(m, InstancedMoniker::try_from(vec!["a:1", "coll:b:2"]).unwrap());
        assert_eq!(m.leaf().map(|m| m.collection()).flatten(), Some(&Name::new("coll").unwrap()));
        assert_eq!(m.leaf().map(|m| m.name()), Some("b"));
        assert_eq!(m.leaf().map(|m| m.instance()), Some(2));
        assert_eq!(m.leaf(), Some(&InstancedChildName::try_from("coll:b:2").unwrap()));
    }

    #[test]
    fn instanced_moniker_parent() {
        let root = InstancedMoniker::root();
        assert_eq!(true, root.is_root());
        assert_eq!(None, root.parent());

        let m = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
        ]);
        assert_eq!("a:1/b:2", format!("{}", m));
        assert_eq!("a:1", format!("{}", m.parent().unwrap()));
        assert_eq!(".", format!("{}", m.parent().unwrap().parent().unwrap()));
        assert_eq!(None, m.parent().unwrap().parent().unwrap().parent());
        assert_eq!(m.leaf(), Some(&InstancedChildName::try_from("b:2").unwrap()));
    }

    #[test]
    fn instanced_moniker_compare() {
        let a = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("c", None, 3).unwrap(),
        ]);
        let a2 = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 3).unwrap(),
            InstancedChildName::try_new("c", None, 3).unwrap(),
        ]);
        let b = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("b", None, 3).unwrap(),
        ]);
        let c = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("c", None, 3).unwrap(),
            InstancedChildName::try_new("d", None, 4).unwrap(),
        ]);
        let d = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("c", None, 3).unwrap(),
        ]);

        assert_eq!(Ordering::Less, a.cmp(&a2));
        assert_eq!(Ordering::Greater, a2.cmp(&a));
        assert_eq!(Ordering::Greater, a.cmp(&b));
        assert_eq!(Ordering::Less, b.cmp(&a));
        assert_eq!(Ordering::Less, a.cmp(&c));
        assert_eq!(Ordering::Greater, c.cmp(&a));
        assert_eq!(Ordering::Equal, a.cmp(&d));
        assert_eq!(Ordering::Equal, d.cmp(&a));
        assert_eq!(Ordering::Less, b.cmp(&c));
        assert_eq!(Ordering::Greater, c.cmp(&b));
        assert_eq!(Ordering::Less, b.cmp(&d));
        assert_eq!(Ordering::Greater, d.cmp(&b));
        assert_eq!(Ordering::Greater, c.cmp(&d));
        assert_eq!(Ordering::Less, d.cmp(&c));
    }

    #[test]
    fn instanced_monikers_has_prefix() {
        let root = InstancedMoniker::root();
        let a = InstancedMoniker::new(vec![InstancedChildName::try_new("a", None, 1).unwrap()]);
        let ab = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
        ]);
        let abc = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("c", None, 3).unwrap(),
        ]);
        let abd = InstancedMoniker::new(vec![
            InstancedChildName::try_new("a", None, 1).unwrap(),
            InstancedChildName::try_new("b", None, 2).unwrap(),
            InstancedChildName::try_new("d", None, 3).unwrap(),
        ]);

        assert!(root.has_prefix(&root));
        assert!(a.has_prefix(&root));
        assert!(ab.has_prefix(&root));
        assert!(abc.has_prefix(&root));
        assert!(abd.has_prefix(&root));

        assert!(!root.has_prefix(&a));
        assert!(a.has_prefix(&a));
        assert!(ab.has_prefix(&a));
        assert!(abc.has_prefix(&a));
        assert!(abd.has_prefix(&a));

        assert!(!root.has_prefix(&ab));
        assert!(!a.has_prefix(&ab));
        assert!(ab.has_prefix(&ab));
        assert!(abc.has_prefix(&ab));
        assert!(abd.has_prefix(&ab));

        assert!(!root.has_prefix(&abc));
        assert!(abc.has_prefix(&abc));
        assert!(!a.has_prefix(&abc));
        assert!(!ab.has_prefix(&abc));
        assert!(!abd.has_prefix(&abc));

        assert!(!abd.has_prefix(&abc));
        assert!(abd.has_prefix(&abd));
        assert!(!a.has_prefix(&abd));
        assert!(!ab.has_prefix(&abd));
        assert!(!abc.has_prefix(&abd));
    }

    #[test]
    fn instanced_moniker_parse_str() -> Result<(), MonikerError> {
        let under_test = |s| InstancedMoniker::parse_str(s);

        assert_eq!(under_test(".")?, InstancedMoniker::root());

        let a = InstancedChildName::try_new("a", None, 0).unwrap();
        let bb = InstancedChildName::try_new("b", Some("b"), 0).unwrap();

        assert_eq!(under_test("a:0")?, InstancedMoniker::new(vec![a.clone()]));
        assert_eq!(under_test("a:0/b:b:0")?, InstancedMoniker::new(vec![a.clone(), bb.clone()]));
        assert_eq!(
            under_test("a:0/b:b:0/a:0/b:b:0")?,
            InstancedMoniker::new(vec![a.clone(), bb.clone(), a.clone(), bb.clone()])
        );

        assert!(under_test("").is_err(), "cannot be empty");
        assert!(under_test("a:0/").is_err(), "path segments cannot be empty");
        assert!(under_test("a:0//b:0").is_err(), "path segments cannot be empty");
        assert!(under_test("a:a").is_err(), "must contain instance id");

        Ok(())
    }
}
