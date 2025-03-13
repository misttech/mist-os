// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_types::{NamespacePath, Path};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use namespace::{Entry as NamespaceEntry, EntryError, Namespace, NamespaceError, Tree};
use sandbox::{Capability, Dict, Directory, RemotableCapability};
use thiserror::Error;
use vfs::directory::entry::serve_directory;
use vfs::execution_scope::ExecutionScope;

/// A builder object for assembling a program's incoming namespace.
pub struct NamespaceBuilder {
    /// Mapping from namespace path to capabilities that can be turned into `Directory`.
    entries: Tree<Capability>,

    /// Path-not-found errors are sent here.
    not_found: UnboundedSender<String>,

    /// Scope in which the namespace vfs executes.
    ///
    /// This can be used to terminate the vfs.
    namespace_scope: ExecutionScope,
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
    pub fn new(namespace_scope: ExecutionScope, not_found: UnboundedSender<String>) -> Self {
        return NamespaceBuilder { entries: Default::default(), not_found, namespace_scope };
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
        let any = match self.entries.get_mut(&dirname) {
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
        dict.insert(path.basename().clone(), cap)
            .map_err(|_| NamespaceError::Duplicate(path.clone().into()).into())
    }

    /// Add a capability `cap` at `path`. As a result, the framework will create a
    /// namespace entry at `path` directly. The capability will be exercised when the user
    /// opens the `path`.
    pub fn add_entry(
        self: &mut Self,
        cap: Capability,
        path: &NamespacePath,
    ) -> Result<(), BuildNamespaceError> {
        match &cap {
            Capability::Directory(_)
            | Capability::Dictionary(_)
            | Capability::DirEntry(_)
            | Capability::DirConnector(_) => {}
            _ => return Err(NamespaceError::EntryError(EntryError::UnsupportedType).into()),
        }
        self.entries.add(path, cap)?;
        Ok(())
    }

    pub fn serve(self: Self) -> Result<Namespace, BuildNamespaceError> {
        let mut entries = vec![];
        for (path, cap) in self.entries.flatten() {
            let directory = match cap {
                Capability::Directory(d) => d,
                Capability::DirConnector(c) => {
                    let (directory, server) =
                        fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
                    let _ = c.send(server);
                    Directory::new(directory)
                }
                cap @ Capability::Dictionary(_) => {
                    let entry =
                        cap.try_into_directory_entry(self.namespace_scope.clone()).map_err(
                            |err| BuildNamespaceError::Conversion { path: path.clone(), err },
                        )?;
                    if entry.entry_info().type_() != fio::DirentType::Directory {
                        return Err(BuildNamespaceError::Conversion {
                            path: path.clone(),
                            err: sandbox::ConversionError::NotSupported,
                        });
                    }
                    sandbox::Directory::new(
                        serve_directory(
                            entry,
                            &self.namespace_scope,
                            fio::OpenFlags::DIRECTORY
                                | fio::OpenFlags::RIGHT_READABLE
                                | fio::OpenFlags::POSIX_EXECUTABLE
                                | fio::OpenFlags::POSIX_WRITABLE,
                        )
                        .map_err(|err| BuildNamespaceError::Serve { path: path.clone(), err })?,
                    )
                }
                _ => return Err(NamespaceError::EntryError(EntryError::UnsupportedType).into()),
            };
            let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
            entries.push(NamespaceEntry { path, directory: client_end.into() })
        }
        let ns = entries.try_into()?;
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

/// Returns a disconnected sender which should ignore all the path-not-found errors.
pub fn ignore_not_found() -> UnboundedSender<String> {
    let (sender, _receiver) = unbounded();
    sender
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::multishot;
    use anyhow::Result;
    use assert_matches::assert_matches;
    use fidl::endpoints::{self, Proxy, ServerEnd};
    use fidl::Peered;
    use fuchsia_fs::directory::DirEntry;
    use futures::channel::mpsc;
    use futures::{StreamExt, TryStreamExt};
    use sandbox::Directory;
    use std::sync::Arc;
    use test_case::test_case;
    use vfs::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
    use vfs::directory::entry_container::Directory as VfsDirectory;
    use vfs::remote::RemoteLike;
    use vfs::{path, pseudo_directory, ObjectRequest, ObjectRequestRef};
    use zx::AsHandleRef;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    fn connector_cap() -> Capability {
        let (sender, _receiver) = multishot();
        Capability::Connector(sender)
    }

    fn directory_cap() -> Capability {
        let (client, _server) = endpoints::create_endpoints();
        Capability::Directory(Directory::new(client))
    }

    fn ns_path(str: &str) -> NamespacePath {
        str.parse().unwrap()
    }

    fn path(str: &str) -> Path {
        str.parse().unwrap()
    }

    fn parents_valid(paths: Vec<&str>) -> Result<(), BuildNamespaceError> {
        let scope = ExecutionScope::new();
        let mut shadow = NamespaceBuilder::new(scope, ignore_not_found());
        for p in paths {
            shadow.add_object(connector_cap(), &path(p))?;
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

        let scope = ExecutionScope::new();
        let mut shadow = NamespaceBuilder::new(scope, ignore_not_found());
        shadow.add_object(connector_cap(), &path("/svc/foo")).unwrap();
        assert_matches!(shadow.add_object(connector_cap(), &path("/svc/foo/bar")), Err(_));

        let scope = ExecutionScope::new();
        let mut not_shadow = NamespaceBuilder::new(scope, ignore_not_found());
        not_shadow.add_object(connector_cap(), &path("/svc/foo")).unwrap();
        assert_matches!(not_shadow.add_entry(directory_cap(), &ns_path("/svc2")), Ok(_));
    }

    #[fuchsia::test]
    async fn test_duplicate_object() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(connector_cap(), &path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_object(connector_cap(), &path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/svc/a"
        );
    }

    #[fuchsia::test]
    async fn test_duplicate_entry() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_entry(directory_cap(), &ns_path("/svc/a")).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_entry(directory_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/svc/a"
        );
    }

    #[fuchsia::test]
    async fn test_duplicate_object_and_entry() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(connector_cap(), &path("/svc/a")).expect("");
        assert_matches!(
            namespace.add_entry(directory_cap(), &ns_path("/svc/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Shadow(path)))
            if path.to_string() == "/svc/a"
        );
    }

    /// If we added a namespaced object at "/foo/bar", thus creating a namespace entry at "/foo",
    /// we cannot add another entry directly at "/foo" again.
    #[fuchsia::test]
    async fn test_duplicate_entry_at_object_parent() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(connector_cap(), &path("/foo/bar")).expect("");
        assert_matches!(
            namespace.add_entry(directory_cap(), &ns_path("/foo")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/foo"
        );
    }

    /// If we directly added an entry at "/foo", it's not possible to add a namespaced object at
    /// "/foo/bar", as that would've required overwriting "/foo" with a namespace entry served by
    /// the framework.
    #[fuchsia::test]
    async fn test_duplicate_object_parent_at_entry() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_entry(directory_cap(), &ns_path("/foo")).expect("");
        assert_matches!(
            namespace.add_object(connector_cap(), &path("/foo/bar")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::Duplicate(path)))
            if path.to_string() == "/foo/bar"
        );
    }

    #[fuchsia::test]
    async fn test_empty() {
        let scope = ExecutionScope::new();
        let namespace = NamespaceBuilder::new(scope, ignore_not_found());
        let ns = namespace.serve().unwrap();
        assert_eq!(ns.flatten().len(), 0);
    }

    #[fuchsia::test]
    async fn test_one_connector_end_to_end() {
        let (sender, receiver) = multishot();

        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(sender.into(), &path("/svc/a")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

        // Check that there is exactly one protocol inside the svc directory.
        let dir = ns.pop().unwrap().directory.into_proxy();
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
        let server_end: zx::Channel = receiver.receive().await.unwrap().channel.into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST).unwrap();
    }

    #[fuchsia::test]
    async fn test_two_connectors_in_same_namespace_entry() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(connector_cap(), &path("/svc/a")).unwrap();
        namespace.add_object(connector_cap(), &path("/svc/b")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

        // Check that there are exactly two protocols inside the svc directory.
        let dir = ns.pop().unwrap().directory.into_proxy();
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
    async fn test_two_connectors_in_different_namespace_entries() {
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_object(connector_cap(), &path("/svc1/a")).unwrap();
        namespace.add_object(connector_cap(), &path("/svc2/b")).unwrap();
        let ns = namespace.serve().unwrap();

        let ns = ns.flatten();
        assert_eq!(ns.len(), 2);
        let (mut svc1, ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_string() == "/svc1");
        let (mut svc2, _ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_string() == "/svc2");

        // Check that there are one protocol inside each directory.
        {
            let dir = svc1.pop().unwrap().directory.into_proxy();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir).await.unwrap(),
                vec![DirEntry { name: "a".to_string(), kind: fio::DirentType::Service },]
            );
        }
        {
            let dir = svc2.pop().unwrap().directory.into_proxy();
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
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, not_found_sender);
        namespace.add_object(connector_cap(), &path("/svc/a")).unwrap();
        let ns = namespace.serve().unwrap();

        let mut ns = ns.flatten();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_string(), "/svc");

        let dir = ns.pop().unwrap().directory.into_proxy();
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
        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, not_found_sender);
        let (_, sender) = sandbox::Connector::new();
        assert_matches!(
            namespace.add_entry(sender.into(), &ns_path("/a")),
            Err(BuildNamespaceError::NamespaceError(NamespaceError::EntryError(
                EntryError::UnsupportedType
            )))
        );
    }

    #[test_case(fio::PERM_READABLE)]
    #[test_case(fio::PERM_READABLE | fio::PERM_EXECUTABLE)]
    #[test_case(fio::PERM_READABLE | fio::PERM_WRITABLE)]
    #[test_case(fio::PERM_READABLE | fio::Flags::PERM_INHERIT_WRITE | fio::Flags::PERM_INHERIT_EXECUTE)]
    #[fuchsia::test]
    async fn test_directory_rights(rights: fio::Flags) {
        fn serve_vfs_dir(
            root: Arc<impl VfsDirectory>,
            rights: fio::Flags,
        ) -> ClientEnd<fio::DirectoryMarker> {
            let scope = ExecutionScope::new();
            let (client, server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
            ObjectRequest::new(rights, &fio::Options::default(), server.into_channel()).handle(
                |request| root.open3(scope.clone(), vfs::path::Path::dot(), rights, request),
            );
            client
        }

        let (open_tx, mut open_rx) = mpsc::channel::<()>(1);

        struct MockDir {
            tx: mpsc::Sender<()>,
            rights: fio::Flags,
        }
        impl DirectoryEntry for MockDir {
            fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
                request.open_remote(self)
            }
        }
        impl GetEntryInfo for MockDir {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
            }
        }
        impl RemoteLike for MockDir {
            fn open(
                self: Arc<Self>,
                _scope: ExecutionScope,
                _flags: fio::OpenFlags,
                _relative_path: path::Path,
                _server_end: ServerEnd<fio::NodeMarker>,
            ) {
                panic!("open is deprecated, use open3 instead")
            }

            fn open3(
                self: Arc<Self>,
                _scope: ExecutionScope,
                relative_path: path::Path,
                flags: fio::Flags,
                _object_request: ObjectRequestRef<'_>,
            ) -> Result<(), zx::Status> {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::Flags::PROTOCOL_DIRECTORY | self.rights);
                self.tx.clone().try_send(()).unwrap();
                Ok(())
            }
        }

        let mock = Arc::new(MockDir { tx: open_tx, rights });

        let fs = pseudo_directory! {
            "foo" => mock,
        };
        let dir = Directory::from(serve_vfs_dir(fs, rights));

        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_entry(dir.into(), &ns_path("/dir")).unwrap();
        let mut ns = namespace.serve().unwrap();
        let dir_proxy = ns.remove(&"/dir".parse().unwrap()).unwrap();
        let dir_proxy = dir_proxy.into_proxy();
        let (_, server_end) = endpoints::create_endpoints::<fio::NodeMarker>();
        dir_proxy
            .open(
                "foo",
                fio::Flags::PROTOCOL_DIRECTORY | rights,
                &fio::Options::default(),
                server_end.into_channel(),
            )
            .unwrap();

        // The MockDir should receive the Open request.
        open_rx.next().await.unwrap();
    }

    #[fuchsia::test]
    async fn test_directory_non_executable() {
        fn serve_vfs_dir(root: Arc<impl VfsDirectory>) -> ClientEnd<fio::DirectoryMarker> {
            let scope = ExecutionScope::new();
            let (client, server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
            let flags = fio::PERM_READABLE;
            ObjectRequest::new(flags, &fio::Options::default(), server.into_channel()).handle(
                |request| root.open3(scope.clone(), vfs::path::Path::dot(), flags, request),
            );
            client
        }

        let (open_tx, mut open_rx) = mpsc::channel::<()>(1);

        struct MockDir(mpsc::Sender<()>);
        impl DirectoryEntry for MockDir {
            fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
                request.open_remote(self)
            }
        }
        impl GetEntryInfo for MockDir {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
            }
        }
        impl RemoteLike for MockDir {
            fn open(
                self: Arc<Self>,
                _scope: ExecutionScope,
                flags: fio::OpenFlags,
                relative_path: path::Path,
                _server_end: ServerEnd<fio::NodeMarker>,
            ) {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE);
                self.0.clone().try_send(()).unwrap();
            }

            fn open3(
                self: Arc<Self>,
                _scope: ExecutionScope,
                relative_path: path::Path,
                flags: fio::Flags,
                _object_request: ObjectRequestRef<'_>,
            ) -> Result<(), zx::Status> {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE);
                self.0.clone().try_send(()).unwrap();
                Ok(())
            }
        }

        let mock = Arc::new(MockDir(open_tx));

        let fs = pseudo_directory! {
            "foo" => mock,
        };
        let dir = Directory::from(serve_vfs_dir(fs));

        let scope = ExecutionScope::new();
        let mut namespace = NamespaceBuilder::new(scope, ignore_not_found());
        namespace.add_entry(dir.into(), &ns_path("/dir")).unwrap();
        let mut ns = namespace.serve().unwrap();
        let dir_proxy = ns.remove(&"/dir".parse().unwrap()).unwrap();
        let dir_proxy = dir_proxy.into_proxy();

        // Try to open as executable. Should fail (ACCESS_DENIED)
        let (node, server_end) = endpoints::create_endpoints::<fio::NodeMarker>();
        dir_proxy
            .open(
                "foo",
                fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE | fio::PERM_EXECUTABLE,
                &fio::Options::default(),
                server_end.into_channel(),
            )
            .unwrap();
        let node = node.into_proxy();
        let mut node = node.take_event_stream();
        assert_matches!(
            node.try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::ACCESS_DENIED, .. })
        );

        // Try to open as read-only. Should succeed.
        let (_, server_end) = endpoints::create_endpoints::<fio::NodeMarker>();
        dir_proxy
            .open(
                "foo",
                fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE,
                &fio::Options::default(),
                server_end.into_channel(),
            )
            .unwrap();

        // The MockDir should receive the Open request.
        open_rx.next().await.unwrap();
    }
}
