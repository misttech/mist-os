// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::stream::TryStreamExt as _,
    io_conformance_util::{test_harness::TestHarness, *},
};

#[fuchsia::test]
async fn enumerate_directory_unsupported() {
    let harness = TestHarness::new().await;

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    // fuchsia.io/Directory.Enumerate
    let (enumerate_proxy, enumerate_server) =
        fidl::endpoints::create_proxy::<fio::DirectoryIteratorMarker>().unwrap();
    dir_proxy.enumerate(&fio::DirectoryEnumerateOptions::default(), enumerate_server).unwrap();
    assert_matches!(
        enumerate_proxy.take_event_stream().try_next().await,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
    );
}
