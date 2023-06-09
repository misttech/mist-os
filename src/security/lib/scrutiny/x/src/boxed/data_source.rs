// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use dyn_clone::clone_trait_object;
use dyn_clone::DynClone;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::rc::Weak;

/// Internal subset of [`api::DataSource`] API that is independent of data source trees. Most
/// implementations only need to implement this trait, and clients that instantiate this trait
/// typically wrap it in a [`DataSource`] struct that forms a node in a data source tree.
pub(crate) trait DataSourceInfo: Debug + DynClone {
    /// The kind of artifact that this data source represents.
    fn kind(&self) -> api::DataSourceKind;

    /// The local path to this data source. Generally only applicable to data sources that have no
    /// parent.
    fn path(&self) -> Option<Box<dyn api::Path>>;

    /// The version of the underlying format of the data source.
    fn version(&self) -> api::DataSourceVersion;
}

clone_trait_object!(DataSourceInfo);

#[derive(Clone, Debug)]
struct DataSourceNode {
    parent: Option<Parent>,
    children: Vec<DataSource>,
    data_source_info: Box<dyn DataSourceInfo>,
}

/// Weak reference to a [`DataSource`] node, conventionally used to refer to a data source's parent
/// in a data source tree.
#[derive(Clone, Debug)]
pub(crate) struct Parent(Weak<RefCell<DataSourceNode>>);

impl Parent {
    fn upgrade(&self) -> Option<DataSource> {
        self.0.upgrade().map(DataSource::from)
    }
}

impl From<Weak<RefCell<DataSourceNode>>> for Parent {
    fn from(value: Weak<RefCell<DataSourceNode>>) -> Self {
        Self(value)
    }
}

/// Canonical instantiation of a data source situated in a tree of data sources. Most data source
/// implementations only implement [`DataSourceInfo`], and clients that instantiate them typically
/// wrap them in a [`DataSource`] struct that forms a node in a data source tree.
#[derive(Clone, Debug)]
pub(crate) struct DataSource(Rc<RefCell<DataSourceNode>>);

impl DataSource {
    /// Constructs the canonical instantiation of `data_source_info`, initially situated in a data
    /// source tree below `parent`, with `children`.
    pub fn new(data_source_info: Box<dyn DataSourceInfo>) -> Self {
        Self(Rc::new(RefCell::new(DataSourceNode {
            parent: None,
            children: vec![],
            data_source_info,
        })))
    }

    /// Empties the `children` collection for this data source.
    ///
    /// # Panics
    ///
    /// This function may panic if multiple `&mut self` operations are performed concurrently.
    #[allow(dead_code)]
    pub fn clear_children(&mut self) {
        let mut node = self.0.borrow_mut();

        let mut children = vec![];
        std::mem::swap(&mut children, &mut node.children);
        for child in children.iter_mut() {
            child.set_parent(None);
        }
    }

    /// Appends a child to the `children` collection for this data source.
    ///
    /// # Panics
    ///
    /// This function may panic if multiple `&mut self` operations are performed concurrently.
    pub fn add_child(&mut self, mut child: DataSource) {
        {
            let mut node = self.0.borrow_mut();
            node.children.push(child.clone());
        }
        child.set_parent(Some(self.downgrade()))
    }

    /// Construct a [`Parent`] (weak) reference to this data source.
    fn downgrade(&self) -> Parent {
        Rc::downgrade(&self.0).into()
    }

    /// Overwrites this data source's parent with `parent`.
    ///
    /// # Panics
    ///
    /// This function may panic if multiple `&mut self` operations are performed concurrently.
    fn set_parent(&mut self, parent: Option<Parent>) {
        let mut node = self.0.borrow_mut();
        node.parent = parent;
    }
}

impl From<Rc<RefCell<DataSourceNode>>> for DataSource {
    fn from(value: Rc<RefCell<DataSourceNode>>) -> Self {
        Self(value)
    }
}

impl api::DataSource for DataSource {
    fn kind(&self) -> api::DataSourceKind {
        self.0.borrow().data_source_info.kind()
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource>> {
        self.0.borrow().parent.as_ref().and_then(Parent::upgrade).map(|data_source| {
            let data_source: Box<dyn api::DataSource> = Box::new(data_source);
            data_source
        })
    }

    fn children(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new(self.0.borrow().children.clone().into_iter().map(|child| {
            let child: Box<dyn api::DataSource> = Box::new(child);
            child
        }))
    }

    fn path(&self) -> Option<Box<dyn api::Path>> {
        self.0.borrow().data_source_info.path()
    }

    fn version(&self) -> api::DataSourceVersion {
        self.0.borrow().data_source_info.version()
    }
}

#[cfg(test)]
pub mod test {
    use super::super::api;
    use super::super::data_source as ds;

    #[derive(Clone, Debug)]
    pub struct DataSourceInfo {
        kind: api::DataSourceKind,
        path: Option<Box<dyn api::Path>>,
        version: api::DataSourceVersion,
    }

    impl DataSourceInfo {
        pub fn new(
            kind: api::DataSourceKind,
            path: Option<Box<dyn api::Path>>,
            version: api::DataSourceVersion,
        ) -> Self {
            Self { kind, path, version }
        }
    }

    impl ds::DataSourceInfo for DataSourceInfo {
        fn kind(&self) -> api::DataSourceKind {
            self.kind.clone()
        }

        fn path(&self) -> Option<Box<dyn api::Path>> {
            self.path.clone()
        }

        fn version(&self) -> api::DataSourceVersion {
            self.version.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::api::DataSource as _;
    use super::test;
    use super::DataSource;

    #[fuchsia::test]
    fn test_add_child() {
        let mut parent = DataSource::new(Box::new(test::DataSourceInfo::new(
            api::DataSourceKind::TufRepository,
            Some(Box::new("test/tuf/repo")),
            api::DataSourceVersion::Unknown,
        )));
        let child = DataSource::new(Box::new(test::DataSourceInfo::new(
            api::DataSourceKind::BlobDirectory,
            Some(Box::new("test/tuf/repo/blobs")),
            api::DataSourceVersion::Unknown,
        )));

        // Even though a clone of `child` is passed in, `child` wraps a `Rc<RefCell<...>>`, so the
        // `add_child()` implementation is able to update both the `parent` children and the `child`
        // parent references.
        parent.add_child(child.clone());

        let parent_ref: &dyn api::DataSource = &parent;
        let child_ref: &dyn api::DataSource = &child;
        let children = parent.children().collect::<Vec<_>>();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].as_ref(), child_ref);
        assert_eq!(child.parent().expect("parent").as_ref(), parent_ref);
    }

    #[fuchsia::test]
    fn test_clear_children() {
        let mut parent = DataSource::new(Box::new(test::DataSourceInfo::new(
            api::DataSourceKind::TufRepository,
            Some(Box::new("test/tuf/repo")),
            api::DataSourceVersion::Unknown,
        )));
        let child = DataSource::new(Box::new(test::DataSourceInfo::new(
            api::DataSourceKind::BlobDirectory,
            Some(Box::new("test/tuf/repo/blobs")),
            api::DataSourceVersion::Unknown,
        )));

        parent.add_child(child.clone());
        parent.clear_children();

        assert_eq!(parent.children().next(), None);
        assert_eq!(child.parent(), None);
    }
}
