// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;
use crate::{
    fs::{fs_node_impl_symlink, fs_node_impl_xattr_delegate},
    task::CurrentTask,
    types::*,
};

/// A node that represents a symlink to another node.
pub struct SymlinkNode {
    /// The target of the symlink (the path to use to find the actual node).
    target: FsString,
    xattrs: MemoryXattrStorage,
}

impl SymlinkNode {
    pub fn new(target: &FsStr) -> Self {
        SymlinkNode { target: target.to_owned(), xattrs: Default::default() }
    }
}

impl FsNodeOps for SymlinkNode {
    fs_node_impl_symlink!();
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(self.target.clone()))
    }
}

/// A SymlinkNode that uses a callback.
pub struct CallbackSymlinkNode<F>
where
    F: Fn() -> Result<SymlinkTarget, Errno> + Send + Sync + 'static,
{
    callback: F,
    xattrs: MemoryXattrStorage,
}

impl<F> CallbackSymlinkNode<F>
where
    F: Fn() -> Result<SymlinkTarget, Errno> + Send + Sync + 'static,
{
    pub fn new(callback: F) -> CallbackSymlinkNode<F> {
        CallbackSymlinkNode { callback, xattrs: Default::default() }
    }
}

impl<F> FsNodeOps for CallbackSymlinkNode<F>
where
    F: Fn() -> Result<SymlinkTarget, Errno> + Send + Sync + 'static,
{
    fs_node_impl_symlink!();
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        (self.callback)()
    }
}
