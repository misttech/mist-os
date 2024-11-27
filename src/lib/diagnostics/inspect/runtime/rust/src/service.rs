// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the `fuchsia.inspect.Tree` protocol server.

use crate::TreeServerSendPreference;
use anyhow::Error;
use fidl_fuchsia_inspect::{
    TreeContent, TreeMarker, TreeNameIteratorRequest, TreeNameIteratorRequestStream, TreeRequest,
    TreeRequestStream,
};
use fidl_fuchsia_mem::Buffer;
use fuchsia_async as fasync;
use fuchsia_inspect::reader::ReadableTree;
use fuchsia_inspect::Inspector;
use futures::{TryFutureExt, TryStreamExt};
use tracing::warn;
use zx::sys::ZX_CHANNEL_MAX_MSG_BYTES;

/// Runs a server for the `fuchsia.inspect.Tree` protocol. This protocol returns the VMO
/// associated with the given tree on `get_content` and allows to open linked trees (lazy nodes).
pub async fn handle_request_stream(
    inspector: Inspector,
    settings: TreeServerSendPreference,
    mut stream: TreeRequestStream,
) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await? {
        match request {
            TreeRequest::GetContent { responder } => {
                // If freezing fails, full snapshot algo needed on live duplicate
                let vmo = match settings {
                    TreeServerSendPreference::DeepCopy => inspector.copy_vmo(),
                    TreeServerSendPreference::Live => inspector.duplicate_vmo(),
                    TreeServerSendPreference::Frozen { ref on_failure } => {
                        inspector.frozen_vmo_copy().or_else(|| match **on_failure {
                            TreeServerSendPreference::DeepCopy => inspector.copy_vmo(),
                            TreeServerSendPreference::Live => inspector.duplicate_vmo(),
                            _ => None,
                        })
                    }
                };

                let buffer_data = vmo.and_then(|vmo| vmo.get_size().ok().map(|size| (vmo, size)));
                let content = TreeContent {
                    buffer: buffer_data.map(|data| Buffer { vmo: data.0, size: data.1 }),
                    ..Default::default()
                };
                responder.send(content)?;
            }
            TreeRequest::ListChildNames { tree_iterator, .. } => {
                let values = inspector.tree_names().await?;
                let request_stream = tree_iterator.into_stream();
                spawn_tree_name_iterator_server(values, request_stream)
            }
            TreeRequest::OpenChild { child_name, tree, .. } => {
                if let Ok(inspector) = inspector.read_tree(&child_name).await {
                    spawn_tree_server_with_stream(inspector, settings.clone(), tree.into_stream())
                        .detach()
                }
            }
            TreeRequest::_UnknownMethod { ordinal, method_type, .. } => {
                warn!(ordinal, ?method_type, "Unknown request");
            }
        }
    }
    Ok(())
}

/// Spawns a server for the `fuchsia.inspect.Tree` protocol. This protocol returns the VMO
/// associated with the given tree on `get_content` and allows to open linked trees (lazy nodes).
///
/// This version of the function accepts a `TreeRequestStream`, making it suitable for the
/// recursive calls performed by the `OpenChild` method on `fuchsia.inspect.Tree`.
/// `spawn_tree_server` is a more ergonomic option for spawning the root tree.
pub fn spawn_tree_server_with_stream(
    inspector: Inspector,
    settings: TreeServerSendPreference,
    stream: TreeRequestStream,
) -> fasync::Task<()> {
    fasync::Task::spawn(async move {
        handle_request_stream(inspector, settings, stream).await.unwrap_or_else(|err: Error| {
            warn!(?err, "failed to run `fuchsia.inspect.Tree` server")
        });
    })
}

/// Spawns a `fuchsia.inspect.Tree` server and returns the task handling
/// `fuchsia.inspect.Tree requests and a `ClientEnd` handle to the tree.
pub fn spawn_tree_server(
    inspector: Inspector,
    settings: TreeServerSendPreference,
) -> Result<(fasync::Task<()>, fidl::endpoints::ClientEnd<TreeMarker>), Error> {
    let (tree, server_end) = fidl::endpoints::create_endpoints::<TreeMarker>();
    let task = spawn_tree_server_with_stream(inspector, settings, server_end.into_stream());
    Ok((task, tree))
}

/// Runs a server for the `fuchsia.inspect.TreeNameIterator` protocol. This protocol returns the
/// given list of values by chunks.
fn spawn_tree_name_iterator_server(values: Vec<String>, mut stream: TreeNameIteratorRequestStream) {
    fasync::Task::spawn(
        async move {
            let mut values_iter = values.into_iter().peekable();
            while let Some(request) = stream.try_next().await? {
                match request {
                    TreeNameIteratorRequest::GetNext { responder } => {
                        let mut bytes_used: usize = 32; // Page overhead of message header + vector
                        let mut result = vec![];
                        loop {
                            match values_iter.peek() {
                                None => break,
                                Some(value) => {
                                    bytes_used += 16; // String overhead
                                    bytes_used += fidl::encoding::round_up_to_align(value.len(), 8);
                                    if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                                        break;
                                    }
                                    result.push(values_iter.next().unwrap());
                                }
                            }
                        }
                        if result.is_empty() {
                            responder.send(&[])?;
                            return Ok(());
                        }
                        responder.send(&result)?;
                    }
                    TreeNameIteratorRequest::_UnknownMethod { ordinal, method_type, .. } => {
                        warn!(ordinal, ?method_type, "Unknown request");
                    }
                }
            }
            Ok(())
        }
        .unwrap_or_else(|err: Error| warn!(?err, "failed to run tree name iterator server")),
    )
    .detach()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, assert_json_diff};
    use fidl_fuchsia_inspect::{TreeNameIteratorMarker, TreeNameIteratorProxy, TreeProxy};
    use fuchsia_async::DurationExt;
    use fuchsia_inspect::reader::{read_with_timeout, DiagnosticsHierarchy, PartialNodeHierarchy};

    use futures::FutureExt;
    use std::time::Duration;

    /// Spawns a `fuchsia.inspect.Tree` server and returns the task handling
    /// `fuchsia.inspect.Tree` requests and a `TreeProxy` handle to the tree.
    pub fn spawn_server_proxy(
        inspector: Inspector,
        settings: TreeServerSendPreference,
    ) -> Result<(fasync::Task<()>, TreeProxy), Error> {
        spawn_tree_server(inspector, settings).map(|(t, p)| (t, p.into_proxy()))
    }

    #[fuchsia::test]
    async fn get_contents() -> Result<(), Error> {
        let (_server, tree) =
            spawn_server_proxy(test_inspector(), TreeServerSendPreference::default())?;
        let tree_content = tree.get_content().await?;
        let hierarchy = parse_content(tree_content)?;
        assert_data_tree!(hierarchy, root: {
            a: 1i64,
        });
        Ok(())
    }

    #[fuchsia::test]
    async fn list_child_names() -> Result<(), Error> {
        let (_server, tree) =
            spawn_server_proxy(test_inspector(), TreeServerSendPreference::default())?;
        let (name_iterator, server_end) =
            fidl::endpoints::create_proxy::<TreeNameIteratorMarker>()?;
        tree.list_child_names(server_end)?;
        verify_iterator(name_iterator, vec!["lazy-0".to_string()]).await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn open_children() -> Result<(), Error> {
        let (_server, tree) =
            spawn_server_proxy(test_inspector(), TreeServerSendPreference::default())?;
        let (child_tree, server_end) = fidl::endpoints::create_proxy::<TreeMarker>()?;
        tree.open_child("lazy-0", server_end)?;
        let tree_content = child_tree.get_content().await?;
        let hierarchy = parse_content(tree_content)?;
        assert_data_tree!(hierarchy, root: {
            b: 2u64,
        });
        let (name_iterator, server_end) =
            fidl::endpoints::create_proxy::<TreeNameIteratorMarker>()?;
        child_tree.list_child_names(server_end)?;
        verify_iterator(name_iterator, vec!["lazy-vals-0".to_string()]).await?;

        let (child_tree_2, server_end) = fidl::endpoints::create_proxy::<TreeMarker>()?;
        child_tree.open_child("lazy-vals-0", server_end)?;
        let tree_content = child_tree_2.get_content().await?;
        let hierarchy = parse_content(tree_content)?;
        assert_data_tree!(hierarchy, root: {
            c: 3.0,
        });
        let (name_iterator, server_end) =
            fidl::endpoints::create_proxy::<TreeNameIteratorMarker>()?;
        child_tree_2.list_child_names(server_end)?;
        verify_iterator(name_iterator, vec![]).await?;

        Ok(())
    }

    #[fuchsia::test]
    async fn default_snapshots_are_private_on_success() -> Result<(), Error> {
        let inspector = test_inspector();
        let (_server, tree_copy) =
            spawn_server_proxy(inspector.clone(), TreeServerSendPreference::default())?;
        let tree_content_copy = tree_copy.get_content().await?;

        inspector.root().record_int("new", 6);

        // A tree that copies the vmo doesn't see the new int
        let hierarchy = parse_content(tree_content_copy)?;
        assert_data_tree!(hierarchy, root: {
            a: 1i64,
        });
        Ok(())
    }

    #[fuchsia::test]
    async fn force_live_snapshot() -> Result<(), Error> {
        let inspector = test_inspector();
        let (_server1, tree_cow) =
            spawn_server_proxy(inspector.clone(), TreeServerSendPreference::default())?;
        let (_server2, tree_live) =
            spawn_server_proxy(inspector.clone(), TreeServerSendPreference::Live)?;
        let tree_content_live = tree_live.get_content().await?;
        let tree_content_cow = tree_cow.get_content().await?;

        inspector.root().record_int("new", 6);

        // A tree that cow's the vmo doesn't see the new int
        let hierarchy = parse_content(tree_content_cow)?;
        assert_data_tree!(hierarchy, root: {
            a: 1i64,
        });

        // A tree that live-duplicates the vmo sees the new int
        let hierarchy = parse_content(tree_content_live)?;
        assert_data_tree!(hierarchy, root: {
            a: 1i64,
            new: 6i64,
        });
        Ok(())
    }

    #[fuchsia::test]
    async fn read_hanging_lazy_node() -> Result<(), Error> {
        let inspector = Inspector::default();
        let root = inspector.root();
        root.record_string("child", "value");

        root.record_lazy_values("lazy-node-always-hangs", || {
            async move {
                fuchsia_async::Timer::new(zx::MonotonicDuration::from_minutes(30).after_now())
                    .await;
                Ok(Inspector::default())
            }
            .boxed()
        });

        root.record_int("int", 3);

        let (_server, proxy) = spawn_server_proxy(inspector, TreeServerSendPreference::default())?;
        let result = read_with_timeout(&proxy, Duration::from_secs(5)).await?;
        assert_json_diff!(result, root: {
            child: "value",
            int: 3i64,
        });

        Ok(())
    }

    async fn verify_iterator(
        name_iterator: TreeNameIteratorProxy,
        values: Vec<String>,
    ) -> Result<(), Error> {
        if !values.is_empty() {
            assert_eq!(name_iterator.get_next().await?, values);
        }
        assert!(name_iterator.get_next().await?.is_empty());
        assert!(name_iterator.get_next().await.is_err());
        Ok(())
    }

    fn parse_content(tree_content: TreeContent) -> Result<DiagnosticsHierarchy, Error> {
        let buffer = tree_content.buffer.unwrap();
        Ok(PartialNodeHierarchy::try_from(&buffer.vmo)?.into())
    }

    fn test_inspector() -> Inspector {
        let inspector = Inspector::default();
        inspector.root().record_int("a", 1);
        inspector.root().record_lazy_child("lazy", || {
            async move {
                let inspector = Inspector::default();
                inspector.root().record_uint("b", 2);
                inspector.root().record_lazy_values("lazy-vals", || {
                    async move {
                        let inspector = Inspector::default();
                        inspector.root().record_double("c", 3.0);
                        Ok(inspector)
                    }
                    .boxed()
                });
                Ok(inspector)
            }
            .boxed()
        });
        inspector
    }
}
