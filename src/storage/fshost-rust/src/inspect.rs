// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error},
    fidl_fuchsia_io::DirectoryProxy,
    fidl_fuchsia_io::OpenFlags,
    fuchsia_fs::directory::{open_directory, open_file, readdir, DirentKind},
    fuchsia_inspect, fuchsia_zircon as zx,
    futures::{future::BoxFuture, FutureExt, TryFutureExt},
};

pub async fn register_stats(root: &fuchsia_inspect::Node, data_dir: DirectoryProxy) {
    root.record_lazy_child("data_stats", move || {
        let data_dir = Clone::clone(&data_dir);
        async move {
            let inspector = fuchsia_inspect::Inspector::default();
            let root = inspector.root();

            // Filesystem info stats
            let stats_node = root.create_child("stats");
            record_filesystem_info(&stats_node, &data_dir).await?;
            root.record(stats_node);

            // Tree size stats
            let tree_size_node = root.create_child("data");
            let _total_size = record_tree_size(&tree_size_node, data_dir).await?;
            root.record(tree_size_node);

            Ok(inspector)
        }
        .or_else(|e: Error| async move {
            let inspector = fuchsia_inspect::Inspector::default();
            let root = inspector.root();
            root.record_string(
                "error",
                format!("Failed to collect fshost inspect metrics: {:?}", e),
            );
            Ok(inspector)
        })
        .boxed()
    });
}

/// Records filesystem information
async fn record_filesystem_info(
    stats_node: &fuchsia_inspect::Node,
    data_dir: &DirectoryProxy,
) -> Result<(), Error> {
    let (status, info_wrapped) =
        data_dir.query_filesystem().await.context("Transport error on query_filesystem")?;
    zx::Status::ok(status).context("query_filesystem failed")?;
    let info = info_wrapped.ok_or(anyhow!("failed to get filesystem info"))?;
    stats_node.record_uint("fvm_free_bytes", info.free_shared_pool_bytes);
    stats_node.record_uint("allocated_inodes", info.total_nodes);
    stats_node.record_uint("used_inodes", info.used_nodes);
    // Total bytes is the size of the partition plus the size it could conceivably grow into.
    // TODO(fxbug.dev/84626): Remove this misleading metric.
    stats_node.record_uint("total_bytes", info.total_bytes + info.free_shared_pool_bytes);
    stats_node.record_uint("allocated_bytes", info.total_bytes);
    stats_node.record_uint("used_bytes", info.used_bytes);
    Ok(())
}

/// Records metrics for the size of each file/directory in the data filesystem's directory hierarchy
fn record_tree_size<'a>(
    node: &'a fuchsia_inspect::Node,
    dir_proxy: DirectoryProxy,
) -> BoxFuture<'a, Result<u64, Error>> {
    async move {
        let mut total_size = 0;
        for dir_entry in readdir(&dir_proxy).await?.iter() {
            match dir_entry.kind {
                DirentKind::File => {
                    let child = node.create_child(&dir_entry.name);
                    let file_proxy =
                        open_file(&dir_proxy, &dir_entry.name, OpenFlags::RIGHT_READABLE).await?;
                    let (status, attrs) = file_proxy.get_attr().await?;
                    zx::Status::ok(status)?;
                    child.record_uint("size", attrs.content_size);
                    total_size += attrs.content_size as u64;
                    node.record(child);
                }
                DirentKind::Directory => {
                    let child = node.create_child(&dir_entry.name);
                    let directory_proxy =
                        open_directory(&dir_proxy, &dir_entry.name, OpenFlags::RIGHT_READABLE)
                            .await
                            .context("open_directory failed")?;
                    let size = record_tree_size(&child, directory_proxy).await?;
                    total_size += size as u64;
                    node.record(child);
                }
                _ => continue,
            }
        }
        node.record_uint("size", total_size);
        Ok(total_size)
    }
    .boxed()
}
