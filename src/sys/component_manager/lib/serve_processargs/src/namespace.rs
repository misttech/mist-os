// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::stream::FuturesUnordered;
use futures::{
    channel::mpsc::{unbounded, UnboundedSender},
    future::BoxFuture,
    FutureExt, StreamExt,
};
use namespace::{Entry as NamespaceEntry, Namespace, NamespaceError, Path as NamespacePath, Tree};
use sandbox::{AnyCapability, Dict, Directory, Remote};
use thiserror::Error;

/// A builder object for assembling a program's incoming namespace.
pub struct NamespaceBuilder {
    /// Mapping from namespace path to capabilities that can be turned into `Directory`.
    entries: Tree<AnyCapability>,

    /// Path-not-found errors are sent here.
    not_found: UnboundedSender<String>,

    futures: FuturesUnordered<BoxFuture<'static, ()>>,
}

#[derive(Error, Debug, Clone)]
pub enum BuildNamespaceError {
    #[error("namespace configuration error: {0}")]
    NamespaceError(#[from] NamespaceError),

    #[error(
        "while installing capabilities within the namespace entry `{path}`, \
        failed to convert the namespace entry to Directory"
    )]
    Conversion { path: NamespacePath },
}

impl NamespaceBuilder {
    pub fn new(not_found: UnboundedSender<String>) -> Self {
        return NamespaceBuilder {
            entries: Default::default(),
            not_found,
            futures: FuturesUnordered::new(),
        };
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at the parent directory of `path`.
    pub fn add_object(
        self: &mut Self,
        cap: AnyCapability,
        path: &NamespacePath,
    ) -> Result<(), BuildNamespaceError> {
        let dirname = path.dirname_ns_path();

        // If these is no such entry, make an empty dictionary.
        let dict: &mut AnyCapability = match self.entries.get_mut(&dirname) {
            None => {
                let (dict, fut) = make_dict_with_not_found_logging(
                    path.dirname().clone().into(),
                    self.not_found.clone(),
                );
                self.futures.push(fut);
                self.entries.add(&dirname, Box::new(dict))?
            }
            Some(dir) => dir,
        };

        // Cast the entry as a dictionary. This may fail if the user added a duplicate
        // namespace entry that is not a dictionary (see `add_entry`).
        let dict: &mut Dict =
            dict.try_into().map_err(|_e| NamespaceError::Duplicate(path.clone()))?;

        if dict.entries.contains_key(path.basename()) {
            return Err(NamespaceError::Duplicate(path.clone()).into());
        }
        dict.entries.insert(path.basename().to_owned(), cap);
        Ok(())
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at `path` directly. The capability will be exercised when the user
    /// opens the `path`.
    pub fn add_entry(
        self: &mut Self,
        cap: AnyCapability,
        path: &NamespacePath,
    ) -> Result<(), BuildNamespaceError> {
        self.entries.add(path, cap)?;
        Ok(())
    }

    pub fn serve(self: Self) -> Result<(Namespace, BoxFuture<'static, ()>), BuildNamespaceError> {
        let mut futures = self.futures;

        let ns = self
            .entries
            .flatten()
            .into_iter()
            .map(|(path, cap)| -> Result<NamespaceEntry, BuildNamespaceError> {
                let directory: Directory = cap
                    .try_into()
                    .map_err(|()| BuildNamespaceError::Conversion { path: path.clone() })?;
                let (client_end, fut) = Box::new(directory).to_zx_handle();
                if let Some(fut) = fut {
                    futures.push(fut);
                }
                Ok(NamespaceEntry { path, directory: client_end.into() })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let fut = async move { while let Some(()) = futures.next().await {} };
        Ok((ns.try_into()?, fut.boxed()))
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

    fn open_cap() -> AnyCapability {
        let (open, _receiver) = multishot();
        Box::new(open)
    }

    fn ns_path(str: &str) -> NamespacePath {
        Path::from_str(str).unwrap().into()
    }

    fn parents_valid(paths: Vec<&str>) -> Result<(), BuildNamespaceError> {
        let mut shadow = NamespaceBuilder::new(ignore_not_found());
        for path in paths {
            shadow.add_object(open_cap(), &ns_path(path))?;
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

        let mut shadow = NamespaceBuilder::new(ignore_not_found());
        shadow.add_object(open_cap(), &ns_path("/svc/foo")).unwrap();
        assert_matches!(shadow.add_object(open_cap(), &ns_path("/svc/foo/bar")), Err(_));

        let mut shadow = NamespaceBuilder::new(ignore_not_found());
        shadow.add_object(open_cap(), &ns_path("/svc/foo")).unwrap();
        assert_matches!(shadow.add_entry(open_cap(), &ns_path("/svc2")), Ok(_));
    }

    #[fuchsia::test]
    fn test_duplicate_object() {
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_object(open_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.as_str() == "/svc/a"
        );
    }

    #[fuchsia::test]
    fn test_duplicate_entry() {
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.as_str() == "/svc/a"
        );
    }

    #[fuchsia::test]
    fn test_duplicate_object_and_entry() {
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Shadow(path)))
            if path.as_str() == "/svc/a"
        );
    }

    /// If we added a namespaced object at "/foo/bar", thus creating a namespace entry at "/foo",
    /// we cannot add another entry directly at "/foo" again.
    #[fuchsia::test]
    fn test_duplicate_entry_at_object_parent() {
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/foo/bar")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/foo")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.as_str() == "/foo"
        );
    }

    /// If we directly added an entry at "/foo", it's not possible to add a namespaced object at
    /// "/foo/bar", as that would've required overwriting "/foo" with a namespace entry served by
    /// the framework.
    #[fuchsia::test]
    fn test_duplicate_object_parent_at_entry() {
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/foo")).expect("");
        assert_matches!(
            namespace.add_object(open_cap(), &ns_path("/foo/bar")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.as_str() == "/foo/bar"
        );
    }

    #[fuchsia::test]
    async fn test_empty() {
        let namespace = NamespaceBuilder::new(ignore_not_found());
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);
        assert_eq!(ns.flatten().len(), 0);
        fut.await;
    }

    #[fuchsia::test]
    async fn test_one_sender_end_to_end() {
        let (open, receiver) = multishot();

        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(Box::new(open), &ns_path("/svc/a")).unwrap();
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        let mut ns = ns.flatten();
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
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc/a")).unwrap();
        namespace.add_object(open_cap(), &ns_path("/svc/b")).unwrap();
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        let mut ns = ns.flatten();
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
        let mut namespace = NamespaceBuilder::new(ignore_not_found());
        namespace.add_object(open_cap(), &ns_path("/svc1/a")).unwrap();
        namespace.add_object(open_cap(), &ns_path("/svc2/b")).unwrap();
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        let ns = ns.flatten();
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
        let mut namespace = NamespaceBuilder::new(not_found_sender);
        namespace.add_object(open_cap(), &ns_path("/svc/a")).unwrap();
        let (ns, fut) = namespace.serve().unwrap();
        let fut = fasync::Task::spawn(fut);

        let mut ns = ns.flatten();
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
