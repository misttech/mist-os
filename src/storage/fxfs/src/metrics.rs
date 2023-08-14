// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_store::ObjectStore,
    fuchsia_inspect::Node,
    futures::FutureExt,
    once_cell::sync::Lazy,
    std::{
        collections::hash_map::HashMap,
        sync::{Mutex, Weak},
    },
};

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

/// This is held to support unmounting and remounting of an object store. Nodes cannot be recreated,
/// so hold onto them and simply replace what they point at instead.
pub struct ObjectStoresTracker {
    stores: Mutex<HashMap<String, Weak<ObjectStore>>>,
}

impl ObjectStoresTracker {
    /// Add a store to be tracked in inspect.
    pub fn register_store(&self, name: &str, store: Weak<ObjectStore>) {
        self.stores.lock().unwrap().insert(name.to_owned(), store);
    }

    /// Stop tracking a store in inspect.
    pub fn unregister_store(&self, name: &str) {
        self.stores.lock().unwrap().remove(name);
    }
}

/// Holder of lazy nodes for the object stores list.
pub fn object_stores_tracker() -> &'static ObjectStoresTracker {
    static OBJECT_STORES_TRACKER: Lazy<ObjectStoresTracker> = Lazy::new(|| {
        let root = root();
        let node = root.create_lazy_child("stores", || {
            async {
                let inspector = fuchsia_inspect::Inspector::default();
                let root = inspector.root();
                for (name, store) in object_stores_tracker().stores.lock().unwrap().iter() {
                    let store_arc = match store.upgrade() {
                        Some(store) => store,
                        None => continue,
                    };
                    root.record_child(name.clone(), move |n| store_arc.record_data(n));
                }
                Ok(inspector)
            }
            .boxed()
        });
        root.record(node);
        ObjectStoresTracker { stores: Mutex::new(HashMap::new()) }
    });

    &OBJECT_STORES_TRACKER
}
