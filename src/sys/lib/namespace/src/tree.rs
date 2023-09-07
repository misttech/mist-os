// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{path::Path, NamespaceError};
use std::collections::HashMap;

/// A tree representation of a namespace.
///
/// Each leaf node may hold an arbitrary payload `T`. Usually this is a directory client endpoint
/// or client proxy.
#[derive(Debug)]
pub struct Tree<T> {
    root: Node<T>,
}

impl<T> Tree<T> {
    pub fn new() -> Self {
        Tree { root: Node::Internal(HashMap::new()) }
    }

    pub fn add(self: &mut Self, path: &Path, thing: T) -> Result<&mut T, NamespaceError> {
        let names = path.split();
        self.root.add(names.into_iter(), thing).map_err(|e| match e {
            AddError::Shadow => NamespaceError::Shadow(path.clone()),
            AddError::Duplicate => NamespaceError::Duplicate(path.clone()),
        })
    }

    pub fn get_mut(&mut self, path: &Path) -> Option<&mut T> {
        let names = path.split();
        self.root.get_mut(names.into_iter())
    }

    pub fn remove(&mut self, path: &Path) -> Option<T> {
        let names = path.split();
        self.root.remove(names.into_iter().peekable())
    }

    pub fn flatten(self) -> Vec<(Path, T)> {
        self.root
            .flatten()
            .into_iter()
            .map(|(path, thing)| (Path::new(path).unwrap(), thing))
            .collect()
    }

    pub fn map_ref<R>(&self, mut f: impl FnMut(&T) -> R) -> Tree<R> {
        Tree { root: self.root.map_ref(&mut f) }
    }
}

impl<T> Default for Tree<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Clone for Tree<T> {
    fn clone(&self) -> Self {
        Self { root: self.root.map_ref(&mut <T as Clone>::clone) }
    }
}

#[derive(Debug)]
enum Node<T> {
    Internal(HashMap<String, Node<T>>),
    Leaf(T),
}

enum AddError {
    Shadow,
    Duplicate,
}

impl<T> Node<T> {
    fn add(&mut self, mut path: std::vec::IntoIter<String>, thing: T) -> Result<&mut T, AddError> {
        match path.next() {
            Some(name) => match self {
                Node::Leaf(_) => Err(AddError::Shadow),
                Node::Internal(children) => {
                    let entry =
                        children.entry(name).or_insert_with(|| Node::Internal(HashMap::new()));
                    entry.add(path, thing)
                }
            },
            None => {
                match self {
                    Node::Internal(children) => {
                        if !children.is_empty() {
                            return Err(AddError::Shadow);
                        }
                    }
                    Node::Leaf(_) => {
                        return Err(AddError::Duplicate);
                    }
                }
                *self = Node::Leaf(thing);
                match self {
                    Node::Internal(_) => unreachable!(),
                    Node::Leaf(ref mut t) => Ok(t),
                }
            }
        }
    }

    fn get_mut(&mut self, mut path: std::vec::IntoIter<String>) -> Option<&mut T> {
        match path.next() {
            Some(name) => match self {
                Node::Leaf(_) => None,
                Node::Internal(children) => match children.get_mut(&name) {
                    Some(node) => node.get_mut(path),
                    None => None,
                },
            },
            None => match self {
                Node::Internal(_) => None,
                Node::Leaf(ref mut n) => Some(n),
            },
        }
    }

    fn remove(&mut self, mut path: std::iter::Peekable<std::vec::IntoIter<String>>) -> Option<T> {
        match path.next() {
            Some(name) => match self {
                Node::Leaf(_) => None,
                Node::Internal(children) => {
                    if path.peek().is_none() {
                        match children.remove(&name) {
                            Some(Node::Leaf(n)) => Some(n),
                            Some(Node::Internal(c)) => {
                                children.insert(name, Node::Internal(c));
                                return None;
                            }
                            None => None,
                        }
                    } else {
                        match children.get_mut(&name) {
                            Some(node) => node.remove(path),
                            None => None,
                        }
                    }
                }
            },
            None => None,
        }
    }

    pub fn flatten(self) -> Vec<(String, T)> {
        fn recurse<T>(path: &str, node: Node<T>, result: &mut Vec<(String, T)>) {
            match node {
                Node::Internal(map) => {
                    for (key, value) in map {
                        let path = format!("{}/{}", path, key);
                        recurse(&path, value, result);
                    }
                }
                Node::Leaf(leaf) => {
                    // The single empty slash is a special case.
                    // When there are intermediate nodes, we can append "/{path}" to the previous
                    // path segment. But if the root is a leaf node, there will be no slash unless
                    // we add one here.
                    let path = if path.is_empty() { "/" } else { path };
                    result.push((path.to_string(), leaf));
                }
            }
        }

        let mut result = Vec::new();
        recurse("", self, &mut result);
        result
    }

    pub fn map_ref<R>(&self, f: &mut impl FnMut(&T) -> R) -> Node<R> {
        match self {
            Node::Internal(map) => Node::Internal(HashMap::from_iter(
                map.iter().map(|(k, v)| (k.clone(), v.map_ref(f))),
            )),
            Node::Leaf(t) => Node::Leaf(f(t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::ns_path;
    use assert_matches::assert_matches;

    #[test]
    fn test_add() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/foo"), 1).unwrap();
        tree.add(&ns_path("/bar"), 2).unwrap();
        tree.add(&ns_path("/x/y/z"), 3).unwrap();
        tree.add(&ns_path("/x/x/z"), 4).unwrap();
        tree.add(&ns_path("/x/y/w"), 5).unwrap();
        assert_matches!(tree.add(&ns_path("/foo"), 6), Err(NamespaceError::Duplicate(_)));
        assert_matches!(tree.add(&ns_path("/bar"), 7), Err(NamespaceError::Duplicate(_)));

        tree.add(&ns_path("/a/b/c"), 0).unwrap();
        assert_matches!(tree.add(&ns_path("/"), 0), Err(NamespaceError::Shadow(_)));
        assert_matches!(tree.add(&ns_path("/a"), 0), Err(NamespaceError::Shadow(_)));
        assert_matches!(tree.add(&ns_path("/a/b"), 0), Err(NamespaceError::Shadow(_)));
        assert_matches!(tree.add(&ns_path("/a/b/c"), 0), Err(NamespaceError::Duplicate(_)));
        assert_matches!(tree.add(&ns_path("/a/b/c/d"), 0), Err(NamespaceError::Shadow(_)));
        assert_matches!(tree.add(&ns_path("/a/b/c/d/e"), 0), Err(NamespaceError::Shadow(_)));
    }

    #[test]
    fn test_root() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/"), 1).unwrap();
        assert_matches!(
            tree.add(&ns_path("/"), 2),
            Err(NamespaceError::Duplicate(path)) if path.as_str() == "/"
        );
        assert_matches!(
            tree.add(&ns_path("/a"), 3),
            Err(NamespaceError::Shadow(path)) if path.as_str() == "/a"
        );
    }

    #[test]
    fn test_get_mut() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/foo"), 1).unwrap();
        assert_eq!(*tree.get_mut(&ns_path("/foo")).unwrap(), 1);
        *tree.get_mut(&ns_path("/foo")).unwrap() = 2;
        assert_eq!(*tree.get_mut(&ns_path("/foo")).unwrap(), 2);
        assert_matches!(tree.get_mut(&ns_path("/bar")), None);

        tree.add(&ns_path("/a/b"), 1).unwrap();
        assert_matches!(tree.get_mut(&ns_path("/a")), None);
        assert_matches!(tree.get_mut(&ns_path("/a/b/c")), None);
        assert_eq!(*tree.get_mut(&ns_path("/a/b")).unwrap(), 1);
        *tree.get_mut(&ns_path("/a/b")).unwrap() = 2;
        assert_eq!(*tree.get_mut(&ns_path("/a/b")).unwrap(), 2);
    }

    #[test]
    fn test_remove() {
        let mut tree = Tree::new();
        assert_matches!(tree.remove(&ns_path("/foo")), None);
        tree.add(&ns_path("/foo"), 1).unwrap();
        assert_matches!(tree.remove(&ns_path("/foo")), Some(1));
        assert_matches!(tree.remove(&ns_path("/foo")), None);

        tree.add(&ns_path("/foo/bar"), 1).unwrap();
        assert_matches!(tree.remove(&ns_path("/foo")), None);
        assert_matches!(tree.remove(&ns_path("/foo/bar/baz")), None);
        assert_matches!(tree.remove(&ns_path("/foo/bar")), Some(1));
        assert_matches!(tree.remove(&ns_path("/foo/bar")), None);
    }

    #[test]
    fn test_flatten() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/a/b/c"), 1).unwrap();
        tree.add(&ns_path("/b/c/d/e"), 2).unwrap();
        tree.add(&ns_path("/b/c/e/f"), 3).unwrap();
        let mut entries = tree.flatten();
        entries.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0.as_str(), "/a/b/c");
        assert_eq!(entries[0].1, 1);
        assert_eq!(entries[1].0.as_str(), "/b/c/d/e");
        assert_eq!(entries[1].1, 2);
        assert_eq!(entries[2].0.as_str(), "/b/c/e/f");
        assert_eq!(entries[2].1, 3);
    }

    #[test]
    fn test_flatten_root() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/"), 1).unwrap();
        let entries = tree.flatten();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.as_str(), "/");
        assert_eq!(entries[0].1, 1);
    }

    #[test]
    fn test_clone() {
        let mut tree = Tree::new();
        tree.add(&ns_path("/foo"), 1).unwrap();
        tree.add(&ns_path("/bar/baz"), 2).unwrap();

        let tree_clone = tree.clone();

        let mut entries = tree.flatten();
        entries.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut entries_clone = tree_clone.flatten();
        entries_clone.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        assert_eq!(entries, entries_clone);
    }
}
