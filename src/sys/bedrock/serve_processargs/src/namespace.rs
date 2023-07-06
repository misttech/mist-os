// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cap::open::Open,
    cm_types::Path,
    fidl::endpoints::create_endpoints,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{HandleBased, Status},
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        future::BoxFuture,
        FutureExt,
    },
    process_builder::NamespaceEntry,
    std::ffi::CString,
    std::{collections::HashMap, sync::Arc},
    thiserror::Error,
    vfs::{
        directory::entry::DirectoryEntry, directory::helper::DirectlyMutable,
        directory::immutable::simple as pfs, execution_scope::ExecutionScope,
    },
};

type Directory = Arc<pfs::Simple>;

/// A builder object for assembling a program's incoming namespace.
pub struct Namespace {
    /// Directories that hold namespaced objects.
    dirs: HashMap<CString, Directory>,

    /// Path-not-found errors are sent here.
    not_found: UnboundedSender<String>,

    namespace_checker: NamespaceChecker,
}

#[derive(Error, Debug)]
pub enum NamespaceError {
    #[error("path `{0}` has an embedded nul")]
    EmbeddedNul(String),

    #[error(
        "the parent of path `{0}` shares a prefix sequence of directories with another namespace entry. \
        This is not supported."
    )]
    Shadow(Path),

    #[error("cannot install two capabilities at the same namespace path `{0}`")]
    Duplicate(Path),
}

impl Namespace {
    pub fn new(not_found: UnboundedSender<String>) -> Self {
        return Namespace {
            dirs: Default::default(),
            not_found,
            namespace_checker: NamespaceChecker::new(),
        };
    }

    /// Add an open capability `open` at `path`. As a result, the framework will create a
    /// namespace entry at the parent directory of `path`.
    pub fn add_open(self: &mut Self, open: Open, path: &Path) -> Result<(), NamespaceError> {
        self.namespace_checker
            .add(path)
            .map_err(|_e: ShadowError| NamespaceError::Shadow(path.clone()))?;

        let c_str = CString::new(path.dirname().to_string())
            .map_err(|_| NamespaceError::EmbeddedNul(path.dirname().to_string()))?;
        let service_dir = self.dirs.entry(c_str).or_insert_with(|| {
            make_dir_with_not_found_logging(path.dirname().clone().into(), self.not_found.clone())
        });
        service_dir.clone().add_entry(path.basename(), open.into_remote()).map_err(|e| {
            if e == Status::ALREADY_EXISTS {
                return NamespaceError::Duplicate(path.clone());
            }
            panic!("unexpected error {e}");
        })?;

        Ok(())
    }

    pub fn serve(self: Self) -> (Vec<NamespaceEntry>, BoxFuture<'static, ()>) {
        let mut ns = vec![];
        let scope = ExecutionScope::new();

        for (path, dir) in self.dirs {
            let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
            dir.clone().open(
                scope.clone(),
                // TODO(https://fxbug.dev/129636): Remove RIGHT_READABLE when `opendir` no longer
                // requires READABLE.
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                vfs::path::Path::dot(),
                server_end.into_channel().into(),
            );

            ns.push(NamespaceEntry { path, directory: client_end });
        }

        // If this future is dropped, stop serving the namespace.
        let guard = scopeguard::guard(scope.clone(), move |scope| {
            scope.shutdown();
        });
        let fut = async move {
            let _guard = guard;
            scope.wait().await;
        }
        .boxed();
        (ns, fut)
    }
}

/// Returns a disconnected sender which should ignore all the path-not-found errors.
pub fn ignore_not_found() -> UnboundedSender<String> {
    let (sender, _receiver) = unbounded();
    sender
}

fn make_dir_with_not_found_logging(
    root_path: String,
    not_found: UnboundedSender<String>,
) -> Arc<pfs::Simple> {
    let new_dir = pfs::simple();
    let not_found = not_found.clone();
    // Grab a copy of the directory path, it will be needed if we log a
    // failed open request.
    new_dir.clone().set_not_found_handler(Box::new(move |path| {
        let requested_path = format!("{}/{}", root_path, path);
        // Ignore the result of sending. The receiver is free to break away to ignore all the
        // not-found errors.
        let _ = not_found.unbounded_send(requested_path);
    }));
    new_dir
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

    fn add(self: &mut Self, path: &Path) -> Result<(), ShadowError> {
        let mut names = path.split();
        // Get rid of the last path component to obtain only names corresponding to directory nodes.
        // Path has been validated to must not be empty.
        let _ = names.pop().unwrap();
        Self::add_path(&mut self.root, names.into_iter())?;
        Ok(())
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
        fidl::{endpoints::Proxy, AsHandleRef, Peered},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon as zx,
        futures::StreamExt,
        std::str::FromStr,
    };

    fn paths_valid(paths: Vec<&str>) -> Result<(), ShadowError> {
        let mut shadow = NamespaceChecker::new();
        for path in paths {
            shadow.add(&Path::from_str(path).unwrap())?;
        }
        Ok(())
    }

    #[fuchsia::test]
    fn test_shadow() {
        assert_matches!(paths_valid(vec!["/svc/foo/bar/Something", "/svc/Something"]), Err(_));
        assert_matches!(paths_valid(vec!["/svc/Something", "/svc/foo/bar/Something"]), Err(_));
        assert_matches!(paths_valid(vec!["/svc/Something", "/foo"]), Err(_));

        assert_matches!(paths_valid(vec!["/foo/bar/a", "/foo/bar/b", "/foo/bar/c"]), Ok(()));
        assert_matches!(paths_valid(vec!["/a", "/b", "/c"]), Ok(()));
    }

    fn open_cap() -> Open {
        let (open, _receiver) = multishot();
        open
    }

    #[fuchsia::test]
    fn test_duplicate() {
        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_open(open_cap(), &Path::from_str("/svc/a").unwrap()).expect("");
        // Adding again will fail.
        assert_matches!(
            namespace.add_open(open_cap(), &Path::from_str("/svc/a").unwrap()),
            Err(NamespaceError::Duplicate(path))
            if path.as_str() == "/svc/a"
        );
    }

    #[fuchsia::test]
    async fn test_empty() {
        let namespace = Namespace::new(ignore_not_found());
        let (ns, fut) = namespace.serve();
        let fut = fasync::Task::spawn(fut);
        assert_eq!(ns.len(), 0);
        fut.await;
    }

    #[fuchsia::test]
    async fn test_one_sender_end_to_end() {
        let (open, receiver) = multishot();

        let mut namespace = Namespace::new(ignore_not_found());
        namespace.add_open(open.into(), &Path::from_str("/svc/a").unwrap()).unwrap();
        let (mut ns, fut) = namespace.serve();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_str().unwrap(), "/svc");

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
        namespace.add_open(open_cap(), &Path::from_str("/svc/a").unwrap()).unwrap();
        namespace.add_open(open_cap(), &Path::from_str("/svc/b").unwrap()).unwrap();
        let (mut ns, fut) = namespace.serve();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_str().unwrap(), "/svc");

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
        namespace.add_open(open_cap(), &Path::from_str("/svc1/a").unwrap()).unwrap();
        namespace.add_open(open_cap(), &Path::from_str("/svc2/b").unwrap()).unwrap();
        let (ns, fut) = namespace.serve();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 2);
        let (mut svc1, ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_str().unwrap() == "/svc1");
        let (mut svc2, _ns): (Vec<_>, Vec<_>) =
            ns.into_iter().partition(|e| e.path.to_str().unwrap() == "/svc2");

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
        namespace.add_open(open_cap(), &Path::from_str("/svc/a").unwrap()).unwrap();
        let (mut ns, fut) = namespace.serve();
        let fut = fasync::Task::spawn(fut);

        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].path.to_str().unwrap(), "/svc");

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
