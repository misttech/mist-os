// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_io as fio;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{ChildOptions, ChildRef, RealmBuilder};
use futures::prelude::*;
use std::fs;
use vfs::directory::{spawn_directory_with_options, DirectoryOptions};
use vfs::file::vmo::read_only;
use vfs::pseudo_directory;
use vfs::test_utils::assertions::reexport::StreamExt;

pub(crate) async fn create_config_data(builder: &RealmBuilder) -> Result<ChildRef, Error> {
    let config_data_dir = pseudo_directory! {
        "data" => pseudo_directory! {
            "test_config.persist" =>
                read_only(include_str!("test_data/config/test_config.persist")),
        }
    };

    // Add a mock component that provides the `config-data` directory to the realm.
    Ok(builder
        .add_local_child(
            "config-data-server",
            move |handles| {
                let proxy = spawn_directory_with_options(
                    config_data_dir.clone(),
                    DirectoryOptions::new(fio::R_STAR_DIR | fio::W_STAR_DIR),
                );
                async move {
                    let mut fs = ServiceFs::new();
                    fs.add_remote("config", proxy);
                    fs.serve_connection(handles.outgoing_dir)
                        .expect("failed to serve config-data ServiceFs");
                    fs.collect::<()>().await;
                    Ok::<(), anyhow::Error>(())
                }
                .boxed()
            },
            ChildOptions::new(),
        )
        .await?)
}

/// Create a /cache directory under /tmp so we can serve it as a directory to supply a "/cache"
/// directory to persistence in create_cache_server().
pub(crate) fn setup_backing_directories() {
    let path = "/tmp/cache";
    fs::create_dir_all(path)
        .map_err(|err| tracing::warn!(%path, ?err, "Could not create directory"))
        .ok();
}

pub(crate) async fn create_cache_server(builder: &RealmBuilder) -> Result<ChildRef, Error> {
    // Add a mock component that provides the `cache` directory to the realm.
    //let cache_server =
    Ok(builder
        .add_local_child(
            "cache-server",
            move |handles| {
                let cache_dir_proxy = fuchsia_fs::directory::open_in_namespace(
                    "/tmp/cache",
                    fio::PERM_READABLE | fio::PERM_WRITABLE,
                )
                .unwrap();
                async move {
                    let mut fs = ServiceFs::new();
                    fs.add_remote("cache", cache_dir_proxy);
                    fs.serve_connection(handles.outgoing_dir)
                        .expect("failed to serve cache ServiceFs");
                    fs.collect::<()>().await;
                    Ok::<(), anyhow::Error>(())
                }
                .boxed()
            },
            ChildOptions::new(),
        )
        .await?)
}
