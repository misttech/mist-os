// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        model::error::ModelError,
        model::hooks::{Event, EventPayload, EventType, HasEventType, Hook, HooksRegistration},
    },
    async_trait::async_trait,
    fuchsia_inspect as inspect, fuchsia_zircon as zx,
    lazy_static::lazy_static,
    moniker::Moniker,
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
};

const MAX_NUMBER_OF_STARTUP_TIME_TRACKED_COMPONENTS: usize = 75;

lazy_static! {
    static ref MONIKER: inspect::StringReference = "moniker".into();
    static ref START_TIME: inspect::StringReference = "time".into();
}

/// Allows to track startup times of components that start early in the boot process (the first
/// 75 components).
pub struct ComponentEarlyStartupTimeStats {
    node: inspect::Node,
    next_id: AtomicUsize,
}

impl ComponentEarlyStartupTimeStats {
    /// Creates a new startup time tracker. Data will be written to the given inspect node.
    pub fn new(node: inspect::Node) -> Self {
        Self { node, next_id: AtomicUsize::new(0) }
    }

    /// Provides the hook events that are needed to work.
    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "ComponentEarlyStartupTimeStats",
            vec![EventType::Started],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    async fn on_component_started(self: &Arc<Self>, moniker: &Moniker, start_time: zx::Time) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id >= MAX_NUMBER_OF_STARTUP_TIME_TRACKED_COMPONENTS {
            return;
        }
        self.node.record_child(id.to_string(), |node| {
            node.record_string(&*MONIKER, moniker.to_string());
            node.record_int(&*START_TIME, start_time.into_nanos());
        });
    }
}

#[async_trait]
impl Hook for ComponentEarlyStartupTimeStats {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match event.event_type() {
            EventType::Started => {
                if let EventPayload::Started { runtime, .. } = &event.payload {
                    self.on_component_started(target_moniker, runtime.start_time).await;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        component::StartReason,
        model::Model,
        testing::test_helpers::{component_decl_with_test_runner, ActionsTest},
    };
    use cm_rust_testing::ComponentDeclBuilder;
    use fuchsia_inspect::{assert_data_tree, DiagnosticsHierarchyGetter};
    use moniker::{ChildMoniker, ChildMonikerBase, MonikerBase};

    #[fuchsia::test]
    async fn tracks_started_components() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        let inspector = inspect::Inspector::default();
        let stats = Arc::new(ComponentEarlyStartupTimeStats::new(
            inspector.root().create_child("start_times"),
        ));
        test.model.root().hooks.install(stats.hooks()).await;

        let root_timestamp =
            start_and_get_timestamp(test.model.clone(), Moniker::root()).await.into_nanos();
        let a_timestamp =
            start_and_get_timestamp(test.model.clone(), vec!["a"].try_into().unwrap())
                .await
                .into_nanos();
        let b_timestamp =
            start_and_get_timestamp(test.model.clone(), vec!["a", "b"].try_into().unwrap())
                .await
                .into_nanos();

        assert_data_tree!(inspector, root: {
            start_times: {
                "0": {
                    moniker: ".",
                    time: root_timestamp,
                },
                "1": {
                    moniker: "a",
                    time: a_timestamp,
                },
                "2": {
                    moniker: "a/b",
                    time: b_timestamp,
                }
            }
        });
    }

    #[fuchsia::test]
    async fn doesnt_track_more_than_75_components() {
        let inspector = inspect::Inspector::default();
        let stats = Arc::new(ComponentEarlyStartupTimeStats::new(
            inspector.root().create_child("start_times"),
        ));

        for i in 0..2 * MAX_NUMBER_OF_STARTUP_TIME_TRACKED_COMPONENTS {
            stats
                .on_component_started(
                    &Moniker::new(vec![ChildMoniker::parse(format!("{}", i)).unwrap()]),
                    zx::Time::from_nanos(i as i64),
                )
                .await;
        }

        let hierarchy = inspector.get_diagnostics_hierarchy();
        let child_count = hierarchy.children[0].children.len();
        assert_eq!(child_count, MAX_NUMBER_OF_STARTUP_TIME_TRACKED_COMPONENTS);
    }

    async fn start_and_get_timestamp(model: Arc<Model>, moniker: Moniker) -> zx::Time {
        let component =
            model.start_instance(&moniker, &StartReason::Root).await.expect("failed to bind");
        let exec = component.lock_execution().await;
        exec.runtime.as_ref().unwrap().timestamp
    }
}
