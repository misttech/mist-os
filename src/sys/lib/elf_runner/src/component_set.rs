// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use futures::lock::Mutex;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use crate::{ElfComponent, ElfComponentInfo};
use id::Id;

/// [`ComponentSet`] tracks all the components executing inside an ELF runner,
/// and presents an iterator over those components. It does this under the
/// constraint that each component may go out of scope concurrently due to
/// stopping on its own, or being stopped by the `ComponentController` protocol
/// or any other reason.
pub struct ComponentSet {
    components: Mutex<HashMap<Id, Weak<ElfComponentInfo>>>,
}

impl ComponentSet {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { components: Mutex::new(Default::default()) })
    }

    /// Adds a component to the set.
    ///
    /// The component will remove itself from the set when it is dropped.
    pub async fn add(self: Arc<Self>, component: &mut ElfComponent) {
        let mut components = self.components.lock().await;
        let id = Id::new();
        let component_set = Arc::downgrade(&self.clone());
        component.set_on_drop(move || {
            fasync::Task::spawn(async move {
                let Some(component_set) = component_set.upgrade() else {
                    return;
                };
                component_set.remove(id).await;
            })
            .detach()
        });
        components.insert(id, Arc::downgrade(component.info()));
    }

    /// Invokes `visitor` over all [`ElfComponentInfo`] objects corresponding to
    /// components that are currently running. Note that this is fundamentally racy
    /// as a component could be stopping imminently during or after the visit.
    ///
    /// TODO(fxbug.dev/307580082): Use this soon in `fuchsia.memory.report` server.
    #[allow(dead_code)]
    pub async fn visit(self: Arc<Self>, visitor: &dyn Fn(&ElfComponentInfo)) {
        let components = self.components.lock().await;
        for (_, component) in components.iter() {
            let Some(component) = component.upgrade() else {
                continue;
            };
            visitor(&component);
        }
    }

    async fn remove(self: Arc<Self>, id: Id) {
        let mut components = self.components.lock().await;
        components.remove(&id);
    }
}

/// A simple incrementing counter.
mod id {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Eq, Hash, PartialEq, Copy, Clone, Debug)]
    pub struct Id(u64);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl Id {
        pub fn new() -> Id {
            Id(NEXT_ID.fetch_add(1, Ordering::SeqCst))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_get_id() {
            let id1 = Id::new();
            let id2 = Id::new();
            assert_ne!(id1, id2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_zircon as zx;
    use futures::FutureExt;
    use std::{
        future,
        sync::atomic::{AtomicUsize, Ordering},
        task::Poll,
    };

    use crate::{runtime_dir::RuntimeDirectory, Job};

    #[test]
    fn test_add_remove_component() {
        // Use a test executor so that we can run until stalled.
        let mut exec = fasync::TestExecutor::new();
        let components = ComponentSet::new();

        // The component set starts out empty.
        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        let mut fut = async {
            components_clone
                .visit(&|_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .await
        }
        .boxed_local();
        assert!(exec.run_until_stalled(&mut fut).is_ready());
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // After adding, it should contain one component.
        let mut fake_component = make_fake_component();
        let mut fut = components.clone().add(&mut fake_component).boxed();
        assert!(exec.run_until_stalled(&mut fut).is_ready());
        drop(fut);

        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        let mut fut = async {
            components_clone
                .visit(&|_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .await
        }
        .boxed_local();
        assert!(exec.run_until_stalled(&mut fut).is_ready());
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // After dropping that component, it should eventually contain zero components.
        drop(fake_component);
        let mut fut = async {
            let _: Poll<()> =
                fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        }
        .boxed();
        assert!(exec.run_until_stalled(&mut fut).is_ready());

        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        let mut fut = async {
            components_clone
                .visit(&|_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .await
        }
        .boxed_local();
        assert!(exec.run_until_stalled(&mut fut).is_ready());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    fn make_fake_component() -> ElfComponent {
        let runtime_dir = RuntimeDirectory::empty();
        let job = Job::Single(fuchsia_runtime::job_default().create_child_job().unwrap());
        let process = fuchsia_runtime::process_self().duplicate(zx::Rights::SAME_RIGHTS).unwrap();
        let lifecycle_channel = None;
        let main_process_critical = false;
        let tasks = vec![];
        let component_url = "hello".to_string();
        let fake_component = ElfComponent::new(
            runtime_dir,
            job,
            process,
            lifecycle_channel,
            main_process_critical,
            tasks,
            component_url,
        );
        fake_component
    }
}
