// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::events::router::EventConsumer;
use crate::events::types::{Event, EventPayload, InspectSinkRequestedPayload};
use crate::identity::ComponentIdentity;
use crate::inspect::container::InspectHandle;
use crate::inspect::repository::InspectRepository;
use anyhow::Error;
use fidl::endpoints::{ControlHandle, Responder};
use futures::StreamExt;
use std::sync::Arc;
use tracing::warn;
use {fidl_fuchsia_inspect as finspect, fuchsia_async as fasync};

pub struct InspectSinkServer {
    /// Shared repository holding the Inspect handles.
    repo: Arc<InspectRepository>,

    /// Scope holding all tasks associated with this server.
    scope: fasync::Scope,
}

impl InspectSinkServer {
    /// Construct a server.
    pub fn new(repo: Arc<InspectRepository>, scope: fasync::Scope) -> Self {
        Self { repo, scope }
    }

    /// Handle incoming events. Mainly for use in EventConsumer impl.
    fn spawn(&self, component: Arc<ComponentIdentity>, stream: finspect::InspectSinkRequestStream) {
        let repo = Arc::clone(&self.repo);
        self.scope.spawn(async move {
            if let Err(e) = Self::handle_requests(repo, component, stream).await {
                warn!("error handling InspectSink requests: {e}");
            }
        });
    }

    async fn handle_requests(
        repo: Arc<InspectRepository>,
        component: Arc<ComponentIdentity>,
        mut stream: finspect::InspectSinkRequestStream,
    ) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            match request {
                finspect::InspectSinkRequest::Publish {
                    payload: finspect::InspectSinkPublishRequest { tree: Some(tree), name, .. },
                    ..
                } => repo.add_inspect_handle(
                    Arc::clone(&component),
                    InspectHandle::tree(tree.into_proxy(), name),
                ),
                finspect::InspectSinkRequest::Publish {
                    payload: finspect::InspectSinkPublishRequest { tree: None, name, .. },
                    control_handle,
                } => {
                    warn!(name, %component, "InspectSink/Publish without a tree");
                    control_handle.shutdown();
                }
                finspect::InspectSinkRequest::Escrow {
                    payload:
                        finspect::InspectSinkEscrowRequest {
                            vmo: Some(vmo),
                            name,
                            token: Some(token),
                            tree,
                            ..
                        },
                    ..
                } => {
                    repo.escrow_handle(
                        Arc::clone(&component),
                        vmo,
                        token,
                        name,
                        tree.map(zx::Koid::from_raw),
                    );
                }
                finspect::InspectSinkRequest::Escrow {
                    control_handle,
                    payload: finspect::InspectSinkEscrowRequest { vmo, token, .. },
                } => {
                    warn!(
                        %component,
                        has_vmo = vmo.is_some(),
                        has_token = token.is_some(),
                        "Attempted to escrow inspect without required data"
                    );
                    control_handle.shutdown();
                }
                finspect::InspectSinkRequest::FetchEscrow {
                    responder,
                    payload:
                        finspect::InspectSinkFetchEscrowRequest { tree, token: Some(token), .. },
                } => {
                    let vmo = repo.fetch_escrow(Arc::clone(&component), token, tree);
                    let _ = responder.send(finspect::InspectSinkFetchEscrowResponse {
                        vmo,
                        ..Default::default()
                    });
                }
                finspect::InspectSinkRequest::FetchEscrow {
                    responder,
                    payload: finspect::InspectSinkFetchEscrowRequest { token: None, .. },
                } => {
                    warn!(%component, "Attempted to fetch escrowed inspect with invalid data");
                    responder.control_handle().shutdown();
                }
                finspect::InspectSinkRequest::_UnknownMethod {
                    ordinal,
                    control_handle,
                    method_type,
                    ..
                } => {
                    warn!(ordinal, ?method_type, "Received unknown request for InspectSink");
                    // Close the connection if we receive an unknown interaction.
                    control_handle.shutdown();
                }
            }
        }

        Ok(())
    }
}

impl EventConsumer for InspectSinkServer {
    fn handle(self: Arc<Self>, event: Event) {
        match event.payload {
            EventPayload::InspectSinkRequested(InspectSinkRequestedPayload {
                component,
                request_stream,
            }) => {
                self.spawn(component, request_stream);
            }
            _ => unreachable!("InspectSinkServer is only subscribed to InspectSinkRequested"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::router::EventConsumer;
    use crate::events::types::{Event, EventPayload, InspectSinkRequestedPayload};
    use crate::identity::ComponentIdentity;
    use crate::inspect::container::InspectHandle;
    use crate::inspect::repository::InspectRepository;
    use crate::pipeline::StaticHierarchyAllowlist;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_json_diff;
    use fidl::endpoints::{create_proxy_and_stream, create_request_stream, ClientEnd};
    use fidl_fuchsia_inspect::{
        InspectSinkMarker, InspectSinkProxy, InspectSinkPublishRequest, TreeMarker,
    };
    use fuchsia_async::Task;
    use fuchsia_inspect::reader::read;
    use fuchsia_inspect::Inspector;
    use futures::Future;
    use inspect_runtime::service::spawn_tree_server_with_stream;
    use inspect_runtime::TreeServerSendPreference;
    use selectors::VerboseError;
    use std::sync::Arc;
    use zx::{self as zx, AsHandleRef};

    struct TestHarness {
        /// Associates a faux component via ComponentIdentity with an InspectSinkProxy
        proxy_pairs: Vec<(Arc<ComponentIdentity>, Option<InspectSinkProxy>)>,

        /// The underlying repository.
        repo: Arc<InspectRepository>,

        /// The server that would be held by the Archivist.
        _server: Arc<InspectSinkServer>,

        /// The koids of the published TreeProxies in the order they were published.
        koids: Vec<zx::Koid>,

        /// The servers for each component's Tree protocol
        tree_pairs: Vec<(Arc<ComponentIdentity>, Option<Task<()>>)>,

        scope: Option<fasync::Scope>,
    }

    impl TestHarness {
        /// Construct an InspectSinkServer with a ComponentIdentity/InspectSinkProxy pair
        /// for each input ComponentIdentity.
        fn new(identity: Vec<Arc<ComponentIdentity>>) -> Self {
            let mut proxy_pairs = vec![];
            let repo = Arc::new(InspectRepository::new(vec![], fasync::Scope::new()));
            let scope = fasync::Scope::new();
            let server = Arc::new(InspectSinkServer::new(Arc::clone(&repo), scope.new_child()));
            for id in identity.into_iter() {
                let (proxy, request_stream) =
                    create_proxy_and_stream::<InspectSinkMarker>().unwrap();

                Arc::clone(&server).handle(Event {
                    timestamp: zx::BootInstant::get(),
                    payload: EventPayload::InspectSinkRequested(InspectSinkRequestedPayload {
                        component: Arc::clone(&id),
                        request_stream,
                    }),
                });

                proxy_pairs.push((id, Some(proxy)));
            }

            Self {
                proxy_pairs,
                repo,
                _server: server,
                koids: vec![],
                tree_pairs: vec![],
                scope: Some(scope),
            }
        }

        /// Publish `tree` via the proxy associated with `component`.
        fn publish(&mut self, component: &Arc<ComponentIdentity>, tree: ClientEnd<TreeMarker>) {
            for (id, proxy) in &self.proxy_pairs {
                if id != component {
                    continue;
                }

                if let Some(proxy) = &proxy {
                    self.koids.push(tree.as_handle_ref().get_koid().unwrap());
                    proxy
                        .publish(InspectSinkPublishRequest {
                            tree: Some(tree),
                            ..Default::default()
                        })
                        .unwrap();
                    return;
                } else {
                    panic!("cannot publish on stopped server/proxy pair");
                }
            }
        }

        /// Start a TreeProxy server and return the proxy.
        fn serve(
            &mut self,
            component: Arc<ComponentIdentity>,
            inspector: Inspector,
            settings: TreeServerSendPreference,
        ) -> ClientEnd<TreeMarker> {
            let (tree, request_stream) = create_request_stream::<TreeMarker>();
            let server = spawn_tree_server_with_stream(inspector, settings, request_stream);
            self.tree_pairs.push((component, Some(server)));
            tree
        }

        /// Drop the server(s) associated with `component`, as initialized by `serve`.
        fn drop_tree_servers(&mut self, component: &Arc<ComponentIdentity>) {
            for (id, ref mut server) in &mut self.tree_pairs {
                if id != component {
                    continue;
                }

                if server.is_none() {
                    continue;
                }

                server.take();
            }
        }

        /// The published koids, with 0 referring to the first published tree.
        fn published_koids(&self) -> &[zx::Koid] {
            &self.koids
        }

        /// Execute closure `assertions` on the `InspectArtifactsContainer` associated with
        /// `identity`.
        ///
        /// This function will wait for data to be available in `self.repo`, and therefore
        /// might hang indefinitely if the data never appears. This is not a problem since
        /// it is a unit test and `fx test` has timeouts available.
        async fn assert<const N: usize, F, Fut>(
            &self,
            identity: &Arc<ComponentIdentity>,
            koids: [zx::Koid; N],
            assertions: F,
        ) where
            F: FnOnce([Arc<InspectHandle>; N]) -> Fut,
            Fut: Future<Output = ()>,
        {
            self.repo.wait_for_artifact(identity).await;
            let containers = self.repo.fetch_inspect_data(
                &Some(vec![selectors::parse_selector::<VerboseError>(&format!("{identity}:root"))
                    .expect("parse selector")]),
                StaticHierarchyAllowlist::new_disabled(),
            );
            assert_eq!(containers.len(), 1);
            assertions(
                koids
                    .iter()
                    .map(|koid| {
                        containers[0]
                            .inspect_handles
                            .iter()
                            .filter_map(|h| h.upgrade())
                            .find(|handle| handle.koid() == *koid)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            )
            .await;
        }

        /// Drops all published proxies, stops the server, and waits for it to complete.
        async fn stop_all(&mut self) {
            for (_, ref mut proxy) in &mut self.proxy_pairs {
                proxy.take();
            }

            self.scope.take().unwrap().close().await;
        }

        async fn wait_until_gone(&self, component: &Arc<ComponentIdentity>) {
            self.repo.wait_until_gone(component).await;
        }
    }

    #[fuchsia::test]
    async fn connect() {
        let identity: Arc<ComponentIdentity> = Arc::new(vec!["a", "b", "foo.cm"].into());

        let mut test = TestHarness::new(vec![Arc::clone(&identity)]);

        let insp = Inspector::default();
        insp.root().record_int("int", 0);
        let tree = test.serve(Arc::clone(&identity), insp, TreeServerSendPreference::default());
        test.publish(&identity, tree);

        let koid = test.published_koids()[0];

        test.assert(&identity, [koid], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, .. } => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       int: 0i64,
                   });
            });
        })
        .await;
    }

    #[fuchsia::test]
    async fn publish_multiple_times_on_the_same_connection() {
        let identity: Arc<ComponentIdentity> = Arc::new(vec!["a", "b", "foo.cm"].into());

        let mut test = TestHarness::new(vec![Arc::clone(&identity)]);

        let insp = Inspector::default();
        insp.root().record_int("int", 0);
        let tree = test.serve(Arc::clone(&identity), insp, TreeServerSendPreference::default());

        let other_insp = Inspector::default();
        other_insp.root().record_double("double", 1.24);
        let other_tree =
            test.serve(Arc::clone(&identity), other_insp, TreeServerSendPreference::default());

        test.publish(&identity, tree);
        test.publish(&identity, other_tree);

        let koid0 = test.published_koids()[0];
        let koid1 = test.published_koids()[1];

        test.assert(&identity, [koid0, koid1], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, ..} => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       int: 0i64,
                   });
            });

            assert_matches!(
                handles[1].as_ref(),
                InspectHandle::Tree { proxy: tree, .. } => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       double: 1.24,
                   });
            });
        })
        .await;
    }

    #[fuchsia::test]
    async fn tree_remains_after_inspect_sink_disconnects() {
        let identity: Arc<ComponentIdentity> = Arc::new(vec!["a", "b", "foo.cm"].into());

        let mut test = TestHarness::new(vec![Arc::clone(&identity)]);

        let insp = Inspector::default();
        insp.root().record_int("int", 0);
        let tree = test.serve(Arc::clone(&identity), insp, TreeServerSendPreference::default());
        test.publish(&identity, tree);

        let koid = test.published_koids()[0];

        test.assert(&identity, [koid], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, .. } => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       int: 0i64,
                   });
            });
        })
        .await;

        test.stop_all().await;

        // the data must remain present as long as the tree server started above is alive
        test.assert(&identity, [koid], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, ..} => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       int: 0i64,
                   });
            });
        })
        .await;
    }

    #[fuchsia::test]
    async fn connect_with_multiple_proxies() {
        let identities: Vec<Arc<ComponentIdentity>> = vec![
            Arc::new(vec!["a", "b", "foo.cm"].into()),
            Arc::new(vec!["a", "b", "foo2.cm"].into()),
        ];

        let mut test = TestHarness::new(identities.clone());

        let insp = Inspector::default();
        insp.root().record_int("int", 0);
        let tree =
            test.serve(Arc::clone(&identities[0]), insp, TreeServerSendPreference::default());

        let insp2 = Inspector::default();
        insp2.root().record_bool("is_insp2", true);
        let tree2 =
            test.serve(Arc::clone(&identities[1]), insp2, TreeServerSendPreference::default());

        test.publish(&identities[0], tree);
        test.publish(&identities[1], tree2);

        let koid_component_0 = test.published_koids()[0];
        let koid_component_1 = test.published_koids()[1];

        test.assert(&identities[0], [koid_component_0], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, .. } => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       int: 0i64,
                   });
            });
        })
        .await;

        test.assert(&identities[1], [koid_component_1], |handles| async move {
            assert_matches!(
                handles[0].as_ref(),
                InspectHandle::Tree { proxy: tree, .. } => {
                   let hierarchy = read(tree).await.unwrap();
                   assert_json_diff!(hierarchy, root: {
                       is_insp2: true,
                   });
            });
        })
        .await;
    }

    #[fuchsia::test]
    async fn dropping_tree_removes_component_identity_from_repo() {
        let identity: Arc<ComponentIdentity> = Arc::new(vec!["a", "b", "foo.cm"].into());

        let mut test = TestHarness::new(vec![Arc::clone(&identity)]);

        let tree = test.serve(
            Arc::clone(&identity),
            Inspector::default(),
            TreeServerSendPreference::default(),
        );
        test.publish(&identity, tree);

        test.stop_all().await;

        // this executing to completion means the identity was present
        test.assert(&identity, [test.published_koids()[0]], |handles: [_; 1]| {
            assert_eq!(handles.len(), 1);
            async {}
        })
        .await;

        test.drop_tree_servers(&identity);

        // this executing to completion means the identity is not there anymore; we know
        // it previously was present
        test.wait_until_gone(&identity).await;
    }
}
