// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    events::{
        router::EventConsumer,
        types::{DiagnosticsReadyPayload, Event, EventPayload},
    },
    identity::ComponentIdentity,
    inspect::container::{
        InspectArtifactsContainer, InspectHandle, UnpopulatedInspectDataContainer,
    },
    pipeline::Pipeline,
};
use diagnostics_hierarchy::HierarchyMatcher;
use fidl_fuchsia_diagnostics::Selector;
use fuchsia_async as fasync;
use fuchsia_sync::RwLock;
use fuchsia_zircon::Koid;
use futures::channel::{mpsc, oneshot};
use futures::prelude::*;
use moniker::ExtendedMoniker;
use selectors::SelectorExt;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tracing::{debug, warn};

pub struct InspectRepository {
    inner: RwLock<InspectRepositoryInner>,
    pipelines: Vec<Weak<Pipeline>>,
}

impl Default for InspectRepository {
    fn default() -> Self {
        Self::new(vec![])
    }
}

impl InspectRepository {
    pub fn new(pipelines: Vec<Weak<Pipeline>>) -> InspectRepository {
        let (snd, rcv) = mpsc::unbounded();
        Self {
            pipelines,
            inner: RwLock::new(InspectRepositoryInner {
                diagnostics_containers: HashMap::new(),
                diagnostics_dir_closed_snd: snd,
                _diagnostics_dir_closed_drain: fasync::Task::spawn(async move {
                    rcv.for_each_concurrent(None, |rx| rx).await
                }),
            }),
        }
    }
    /// Return all of the DirectoryProxies that contain Inspect hierarchies
    /// which contain data that should be selected from.
    pub fn fetch_inspect_data(
        &self,
        component_selectors: &Option<Vec<Selector>>,
        moniker_to_static_matcher_map: Option<HashMap<ExtendedMoniker, Arc<HierarchyMatcher>>>,
    ) -> Vec<UnpopulatedInspectDataContainer> {
        self.inner.read().fetch_inspect_data(component_selectors, moniker_to_static_matcher_map)
    }

    fn add_inspect_artifacts(
        self: &Arc<Self>,
        identity: Arc<ComponentIdentity>,
        proxy_handle: impl Into<InspectHandle>,
    ) {
        // Hold the lock while we insert and update pipelines.
        let mut guard = self.inner.write();
        // insert_inspect_artifact_container returns None when we were already tracking the
        // directory for this component. If that's the case we can return early.
        let Some(on_closed_fut) =
            guard.insert_inspect_artifact_container(Arc::clone(&identity), proxy_handle)
        else {
            return;
        };

        let identity_clone = Arc::clone(&identity);
        let this_weak = Arc::downgrade(self);
        let _ = guard.diagnostics_dir_closed_snd.unbounded_send(fasync::Task::spawn(async move {
            if let Ok(koid_to_remove) = on_closed_fut.await {
                if let Some(this) = this_weak.upgrade() {
                    // Hold the lock while we remove and update pipelines.
                    let mut guard = this.inner.write();

                    if let Some(container) = guard.diagnostics_containers.get_mut(&identity_clone) {
                        if container.remove_handle(koid_to_remove) != 0 {
                            return;
                        }
                    }

                    guard.diagnostics_containers.remove(&identity_clone);

                    for pipeline_weak in &this.pipelines {
                        if let Some(pipeline) = pipeline_weak.upgrade() {
                            pipeline.write().remove(&identity_clone.moniker);
                        }
                    }
                }
            }
        }));

        // Let each pipeline know that a new component arrived, and allow the pipeline
        // to eagerly bucket static selectors based on that component's moniker.
        for pipeline_weak in self.pipelines.iter() {
            if let Some(pipeline) = pipeline_weak.upgrade() {
                pipeline.write().add_inspect_artifacts(&identity.moniker).unwrap_or_else(|e| {
                    warn!(%identity, ?e,
                                "Failed to add inspect artifacts to pipeline wrapper");
                });
            }
        }
    }

    pub(crate) fn add_inspect_handle(
        self: &Arc<Self>,
        component: Arc<ComponentIdentity>,
        handle: impl Into<InspectHandle>,
    ) {
        debug!(identity = %component, "Diagnostics directory is ready.");
        // Update the central repository to reference the new diagnostics source.
        self.add_inspect_artifacts(Arc::clone(&component), handle);
    }
}

#[cfg(test)]
impl InspectRepository {
    pub(crate) fn terminate_inspect(&self, identity: Arc<ComponentIdentity>) {
        self.inner.write().diagnostics_containers.remove(&identity);
    }

    fn has_match(&self, identity: &Arc<ComponentIdentity>) -> bool {
        let lock = self.inner.read();
        lock.get_diagnostics_containers().get(identity).is_some()
    }

    /// Wait for data to appear for `identity`. Will run indefinitely if no data shows up.
    pub(crate) async fn wait_for_artifact(&self, identity: &Arc<ComponentIdentity>) {
        loop {
            if self.has_match(identity) {
                return;
            }

            fasync::Timer::new(fuchsia_zircon::Time::after(fuchsia_zircon::Duration::from_millis(
                100,
            )))
            .await;
        }
    }

    /// Wait until nothing is present for `identity`. Will run indefinitely if data persists.
    pub(crate) async fn wait_until_gone(&self, identity: &Arc<ComponentIdentity>) {
        loop {
            if !self.has_match(identity) {
                return;
            }

            fasync::Timer::new(fuchsia_zircon::Time::after(fuchsia_zircon::Duration::from_millis(
                100,
            )))
            .await;
        }
    }
}

impl EventConsumer for InspectRepository {
    fn handle(self: Arc<Self>, event: Event) {
        match event.payload {
            EventPayload::DiagnosticsReady(DiagnosticsReadyPayload { component, directory }) => {
                self.add_inspect_handle(component, directory);
            }
            _ => unreachable!("Inspect repository is only subscribed to diagnostics ready"),
        }
    }
}

struct InspectRepositoryInner {
    /// All the diagnostics directories that we are tracking.
    diagnostics_containers: HashMap<Arc<ComponentIdentity>, InspectArtifactsContainer>,

    /// Tasks waiting for PEER_CLOSED signals on diagnostics directories are sent here.
    diagnostics_dir_closed_snd: mpsc::UnboundedSender<fasync::Task<()>>,

    /// Task draining all diagnostics directory PEER_CLOSED signal futures.
    _diagnostics_dir_closed_drain: fasync::Task<()>,
}

impl InspectRepositoryInner {
    // Inserts an InspectArtifactsContainer into the data repository.
    fn insert_inspect_artifact_container(
        &mut self,
        identity: Arc<ComponentIdentity>,
        proxy_handle: impl Into<InspectHandle>,
    ) -> Option<oneshot::Receiver<Koid>> {
        let mut diag_repo_entry_opt = self.diagnostics_containers.get_mut(&identity);
        match diag_repo_entry_opt {
            None => {
                // An entry with no values implies that the somehow we observed the
                // creation of a component lower in the topology before observing this
                // one. If this is the case, just instantiate as though it's our first
                // time encountering this moniker segment.
                let (inspect_container, on_closed_fut) =
                    InspectArtifactsContainer::new(proxy_handle);
                self.diagnostics_containers.insert(identity, inspect_container);
                Some(on_closed_fut)
            }
            Some(ref mut artifacts_container) => artifacts_container.push_handle(proxy_handle),
        }
    }

    fn fetch_inspect_data(
        &self,
        component_selectors: &Option<Vec<Selector>>,
        moniker_to_static_matcher_map: Option<HashMap<ExtendedMoniker, Arc<HierarchyMatcher>>>,
    ) -> Vec<UnpopulatedInspectDataContainer> {
        let mut containers = vec![];
        for (identity, container) in self.diagnostics_containers.iter() {
            let optional_hierarchy_matcher = match &moniker_to_static_matcher_map {
                Some(map) => {
                    match map.get(&identity.moniker) {
                        Some(inspect_matcher) => Some(Arc::clone(inspect_matcher)),
                        // Return early if there were static selectors, and none were for this
                        // moniker.
                        None => continue,
                    }
                }
                None => None,
            };

            // Verify that the dynamic selectors contain an entry that applies to
            // this moniker as well.
            if !match component_selectors {
                Some(component_selectors) => component_selectors
                    .iter()
                    .any(|s| matches!(identity.moniker.matches_selector(s), Ok(true))),
                None => true,
            } {
                continue;
            }

            // This artifact contains inspect and matches a passed selector.
            if let Some(unpopulated) =
                container.create_unpopulated(identity, optional_hierarchy_matcher)
            {
                containers.push(unpopulated);
            }
        }
        containers
    }
}

#[cfg(test)]
impl InspectRepositoryInner {
    pub(crate) fn get(
        &self,
        identity: &Arc<ComponentIdentity>,
    ) -> Option<&InspectArtifactsContainer> {
        self.diagnostics_containers.get(identity)
    }

    pub(crate) fn get_diagnostics_containers(
        &self,
    ) -> &HashMap<Arc<ComponentIdentity>, InspectArtifactsContainer> {
        &self.diagnostics_containers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use fuchsia_zircon::DurationNum;
    use selectors::FastError;

    const TEST_URL: &str = "fuchsia-pkg://test";

    #[fuchsia::test]
    fn inspect_repo_disallows_duplicated_dirs() {
        let _exec = fuchsia_async::LocalExecutor::new();
        let inspect_repo = Arc::new(InspectRepository::default());
        let moniker = ExtendedMoniker::parse_str("./a/b/foo").unwrap();
        let identity = Arc::new(ComponentIdentity::new(moniker, TEST_URL));

        let (proxy, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("create directory proxy");

        Arc::clone(&inspect_repo).handle(Event {
            timestamp: zx::Time::get_monotonic(),
            payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                component: Arc::clone(&identity),
                directory: proxy,
            }),
        });

        let (proxy, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("create directory proxy");

        Arc::clone(&inspect_repo).handle(Event {
            timestamp: zx::Time::get_monotonic(),
            payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                component: Arc::clone(&identity),
                directory: proxy,
            }),
        });

        assert!(inspect_repo.inner.read().get(&identity).is_some());
    }

    #[fuchsia::test]
    async fn repo_removes_entries_when_inspect_is_disconnected() {
        let data_repo = Arc::new(InspectRepository::default());
        let moniker = ExtendedMoniker::parse_str("./a/b/foo").unwrap();
        let identity = Arc::new(ComponentIdentity::new(moniker, TEST_URL));
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("create directory proxy");
        {
            Arc::clone(&data_repo).handle(Event {
                timestamp: zx::Time::get_monotonic(),
                payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                    component: Arc::clone(&identity),
                    directory: proxy,
                }),
            });
            assert!(data_repo.inner.read().get(&identity).is_some());
        }
        drop(server_end);
        while data_repo.inner.read().get(&identity).is_some() {
            fasync::Timer::new(fasync::Time::after(100_i64.millis())).await;
        }
    }

    #[fuchsia::test]
    async fn repo_integrates_with_the_pipeline() {
        let selector = selectors::parse_selector::<FastError>(r#"a/b/foo:root"#).unwrap();
        let static_selectors_opt = Some(vec![selector]);
        let pipeline = Arc::new(Pipeline::for_test(static_selectors_opt));
        let data_repo = Arc::new(InspectRepository::new(vec![Arc::downgrade(&pipeline)]));
        let moniker = ExtendedMoniker::parse_str("./a/b/foo").unwrap();
        let identity = Arc::new(ComponentIdentity::new(moniker.clone(), TEST_URL));
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("create directory proxy");
        {
            Arc::clone(&data_repo).handle(Event {
                timestamp: zx::Time::get_monotonic(),
                payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                    component: Arc::clone(&identity),
                    directory: proxy,
                }),
            });
            assert!(data_repo.inner.read().get(&identity).is_some());
            assert!(pipeline.read().static_selectors_matchers().unwrap().contains_key(&moniker));
        }

        // When the directory disconnects, both the pipeline matchers and the repo are cleaned
        drop(server_end);
        while data_repo.inner.read().get(&identity).is_some() {
            fasync::Timer::new(fasync::Time::after(100_i64.millis())).await;
        }

        assert!(!pipeline.read().static_selectors_matchers().unwrap().contains_key(&moniker));
    }

    #[fuchsia::test]
    fn data_repo_filters_inspect_by_selectors() {
        let _exec = fuchsia_async::LocalExecutor::new();
        let data_repo = Arc::new(InspectRepository::default());
        let moniker = ExtendedMoniker::parse_str("./a/b/foo").unwrap();
        let identity = Arc::new(ComponentIdentity::new(moniker, TEST_URL));

        Arc::clone(&data_repo).handle(Event {
            timestamp: zx::Time::get_monotonic(),
            payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                component: Arc::clone(&identity),
                directory: fuchsia_fs::directory::open_in_namespace(
                    "/tmp",
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                )
                .expect("open root"),
            }),
        });

        let moniker2 = ExtendedMoniker::parse_str("./a/b/foo2").unwrap();
        let identity2 = Arc::new(ComponentIdentity::new(moniker2, TEST_URL));

        Arc::clone(&data_repo).handle(Event {
            timestamp: zx::Time::get_monotonic(),
            payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                component: Arc::clone(&identity2),
                directory: fuchsia_fs::directory::open_in_namespace(
                    "/tmp",
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                )
                .expect("open root"),
            }),
        });

        assert_eq!(2, data_repo.inner.read().fetch_inspect_data(&None, None).len());

        let selectors = Some(vec![
            selectors::parse_selector::<FastError>("a/b/foo:root").expect("parse selector")
        ]);
        assert_eq!(1, data_repo.inner.read().fetch_inspect_data(&selectors, None).len());

        let selectors = Some(vec![
            selectors::parse_selector::<FastError>("a/b/f*:root").expect("parse selector")
        ]);
        assert_eq!(2, data_repo.inner.read().fetch_inspect_data(&selectors, None).len());

        let selectors =
            Some(vec![selectors::parse_selector::<FastError>("foo:root").expect("parse selector")]);
        assert_eq!(0, data_repo.inner.read().fetch_inspect_data(&selectors, None).len());
    }
}
