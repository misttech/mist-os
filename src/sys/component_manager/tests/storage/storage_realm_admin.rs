// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use component_events::events::*;
use component_events::matcher::*;
use fidl::endpoints::{create_endpoints, ClientEnd};
use fuchsia_component::client::connect_to_protocol;
use futures::TryStreamExt;
use maplit::hashset;
use std::collections::HashSet;
use {fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys};

#[fuchsia::main]
async fn main() {
    // This must be the component ID for `storage_user_with_instance_id`
    // in `component_id_index_for_debug.json5`.
    let component_storage_id = "30f79a42f42300a635c8e04f92002e992368a4947199244554cdb5ec0c023be0";
    assert_eq!(component_storage_id.len(), fsys::MAX_STORAGE_ID_LENGTH as usize);

    // Wait for storage_user to stop, ensuring the storage contains the written file.
    let mut event_stream = EventStream::open().await.unwrap();
    EventMatcher::ok()
        .moniker("./storage_user_with_instance_id")
        .wait::<Stopped>(&mut event_stream)
        .await
        .unwrap();

    let storage_admin = connect_to_protocol::<fsys::StorageAdminMarker>().unwrap();

    // Open the storage by ID.
    let (node_client_end, node_server) = create_endpoints::<fio::NodeMarker>();
    let directory = ClientEnd::<fio::DirectoryMarker>::new(node_client_end.into_channel());
    let dir_proxy = directory.into_proxy();
    storage_admin
        .open_component_storage_by_id(&component_storage_id, node_server)
        .await
        .expect("failed to call OpenComponentStorageById")
        .expect("OpenComponentStorageById returned an error");

    // The storage should contain the file written by `storage_user_with_instance_id`.
    let filename = "hippo";
    let expected_contents = "hippos_are_neat";

    let filenames: HashSet<_> = fuchsia_fs::directory::readdir_recursive(&dir_proxy, None)
        .map_ok(|dir_entry| dir_entry.name)
        .try_collect()
        .await
        .expect("Error reading directory");
    assert_eq!(filenames, hashset! {filename.to_string()});
    let file =
        fuchsia_fs::directory::open_file_async(&dir_proxy, &filename, fio::PERM_READABLE).unwrap();
    assert_eq!(
        fuchsia_fs::file::read_to_string(&file).await.unwrap(),
        expected_contents.to_string()
    );
}
