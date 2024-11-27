// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]

use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_pkg_test::{RealmFactoryMarker, RealmOptions};
use fio::DirectoryMarker;
use fuchsia_component::client::connect_to_protocol;
use {fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio};

mod directory;
mod file;
mod node;

fn repeat_by_n(seed: char, n: usize) -> String {
    std::iter::repeat(seed).take(n).collect()
}

async fn dirs_to_test() -> impl Iterator<Item = PackageSource> {
    // Bind to parent to ensure driver test realm is started
    let _ = connect_to_protocol::<fcomponent::BinderMarker>().unwrap();
    let realm_factory =
        connect_to_protocol::<RealmFactoryMarker>().expect("connect to realm_factory");
    let (directory, server_end) = fidl::endpoints::create_proxy::<DirectoryMarker>();
    realm_factory
        .create_realm(RealmOptions { pkg_directory_server: Some(server_end), ..Default::default() })
        .await
        .expect("create_realm fidl failed")
        .expect("create_realm failed");

    let connect = || async move { PackageSource { dir: directory } };
    IntoIterator::into_iter([connect().await])
}

struct PackageSource {
    dir: fio::DirectoryProxy,
}

// TODO(https://fxbug.dev/42056856): Once all fuchsia.io servers have transitioned to report
// the discoverable protocol names, remove the hard-coded *_PROTOCOL_NAME constants below.
const NODE_PROTOCOL_NAMES: [&str; 2] = [fio::NodeMarker::PROTOCOL_NAME, fio::NODE_PROTOCOL_NAME];
const FILE_PROTOCOL_NAMES: [&str; 2] = [fio::FileMarker::PROTOCOL_NAME, fio::FILE_PROTOCOL_NAME];
const DIRECTORY_PROTOCOL_NAMES: [&str; 2] =
    [fio::DirectoryMarker::PROTOCOL_NAME, fio::DIRECTORY_PROTOCOL_NAME];
