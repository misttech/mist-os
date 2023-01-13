// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fuchsia_inspect::Node, once_cell::sync::Lazy, std::sync::Mutex};

/// Root node to which the filesystem Inspect tree will be attached.
fn root() -> Node {
    #[cfg(target_os = "fuchsia")]
    static FXFS_ROOT_NODE: Lazy<Mutex<fuchsia_inspect::Node>> =
        Lazy::new(|| Mutex::new(fuchsia_inspect::component::inspector().root().clone_weak()));
    #[cfg(not(target_os = "fuchsia"))]
    static FXFS_ROOT_NODE: Lazy<Mutex<Node>> = Lazy::new(|| Mutex::new(Node::default()));

    FXFS_ROOT_NODE.lock().unwrap().clone_weak()
}

/// `fs.detail` node for holding fxfs-specific metrics.
pub fn detail() -> Node {
    static DETAIL_NODE: Lazy<Mutex<Node>> =
        Lazy::new(|| Mutex::new(root().create_child("fs.detail")));

    DETAIL_NODE.lock().unwrap().clone_weak()
}

/// Node which contains an entry for each object store.
pub fn object_stores() -> Node {
    static OBJECT_STORES_NODE: Lazy<Mutex<Node>> =
        Lazy::new(|| Mutex::new(root().create_child("stores")));

    OBJECT_STORES_NODE.lock().unwrap().clone_weak()
}
