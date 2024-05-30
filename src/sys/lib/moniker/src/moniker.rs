// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{child_name::ChildName, error::MonikerError},
    core::cmp::{self, Ordering, PartialEq},
    std::{fmt, hash::Hash},
};

/// [Moniker] describes the identity of a component instance in terms of its path relative to the
/// root of the component instance tree.
///
/// Display notation: ".", "name1", "name1/name2", ...
#[derive(Eq, PartialEq, Clone, Hash, Default)]
pub struct Moniker {
    path: Vec<ChildName>,
}

impl Moniker {
    pub fn new(path: Vec<ChildName>) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Vec<ChildName> {
        &self.path
    }

    pub fn path_mut(&mut self) -> &mut Vec<ChildName> {
        &mut self.path
    }

    pub fn parse<T: AsRef<str>>(path: &[T]) -> Result<Self, MonikerError> {
        let path: Result<Vec<ChildName>, MonikerError> =
            path.iter().map(ChildName::parse).collect();
        Ok(Self::new(path?))
    }

    pub fn parse_str(input: &str) -> Result<Self, MonikerError> {
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
            stripped.split('/').map(ChildName::parse).collect::<Result<_, MonikerError>>()?;
        Ok(Self::new(path))
    }

    /// Concatenates other onto the end of this moniker.
    pub fn concat(&self, other: &Moniker) -> Self {
        let mut path = self.path().clone();
        let mut other_path = other.path().clone();
        path.append(&mut other_path);
        Self::new(path)
    }

    /// Indicates whether this moniker is prefixed by prefix.
    pub fn has_prefix(&self, prefix: &Moniker) -> bool {
        if self.path().len() < prefix.path().len() {
            return false;
        }

        prefix.path().iter().enumerate().all(|item| *item.1 == self.path()[item.0])
    }

    pub fn root() -> Self {
        Self::new(vec![])
    }

    pub fn leaf(&self) -> Option<&ChildName> {
        self.path().last()
    }

    pub fn is_root(&self) -> bool {
        self.path().is_empty()
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            None
        } else {
            let l = self.path().len() - 1;
            Some(Self::new(self.path()[..l].to_vec()))
        }
    }

    pub fn child(&self, child: ChildName) -> Self {
        let mut path = self.path().clone();
        path.push(child);
        Self::new(path)
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
        let mut path = self.path().clone();
        path.drain(0..prefix_len);
        Ok(Self::new(path))
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
        assert_eq!(m.leaf().map(|m| m.name().as_str()), Some("b"));
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
    fn moniker_concat() {
        let scope_root: Moniker = vec!["a:test1", "b:test2"].try_into().unwrap();

        let relative: Moniker = vec!["c:test3", "d:test4"].try_into().unwrap();
        let descendant = scope_root.concat(&relative);
        assert_eq!("a:test1/b:test2/c:test3/d:test4", format!("{}", descendant));

        let relative: Moniker = vec![].try_into().unwrap();
        let descendant = scope_root.concat(&relative);
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
