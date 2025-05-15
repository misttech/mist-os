// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::child_name::ChildName;
use crate::error::MonikerError;
use cm_rust::{FidlIntoNative, NativeIntoFidl};
use core::cmp::{self, Ordering, PartialEq};
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

/// [Moniker] describes the identity of a component instance in terms of its path relative to the
/// root of the component instance tree.
///
/// Display notation: ".", "name1", "name1/name2", ...
#[derive(Eq, PartialEq, Clone, Hash, Default)]
pub struct Moniker {
    path: Arc<[ChildName]>,
}

impl Moniker {
    pub fn new(path: &[ChildName]) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &[ChildName] {
        &self.path
    }

    pub fn parse<T: AsRef<str>>(path: &[T]) -> Result<Self, MonikerError> {
        let path = path
            .iter()
            .map(ChildName::parse)
            .collect::<Result<Arc<[ChildName]>, MonikerError>>()?;
        Ok(Self { path })
    }

    pub fn parse_str(input: &str) -> Result<Self, MonikerError> {
        if input.is_empty() {
            return Err(MonikerError::invalid_moniker(input));
        }
        if input == "/" || input == "." || input == "./" {
            return Ok(Self::root());
        }

        // Optionally strip a prefix of "/" or "./".
        let stripped = match input.strip_prefix("/") {
            Some(s) => s,
            None => match input.strip_prefix("./") {
                Some(s) => s,
                None => input,
            },
        };
        let path = stripped
            .split('/')
            .map(ChildName::parse)
            .collect::<Result<Arc<[ChildName]>, MonikerError>>()?;
        Ok(Self { path })
    }

    /// Concatenates other onto the end of this moniker.
    pub fn concat(&self, other: &Moniker) -> Self {
        let path = self.path.iter().chain(other.path.iter()).cloned().collect::<Arc<[ChildName]>>();
        Self { path }
    }

    /// Indicates whether this moniker is prefixed by prefix.
    pub fn has_prefix(&self, prefix: &Moniker) -> bool {
        if self.path().len() < prefix.path().len() {
            return false;
        }

        self.path[..prefix.path.len()] == *prefix.path
    }

    pub fn root() -> Self {
        Self::new(&[])
    }

    /// Returns the last child of this moniker if this is not the root moniker.
    pub fn leaf(&self) -> Option<&ChildName> {
        self.path().last()
    }

    pub fn is_root(&self) -> bool {
        self.path().is_empty()
    }

    /// Creates a new moniker with the last child removed. Returns `None` if this is the root
    /// moniker.
    pub fn parent(&self) -> Option<Self> {
        self.path.split_last().map(|(_, parent)| Self::new(parent))
    }

    /// Creates a new Moniker with `child` added to the end of this moniker.
    pub fn child(&self, child: ChildName) -> Self {
        let path =
            self.path.iter().cloned().chain(std::iter::once(child)).collect::<Arc<[ChildName]>>();
        Self { path }
    }

    /// Splits off the last child of this moniker returning the parent as a moniker and the child.
    /// Returns `None` if this is the root moniker.
    pub fn split_leaf(&self) -> Option<(Self, ChildName)> {
        self.path.split_last().map(|(child, parent_path)| (Self::new(parent_path), child.clone()))
    }

    /// Strips the moniker parts in prefix from the beginning of this moniker.
    pub fn strip_prefix(&self, prefix: &Moniker) -> Result<Self, MonikerError> {
        if !self.has_prefix(prefix) {
            return Err(MonikerError::MonikerDoesNotHavePrefix {
                moniker: self.to_string(),
                prefix: prefix.to_string(),
            });
        }

        let prefix_len = prefix.path().len();
        Ok(Self::new(&self.path[prefix_len..]))
    }
}

impl FidlIntoNative<Moniker> for String {
    fn fidl_into_native(self) -> Moniker {
        // This is used in routing::capability_source::CapabilitySource, and the FIDL version of
        // this should only be generated in-process from already valid monikers.
        self.parse().unwrap()
    }
}

impl NativeIntoFidl<String> for Moniker {
    fn native_into_fidl(self) -> String {
        self.to_string()
    }
}

impl TryFrom<&[&str]> for Moniker {
    type Error = MonikerError;

    fn try_from(rep: &[&str]) -> Result<Self, MonikerError> {
        Self::parse(rep)
    }
}

impl<const N: usize> TryFrom<[&str; N]> for Moniker {
    type Error = MonikerError;

    fn try_from(rep: [&str; N]) -> Result<Self, MonikerError> {
        Self::parse(&rep)
    }
}

impl TryFrom<&str> for Moniker {
    type Error = MonikerError;

    fn try_from(input: &str) -> Result<Self, MonikerError> {
        Self::parse_str(input)
    }
}

impl std::str::FromStr for Moniker {
    type Err = MonikerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s)
    }
}

impl cmp::Ord for Moniker {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        let min_size = cmp::min(self.path().len(), other.path().len());
        for i in 0..min_size {
            if self.path()[i] < other.path()[i] {
                return cmp::Ordering::Less;
            } else if self.path()[i] > other.path()[i] {
                return cmp::Ordering::Greater;
            }
        }
        if self.path().len() > other.path().len() {
            return cmp::Ordering::Greater;
        } else if self.path().len() < other.path().len() {
            return cmp::Ordering::Less;
        }

        cmp::Ordering::Equal
    }
}

impl PartialOrd for Moniker {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Moniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path().is_empty() {
            write!(f, ".")?;
        } else {
            write!(f, "{}", self.path()[0])?;
            for segment in self.path()[1..].iter() {
                write!(f, "/{}", segment)?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Moniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_types::Name;

    #[test]
    fn monikers() {
        let root = Moniker::root();
        assert_eq!(true, root.is_root());
        assert_eq!(".", format!("{}", root));
        assert_eq!(root, Moniker::new(&[]));
        assert_eq!(root, Moniker::try_from([]).unwrap());

        let m = Moniker::new(&[
            ChildName::try_new("a", None).unwrap(),
            ChildName::try_new("b", Some("coll")).unwrap(),
        ]);
        assert_eq!(false, m.is_root());
        assert_eq!("a/coll:b", format!("{}", m));
        assert_eq!(m, Moniker::try_from(["a", "coll:b"]).unwrap());
        assert_eq!(m.leaf().map(|m| m.collection()).flatten(), Some(&Name::new("coll").unwrap()));
        assert_eq!(m.leaf().map(|m| m.name().as_str()), Some("b"));
        assert_eq!(m.leaf(), Some(&ChildName::try_from("coll:b").unwrap()));
    }

    #[test]
    fn moniker_parent() {
        let root = Moniker::root();
        assert_eq!(true, root.is_root());
        assert_eq!(None, root.parent());

        let m = Moniker::new(&[
            ChildName::try_new("a", None).unwrap(),
            ChildName::try_new("b", None).unwrap(),
        ]);
        assert_eq!("a/b", format!("{}", m));
        assert_eq!("a", format!("{}", m.parent().unwrap()));
        assert_eq!(".", format!("{}", m.parent().unwrap().parent().unwrap()));
        assert_eq!(None, m.parent().unwrap().parent().unwrap().parent());
        assert_eq!(m.leaf(), Some(&ChildName::try_from("b").unwrap()));
    }

    #[test]
    fn moniker_concat() {
        let scope_root: Moniker = ["a:test1", "b:test2"].try_into().unwrap();

        let relative: Moniker = ["c:test3", "d:test4"].try_into().unwrap();
        let descendant = scope_root.concat(&relative);
        assert_eq!("a:test1/b:test2/c:test3/d:test4", format!("{}", descendant));

        let relative: Moniker = [].try_into().unwrap();
        let descendant = scope_root.concat(&relative);
        assert_eq!("a:test1/b:test2", format!("{}", descendant));
    }

    #[test]
    fn moniker_parse_str() {
        assert_eq!(Moniker::try_from("/foo").unwrap(), Moniker::try_from(["foo"]).unwrap());
        assert_eq!(Moniker::try_from("./foo").unwrap(), Moniker::try_from(["foo"]).unwrap());
        assert_eq!(Moniker::try_from("foo").unwrap(), Moniker::try_from(["foo"]).unwrap());
        assert_eq!(Moniker::try_from("/").unwrap(), Moniker::try_from([]).unwrap());
        assert_eq!(Moniker::try_from("./").unwrap(), Moniker::try_from([]).unwrap());

        assert!(Moniker::try_from("//foo").is_err());
        assert!(Moniker::try_from(".//foo").is_err());
        assert!(Moniker::try_from("/./foo").is_err());
        assert!(Moniker::try_from("../foo").is_err());
        assert!(Moniker::try_from(".foo").is_err());
    }

    #[test]
    fn moniker_has_prefix() {
        assert!(Moniker::parse_str("a").unwrap().has_prefix(&Moniker::parse_str("a").unwrap()));
        assert!(Moniker::parse_str("a/b").unwrap().has_prefix(&Moniker::parse_str("a").unwrap()));
        assert!(Moniker::parse_str("a/b:test")
            .unwrap()
            .has_prefix(&Moniker::parse_str("a").unwrap()));
        assert!(Moniker::parse_str("a/b/c/d")
            .unwrap()
            .has_prefix(&Moniker::parse_str("a/b/c").unwrap()));
        assert!(!Moniker::parse_str("a/b")
            .unwrap()
            .has_prefix(&Moniker::parse_str("a/b/c").unwrap()));
        assert!(!Moniker::parse_str("a/c")
            .unwrap()
            .has_prefix(&Moniker::parse_str("a/b/c").unwrap()));
        assert!(!Moniker::root().has_prefix(&Moniker::parse_str("a").unwrap()));
        assert!(!Moniker::parse_str("a/b:test")
            .unwrap()
            .has_prefix(&Moniker::parse_str("a/b").unwrap()));
    }

    #[test]
    fn moniker_child() {
        assert_eq!(
            Moniker::root().child(ChildName::try_from("a").unwrap()),
            Moniker::parse_str("a").unwrap()
        );
        assert_eq!(
            Moniker::parse_str("a").unwrap().child(ChildName::try_from("b").unwrap()),
            Moniker::parse_str("a/b").unwrap()
        );
        assert_eq!(
            Moniker::parse_str("a:test").unwrap().child(ChildName::try_from("b").unwrap()),
            Moniker::parse_str("a:test/b").unwrap()
        );
        assert_eq!(
            Moniker::parse_str("a").unwrap().child(ChildName::try_from("b:test").unwrap()),
            Moniker::parse_str("a/b:test").unwrap()
        );
    }

    #[test]
    fn moniker_split_leaf() {
        assert_eq!(Moniker::root().split_leaf(), None);
        assert_eq!(
            Moniker::parse_str("a/b:test").unwrap().split_leaf(),
            Some((Moniker::parse_str("a").unwrap(), ChildName::try_from("b:test").unwrap()))
        );
    }

    #[test]
    fn moniker_strip_prefix() {
        assert_eq!(
            Moniker::parse_str("a").unwrap().strip_prefix(&Moniker::parse_str("a").unwrap()),
            Ok(Moniker::root())
        );
        assert_eq!(
            Moniker::parse_str("a/b").unwrap().strip_prefix(&Moniker::parse_str("a").unwrap()),
            Ok(Moniker::parse_str("b").unwrap())
        );
        assert!(Moniker::parse_str("a/b")
            .unwrap()
            .strip_prefix(&Moniker::parse_str("b").unwrap())
            .is_err());
        assert!(Moniker::root().strip_prefix(&Moniker::parse_str("b").unwrap()).is_err());
    }
}
