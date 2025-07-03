// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_inspect::{Inspector, LazyNode, Node};
use fuchsia_sync::Mutex;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;

/// Root node to which the filesystem Inspect tree will be attached.
fn root() -> Node {
    #[cfg(target_os = "fuchsia")]
    static FXFS_ROOT_NODE: Lazy<Mutex<fuchsia_inspect::Node>> =
        Lazy::new(|| Mutex::new(fuchsia_inspect::component::inspector().root().clone_weak()));
    #[cfg(not(target_os = "fuchsia"))]
    static FXFS_ROOT_NODE: Lazy<Mutex<Node>> = Lazy::new(|| Mutex::new(Node::default()));

    FXFS_ROOT_NODE.lock().clone_weak()
}

/// `fs.detail` node for holding fxfs-specific metrics.
pub fn detail() -> Node {
    static DETAIL_NODE: Lazy<Mutex<Node>> =
        Lazy::new(|| Mutex::new(root().create_child("fs.detail")));

    DETAIL_NODE.lock().clone_weak()
}

pub fn register_fs(
    populate_stores_fn: impl Fn() -> BoxFuture<'static, Result<Inspector, Error>>
        + Sync
        + Send
        + 'static,
) -> LazyNode {
    root().create_lazy_child("stores", populate_stores_fn)
}
