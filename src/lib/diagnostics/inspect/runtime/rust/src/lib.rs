// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

//! # Inspect Runtime
//!
//! This library contains the necessary functions to serve inspect from a component.

use fidl::endpoints::ClientEnd;
use fidl_fuchsia_inspect::{InspectSinkMarker, InspectSinkPublishRequest};
use fuchsia_async as fasync;
use fuchsia_component::client;
use fuchsia_inspect::Inspector;
use tracing::error;

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

    /// An optional name value which will show up in the metadata of snapshots
    /// taken from this `fuchsia.inspect.Tree` server.
    pub(crate) tree_name: Option<String>,

    /// Channel over which the InspectSink protocol will be used.
    pub(crate) inspect_sink_client: Option<ClientEnd<InspectSinkMarker>>,
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
    pub fn on_inspect_sink_client(mut self, client: ClientEnd<InspectSinkMarker>) -> Self {
        self.inspect_sink_client = Some(client);
        self
    }
}

/// Spawns a server handling `fuchsia.inspect.Tree` requests and a handle
/// to the `fuchsia.inspect.Tree` is published using `fuchsia.inspect.InspectSink`.
///
/// Whenever the client wishes to stop publishing Inspect, the Task may be dropped.
///
/// `None` will be returned on FIDL failures. This includes:
/// * Failing to convert a FIDL endpoint for `fuchsia.inspect.Tree`'s `TreeMarker` into a stream
/// * Failing to connect to the `InspectSink` protocol
/// * Failing to send the connection over the wire
#[must_use]
pub fn publish(inspector: &Inspector, options: PublishOptions) -> Option<fasync::Task<()>> {
    let PublishOptions { vmo_preference, tree_name, inspect_sink_client } = options;
    let (server_task, tree) = match service::spawn_tree_server(inspector.clone(), vmo_preference) {
        Ok((task, tree)) => (task, Some(tree)),
        Err(err) => {
            error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
            return None;
        }
    };

    let inspect_sink = match inspect_sink_client {
        None => match client::connect_to_protocol::<InspectSinkMarker>() {
            Ok(inspect_sink) => inspect_sink,
            Err(err) => {
                error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
                return None;
            }
        },
        Some(client_end) => match client_end.into_proxy() {
            Ok(proxy) => proxy,
            Err(err) => {
                error!(%err, "failed to convert ClientEnd to Proxy");
                return None;
            }
        },
    };

    if let Err(err) = inspect_sink.publish(InspectSinkPublishRequest {
        tree,
        name: tree_name,
        ..InspectSinkPublishRequest::default()
    }) {
        error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
        return None;
    }

    Some(server_task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use component_events::{
        events::{EventStream, Started},
        matcher::EventMatcher,
    };
    use diagnostics_assertions::assert_json_diff;
    use diagnostics_reader::{ArchiveReader, Inspect};
    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_inspect::{InspectSinkRequest, InspectSinkRequestStream};
    use fuchsia_component_test::ScopedInstance;
    use fuchsia_inspect::{reader::read, InspectorConfig};
    use fuchsia_zircon as zx;
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
                .on_inspect_sink_client(ClientEnd::<InspectSinkMarker>::new(client)),
        );
        let mut request_stream =
            InspectSinkRequestStream::from_channel(fidl::AsyncChannel::from_channel(server));

        let tree = request_stream.next().await.unwrap();

        assert_matches!(tree, Ok(InspectSinkRequest::Publish {
            payload: InspectSinkPublishRequest { tree: Some(tree), .. }, ..}) => {
                let hierarchy = read(&tree.into_proxy().unwrap()).await.unwrap();
                assert_json_diff!(hierarchy, root: {
                    hello: "world"
                });
            }
        );

        assert!(request_stream.next().await.is_none());
    }
}
