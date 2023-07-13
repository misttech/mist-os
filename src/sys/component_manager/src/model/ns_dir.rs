// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakComponentInstance, dir_tree::DirTree, error::ResolveActionError,
        mutable_directory::MutableDirectory, routing_fns::route_fn,
    },
    cm_rust::ComponentDecl,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    std::sync::Arc,
    vfs::{
        directory::{entry::DirectoryEntry, immutable as pfs},
        execution_scope::ExecutionScope,
        path::Path,
        remote::remote_dir,
    },
};

type Directory = Arc<pfs::Simple>;

/// A pseudo-directory that proxies open requests to capabilities that have been exposed
/// by a component in its manifest.
pub struct NamespaceDir {
    root_dir: Directory,
    execution_scope: ExecutionScope,
}

impl NamespaceDir {
    /// Creates a new NamespaceDir with an explicit execution scope.
    pub fn new(
        scope: ExecutionScope,
        component: WeakComponentInstance,
        decl: &ComponentDecl,
        pkg_dir: Option<fio::DirectoryProxy>,
    ) -> Result<Self, ResolveActionError> {
        let mut dir = pfs::simple();
        let tree = DirTree::build_from_uses(route_fn, component.clone(), decl);
        tree.install(&mut dir).map_err(|err| ResolveActionError::NamespaceDirError {
            moniker: component.moniker.clone(),
            err,
        })?;

        if let Some(pkg_dir) = pkg_dir {
            dir.add_node("pkg", remote_dir(pkg_dir)).map_err(|err| {
                ResolveActionError::NamespaceDirError { moniker: component.moniker.clone(), err }
            })?;
        }

        Ok(NamespaceDir { root_dir: dir, execution_scope: scope })
    }

    /// Opens a new connection to this NamespaceDir that is closed once this
    /// NamespaceDir is dropped.
    pub fn open(&self, flags: fio::OpenFlags, path: Path, server_end: ServerEnd<fio::NodeMarker>) {
        self.root_dir.clone().open(self.execution_scope.clone(), flags, path, server_end);
    }
}

impl Drop for NamespaceDir {
    fn drop(&mut self) {
        // Explicitly call shutdown to terminate all outstanding requests to
        // this pseudo-directory. ExecutionScope is ref-counted so we can't rely
        // on its drop impl to clean things up at this point.
        self.execution_scope.shutdown();
    }
}
