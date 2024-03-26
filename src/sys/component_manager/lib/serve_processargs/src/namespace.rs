// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_types::{NamespacePath, Path};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use namespace::{Entry as NamespaceEntry, Namespace, NamespaceError, Tree};
use sandbox::{Capability, Dict};
use thiserror::Error;
use vfs::{directory::entry::serve_directory, execution_scope::ExecutionScope};

/// A builder object for assembling a program's incoming namespace.
pub struct NamespaceBuilder {
    /// Mapping from namespace path to capabilities that can be turned into `Directory`.
    entries: Tree<Capability>,

    /// Path-not-found errors are sent here.
    not_found: UnboundedSender<String>,

    /// Scope in which the namespace vfs executes.
    ///
    /// This can be used to terminate the vfs.
    scope: ExecutionScope,
}

#[derive(Error, Debug, Clone)]
pub enum BuildNamespaceError {
    #[error(transparent)]
    NamespaceError(#[from] NamespaceError),

    #[error(
        "while installing capabilities within the namespace entry `{path}`, \
        failed to convert the namespace entry to Directory: {err}"
    )]
    Conversion {
        path: NamespacePath,
        #[source]
        err: sandbox::ConversionError,
    },

    #[error("unable to serve `{path}` after converting to directory: {err}")]
    Serve {
        path: NamespacePath,
        #[source]
        err: fidl::Status,
    },
}

impl NamespaceBuilder {
    pub fn new(scope: ExecutionScope, not_found: UnboundedSender<String>) -> Self {
        return NamespaceBuilder { entries: Default::default(), not_found, scope };
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at the parent directory of `path`.
    pub fn add_object(
        self: &mut Self,
        cap: Capability,
        path: &Path,
    ) -> Result<(), BuildNamespaceError> {
        let dirname = path.parent();

        // Get the entry, or if it doesn't exist, make an empty dictionary.
        let any = match self.entries.get(&dirname) {
            Some(dir) => dir,
            None => {
                let dict = self.make_dict_with_not_found_logging(dirname.to_string());
                self.entries.add(&dirname, Capability::Dictionary(dict))?
            }
        };

        // Cast the namespace entry as a Dict. This may fail if the user added a duplicate
        // namespace entry that is not a Dict (see `add_entry`).
        let dict = match any {
            Capability::Dictionary(d) => d,
            _ => Err(NamespaceError::Duplicate(path.clone().into()))?,
        };

        // Insert the capability into the Dict.
        {
            let mut entries = dict.lock_entries();
            if entries.contains_key(path.basename().as_str()) {
                return Err(NamespaceError::Duplicate(path.clone().into()).into());
            }
            entries.insert(path.basename().clone(), cap);
        }
        Ok(())
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at `path` directly. The capability will be exercised when the user
    /// opens the `path`.
    pub fn add_entry(
        self: &mut Self,
        cap: Capability,
        path: &NamespacePath,
    ) -> Result<(), BuildNamespaceError> {
        self.entries.add(path, cap)?;
        Ok(())
    }

    pub fn serve(self: Self) -> Result<Namespace, BuildNamespaceError> {
        let ns = self
            .entries
            .flatten()
            .into_iter()
            .map(|(path, cap)| -> Result<NamespaceEntry, BuildNamespaceError> {
                let directory = match cap {
                    Capability::Directory(d) => d,
                    cap => {
                        let entry = cap.try_into_directory_entry().map_err(|err| {
                            BuildNamespaceError::Conversion { path: path.clone(), err }
                        })?;
                        if entry.entry_info().type_() != fio::DirentType::Directory {
                            return Err(BuildNamespaceError::Conversion {
                                path: path.clone(),
                                err: sandbox::ConversionError::NotSupported,
                            });
                        }
                        sandbox::Directory::new(
                            serve_directory(
                                entry,
                                &self.scope,
                                fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                            )
                            .map_err(|err| {
                                BuildNamespaceError::Serve { path: path.clone(), err }
                            })?,
                        )
                    }
                };
                let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
                Ok(NamespaceEntry { path, directory: client_end.into() })
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()?;
        Ok(ns)
    }

    fn make_dict_with_not_found_logging(&self, root_path: String) -> Dict {
        let not_found = self.not_found.clone();
        let new_dict = Dict::new_with_not_found(move |key| {
            let requested_path = format!("{}/{}", root_path, key);
            // Ignore the result of sending. The receiver is free to break away to ignore all the
            // not-found errors.
            let _ = not_found.unbounded_send(requested_path);
        });
        new_dict
    }
}

impl Clone for NamespaceBuilder {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            not_found: self.not_found.clone(),
            scope: self.scope.clone(),
        }
    }
}

/// Returns a disconnected sender which should ignore all the path-not-found errors.
pub fn ignore_not_found() -> UnboundedSender<String> {
    let (sender, _receiver) = unbounded();
    sender
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_util::multishot,
        anyhow::Result,
        assert_matches::assert_matches,
        fidl::{endpoints::Proxy, AsHandleRef, Peered},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon as zx,
        futures::StreamExt,
    };

    fn open_cap() -> Capability {
        let (open, _receiver) = multishot();
        Capability::Open(open)
    }

    fn ns_path(str: &str) -> NamespacePath {
        str.parse().unwrap()
    }

    fn path(str: &str) -> Path {
        str.parse().unwrap()
    }

    fn parents_valid(paths: Vec<&str>) -> Result<(), BuildNamespaceError> {
        let mut shadow = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        for p in paths {
            shadow.add_object(open_cap(), &path(p))?;
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_shadow() {
        assert_matches!(parents_valid(vec!["/svc/foo/bar/Something", "/svc/Something"]), Err(_));
        assert_matches!(parents_valid(vec!["/svc/Something", "/svc/foo/bar/Something"]), Err(_));
        assert_matches!(parents_valid(vec!["/svc/Something", "/foo"]), Err(_));

        assert_matches!(parents_valid(vec!["/foo/bar/a", "/foo/bar/b", "/foo/bar/c"]), Ok(()));
        assert_matches!(parents_valid(vec!["/a", "/b", "/c"]), Ok(()));

        let mut shadow = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        shadow.add_object(open_cap(), &path("/svc/foo")).unwrap();
        assert_matches!(shadow.add_object(open_cap(), &path("/svc/foo/bar")), Err(_));

        let mut shadow = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        shadow.add_object(open_cap(), &path("/svc/foo")).unwrap();
        assert_matches!(shadow.add_entry(open_cap(), &ns_path("/svc2")), Ok(_));
    }

    #[fuchsia::test]
    async fn test_duplicate_object() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_object(open_cap(), &path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_object(open_cap(), &path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/svc/a"
        );
    }

    #[fuchsia::test]
    async fn test_duplicate_entry() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/svc/a"
        );
    }

    #[fuchsia::test]
    async fn test_duplicate_object_and_entry() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_object(open_cap(), &path("/svc/a")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Shadow(path)))
            if path.to_string() == "/svc/a"
        );
    }

    /// If we added a namespaced object at "/foo/bar", thus creating a namespace entry at "/foo",
    /// we cannot add another entry directly at "/foo" again.
    #[fuchsia::test]
    async fn test_duplicate_entry_at_object_parent() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_object(open_cap(), &path("/foo/bar")).expect("");
        assert_matches!(
            namespace.add_entry(open_cap(), &ns_path("/foo")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/foo"
        );
    }

    /// If we directly added an entry at "/foo", it's not possible to add a namespaced object at
    /// "/foo/bar", as that would've required overwriting "/foo" with a namespace entry served by
    /// the framework.
    #[fuchsia::test]
    async fn test_duplicate_object_parent_at_entry() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_entry(open_cap(), &ns_path("/foo")).expect("");
        assert_matches!(
            namespace.add_object(open_cap(), &path("/foo/bar")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/foo/bar"
        );
    }

    #[fuchsia::test]
    async fn test_empty() {
        let namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        let ns = namespace.serve().unwrap();
        assert_eq!(ns.flatten().len(), 0);
    }

    #[fuchsia::test]
    async fn test_one_sender_end_to_end() {
        let (open, receiver) = multishot();

        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_object(Capability::Open(open), &path("/svc/a")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

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
        let server_end: zx::Channel = receiver.0.recv().await.unwrap().get_handle().unwrap().into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).unwrap();
    }

    #[fuchsia::test]
    async fn test_two_senders_in_same_namespace_entry() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope.clone(), ignore_not_found());
        namespace.add_object(open_cap(), &path("/svc/a")).unwrap();
        namespace.add_object(open_cap(), &path("/svc/b")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

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
    }

    #[fuchsia::test]
    async fn test_two_senders_in_different_namespace_entries() {
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        namespace.add_object(open_cap(), &path("/svc1/a")).unwrap();
        namespace.add_object(open_cap(), &path("/svc2/b")).unwrap();
        let ns = namespace.serve().unwrap();

        let ns = ns.flatten();
        assert_eq!(ns.len(), 2);
        let (mut svc1, ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_string() == "/svc1");
        let (mut svc2, _ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_string() == "/svc2");

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
    }

    #[fuchsia::test]
    async fn test_not_found() {
        let (not_found_sender, mut not_found_receiver) = unbounded();
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), not_found_sender);
        namespace.add_object(open_cap(), &path("/svc/a")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

        let dir = ns.pop().unwrap().directory.into_proxy().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let _ = fdio::service_connect_at(
            &dir.into_channel().unwrap().into_zx_channel(),
            "non_existent",
            server_end,
        );

        // Server endpoint is closed because the path does not exist.
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();

        // We should get a notification about this path.
        assert_eq!(not_found_receiver.next().await, Some("/svc/non_existent".to_string()));

        drop(ns);
    }

    #[fuchsia::test]
    async fn test_not_directory() {
        let (not_found_sender, _) = unbounded();
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), not_found_sender);
        let (_, sender) = sandbox::Receiver::new();
        namespace.add_entry(sender.into(), &ns_path("/a")).unwrap();
        assert_matches!(namespace.serve(), Err(BuildNamespaceError::Conversion { .. }));
    }
}
