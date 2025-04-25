// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_io as fio;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{ChildOptions, ChildRef, RealmBuilder};
use futures::prelude::*;
use tempfile::TempDir;
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
                let proxy = vfs::directory::serve(
                    config_data_dir.clone(),
                    fio::PERM_READABLE | fio::PERM_WRITABLE,
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

pub struct TestFs {
    cache: TempDir,
}

impl TestFs {
    pub fn new() -> Self {
        Self { cache: TempDir::new().expect("Failed to create temporary directory") }
    }

    pub fn cache(&self) -> &str {
        self.cache.path().to_str().context("Failed to convert path to string").unwrap()
    }

    /// Add a mock component that provides the `cache` directory to the realm.
    pub async fn serve_cache(&self, builder: &RealmBuilder) -> Result<ChildRef, Error> {
        let cache_path = self.cache().to_string();

        builder
            .add_local_child(
                "cache-server",
                move |handles| {
                    let cache_dir_proxy = fuchsia_fs::directory::open_in_namespace(
                        &cache_path,
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
            .await
            .context("Failed to add cache-server")
    }
}
