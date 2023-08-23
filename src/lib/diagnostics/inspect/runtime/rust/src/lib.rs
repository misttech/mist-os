// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

//! # Inspect Runtime
//!
//! This library contains the necessary functions to serve inspect from a component.

use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_inspect::{InspectSinkMarker, InspectSinkPublishRequest, TreeMarker};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_component::client;
use fuchsia_component::server::{ServiceFs, ServiceObjTrait};
use fuchsia_inspect::{Error, Inspector};
use futures::prelude::*;
use std::sync::Arc;
use tracing::{error, warn};
use vfs::{
    directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path, pseudo_directory,
};

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
#[derive(Default, Clone)]
pub struct PublishOptions {
    /// This specifies how the VMO should be sent over the `fuchsia.inspect.Tree` server.
    ///
    /// Default behavior is
    /// `TreeServerSendPreference::Frozen { on_failure: TreeServerSendPreference::Live }`.
    pub(crate) vmo_preference: TreeServerSendPreference,

    /// An optional name value which will show up in the metadata of snapshots
    /// taken from this `fuchsia.inspect.Tree` server.
    pub(crate) tree_name: Option<String>,
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
}

/// Spawns a server handling `fuchsia.inspect.Tree` requests and a handle
/// to the `fuchsia.inspect.Tree` is published using `fuchsia.inspect.InspectSink`.
///
/// The returned `fuchsia_async::Task` must be polled to handle incoming Tree
/// requests. Whenever the client wishes to stop publishing Inspect, the Task may
/// be dropped.
///
/// `None` will be returned on FIDL failures. This includes:
/// * Failing to convert a FIDL endpoint for `fuchsia.inspect.Tree`'s `TreeMarker` into a stream
/// * Failing to connect to the `InspectSink` protocol
/// * Failing to send the connection over the wire
pub fn publish(inspector: &Inspector, options: PublishOptions) -> Option<fasync::Task<()>> {
    let name = options.tree_name.clone();
    let (server_task, tree) = match service::spawn_tree_server(inspector.clone(), options) {
        Ok((task, tree)) => (task, Some(tree)),
        Err(err) => {
            error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
            return None;
        }
    };

    let inspect_sink = match client::connect_to_protocol::<InspectSinkMarker>() {
        Ok(inspect_sink) => inspect_sink,
        Err(err) => {
            error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
            return None;
        }
    };

    if let Err(err) = inspect_sink.publish(InspectSinkPublishRequest {
        tree,
        name,
        ..InspectSinkPublishRequest::default()
    }) {
        error!(%err, "failed to spawn the fuchsia.inspect.Tree server");
        return None;
    }

    Some(server_task)
}

/// Directory within the outgoing directory of a component where the diagnostics service should be
/// added.
pub const DIAGNOSTICS_DIR: &str = "diagnostics";

/// Spawns a server with options for handling `fuchsia.inspect.Tree` requests in
/// the outgoing diagnostics directory.
pub fn serve_with_options<'a, ServiceObjTy: ServiceObjTrait>(
    inspector: &Inspector,
    options: PublishOptions,
    service_fs: &mut ServiceFs<ServiceObjTy>,
) -> Result<(), Error> {
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
        .map_err(|e| Error::fidl(e.into()))?;
    let dir = create_diagnostics_dir_with_options(inspector.clone(), options);
    let server_end = server.into_channel().into();
    let scope = ExecutionScope::new();
    dir.open(scope, fio::OpenFlags::RIGHT_READABLE, Path::dot(), server_end);
    service_fs.add_remote(DIAGNOSTICS_DIR, proxy);

    Ok(())
}

/// Spawns a server for handling `fuchsia.inspect.Tree` requests in the outgoing diagnostics
/// directory.
pub fn serve<'a, ServiceObjTy: ServiceObjTrait>(
    inspector: &Inspector,
    service_fs: &mut ServiceFs<ServiceObjTy>,
) -> Result<(), Error> {
    serve_with_options(inspector, PublishOptions::default(), service_fs)
}

/// Creates the outgoing diagnostics directory with options. Should be added to the component's
/// outgoing directory at `DIAGNOSTICS_DIR`. Use `serve_with_options` if the component's outgoing
/// directory is served by `ServiceFs`.
pub fn create_diagnostics_dir_with_options(
    inspector: fuchsia_inspect::Inspector,
    options: PublishOptions,
) -> Arc<dyn DirectoryEntry> {
    pseudo_directory! {
        TreeMarker::PROTOCOL_NAME =>
            vfs::service::host(move |stream| {
                service::handle_request_stream(
                    inspector.clone(),
                    options.clone(),
                    stream
                ).unwrap_or_else(|e| {
                    warn!(
                        "error handling fuchsia.inspect/Tree connection: {e:#}"
                    );
                })
            }),
    }
}

/// Creates the outgoing diagnostics directory. Should be added to the component's outgoing
/// directory at `DIAGNOSTICS_DIR`. Use `serve` if the component's outgoing directory is served by
/// `ServiceFs`.
pub fn create_diagnostics_dir(inspector: Inspector) -> Arc<dyn DirectoryEntry> {
    create_diagnostics_dir_with_options(inspector, PublishOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use component_debug::dirs::{open_instance_dir_root_readable, OpenDirType};
    use component_events::{
        events::{EventStream, Started},
        matcher::EventMatcher,
    };
    use diagnostics_reader::{ArchiveReader, Inspect};
    use fidl_fuchsia_sys2 as fsys;
    use fuchsia_async as fasync;
    use fuchsia_component::{client, server::ServiceObj};
    use fuchsia_component_test::ScopedInstance;
    use fuchsia_inspect::{
        reader::{read, DiagnosticsHierarchy},
        testing::assert_json_diff,
        InspectorConfig,
    };
    use fuchsia_zircon::DurationNum;
    use futures::FutureExt;

    const TEST_PUBLISH_COMPONENT_URL: &str = "#meta/inspect_test_component.cm";
    const TEST_SERVE_COMPONENT_URL: &str = "#meta/inspect_test_component_serve_fn.cm";

    #[fuchsia::test]
    async fn new_no_op() {
        let inspector = Inspector::new(InspectorConfig::default().no_op());
        assert!(!inspector.is_valid());

        // Ensure serve doesn't crash on a No-Op inspector.
        // The idea is that in this context, serve will hang if the server is running
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
    async fn serve_new_no_op() -> Result<(), Error> {
        let mut fs: ServiceFs<ServiceObj<'_, ()>> = ServiceFs::new();

        let inspector = Inspector::new(InspectorConfig::default().no_op());
        assert!(!inspector.is_valid());

        // Ensure serve doesn't crash on a No-Op inspector
        serve(&inspector, &mut fs)
    }

    #[fuchsia::test]
    async fn serve_connect_to_service() -> Result<(), anyhow::Error> {
        let mut event_stream = EventStream::open().await.unwrap();

        let app = ScopedInstance::new("coll".to_string(), TEST_SERVE_COMPONENT_URL.to_string())
            .await
            .expect("failed to create test component");

        let started_stream = EventMatcher::ok()
            .moniker_regex(app.child_name().to_owned())
            .wait::<Started>(&mut event_stream);

        app.connect_to_binder().expect("failed to connect to Binder protocol");

        started_stream.await.expect("failed to observe Started event");

        let realm_query = client::connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();
        let moniker = format!("./coll:{}", app.child_name());
        let moniker = moniker.as_str().try_into().unwrap();
        let out_dir = loop {
            if let Ok(out_dir) =
                open_instance_dir_root_readable(&moniker, OpenDirType::Outgoing, &realm_query).await
            {
                break out_dir;
            }
            fasync::Timer::new(fasync::Time::after(100_i64.millis())).await;
        };
        let diagnostics_dir = fuchsia_fs::directory::open_directory(
            &out_dir,
            "diagnostics",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await
        .expect("opened diagnostics");

        let tree = client::connect_to_protocol_at_dir_root::<TreeMarker>(&diagnostics_dir)
            .expect("connected to tree");

        let hierarchy = read(&tree).await?;
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
}
