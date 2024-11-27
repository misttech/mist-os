// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

//! # Inspect Runtime
//!
//! This library contains the necessary functions to serve inspect from a component.

use fidl::endpoints::ClientEnd;
use fidl::AsHandleRef;
use fuchsia_component::client;
use fuchsia_inspect::Inspector;
use pin_project::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::error;
use {fidl_fuchsia_inspect as finspect, fuchsia_async as fasync};

#[cfg(fuchsia_api_level_at_least = "HEAD")]
pub use finspect::EscrowToken;

pub mod service;

/// A setting for the fuchsia.inspect.Tree server that indicates how the server should send
/// the Inspector's VMO. For fallible methods of sending, a fallback is also set.
#[derive(Clone)]
pub enum TreeServerSendPreference {
    /// Frozen denotes sending a copy-on-write VMO.
    /// `on_failure` refers to failure behavior, as not all VMOs
    /// can be frozen. In particular, freezing a VMO requires writing to it,
    /// so if an Inspector is created with a read-only VMO, freezing will fail.
    ///
    /// Failure behavior should be one of Live or DeepCopy.
    ///
    /// Frozen { on_failure: Live } is the default value of TreeServerSendPreference.
    Frozen { on_failure: Box<TreeServerSendPreference> },

    /// Live denotes sending a live handle to the VMO.
    ///
    /// A client might want this behavior if they have time sensitive writes
    /// to the VMO, because copy-on-write behavior causes the initial write
    /// to a page to be around 1% slower.
    Live,

    /// DeepCopy will send a private copy of the VMO. This should probably
    /// not be a client's first choice, as Frozen(DeepCopy) will provide the
    /// same semantic behavior while possibly avoiding an expensive copy.
    ///
    /// A client might want this behavior if they have time sensitive writes
    /// to the VMO, because copy-on-write behavior causes the initial write
    /// to a page to be around 1% slower.
    DeepCopy,
}

impl TreeServerSendPreference {
    /// Create a new [`TreeServerSendPreference`] that sends a frozen/copy-on-write VMO of the tree,
    /// falling back to the specified `failure_mode` if a frozen VMO cannot be provided.
    ///
    /// # Arguments
    ///
    /// * `failure_mode` - Fallback behavior to use if freezing the Inspect VMO fails.
    ///
    pub fn frozen_or(failure_mode: TreeServerSendPreference) -> Self {
        TreeServerSendPreference::Frozen { on_failure: Box::new(failure_mode) }
    }
}

impl Default for TreeServerSendPreference {
    fn default() -> Self {
        TreeServerSendPreference::frozen_or(TreeServerSendPreference::Live)
    }
}

/// Optional settings for serving `fuchsia.inspect.Tree`
#[derive(Default)]
pub struct PublishOptions {
    /// This specifies how the VMO should be sent over the `fuchsia.inspect.Tree` server.
    ///
    /// Default behavior is
    /// `TreeServerSendPreference::Frozen { on_failure: TreeServerSendPreference::Live }`.
    pub(crate) vmo_preference: TreeServerSendPreference,

    /// An name value which will show up in the metadata of snapshots
    /// taken from this `fuchsia.inspect.Tree` server. Defaults to
    /// fuchsia.inspect#DEFAULT_TREE_NAME.
    pub(crate) tree_name: Option<String>,

    /// Channel over which the InspectSink protocol will be used.
    pub(crate) inspect_sink_client: Option<ClientEnd<finspect::InspectSinkMarker>>,
}

impl PublishOptions {
    /// This specifies how the VMO should be sent over the `fuchsia.inspect.Tree` server.
    ///
    /// Default behavior is
    /// `TreeServerSendPreference::Frozen { on_failure: TreeServerSendPreference::Live }`.
    pub fn send_vmo_preference(mut self, preference: TreeServerSendPreference) -> Self {
        self.vmo_preference = preference;
        self
    }

    /// This sets an optional name value which will show up in the metadata of snapshots
    /// taken from this `fuchsia.inspect.Tree` server.
    ///
    /// Default behavior is an empty string.
    pub fn inspect_tree_name(mut self, name: impl Into<String>) -> Self {
        self.tree_name = Some(name.into());
        self
    }

    /// This allows the client to provide the InspectSink client channel.
    pub fn on_inspect_sink_client(
        mut self,
        client: ClientEnd<finspect::InspectSinkMarker>,
    ) -> Self {
        self.inspect_sink_client = Some(client);
        self
    }
}

/// Spawns a server handling `fuchsia.inspect.Tree` requests and a handle
/// to the `fuchsia.inspect.Tree` is published using `fuchsia.inspect.InspectSink`.
///
/// Whenever the client wishes to stop publishing Inspect, the Controller may be dropped.
///
/// `None` will be returned on FIDL failures. This includes:
/// * Failing to convert a FIDL endpoint for `fuchsia.inspect.Tree`'s `TreeMarker` into a stream
/// * Failing to connect to the `InspectSink` protocol
/// * Failing to send the connection over the wire
#[must_use]
pub fn publish(
    inspector: &Inspector,
    options: PublishOptions,
) -> Option<PublishedInspectController> {
    let PublishOptions { vmo_preference, tree_name, inspect_sink_client } = options;
    let (server_task, tree) = match service::spawn_tree_server(inspector.clone(), vmo_preference) {
        Ok((task, tree)) => (task, tree),
        Err(err) => {
            error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
            return None;
        }
    };

    let inspect_sink = match inspect_sink_client {
        None => match client::connect_to_protocol::<finspect::InspectSinkMarker>() {
            Ok(inspect_sink) => inspect_sink,
            Err(err) => {
                error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
                return None;
            }
        },
        Some(client_end) => client_end.into_proxy(),
    };

    // unwrap: safe since we have a valid tree handle coming from the server we spawn.
    let tree_koid = tree.basic_info().unwrap().koid;
    if let Err(err) = inspect_sink.publish(finspect::InspectSinkPublishRequest {
        tree: Some(tree),
        name: tree_name,
        ..finspect::InspectSinkPublishRequest::default()
    }) {
        error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
        return None;
    }

    Some(PublishedInspectController::new(inspector.clone(), server_task, tree_koid))
}

#[pin_project]
pub struct PublishedInspectController {
    #[pin]
    task: fasync::Task<()>,
    inspector: Inspector,
    tree_koid: zx::Koid,
}

#[cfg(fuchsia_api_level_at_least = "HEAD")]
#[derive(Default)]
pub struct EscrowOptions {
    name: Option<String>,
    inspect_sink: Option<finspect::InspectSinkProxy>,
}

#[cfg(fuchsia_api_level_at_least = "HEAD")]
impl EscrowOptions {
    /// Sets the name with which the Inspect handle will be escrowed.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the inspect sink channel to use for escrowing.
    pub fn inspect_sink(mut self, proxy: finspect::InspectSinkProxy) -> Self {
        self.inspect_sink = Some(proxy);
        self
    }
}

impl PublishedInspectController {
    fn new(inspector: Inspector, task: fasync::Task<()>, tree_koid: zx::Koid) -> Self {
        Self { inspector, task, tree_koid }
    }

    /// Escrows a frozen copy of the VMO of the associated Inspector replacing the current live
    /// handle in the server.
    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    pub async fn escrow_frozen(self, opts: EscrowOptions) -> Option<EscrowToken> {
        let inspect_sink = match opts.inspect_sink {
            Some(proxy) => proxy,
            None => match client::connect_to_protocol::<finspect::InspectSinkMarker>() {
                Ok(inspect_sink) => inspect_sink,
                Err(err) => {
                    error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
                    return None;
                }
            },
        };
        let (ep0, ep1) = zx::EventPair::create();
        let Some(vmo) = self.inspector.frozen_vmo_copy() else {
            error!("failed to get a frozen vmo, aborting escrow");
            return None;
        };
        if let Err(err) = inspect_sink.escrow(finspect::InspectSinkEscrowRequest {
            vmo: Some(vmo),
            name: opts.name,
            token: Some(EscrowToken { token: ep0 }),
            tree: Some(self.tree_koid.raw_koid()),
            ..Default::default()
        }) {
            error!(%err, "failed to escrow inspect data");
            return None;
        }
        self.task.await;
        Some(EscrowToken { token: ep1 })
    }
}

impl Future for PublishedInspectController {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.task.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use component_events::events::{EventStream, Started};
    use component_events::matcher::EventMatcher;
    use diagnostics_assertions::assert_json_diff;
    use diagnostics_hierarchy::DiagnosticsHierarchy;
    use diagnostics_reader::{ArchiveReader, Inspect};
    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_inspect::{InspectSinkRequest, InspectSinkRequestStream};
    use fuchsia_component_test::ScopedInstance;
    use fuchsia_inspect::reader::snapshot::Snapshot;
    use fuchsia_inspect::reader::{read, PartialNodeHierarchy};
    use fuchsia_inspect::InspectorConfig;

    use futures::{FutureExt, StreamExt};

    const TEST_PUBLISH_COMPONENT_URL: &str = "#meta/inspect_test_component.cm";

    #[fuchsia::test]
    async fn new_no_op() {
        let inspector = Inspector::new(InspectorConfig::default().no_op());
        assert!(!inspector.is_valid());

        // Ensure publish doesn't crash on a No-Op inspector.
        // The idea is that in this context, publish will hang if the server is running
        // correctly. That is, if there is an error condition, it will be immediate.
        assert_matches!(
            publish(&inspector, PublishOptions::default()).unwrap().now_or_never(),
            None
        );
    }

    #[fuchsia::test]
    async fn connect_to_service() -> Result<(), anyhow::Error> {
        let mut event_stream = EventStream::open().await.unwrap();

        let app = ScopedInstance::new_with_name(
            "interesting_name".into(),
            "coll".to_string(),
            TEST_PUBLISH_COMPONENT_URL.to_string(),
        )
        .await
        .expect("failed to create test component");

        let started_stream = EventMatcher::ok()
            .moniker_regex(app.child_name().to_owned())
            .wait::<Started>(&mut event_stream);

        app.connect_to_binder().expect("failed to connect to Binder protocol");

        started_stream.await.expect("failed to observe Started event");

        let hierarchy = ArchiveReader::new()
            .add_selector("coll\\:interesting_name:root")
            .snapshot::<Inspect>()
            .await?
            .into_iter()
            .next()
            .and_then(|result| result.payload)
            .expect("one Inspect hierarchy");

        assert_json_diff!(hierarchy, root: {
            "tree-0": 0u64,
            int: 3i64,
            "lazy-node": {
                a: "test",
                child: {
                    double: 3.25,
                },
            }
        });

        Ok(())
    }

    #[fuchsia::test]
    async fn publish_new_no_op() {
        let inspector = Inspector::new(InspectorConfig::default().no_op());
        assert!(!inspector.is_valid());

        // Ensure publish doesn't crash on a No-Op inspector
        let _task = publish(&inspector, PublishOptions::default());
    }

    #[fuchsia::test]
    async fn publish_on_provided_channel() {
        let (client, server) = zx::Channel::create();
        let inspector = Inspector::default();
        inspector.root().record_string("hello", "world");
        let _inspect_sink_server_task = publish(
            &inspector,
            PublishOptions::default()
                .on_inspect_sink_client(ClientEnd::<finspect::InspectSinkMarker>::new(client)),
        );
        let mut request_stream =
            InspectSinkRequestStream::from_channel(fidl::AsyncChannel::from_channel(server));

        let tree = request_stream.next().await.unwrap();

        assert_matches!(tree, Ok(InspectSinkRequest::Publish {
            payload: finspect::InspectSinkPublishRequest { tree: Some(tree), .. }, ..}) => {
                let hierarchy = read(&tree.into_proxy()).await.unwrap();
                assert_json_diff!(hierarchy, root: {
                    hello: "world"
                });
            }
        );

        assert!(request_stream.next().await.is_none());
    }

    #[fuchsia::test]
    async fn controller_supports_escrowing_a_copy() {
        let inspector = Inspector::default();
        inspector.root().record_string("hello", "world");

        let (client, mut request_stream) = fidl::endpoints::create_request_stream().unwrap();
        let controller =
            publish(&inspector, PublishOptions::default().on_inspect_sink_client(client))
                .expect("got controller");

        let request = request_stream.next().await.unwrap();
        let tree_koid = match request {
            Ok(InspectSinkRequest::Publish {
                payload: finspect::InspectSinkPublishRequest { tree: Some(tree), .. },
                ..
            }) => tree.basic_info().unwrap().koid,
            other => {
                panic!("unexpected request: {other:?}");
            }
        };
        let (proxy, mut request_stream) =
            fidl::endpoints::create_proxy_and_stream::<finspect::InspectSinkMarker>().unwrap();
        let (client_token, request) = futures::future::join(
            controller.escrow_frozen(EscrowOptions {
                name: Some("test".into()),
                inspect_sink: Some(proxy),
            }),
            request_stream.next(),
        )
        .await;
        match request {
            Some(Ok(InspectSinkRequest::Escrow {
                payload:
                    finspect::InspectSinkEscrowRequest {
                        vmo: Some(vmo),
                        name: Some(name),
                        token: Some(EscrowToken { token }),
                        tree: Some(tree),
                        ..
                    },
                ..
            })) => {
                assert_eq!(name, "test");
                assert_eq!(tree, tree_koid.raw_koid());

                // An update to the inspector isn't reflected here, since it was  CoW.
                inspector.root().record_string("hey", "not there");

                let snapshot = Snapshot::try_from(&vmo).expect("valid vmo");
                let hierarchy: DiagnosticsHierarchy =
                    PartialNodeHierarchy::try_from(snapshot).expect("valid snapshot").into();
                assert_json_diff!(hierarchy, root: {
                    hello: "world"
                });
                assert_eq!(
                    client_token.unwrap().token.basic_info().unwrap().koid,
                    token.basic_info().unwrap().related_koid
                );
            }
            other => {
                panic!("unexpected request: {other:?}");
            }
        };
    }
}
