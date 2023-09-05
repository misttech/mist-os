// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::stream::FuturesUnordered;
use futures::{
    channel::mpsc::{unbounded, UnboundedSender},
    future::BoxFuture,
    FutureExt, StreamExt,
};
use namespace::{Entry as NamespaceEntry, Path as NamespacePath, PathError};
use sandbox::{AnyCapability, Dict, Open, Remote};
use std::collections::HashMap;
use thiserror::Error;

/// A builder object for assembling a program's incoming namespace.
pub struct Namespace {
    /// Mapping from namespace path to capabilities that can be turned into `Open`.
    entries: HashMap<NamespacePath, AnyCapability>,

    /// Path-not-found errors are sent here.
    not_found: UnboundedSender<String>,

    futures: FuturesUnordered<BoxFuture<'static, ()>>,

    namespace_checker: NamespaceChecker,
}

#[derive(Error, Debug, Clone)]
pub enum NamespaceError {
    #[error("path is invalid for a namespace entry: `{0}`")]
    InvalidPath(#[from] PathError),

    #[error(
        "the parent of path `{0}` shares a prefix sequence of directories with another namespace entry. \
        This is not supported."
    )]
    Shadow(NamespacePath),

    #[error("cannot install two capabilities at the same namespace path `{0}`")]
    Duplicate(NamespacePath),

    #[error(
        "while installing capabilities within the namespace entry `{path}`, \
        failed to convert the namespace entry to Open"
    )]
    TryIntoOpenError { path: NamespacePath },
}

impl Namespace {
    pub fn new(not_found: UnboundedSender<String>) -> Self {
        return Namespace {
            entries: Default::default(),
            not_found,
            futures: FuturesUnordered::new(),
            namespace_checker: NamespaceChecker::new(),
        };
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at the parent directory of `path`.
    pub fn add_object(
        self: &mut Self,
        cap: AnyCapability,
        path: &NamespacePath,
    ) -> Result<(), NamespaceError> {
        let dirname = self.namespace_checker.add_parent(path)?;

        // If these is no such entry, make an empty dictionary.
        let dict: &mut AnyCapability = self.entries.entry(dirname).or_insert_with(|| {
            let (dict, fut) = make_dict_with_not_found_logging(
                path.dirname().clone().into(),
                self.not_found.clone(),
            );
            self.futures.push(fut);
            Box::new(dict)
        });

        // Cast the entry as a dictionary. This may fail if the user added a duplicate
        // namespace entry that is not a dictionary (see `add_entry`).
        let dict: &mut Dict =
            dict.try_into().map_err(|_e| NamespaceError::Duplicate(path.clone()))?;

        // TODO: we can replace this dance with `try_insert` when
        // https://github.com/rust-lang/rust/issues/82766 is stabilized.
        if dict.entries.contains_key(path.basename()) {
            return Err(NamespaceError::Duplicate(path.clone()));
        }
        if let Some(_) = dict.entries.insert(path.basename().to_owned(), cap) {
            unreachable!("should not find existing entry");
        }
        Ok(())
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at `path` directly. The capability will be exercised when the user
    /// opens the `path`.
    pub fn add_entry(
        self: &mut Self,
        cap: AnyCapability,
        path: &NamespacePath,
    ) -> Result<(), NamespaceError> {
        let path = self.namespace_checker.add(path)?;

        // Ensure that there is no such entry beforehand.
        if self.entries.contains_key(&path) {
            return Err(NamespaceError::Duplicate(path.clone()));
        }

        self.entries.insert(path, cap);
        Ok(())
    }

    pub fn serve(
        self: Self,
    ) -> Result<(Vec<NamespaceEntry>, BoxFuture<'static, ()>), NamespaceError> {
        let mut ns = vec![];
        let mut futures = self.futures;

        for (path, cap) in self.entries {
            let open: Open = cap
                .try_into()
                .map_err(|_| NamespaceError::TryIntoOpenError { path: path.clone() })?;
            let (client_end, fut) = Box::new(open).to_zx_handle();

            ns.push(NamespaceEntry { path, directory: client_end.into() });
            if let Some(fut) = fut {
                futures.push(fut);
            }
        }

        let fut = async move { while let Some(()) = futures.next().await {} };
        Ok((ns, fut.boxed()))
    }
}

/// Returns a disconnected sender which should ignore all the path-not-found errors.
pub fn ignore_not_found() -> UnboundedSender<String> {
    let (sender, _receiver) = unbounded();
    sender
}

fn make_dict_with_not_found_logging(
    root_path: String,
    not_found: UnboundedSender<String>,
) -> (Dict, BoxFuture<'static, ()>) {
    let (entry_not_found, mut entry_not_found_receiver) = unbounded();
    let new_dict = Dict::new_with_not_found(entry_not_found);
    let not_found = not_found.clone();
    // Grab a copy of the directory path, it will be needed if we log a
    // failed open request.
    let fut = async move {
        while let Some(path) = entry_not_found_receiver.next().await {
            let requested_path = format!("{}/{}", root_path, path);
            // Ignore the result of sending. The receiver is free to break away to ignore all the
            // not-found errors.
            let _ = not_found.unbounded_send(requested_path);
        }
    }
    .boxed();
    (new_dict, fut)
}

enum TreeNode {
    IntermediateDir(HashMap<String, TreeNode>),
    NamespaceNode,
}

/// [NamespaceChecker] checks for invalid namespace configurations.
///
/// The `cml` compiler and validators will prevent invalid configurations, but bedrock APIs may be
/// used from other contexts that do not give us these assurances. Hence we should always check if
/// the user specified a valid set of namespace entries.
struct NamespaceChecker {
    root: TreeNode,
}

#[derive(Error, Debug)]
#[error("ShadowError")]
struct ShadowError;

impl NamespaceChecker {
    fn new() -> Self {
        NamespaceChecker { root: TreeNode::IntermediateDir(HashMap::new()) }
    }

    fn add_parent(self: &mut Self, path: &NamespacePath) -> Result<NamespacePath, NamespaceError> {
        let mut names = path.split();
        // Get rid of the last path component to obtain only names corresponding to directory nodes.
        // Path has been validated to must not be empty.
        let _ = names.pop().unwrap();
        Self::add_path(&mut self.root, names.into_iter())
            .map_err(|_: ShadowError| NamespaceError::Shadow(path.clone()))?;
        let path = NamespacePath::new(path.dirname())?;
        Ok(path)
    }

    fn add(self: &mut Self, path: &NamespacePath) -> Result<NamespacePath, NamespaceError> {
        let names = path.split();
        Self::add_path(&mut self.root, names.into_iter())
            .map_err(|_: ShadowError| NamespaceError::Shadow(path.clone()))?;
        let path = NamespacePath::new(path.as_str())?;
        Ok(path)
    }

    fn add_path(
        node: &mut TreeNode,
        mut path: std::vec::IntoIter<String>,
    ) -> Result<(), ShadowError> {
        match path.next() {
            Some(name) => match node {
                TreeNode::NamespaceNode => Err(ShadowError),
                TreeNode::IntermediateDir(children) => {
                    let entry = children
                        .entry(name)
                        .or_insert_with(|| TreeNode::IntermediateDir(HashMap::new()));
                    Self::add_path(entry, path)
                }
            },
            None => {
                match node {
                    TreeNode::IntermediateDir(children) => {
                        if !children.is_empty() {
                            return Err(ShadowError);
                        }
                    }
                    TreeNode::NamespaceNode => {}
                }
                *node = TreeNode::NamespaceNode;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_util::multishot,
        anyhow::Result,
        assert_matches::assert_matches,
        cm_types::Path,
        fidl::{endpoints::Proxy, AsHandleRef, Peered},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon as zx,
        fuchsia_zircon::HandleBased,
        futures::StreamExt,
        std::str::FromStr,
    };

    fn ns_path(str: &str) -> NamespacePath {
        Path::from_str(str).unwrap().into()
    }

    fn parents_valid(paths: Vec<&str>) -> Result<(), NamespaceError> {
        let mut shadow = NamespaceChecker::new();
        for path in paths {
            shadow.add_parent(&ns_path(path))?;
        }
        Ok(())
    }

    #[fuchsia::test]
    fn test_shadow() {
        assert_matches!(parents_valid(vec!["/svc/foo/bar/Something", "/svc/Something"]), Err(_));
        assert_matches!(parents_valid(vec!["/svc/Something", "/svc/foo/bar/Something"]), Err(_));
        assert_matches!(parents_valid(vec!["/svc/Something", "/foo"]), Err(_));

        assert_matches!(parents_valid(vec!["/foo/bar/a", "/foo/bar/b", "/foo/bar/c"]), Ok(()));
        assert_matches!(parents_valid(vec!["/a", "/b", "/c"]), Ok(()));

        let mut shadow = NamespaceChecker::new();
        shadow.add_parent(&ns_path("/svc/foo")).unwrap();
        assert_matches!(shadow.add(&ns_path("/svc/foo/bar")), Err(_));

        let mut shadow = NamespaceChecker::new();
        shadow.add_parent(&ns_path("/svc/foo")).unwrap();
        assert_matches!(shadow.add(&ns_path("/svc2")), Ok(_));
    }

    fn open_cap() -> AnyCapability {
        let (open, _receiver) = multishot();
        Box::new(open)
    }

    #[fuchsia::test]
    fn test_duplicate_object() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_object(open_cap(), &ns_path("/svc/a")),
            Err(NamespaceError::Duplicate(path))
            if path.as_str() == "/svc/a"
        );
    }

    #[fuchsia::test]
    fn test_duplicate_entry() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(NamespaceError::Duplicate(path))
            if path.as_str() == "/svc/a"
        );
    }

    #[fuchsia::test]
    fn test_duplicate_object_and_entry() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(NamespaceError::Shadow(path))
            if path.as_str() == "/svc/a"
        );
    }

    /// If we added a namespaced object at "/foo/bar", thus creating a namespace entry at "/foo",
    /// we cannot add another entry directly at "/foo" again.
    #[fuchsia::test]
    fn test_duplicate_entry_at_object_parent() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/foo/bar")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/foo")),
            Err(NamespaceError::Duplicate(path))
            if path.as_str() == "/foo"
        );
    }

    /// If we directly added an entry at "/foo", it's not possible to add a namespaced object at
    /// "/foo/bar", as that would've required overwriting "/foo" with a namespace entry served by
    /// the framework.
    #[fuchsia::test]
    fn test_duplicate_object_parent_at_entry() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/foo")).expect("");
        assert_matches!(
            namespace.add_object(open_cap(), &ns_path("/foo/bar")),
            Err(NamespaceError::Duplicate(path))
            if path.as_str() == "/foo/bar"
        );
    }

    #[fuchsia::test]
    async fn test_empty() {
        let namespace = Namespace::new(ignore_not_found());
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);
        assert_eq!(ns.len(), 0);
        fut.await;
    }

    #[fuchsia::test]
    async fn test_one_sender_end_to_end() {
        let (open, receiver) = multishot();

        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(Box::new(open), &ns_path("/svc/a")).unwrap();
        let (mut ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.as_str(), "/svc");

        // Check that there is exactly one protocol inside the svc directory.
        let dir = ns.pop().unwrap().directory.into_proxy().unwrap();
        let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();
        assert_eq!(
            entries,
            vec![DirEntry { name: "a".to_string(), kind: fio::DirentType::Service }]
        );

        // Connect to the protocol using namespace functionality.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir.into_channel().unwrap().into_zx_channel(), "a", server_end)
            .unwrap();

        // Make sure the server_end is received, and test connectivity.
        let server_end: zx::Channel = receiver.0.recv().await.unwrap().into_handle().into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).unwrap();

        fut.await
    }

    #[fuchsia::test]
    async fn test_two_senders_in_same_namespace_entry() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).unwrap();
        namespace.add_object(open_cap(), &ns_path("/svc/b")).unwrap();
        let (mut ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.as_str(), "/svc");

        // Check that there are exactly two protocols inside the svc directory.
        let dir = ns.pop().unwrap().directory.into_proxy().unwrap();
        let mut entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();
        let mut expectation = vec![
            DirEntry { name: "a".to_string(), kind: fio::DirentType::Service },
            DirEntry { name: "b".to_string(), kind: fio::DirentType::Service },
        ];
        entries.sort();
        expectation.sort();
        assert_eq!(entries, expectation);

        drop(dir);
        fut.await
    }

    #[fuchsia::test]
    async fn test_two_senders_in_different_namespace_entries() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc1/a")).unwrap();
        namespace.add_object(open_cap(), &ns_path("/svc2/b")).unwrap();
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 2);
        let (mut svc1, ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.as_str() == "/svc1");
        let (mut svc2, _ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.as_str() == "/svc2");

        // Check that there are one protocol inside each directory.
        {
            let dir = svc1.pop().unwrap().directory.into_proxy().unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir).await.unwrap(),
                vec![DirEntry { name: "a".to_string(), kind: fio::DirentType::Service },]
            );
        }
        {
            let dir = svc2.pop().unwrap().directory.into_proxy().unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir).await.unwrap(),
                vec![DirEntry { name: "b".to_string(), kind: fio::DirentType::Service },]
            );
        }

        drop(svc1);
        drop(svc2);
        fut.await
    }

    #[fuchsia::test]
    async fn test_not_found() {
        let (not_found_sender, mut not_found_receiver) = unbounded();
        let mut namespace = Namespace::new(not_found_sender);
        namespace.add_object(open_cap(), &ns_path("/svc/a")).unwrap();
        let (mut ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.as_str(), "/svc");

        let dir = ns.pop().unwrap().directory.into_proxy().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let _ = fdio::service_connect_at(
            &dir.into_channel().unwrap().into_zx_channel(),
            "non_existent",
            server_end,
        );

        // Server endpoint is closed because the path does not exist.
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();

        // We should get a notification about this path.
        assert_eq!(not_found_receiver.next().await, Some("/svc/non_existent".to_string()));

        drop(ns);
        fut.await
    }
}
