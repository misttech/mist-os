// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        child_name::{ChildName, ChildNameBase},
        error::MonikerError,
    },
    core::cmp::{self, Ord, Ordering, PartialEq},
    std::{fmt, hash::Hash},
};

/// MonikerBase is the common trait for both InstancedMoniker
/// and Moniker concrete types.
///
/// MonikerBase describes the identity of a component instance in terms of its path
/// relative to the root of the component instance tree.
pub trait MonikerBase: Default + Eq + PartialEq + fmt::Debug + Clone + Hash + fmt::Display {
    type Part: ChildNameBase;

    fn new(path: Vec<Self::Part>) -> Self;

    fn parse<T: AsRef<str>>(path: &[T]) -> Result<Self, MonikerError> {
        let path: Result<Vec<Self::Part>, MonikerError> =
            path.iter().map(|x| Self::Part::parse(x)).collect();
        Ok(Self::new(path?))
    }

    fn parse_str(input: &str) -> Result<Self, MonikerError> {
        if input.is_empty() {
            return Err(MonikerError::invalid_moniker(input));
        }
        if input == "/" || input == "." || input == "./" {
            return Ok(Self::new(vec![]));
        }

        // Optionally strip a prefix of "/" or "./".
        let stripped = match input.strip_prefix("/") {
            Some(s) => s,
            None => match input.strip_prefix("./") {
                Some(s) => s,
                None => input,
            },
        };
        let path =
            stripped.split('/').map(Self::Part::parse).collect::<Result<_, MonikerError>>()?;
        Ok(Self::new(path))
    }

    /// Creates an absolute moniker for a descendant of this component instance.
    fn descendant<T: MonikerBase<Part = Self::Part>>(&self, descendant: &T) -> Self {
        let mut path = self.path().clone();
        let mut relative_path = descendant.path().clone();
        path.append(&mut relative_path);
        Self::new(path)
    }

    fn path(&self) -> &Vec<Self::Part>;

    fn path_mut(&mut self) -> &mut Vec<Self::Part>;

    /// Indicates whether `other` is contained within the realm specified by
    /// this MonikerBase.
    fn contains_in_realm<S: MonikerBase<Part = Self::Part>>(&self, other: &S) -> bool {
        if other.path().len() < self.path().len() {
            return false;
        }

        self.path().iter().enumerate().all(|item| *item.1 == other.path()[item.0])
    }

    fn root() -> Self {
        Self::new(vec![])
    }

    fn leaf(&self) -> Option<&Self::Part> {
        self.path().last()
    }

    fn is_root(&self) -> bool {
        self.path().is_empty()
    }

    fn parent(&self) -> Option<Self> {
        if self.is_root() {
            None
        } else {
            let l = self.path().len() - 1;
            Some(Self::new(self.path()[..l].to_vec()))
        }
    }

    fn child(&self, child: Self::Part) -> Self {
        let mut path = self.path().clone();
        path.push(child);
        Self::new(path)
    }

    fn scope_down<T: MonikerBase<Part = Self::Part>>(
        parent_scope: &T,
        child: &T,
    ) -> Result<Self, MonikerError> {
        if !parent_scope.contains_in_realm(child) {
            return Err(MonikerError::ParentDoesNotContainChild {
                parent: parent_scope.to_string(),
                child: child.to_string(),
            });
        }

        let parent_len = parent_scope.path().len();
        let mut children = child.path().clone();
        children.drain(0..parent_len);
        Ok(Self::new(children))
    }

    fn compare(&self, other: &Self) -> cmp::Ordering {
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

        return cmp::Ordering::Equal;
    }

    fn format(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

/// Moniker describes the identity of a component instance
/// in terms of its path relative to the root of the component instance
/// tree. The constituent parts of a Moniker do not include the
/// instance ID of the child.
///
/// Display notation: ".", "name1", "name1/name2", ...
#[derive(Eq, PartialEq, Clone, Hash, Default)]
pub struct Moniker {
    path: Vec<ChildName>,
}

impl MonikerBase for Moniker {
    type Part = ChildName;

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

impl TryFrom<Vec<&str>> for Moniker {
    type Error = MonikerError;

    fn try_from(rep: Vec<&str>) -> Result<Self, MonikerError> {
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
        self.compare(other)
    }
}

impl PartialOrd for Moniker {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Moniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

impl fmt::Debug for Moniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
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
        assert_eq!(root, Moniker::new(vec![]));
        assert_eq!(root, Moniker::try_from(vec![]).unwrap());

        let m = Moniker::new(vec![
            ChildName::try_new("a", None).unwrap(),
            ChildName::try_new("b", Some("coll")).unwrap(),
        ]);
        assert_eq!(false, m.is_root());
        assert_eq!("a/coll:b", format!("{}", m));
        assert_eq!(m, Moniker::try_from(vec!["a", "coll:b"]).unwrap());
        assert_eq!(m.leaf().map(|m| m.collection()).flatten(), Some(&Name::new("coll").unwrap()));
        assert_eq!(m.leaf().map(|m| m.name()), Some("b"));
        assert_eq!(m.leaf(), Some(&ChildName::try_from("coll:b").unwrap()));
    }

    #[test]
    fn moniker_parent() {
        let root = Moniker::root();
        assert_eq!(true, root.is_root());
        assert_eq!(None, root.parent());

        let m = Moniker::new(vec![
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
    fn moniker_descendant() {
        let scope_root: Moniker = vec!["a:test1", "b:test2"].try_into().unwrap();

        let relative: Moniker = vec!["c:test3", "d:test4"].try_into().unwrap();
        let descendant = scope_root.descendant(&relative);
        assert_eq!("a:test1/b:test2/c:test3/d:test4", format!("{}", descendant));

        let relative: Moniker = vec![].try_into().unwrap();
        let descendant = scope_root.descendant(&relative);
        assert_eq!("a:test1/b:test2", format!("{}", descendant));
    }

    #[test]
    fn moniker_parse_str() {
        assert_eq!(Moniker::try_from("/foo").unwrap(), Moniker::try_from(vec!["foo"]).unwrap());
        assert_eq!(Moniker::try_from("./foo").unwrap(), Moniker::try_from(vec!["foo"]).unwrap());
        assert_eq!(Moniker::try_from("foo").unwrap(), Moniker::try_from(vec!["foo"]).unwrap());
        assert_eq!(Moniker::try_from("/").unwrap(), Moniker::try_from(vec![]).unwrap());
        assert_eq!(Moniker::try_from("./").unwrap(), Moniker::try_from(vec![]).unwrap());

        assert!(Moniker::try_from("//foo").is_err());
        assert!(Moniker::try_from(".//foo").is_err());
        assert!(Moniker::try_from("/./foo").is_err());
        assert!(Moniker::try_from("../foo").is_err());
        assert!(Moniker::try_from(".foo").is_err());
    }
}
