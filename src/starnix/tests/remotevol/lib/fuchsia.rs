// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::ArchiveReader;
use fidl::endpoints::create_proxy;
use fuchsia_async::DurationExt;
use fuchsia_component_test::RealmInstance;
use log::info;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

pub const PROGRAM_COLLECTION: &str = "debian_programs";

pub async fn open_sysrq_trigger(realm: &RealmInstance) -> fio::FileProxy {
    info!("opening sysrq trigger");
    fuchsia_fs::directory::open_file(
        realm.root.get_exposed_dir(),
        "/fs_root/proc/sysrq-trigger",
        fuchsia_fs::PERM_WRITABLE,
    )
    .await
    .unwrap()
}

pub async fn wait_for_starnix_volume_to_be_mounted() {
    let test_fxfs_inspect =
        ArchiveReader::inspect().select_all_for_component("test-fxfs").snapshot().await.unwrap();
    loop {
        fasync::Timer::new(fasync::MonotonicDuration::from_millis(100).after_now()).await;
        if test_fxfs_inspect.len() == 0 {
            continue;
        }
        let payload = test_fxfs_inspect[0].payload.as_ref().unwrap();
        if let Some(child) = payload.get_child("starnix_volume") {
            if child.get_property("mounted").and_then(|p| p.boolean()) == Some(true) {
                break;
            }
        } else {
            continue;
        }
    }
}

pub async fn get_storage_for_component_instance(
    moniker_prefix: &str,
    storage_admin: fidl_fuchsia_sys2::StorageAdminProxy,
) -> fio::DirectoryProxy {
    let (storage_user_iterator, storage_user_iterator_server_end) =
        create_proxy::<fidl_fuchsia_sys2::StorageIteratorMarker>();
    storage_admin
        .list_storage_in_realm(".", storage_user_iterator_server_end)
        .await
        .unwrap()
        .unwrap();
    let mut matching_storage_users = vec![];
    loop {
        let chunk = storage_user_iterator.next().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        let mut matches: Vec<String> = chunk
            .into_iter()
            .filter(|moniker| {
                info!("moniker is {moniker}");
                moniker.starts_with(moniker_prefix)
            })
            .collect();
        matching_storage_users.append(&mut matches);
    }
    assert!(!matching_storage_users.is_empty());
    let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
    storage_admin
        .open_storage(matching_storage_users.first().unwrap(), server_end.into_channel().into())
        .await
        .unwrap()
        .unwrap();
    proxy
}
