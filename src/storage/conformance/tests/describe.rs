// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

#[fuchsia::test]
async fn directory_query() {
    let harness = TestHarness::new().await;
    let dir = harness.get_directory(vec![], fio::Flags::empty());

    {
        let dir = open_node::<fio::DirectoryMarker>(
            &dir,
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DIRECTORY,
            ".",
        )
        .await;

        let protocol = dir.query().await.expect("query failed");
        assert_eq!(std::str::from_utf8(&protocol), Ok(fio::NODE_PROTOCOL_NAME));
    }
    {
        let dir = open_node::<fio::DirectoryMarker>(&dir, fio::OpenFlags::DIRECTORY, ".").await;

        let protocol = dir.query().await.expect("query failed");
        assert_eq!(std::str::from_utf8(&protocol), Ok(fio::DIRECTORY_PROTOCOL_NAME));
    }
}

#[fuchsia::test]
async fn file_query() {
    let harness = TestHarness::new().await;

    let entries = vec![file(TEST_FILE, vec![])];
    let dir = harness.get_directory(entries, fio::Flags::empty());
    {
        let file = open_node::<fio::FileMarker>(
            &dir,
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::NOT_DIRECTORY,
            TEST_FILE,
        )
        .await;

        let protocol = file.query().await.expect("query failed");
        assert_eq!(std::str::from_utf8(&protocol), Ok(fio::NODE_PROTOCOL_NAME));
    }
    {
        let file =
            open_node::<fio::FileMarker>(&dir, fio::OpenFlags::NOT_DIRECTORY, TEST_FILE).await;

        let protocol = file.query().await.expect("query failed");
        assert_eq!(std::str::from_utf8(&protocol), Ok(fio::FILE_PROTOCOL_NAME));
    }
}
